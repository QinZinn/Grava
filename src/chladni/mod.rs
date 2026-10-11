//! Terminal- and backend-independent mode catalogs, vibration fields, and
//! the musical mapping from audio features to mode weights.

pub mod bessel;
pub mod circle;
pub mod field;
pub mod mapping;
pub mod modes;
pub mod square;

pub use circle::CircleMode;
pub use field::{ActiveField, FieldCache, FieldError, FieldGrid};
pub use mapping::{Mapper, ModeWeights};
pub use modes::{Mode, ModeCatalog, order_limit};
pub use square::{ModeError, SquareMode};
