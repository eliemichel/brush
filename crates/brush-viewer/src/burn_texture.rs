use std::sync::Arc;

use brush_render::PrimaryBackend;
use burn::tensor::{Int, Tensor};
use burn_wgpu::{JitTensor, WgpuRuntime};
use eframe::egui_wgpu::Renderer;
use egui::epaint::mutex::RwLock as EguiRwLock;
use egui::TextureId;
use wgpu::ImageDataLayout;

fn copy_buffer_to_texture(
    img: JitTensor<WgpuRuntime, i32>,
    texture: &wgpu::Texture,
    encoder: &mut wgpu::CommandEncoder,
) {
    let [height, width, c] = img.shape.dims();

    let padded_shape = vec![height, width.div_ceil(64) * 64, c];

    // Create padded tensor if needed. The bytes_per_row needs to be divisible
    // by 256 in WebGPU, so 4 bytes per pixel means width needs to be disible by 64.
    let padded = if width % 64 != 0 {
        let padded = Tensor::<PrimaryBackend, 3, Int>::zeros(&padded_shape, &img.device);
        let padded = padded.slice_assign([0..height, 0..width], Tensor::from_primitive(img));
        padded.into_primitive()
    } else {
        img
    };

    padded.client.sync(burn::tensor::backend::SyncType::Flush);
    let img_res = padded.client.get_resource(padded.handle.clone().binding());

    // Put compute passes in encoder before copying the buffer.
    let bytes_per_row = Some(4 * padded_shape[1] as u32);

    // Now copy the buffer to the texture.
    encoder.copy_buffer_to_texture(
        wgpu::ImageCopyBuffer {
            buffer: img_res.resource().buffer.as_ref(),
            layout: ImageDataLayout {
                offset: img_res.resource().offset(),
                bytes_per_row,
                rows_per_image: None,
            },
        },
        wgpu::ImageCopyTexture {
            texture,
            mip_level: 0,
            origin: wgpu::Origin3d { x: 0, y: 0, z: 0 },
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::Extent3d {
            width: width as u32,
            height: height as u32,
            depth_or_array_layers: 1,
        },
    );
}

struct TextureState {
    texture: wgpu::Texture,
    id: TextureId,
}

pub struct BurnTexture {
    state: Option<TextureState>,
}

fn create_texture(size: glam::UVec2, device: &wgpu::Device) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some("Splat backbuffer"),
        size: wgpu::Extent3d {
            width: size.x,
            height: size.y,
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::Rgba8UnormSrgb],
    })
}

impl BurnTexture {
    pub fn new() -> Self {
        Self { state: None }
    }

    pub fn update_texture(
        &mut self,
        tensor: Tensor<PrimaryBackend, 3>,
        device: &wgpu::Device,
        renderer: Arc<EguiRwLock<Renderer>>,
        encoder: &mut wgpu::CommandEncoder,
    ) -> TextureId {
        let [h, w, _] = tensor.shape().dims();
        let size = glam::uvec2(w as u32, h as u32);

        let dirty = if let Some(s) = self.state.as_ref() {
            s.texture.width() != size.x || s.texture.height() != size.y
        } else {
            true
        };

        if dirty {
            let texture = create_texture(glam::uvec2(w as u32, h as u32), device);

            if let Some(s) = self.state.as_mut() {
                s.texture = texture;

                renderer.write().update_egui_texture_from_wgpu_texture(
                    device,
                    &s.texture.create_view(&Default::default()),
                    wgpu::FilterMode::Linear,
                    s.id,
                );
            } else {
                let id = renderer.write().register_native_texture(
                    device,
                    &texture.create_view(&Default::default()),
                    wgpu::FilterMode::Linear,
                );

                self.state = Some(TextureState { texture, id });
            }
        }

        let Some(s) = self.state.as_ref() else {
            unreachable!("Somehow failed to initialize")
        };

        let prim_tensor = tensor.into_primitive().tensor();

        copy_buffer_to_texture(
            brush_kernel::bitcast_tensor(prim_tensor),
            &s.texture,
            encoder,
        );
        s.id
    }

    pub(crate) fn id(&self) -> Option<TextureId> {
        self.state.as_ref().map(|s| s.id)
    }
}
