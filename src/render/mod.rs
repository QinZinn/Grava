//! Particle rasterization, cell geometry, and terminal output.

pub mod border;
pub mod canvas;
pub mod color;
pub mod colorize;
pub mod frame;
pub mod glyphs;
pub mod hud;
pub mod layout;
pub mod term;

pub use canvas::{Canvas, PlateCell};
pub use frame::{Cell, Frame};
pub use glyphs::GlyphSet;
pub use layout::PlateLayout;
