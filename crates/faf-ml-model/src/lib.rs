// The Wgpu backend's types are enormous (wgpu-core validation structs), and
// `optim.step` over a Module hits rustc's default trait-solver recursion limit.
#![recursion_limit = "256"]

//! faf-ml-model — SSD-style single-shot detector for FAF strategic icons.
//!
//! Consumes the faf-ml platform store (synthetic screenshots + JSON labels)
//! or a YOLO datagen dir (640×640 PNGs + YOLO labels, 193 classes of
//! tiny near-square sprites). Design:
//!   * VGG-ish backbone (3×3 same-pad convs + relu, stride-2 downsampling —
//!     NO BatchNorm: synthetic domain, keeps train/eval identical) with
//!     feature-map taps at strides 8/16/32/64 (80²/40²/20²/10² cells)
//!   * per-cell anchors sized to icon pixels at that stride (see `anchors`)
//!   * per-scale class head (193 classes + background) and box head
//!     (center-form offsets, ×10/×5 encoding as in d2l §14.4–14.7)
//!   * host-side anchor→GT matching (`matching` — index math, unit-tested)
//!   * masked CE + masked L1 loss (`loss`)
//!
//! The detector crate is pure ML: no web/server deps.

pub mod anchors;
pub mod data;
pub mod loss;
pub mod manager;
pub mod matching;
pub mod model;
pub mod predict;
pub mod train;

use burn::backend::{Autodiff, Wgpu};

/// Compute backend for training/inference: `Wgpu` = GPU via Vulkan (works on
/// the RTX 3090 without CUDA).
pub type B = Wgpu;

/// The same backend with autodiff enabled — used by training.
pub type AdB = Autodiff<B>;

/// Portable CPU backend pair (`TrainingConfig::cpu` on `TrainParams`). Note
/// the Int element type differs (i32 on Wgpu, i64 on NdArray) — always
/// convert through `i64::from(scalar)` / `elem()` instead of assuming a
/// concrete type.
pub type CpuB = burn::backend::NdArray<f32>;
pub type CpuAdB = Autodiff<CpuB>;
