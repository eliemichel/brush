use std::mem::size_of;

use burn_compute::{
    channel::ComputeChannel,
    client::ComputeClient,
    server::{Binding, ComputeServer},
};

use burn::backend::wgpu::{
    Kernel, KernelSource, SourceKernel, SourceTemplate, WorkGroup, WorkgroupSize,
};

use super::shaders::*;
use bytemuck::NoUninit;
use glam::{uvec3, UVec3};
use naga_oil::compose::ShaderDefValue;
use tracing::info_span;

pub(crate) trait SplatKernel<S: ComputeServer<Kernel = Kernel>, C: ComputeChannel<S>>
where
    Self: KernelSource + Sized + Copy + Clone + 'static,
{
    const SPAN_NAME: &'static str;
    const WORKGROUP_SIZE: [u32; 3];
    type Uniforms: NoUninit;

    fn execute(
        self,
        client: &ComputeClient<S, C>,
        uniforms: Self::Uniforms,
        read_handles: &[Binding<S>],
        write_handles: &[Binding<S>],
        executions: [u32; 3],
    ) {
        let _span = info_span!("Executing", "{}", Self::SPAN_NAME).entered();

        {
            let _span = info_span!("Setup", "{}", Self::SPAN_NAME).entered();

            let exec_vec = UVec3::from_array(executions);
            let group_size = UVec3::from_array(Self::WORKGROUP_SIZE);
            let execs = uvec3(
                exec_vec.x.div_ceil(group_size.x),
                exec_vec.y.div_ceil(group_size.y),
                exec_vec.z.div_ceil(group_size.z),
            );

            let kernel = Kernel::Custom(Box::new(SourceKernel::new(
                self,
                WorkGroup::new(execs.x, execs.y, execs.z),
                WorkgroupSize::new(group_size.x, group_size.y, group_size.z),
            )));

            if size_of::<Self::Uniforms>() != 0 {
                let uniform_data = client.create(bytemuck::bytes_of(&uniforms)).binding();
                let total_handles =
                    [[uniform_data].as_slice(), read_handles, write_handles].concat();
                client.execute(kernel, total_handles);
            } else {
                let total_handles = [read_handles, write_handles].concat();
                client.execute(kernel, total_handles);
            }
        }
    }
}

#[macro_export]
macro_rules! kernel_source_gen {
    ($struct_name:ident { $($field_name:ident),* }, $module:ident, $uniforms:ty) => {
        #[derive(Debug, Copy, Clone)]
        pub(crate) struct $struct_name {
            $(
                $field_name: bool,
            )*
        }

        impl $struct_name {
            pub fn new($($field_name: bool),*) -> Self {
                Self {
                    $(
                        $field_name,
                    )*
                }
            }

            fn create_shader_hashmap(&self) -> std::collections::HashMap<String, ShaderDefValue> {
                let map = std::collections::HashMap::new();
                $(
                    let mut map = map;

                    if self.$field_name {
                        map.insert(stringify!($field_name).to_owned().to_uppercase(), ShaderDefValue::Bool(true));
                    }
                )*
                map
            }
        }

        impl KernelSource for $struct_name {
            fn source(&self) -> SourceTemplate {
                let mut composer = naga_oil::compose::Composer::default();
                let shader_defs = self.create_shader_hashmap();
                $module::load_shader_modules_embedded(
                    &mut composer,
                    &shader_defs,
                );
                let module = $module::load_naga_module_embedded(
                    &mut composer,
                    shader_defs,
                );
                let info = wgpu::naga::valid::Validator::new(
                    wgpu::naga::valid::ValidationFlags::empty(),
                    wgpu::naga::valid::Capabilities::all(),
                )
                .validate(&module)
                .unwrap();
                let shader_string = wgpu::naga::back::wgsl::write_string(
                    &module,
                    &info,
                    wgpu::naga::back::wgsl::WriterFlags::EXPLICIT_TYPES,
                )
                .expect("failed to convert naga module to source");
                SourceTemplate::new(shader_string)
            }
        }

        impl<S: ComputeServer<Kernel = Kernel>, C: ComputeChannel<S>> SplatKernel<S, C>
            for $struct_name
        {
            const SPAN_NAME: &'static str = stringify!($struct_name);
            type Uniforms = $uniforms;
            const WORKGROUP_SIZE: [u32; 3] = $module::compute::MAIN_WORKGROUP_SIZE;
        }
    };
}

kernel_source_gen!(ProjectSplats {}, project_forward, project_forward::Uniforms);
kernel_source_gen!(
    MapGaussiansToIntersect {},
    map_gaussian_to_intersects,
    map_gaussian_to_intersects::Uniforms
);
kernel_source_gen!(GetTileBinEdges {}, get_tile_bin_edges, ());
kernel_source_gen!(Rasterize { raster_u32 }, rasterize, rasterize::Uniforms);
kernel_source_gen!(
    RasterizeBackwards {},
    rasterize_backwards,
    rasterize_backwards::Uniforms
);
kernel_source_gen!(
    ProjectBackwards {},
    project_backwards,
    project_backwards::Uniforms
);

kernel_source_gen!(PrefixSumScan {}, prefix_sum_scan, ());
kernel_source_gen!(PrefixSumScanSums {}, prefix_sum_scan_sums, ());
kernel_source_gen!(PrefixSumAddScannedSums {}, prefix_sum_add_scanned_sums, ());

kernel_source_gen!(SortCount {}, sort_count, sorting::Config);
kernel_source_gen!(SortReduce {}, sort_reduce, ());
kernel_source_gen!(SortScanAdd {}, sort_scan_add, ());
kernel_source_gen!(SortScan {}, sort_scan, ());
kernel_source_gen!(SortScatter {}, sort_scatter, sorting::Config);
