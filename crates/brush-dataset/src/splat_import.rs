use std::collections::HashSet;

use async_fn_stream::try_fn_stream;
use brush_render::{render::rgb_to_sh, Backend};
use glam::{Quat, Vec3};
use ply_rs::{
    parser::Parser,
    ply::{Property, PropertyAccess},
};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio_stream::Stream;
use tracing::trace_span;

use anyhow::Result;
use brush_render::gaussian_splats::Splats;

pub(crate) struct GaussianData {
    pub(crate) means: Vec3,
    pub(crate) scale: Vec3,
    pub(crate) opacity: f32,
    pub(crate) rotation: Quat,
    pub(crate) sh_dc: [f32; 3],
    // NB: This is in the inria format, aka [channels, coeffs]
    // not [coeffs, channels].
    pub(crate) sh_coeffs_rest: Vec<f32>,
}

impl PropertyAccess for GaussianData {
    fn new() -> Self {
        GaussianData {
            means: Vec3::ZERO,
            scale: Vec3::ZERO,
            opacity: 0.0,
            rotation: Quat::IDENTITY,
            sh_dc: [0.0, 0.0, 0.0],
            sh_coeffs_rest: Vec::new(),
        }
    }

    fn set_property(&mut self, key: &str, property: Property) {
        let ascii = key.as_bytes();

        if let Property::Float(value) = property {
            match ascii {
                b"x" => self.means[0] = value,
                b"y" => self.means[1] = value,
                b"z" => self.means[2] = value,
                b"scale_0" => self.scale[0] = value,
                b"scale_1" => self.scale[1] = value,
                b"scale_2" => self.scale[2] = value,
                b"opacity" => self.opacity = value,
                b"rot_0" => self.rotation.w = value,
                b"rot_1" => self.rotation.x = value,
                b"rot_2" => self.rotation.y = value,
                b"rot_3" => self.rotation.z = value,
                b"f_dc_0" => self.sh_dc[0] = value,
                b"f_dc_1" => self.sh_dc[1] = value,
                b"f_dc_2" => self.sh_dc[2] = value,
                _ if key.starts_with("f_rest_") => {
                    if let Ok(idx) = key["f_rest_".len()..].parse::<u32>() {
                        if idx >= self.sh_coeffs_rest.len() as u32 {
                            self.sh_coeffs_rest.resize(idx as usize + 1, 0.0);
                        }
                        self.sh_coeffs_rest[idx as usize] = value;
                    }
                }
                _ => (),
            }
        } else if let Property::UChar(value) = property {
            match ascii {
                b"red" => self.sh_dc[0] = rgb_to_sh(value as f32 / 255.0),
                b"green" => self.sh_dc[1] = rgb_to_sh(value as f32 / 255.0),
                b"blue" => self.sh_dc[2] = rgb_to_sh(value as f32 / 255.0),
                _ => {}
            }
        } else {
            return;
        };
    }

    fn get_float(&self, key: &str) -> Option<f32> {
        let ascii = key.as_bytes();

        match ascii {
            b"x" => Some(self.means[0]),
            b"y" => Some(self.means[1]),
            b"z" => Some(self.means[2]),
            b"scale_0" => Some(self.scale[0]),
            b"scale_1" => Some(self.scale[1]),
            b"scale_2" => Some(self.scale[2]),
            b"opacity" => Some(self.opacity),
            b"rot_0" => Some(self.rotation.w),
            b"rot_1" => Some(self.rotation.x),
            b"rot_2" => Some(self.rotation.y),
            b"rot_3" => Some(self.rotation.z),
            b"f_dc_0" => Some(self.sh_dc[0]),
            b"f_dc_1" => Some(self.sh_dc[1]),
            b"f_dc_2" => Some(self.sh_dc[2]),
            _ if key.starts_with("f_rest_") => {
                if let Ok(idx) = key["f_rest_".len()..].parse::<usize>() {
                    self.sh_coeffs_rest.get(idx).copied()
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

fn interleave_coeffs(sh_dc: [f32; 3], sh_rest: &[f32]) -> Vec<f32> {
    let channels = 3;
    let coeffs_per_channel = sh_rest.len() / channels;
    let mut result = Vec::with_capacity(sh_rest.len() + 3);
    result.extend(sh_dc);

    for i in 0..coeffs_per_channel {
        for j in 0..channels {
            let index = j * coeffs_per_channel + i;
            result.push(sh_rest[index]);
        }
    }
    result
}

pub fn load_splat_from_ply<T: AsyncRead + Unpin + 'static, B: Backend>(
    reader: T,
    subsample_points: Option<u32>,
    device: B::Device,
) -> impl Stream<Item = Result<Splats<B>>> + 'static {
    // set up a reader, in this case a file.
    let mut reader = BufReader::new(reader);

    let update_every = 25000;
    let _span = trace_span!("Read splats").entered();
    let gaussian_parser = Parser::<GaussianData>::new();

    try_fn_stream(|emitter| async move {
        let header = gaussian_parser.read_header(&mut reader).await?;

        for element in &header.elements {
            if element.name == "vertex" {
                let properties: HashSet<_> =
                    element.properties.iter().map(|x| x.name.clone()).collect();

                if ["x", "y", "z"].into_iter().any(|p| !properties.contains(p)) {
                    Err(anyhow::anyhow!("Invalid splat ply. Missing properties!"))?
                }

                let n_sh_coeffs = (3 + element
                    .properties
                    .iter()
                    .filter_map(|x| {
                        x.name
                            .strip_prefix("f_rest_")
                            .and_then(|x| x.parse::<u32>().ok())
                    })
                    .max()
                    .unwrap_or(0)) as usize;

                let mut means = Vec::with_capacity(element.count);
                let mut scales = properties
                    .contains("scale_0")
                    .then(|| Vec::with_capacity(element.count));
                let mut rotation = properties
                    .contains("rot_0")
                    .then(|| Vec::with_capacity(element.count));
                let mut sh_coeffs = (properties.contains("f_dc_0") || properties.contains("red"))
                    .then(|| Vec::with_capacity(element.count * n_sh_coeffs));
                let mut opacity = properties
                    .contains("opacity")
                    .then(|| Vec::with_capacity(element.count));

                let mut ascii_line = String::new();

                for i in 0..element.count {
                    // Ocassionally yield.
                    if i % 500 == 0 {
                        tokio::task::yield_now().await;
                    }

                    // Occasionally send some updated splats.
                    if i % update_every == update_every - 1 {
                        let splats = Splats::from_raw(
                            means.clone(),
                            rotation.clone(),
                            scales.clone(),
                            sh_coeffs.clone(),
                            opacity.clone(),
                            &device,
                        );

                        emitter.emit(splats).await;
                    }

                    let splat = match header.encoding {
                        ply_rs::ply::Encoding::Ascii => {
                            reader.read_line(&mut ascii_line).await?;
                            let elem = gaussian_parser.read_ascii_element(&ascii_line, element)?;
                            ascii_line.clear();
                            elem
                        }
                        ply_rs::ply::Encoding::BinaryBigEndian => {
                            gaussian_parser
                                .read_big_endian_element(&mut reader, element)
                                .await?
                        }
                        ply_rs::ply::Encoding::BinaryLittleEndian => {
                            gaussian_parser
                                .read_little_endian_element(&mut reader, element)
                                .await?
                        }
                    };

                    // Doing this after first reading and parsing the points is quite wasteful, but
                    // we do need to advance the reader.
                    if let Some(subsample) = subsample_points {
                        if i % subsample as usize != 0 {
                            continue;
                        }
                    }

                    means.push(splat.means);
                    if let Some(scales) = scales.as_mut() {
                        scales.push(splat.scale);
                    }
                    if let Some(rotation) = rotation.as_mut() {
                        rotation.push(splat.rotation.normalize());
                    }
                    if let Some(opacity) = opacity.as_mut() {
                        opacity.push(splat.opacity);
                    }
                    if let Some(sh_coeffs) = sh_coeffs.as_mut() {
                        let sh_coeffs_interleaved =
                            interleave_coeffs(splat.sh_dc, &splat.sh_coeffs_rest);
                        sh_coeffs.extend(sh_coeffs_interleaved);
                    }
                }

                let splats = Splats::from_raw(means, rotation, scales, sh_coeffs, opacity, &device);

                if splats.num_splats() == 0 {
                    Err(anyhow::anyhow!("No splats found"))?;
                }

                emitter.emit(splats).await;
            }
        }

        Ok(())
    })
}
