use std::{pin::Pin, sync::Arc};

use async_fn_stream::try_fn_stream;
use async_std::{
    channel::{Receiver, Sender, TrySendError},
    stream::{Stream, StreamExt},
    task,
};
use brush_dataset::{self, splat_import, Dataset, LoadDatasetArgs, LoadInitArgs, ZipData};
use brush_render::camera::Camera;
use brush_render::gaussian_splats::Splats;
use brush_render::PrimaryBackend;
use brush_train::eval::EvalStats;
use brush_train::train::TrainStepStats;
use burn::backend::Autodiff;
use burn_wgpu::{RuntimeOptions, WgpuDevice};
use eframe::egui;
use egui::Hyperlink;
use egui_tiles::Tiles;
use glam::{Quat, Vec3};
use web_time::Instant;

use crate::{
    orbit_controls::OrbitControls,
    panels::{DatasetPanel, LoadDataPanel, ScenePanel, StatsPanel},
    train_loop::{self, TrainMessage},
    PaneType, ViewerTree,
};

struct TrainStats {
    loss: f32,
    train_image_index: usize,
}

#[derive(Clone)]
pub(crate) enum ViewerMessage {
    PickFile,
    StartLoading {
        training: bool,
    },
    /// Some process errored out, and want to display this error
    /// to the user.
    Error(Arc<anyhow::Error>),
    /// Loaded a splat from a ply file.
    ///
    /// Nb: This includes all the intermediately loaded splats.
    Splats {
        iter: u32,
        splats: Box<Splats<PrimaryBackend>>,
    },
    /// Loaded a bunch of viewpoints to train on.
    Dataset {
        data: Dataset,
    },
    /// Splat, or dataset and initial splat, are done loading.
    DoneLoading {
        training: bool,
    },
    /// Some number of training steps are done.
    TrainStep {
        stats: Box<TrainStepStats<Autodiff<PrimaryBackend>>>,
        iter: u32,
        timestamp: Instant,
    },
    /// Eval was run sucesfully with these results.
    EvalResult {
        iter: u32,
        eval: EvalStats<PrimaryBackend>,
    },
}

pub struct Viewer {
    tree: egui_tiles::Tree<PaneType>,
    tree_ctx: ViewerTree,
}

// TODO: Bit too much random shared state here.
pub(crate) struct ViewerContext {
    pub dataset: Dataset,
    pub camera: Camera,
    pub controls: OrbitControls,

    device: WgpuDevice,
    ctx: egui::Context,

    sender: Option<Sender<TrainMessage>>,
    receiver: Option<Receiver<ViewerMessage>>,
}

fn process_loop(
    device: WgpuDevice,
    train_receiver: Receiver<TrainMessage>,
    load_data_args: LoadDatasetArgs,
    load_init_args: LoadInitArgs,
) -> Pin<Box<impl Stream<Item = anyhow::Result<ViewerMessage>>>> {
    let stream = try_fn_stream(|emitter| async move {
        let _ = emitter.emit(ViewerMessage::PickFile).await;
        let picked = rrfd::pick_file().await?;

        if picked.file_name.contains(".ply") {
            let _ = emitter
                .emit(ViewerMessage::StartLoading { training: false })
                .await;
            let data = picked.data;
            let splat_stream =
                splat_import::load_splat_from_ply::<PrimaryBackend>(data, device.clone());
            let mut splat_stream = std::pin::pin!(splat_stream);
            while let Some(splats) = splat_stream.next().await {
                emitter
                    .emit(ViewerMessage::Splats {
                        iter: 0, // For viewing just use "training step 0", bit weird.
                        splats: Box::new(splats?),
                    })
                    .await;
            }
        } else if picked.file_name.contains(".zip") {
            let _ = emitter
                .emit(ViewerMessage::StartLoading { training: true })
                .await;

            let stream = train_loop::train_loop(
                ZipData::from(picked.data),
                device,
                train_receiver,
                load_data_args,
                load_init_args,
            );
            let mut stream = std::pin::pin!(stream);
            while let Some(message) = stream.next().await {
                emitter.emit(message?).await;
            }
        } else {
            anyhow::bail!("Only .ply and .zip files are supported.")
        }

        Ok(())
    });

    Box::pin(stream)
}

impl ViewerContext {
    fn new(device: WgpuDevice, ctx: egui::Context) -> Self {
        Self {
            camera: Camera::new(
                -Vec3::Z * 5.0,
                Quat::IDENTITY,
                glam::vec2(0.5, 0.5),
                glam::vec2(0.5, 0.5),
            ),
            controls: OrbitControls::new(),
            device,
            ctx,
            dataset: Dataset::empty(),
            receiver: None,
            sender: None,
        }
    }

    pub fn focus_view(&mut self, cam: &Camera) {
        self.camera = cam.clone();
        self.controls.focus = self.camera.position
            + self.camera.rotation
                * glam::Vec3::Z
                * self.dataset.train.bounds(0.0, 0.0).extent.length()
                * 0.5;
    }

    pub(crate) fn start_data_load(
        &mut self,
        load_data_args: LoadDatasetArgs,
        load_init_args: LoadInitArgs,
    ) {
        let device = self.device.clone();
        log::info!("Start data load");

        // create a channel for the train loop.
        let (train_sender, train_receiver) = async_std::channel::unbounded();

        // Create a small channel. We don't want 10 updated splats to be stuck in the queue eating up memory!
        // Bigger channels could mean the train loop spends less time waiting for the UI though.
        let (sender, receiver) = async_std::channel::bounded(1);

        self.receiver = Some(receiver);
        self.sender = Some(train_sender);

        self.dataset = Dataset::empty();
        let ctx = self.ctx.clone();

        let fut = async move {
            // Map errors to a viewer message containing thee error.
            let mut stream = process_loop(device, train_receiver, load_data_args, load_init_args)
                .map(|m| match m {
                    Ok(m) => m,
                    Err(e) => ViewerMessage::Error(Arc::new(e)),
                });

            // Loop until there are no more messages, processing is done.
            while let Some(m) = stream.next().await {
                ctx.request_repaint();

                // If channel is closed, bail.
                if sender.send(m).await.is_err() {
                    break;
                }
            }
        };

        #[cfg(target_family = "wasm")]
        {
            let fut =
                crate::timeout_future::with_timeout_yield(fut, web_time::Duration::from_millis(5));
            task::spawn_local(fut);
        }

        #[cfg(not(target_family = "wasm"))]
        {
            task::spawn(fut);
        }
    }

    pub fn send_train_message(&self, message: TrainMessage) {
        if let Some(sender) = self.sender.as_ref() {
            match sender.try_send(message) {
                Ok(_) => {}
                Err(TrySendError::Closed(_)) => {}
                Err(TrySendError::Full(_)) => {
                    unreachable!("Should use an unbounded channel for train messages.")
                }
            }
        }
    }
}

impl Viewer {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let state = cc.wgpu_render_state.as_ref().unwrap();

        // Run the burn backend on the egui WGPU device.
        let device = burn::backend::wgpu::init_existing_device(
            state.adapter.clone(),
            state.device.clone(),
            state.queue.clone(),
            // Splatting workload is much more granular, so don't want to flush as often.
            RuntimeOptions {
                tasks_max: 64,
                memory_config: burn_wgpu::MemoryConfiguration::ExclusivePages,
            },
        );

        cfg_if::cfg_if! {
            if #[cfg(target_family = "wasm")] {
                use tracing_subscriber::layer::SubscriberExt;

                let subscriber = tracing_subscriber::registry().with(tracing_wasm::WASMLayer::new(Default::default()));
                tracing::subscriber::set_global_default(subscriber)
                    .expect("Failed to set tracing subscriber");
            } else if #[cfg(feature = "tracy")] {
                use tracing_subscriber::layer::SubscriberExt;
                let subscriber = tracing_subscriber::registry()
                    .with(tracing_tracy::TracyLayer::default())
                    .with(sync_span::SyncLayer::new(device.clone()));
                tracing::subscriber::set_global_default(subscriber)
                    .expect("Failed to set tracing subscriber");
            }
        }

        let mut tiles: Tiles<PaneType> = egui_tiles::Tiles::default();

        let context = ViewerContext::new(device.clone(), cc.egui_ctx.clone());

        let scene_pane = ScenePanel::new(
            state.queue.clone(),
            state.device.clone(),
            state.renderer.clone(),
        );

        #[allow(unused_mut)]
        let mut sides = vec![
            tiles.insert_pane(Box::new(LoadDataPanel::new())),
            tiles.insert_pane(Box::new(StatsPanel::new(device.clone()))),
        ];

        #[cfg(not(target_family = "wasm"))]
        {
            sides.push(tiles.insert_pane(Box::new(crate::panels::RerunPanel::new(device.clone()))));
        }

        #[cfg(feature = "tracing")]
        {
            sides.push(tiles.insert_pane(Box::new(TracingPanel::default())));
        }

        let side_panel = tiles.insert_vertical_tile(sides);
        let scene_pane_id = tiles.insert_pane(Box::new(scene_pane));
        let dataset_panel = tiles.insert_pane(Box::new(DatasetPanel::new()));

        let mut lin = egui_tiles::Linear::new(
            egui_tiles::LinearDir::Horizontal,
            vec![side_panel, scene_pane_id, dataset_panel],
        );
        lin.shares.set_share(side_panel, 0.25);

        let root = tiles.insert_container(lin);
        let tree = egui_tiles::Tree::new("my_tree", root, tiles);

        let tree_ctx = ViewerTree { context };
        Viewer { tree, tree_ctx }
    }

    fn url_button(&mut self, label: &str, url: &str, ui: &mut egui::Ui) {
        ui.add(Hyperlink::from_label_and_url(label, url).open_in_new_tab(true));
    }
}

impl eframe::App for Viewer {
    fn update(&mut self, ctx: &egui::Context, _: &mut eframe::Frame) {
        if let Some(rec) = self.tree_ctx.context.receiver.clone() {
            while let Ok(message) = rec.try_recv() {
                for (_, pane) in self.tree.tiles.iter_mut() {
                    match pane {
                        egui_tiles::Tile::Pane(pane) => {
                            pane.on_message(message.clone(), &mut self.tree_ctx.context);
                        }
                        egui_tiles::Tile::Container(_) => {}
                    }
                }

                ctx.request_repaint();
            }
        }

        egui::CentralPanel::default().show(ctx, |ui| {
            // Close when pressing escape (in a native viewer anyway).
            if ui.input(|r| r.key_pressed(egui::Key::Escape)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            self.tree.ui(&mut self.tree_ctx, ui);
        });
    }
}
