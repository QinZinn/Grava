//! Terminal- and backend-independent audio analysis: loudness, spectral peaks,
//! pitch classes, and onsets. Everything is preallocated, so analyzing a frame
//! never allocates.

pub mod analyzer;
pub mod chroma;
pub mod loudness;
pub mod onset;
pub mod peaks;

pub use analyzer::{Analyzer, AudioFeatures};
