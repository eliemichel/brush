#![allow(dead_code)]
#![allow(clippy::too_many_arguments)]

use std::error::Error;
mod camera;
mod dataset_readers;
mod gaussian_splats;
mod loss_utils;
mod renderer;
mod scene;
mod spherical_harmonics;
mod train;
mod utils;

use burn::backend::{
    wgpu::{AutoGraphicsApi, Wgpu},
    Autodiff,
};

use train::TrainConfig;

fn main() -> Result<(), Box<dyn Error>> {
    let device = Default::default();
    type BackGPU = Wgpu<AutoGraphicsApi, f32, i32>;
    type DiffBack = Autodiff<BackGPU>;
    let config = TrainConfig::new("../nerf_synthetic/lego/".to_owned());
    train::train::<DiffBack>(&config, &device)?;
    Ok(())
}
