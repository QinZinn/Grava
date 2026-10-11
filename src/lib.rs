//! Reusable core of grava, a terminal Chladni sand visualizer.
//!
//! Plate and drum fields, audio analysis and the seedable sand simulation are
//! independent of audio backends and terminals. The engine and the session
//! that drives it can be run without a terminal, so every frame is testable.

#![forbid(unsafe_code)]

pub mod app;
pub mod audio;
pub mod chladni;
pub mod cli;
pub mod config;
pub mod dsp;
pub mod engine;
pub mod manual;
pub mod pace;
pub mod render;
pub mod session;
pub mod sim;
