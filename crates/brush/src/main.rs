#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::single_range_in_vec_init)]

mod burn_texture;
mod dataset_readers;
mod gaussian_splats;
mod orbit_controls;
mod scene;
mod splat_import;
mod ssim;
mod train;
mod utils;
mod viewer;
mod wgpu_config;

use viewer::Viewer;

#[cfg(feature = "tracy")]
use tracing_subscriber::layer::SubscriberExt;

fn main() -> anyhow::Result<()> {
    let wgpu_options = wgpu_config::get_config();

    #[cfg(not(target_arch = "wasm32"))]
    {
        #[cfg(feature = "tracy")]
        tracing::subscriber::set_global_default(
            tracing_subscriber::registry().with(tracing_tracy::TracyLayer::default()),
        )?;

        // Build app display.
        let native_options = eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size(egui::Vec2::new(1280.0, 720.0))
                .with_active(true),
            // Need a slightly more careful wgpu init to support burn.
            wgpu_options,
            ..Default::default()
        };
        eframe::run_native(
            "Brush 🖌️",
            native_options,
            Box::new(move |cc| Ok(Box::new(Viewer::new(cc)))),
        )
        .unwrap();
    }

    #[cfg(target_arch = "wasm32")]
    {
        tracing_wasm::set_as_global_default();

        #[cfg(debug_assertions)]
        {
            console_error_panic_hook::set_once();
            tracing_wasm::set_as_global_default();
        }

        let web_options = eframe::WebOptions {
            wgpu_options,
            ..Default::default()
        };

        wasm_bindgen_futures::spawn_local(async {
            eframe::WebRunner::new()
                .start(
                    "main_canvas", // hardcode it
                    web_options,
                    Box::new(|cc| Ok(Box::new(Viewer::new(cc)))),
                )
                .await
                .expect("failed to start eframe");
        });
    }

    Ok(())
}
