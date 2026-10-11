//! Allocation-free particle counts and occupancy rasterization.

use crossterm::style::Color;

use crate::config::schema::RenderConfig;
use crate::sim::particles::Particles;

use super::{Frame, GlyphSet, PlateLayout};

/// A cell holds this many times the average grain count when it is fully
/// dense: sand heaped on a nodal line reaches it, a scatter does not.
pub const DENSITY_SATURATION: f32 = 3.0;
/// The ASCII ramp uses the same scale.
const ASCII_SATURATION: f32 = DENSITY_SATURATION;
/// The character of a ring cell with no sand on it, in the ASCII set.
const RING_ASCII: char = 'o';

/// What a colorizer may know about a non-empty plate cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlateCell {
    /// Cell column and row within the plate, from its top-left corner.
    pub x: u16,
    pub y: u16,
    /// 0..1: how heaped the sand is here.
    pub density: f32,
    /// The cell lies on the drum's circular border.
    pub ring: bool,
}

pub struct Canvas {
    layout: PlateLayout,
    glyphs: GlyphSet,
    counts: Vec<u16>,
    afterimage: Option<Vec<f32>>,
    trail: f32,
    threshold: f32,
    ascii_ramp: Vec<char>,
    particle_count: usize,
    /// The share of the layout the plate covers; a drum covers pi/4.
    plate_fraction: f32,
    /// Glyph bits of a circular border, per plate cell.
    ring: Option<Vec<u8>>,
}

impl Canvas {
    /// Buffers are allocated once, then replaced only when the geometry changes.
    pub fn new(layout: PlateLayout, config: &RenderConfig, glyphs: GlyphSet) -> Self {
        let length = layout.subpixel_width * layout.subpixel_height;
        let mut ascii_ramp: Vec<char> = config.ascii_ramp.chars().collect();
        if ascii_ramp.is_empty() {
            ascii_ramp.push(' ');
        }
        Self {
            layout,
            glyphs,
            counts: vec![0; length],
            afterimage: (config.trail > 0.0).then(|| vec![0.0; length]),
            trail: config.trail,
            threshold: config.dot_threshold,
            ascii_ramp,
            particle_count: 0,
            plate_fraction: 1.0,
            ring: None,
        }
    }

    /// Draw a ring (see `border::circle_ring`) around the sand, or clear it.
    pub fn set_ring(&mut self, ring: Option<Vec<u8>>) {
        self.ring = ring.filter(|ring| {
            ring.len() == usize::from(self.layout.width) * usize::from(self.layout.height)
        });
    }

    /// Normalize cell densities by the plate's share of the layout.
    pub fn set_plate_fraction(&mut self, fraction: f32) {
        if fraction.is_finite() && fraction > 0.0 {
            self.plate_fraction = fraction.min(1.0);
        }
    }

    pub const fn layout(&self) -> PlateLayout {
        self.layout
    }

    pub fn counts(&self) -> &[u16] {
        &self.counts
    }

    /// Positions use normalized plate coordinates. Edge positions map to the
    /// last sub-pixel; non-finite positions are ignored rather than drawing noise.
    pub fn rasterize(&mut self, x: &[f32], y: &[f32]) {
        self.rasterize_positions::<true>(x, y);
    }

    /// Rasterize simulation storage without validating each coordinate again.
    /// Particles keep their private positions finite and within `0..=1` after
    /// every update. The endpoint still maps to the final sub-pixel.
    pub fn rasterize_particles(&mut self, particles: &Particles) {
        self.rasterize_positions::<false>(particles.x(), particles.y());
    }

    fn rasterize_positions<const CHECKED: bool>(&mut self, x: &[f32], y: &[f32]) {
        self.counts.fill(0);
        self.particle_count = if CHECKED { 0 } else { x.len().min(y.len()) };
        let width = self.layout.subpixel_width;
        let height = self.layout.subpixel_height;
        if width == 0 || height == 0 {
            return;
        }
        for (&x, &y) in x.iter().zip(y) {
            let (x, y) = if CHECKED {
                if !x.is_finite() || !y.is_finite() {
                    continue;
                }
                (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))
            } else {
                (x, y)
            };
            let column = ((x * width as f32) as usize).min(width - 1);
            let row = ((y * height as f32) as usize).min(height - 1);
            let count = &mut self.counts[row * width + column];
            *count = count.saturating_add(1);
            if CHECKED {
                self.particle_count += 1;
            }
        }
        if let Some(afterimage) = &mut self.afterimage {
            for (value, count) in afterimage.iter_mut().zip(&self.counts) {
                *value = *value * self.trail + f32::from(*count);
            }
        }
    }

    fn value(&self, index: usize) -> f32 {
        match &self.afterimage {
            Some(afterimage) => afterimage[index],
            None => f32::from(self.counts[index]),
        }
    }

    /// Density relative to the expected particle count per terminal cell.
    /// Saturation controls how many times the average count becomes full density.
    pub fn cell_density(&self, x: u16, y: u16, saturation: f32) -> f32 {
        if x >= self.layout.width || y >= self.layout.height || self.particle_count == 0 {
            return 0.0;
        }
        let (dots_x, dots_y) = self.glyphs.subpixels();
        let mut total = 0.0;
        for dy in 0..dots_y {
            let row = (usize::from(y) * dots_y + dy) * self.layout.subpixel_width;
            for dx in 0..dots_x {
                total += self.value(row + usize::from(x) * dots_x + dx);
            }
        }
        let expected = self.particle_count as f32
            / (f32::from(self.layout.width) * f32::from(self.layout.height) * self.plate_fraction);
        (total / (expected * saturation.max(f32::EPSILON))).clamp(0.0, 1.0)
    }

    /// Write plate cells into an existing frame, without color. Call clear
    /// first when other regions of the frame may have changed.
    pub fn write_frame(&self, frame: &mut Frame) {
        self.write_cells(frame, |_| None);
    }

    /// Write plate cells into an existing frame. `color` picks the foreground
    /// of every non-empty cell; empty cells keep whatever background the frame
    /// already has. No storage is resized here.
    pub fn write_cells(
        &self,
        frame: &mut Frame,
        mut color: impl FnMut(PlateCell) -> Option<Color>,
    ) {
        let (dots_x, dots_y) = self.glyphs.subpixels();
        for cell_y in 0..self.layout.height {
            for cell_x in 0..self.layout.width {
                let ring_bits = self.ring.as_ref().map_or(0, |ring| {
                    ring[usize::from(cell_y) * usize::from(self.layout.width) + usize::from(cell_x)]
                });
                let character = match self.glyphs {
                    GlyphSet::Ascii => {
                        let index =
                            usize::from(cell_y) * self.layout.subpixel_width + usize::from(cell_x);
                        if self.value(index) < self.threshold {
                            if ring_bits != 0 {
                                RING_ASCII
                            } else {
                                self.ascii_ramp[0]
                            }
                        } else {
                            let density = self.cell_density(cell_x, cell_y, ASCII_SATURATION);
                            let level =
                                (density * (self.ascii_ramp.len() - 1) as f32).round() as usize;
                            self.ascii_ramp[level]
                        }
                    }
                    set => {
                        let mut bits = ring_bits;
                        for dy in 0..dots_y {
                            let row =
                                (usize::from(cell_y) * dots_y + dy) * self.layout.subpixel_width;
                            for dx in 0..dots_x {
                                if self.value(row + usize::from(cell_x) * dots_x + dx)
                                    >= self.threshold
                                {
                                    bits |= set.bit(dx, dy);
                                }
                            }
                        }
                        set.glyph(bits)
                    }
                };
                let Some(slot) = frame.cell_mut(self.layout.x + cell_x, self.layout.y + cell_y)
                else {
                    continue;
                };
                slot.character = character;
                slot.fg = (character != ' ')
                    .then(|| {
                        color(PlateCell {
                            x: cell_x,
                            y: cell_y,
                            density: self.cell_density(cell_x, cell_y, DENSITY_SATURATION),
                            ring: ring_bits != 0,
                        })
                    })
                    .flatten();
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::config::schema::LayoutConfig;
    use crate::render::layout::Margins;

    fn canvas(config: &RenderConfig) -> Canvas {
        let layout = PlateLayout::new(
            4,
            2,
            &LayoutConfig {
                padding: [0, 0],
                ..LayoutConfig::default()
            },
            0.5,
            GlyphSet::Braille,
            Margins::default(),
        )
        .unwrap();
        Canvas::new(layout, config, GlyphSet::Braille)
    }

    #[test]
    fn particle_coordinates_select_the_documented_braille_bits() {
        let mut canvas = canvas(&RenderConfig::default());
        canvas.rasterize(&[0.0, 0.99, 1.0], &[0.0, 0.99, 1.0]);
        let mut frame = Frame::new(4, 2);
        canvas.write_frame(&mut frame);
        assert_eq!(frame.cell(0, 0).unwrap().character, '⠁');
        assert_eq!(frame.cell(3, 1).unwrap().character, '⢀');
        assert_eq!(
            canvas
                .counts()
                .iter()
                .map(|value| u32::from(*value))
                .sum::<u32>(),
            3
        );
    }

    #[test]
    fn counts_saturate_and_rasterization_reuses_storage() {
        let mut canvas = canvas(&RenderConfig::default());
        let pointer = canvas.counts().as_ptr();
        let coordinates = vec![0.5; usize::from(u16::MAX) + 10];
        canvas.rasterize(&coordinates, &coordinates);
        assert_eq!(canvas.counts().iter().max(), Some(&u16::MAX));
        canvas.rasterize(&[f32::NAN], &[0.5]);
        assert!(canvas.counts().iter().all(|value| *value == 0));
        assert_eq!(canvas.counts().as_ptr(), pointer);
    }

    #[test]
    fn trail_fades_and_threshold_is_inclusive() {
        let mut canvas = canvas(&RenderConfig {
            trail: 0.5,
            dot_threshold: 1.0,
            ..RenderConfig::default()
        });
        let mut frame = Frame::new(4, 2);
        canvas.rasterize(&[0.0, 0.0], &[0.0, 0.0]);
        canvas.rasterize(&[], &[]);
        canvas.write_frame(&mut frame);
        assert_eq!(frame.cell(0, 0).unwrap().character, '⠁');
        canvas.rasterize(&[], &[]);
        canvas.write_frame(&mut frame);
        assert_eq!(frame.cell(0, 0).unwrap().character, ' ');
    }

    #[test]
    fn density_is_normalized_and_clamped() {
        let mut canvas = canvas(&RenderConfig::default());
        canvas.rasterize(&[0.0], &[0.0]);
        assert_eq!(canvas.cell_density(0, 0, 1.0), 1.0);
        assert_eq!(canvas.cell_density(1, 0, 1.0), 0.0);
        assert_eq!(canvas.cell_density(4, 0, 1.0), 0.0);
    }

    #[test]
    fn simulation_rasterization_matches_checked_coordinates() {
        let config = RenderConfig {
            trail: 0.6,
            ..RenderConfig::default()
        };
        let mut checked = canvas(&config);
        let mut trusted = canvas(&config);
        let mut particles = Particles::new(4_000, 42);
        for _ in 0..3 {
            checked.rasterize(particles.x(), particles.y());
            trusted.rasterize_particles(&particles);
            assert_eq!(checked.counts(), trusted.counts());
            assert_eq!(checked.afterimage, trusted.afterimage);
            assert_eq!(checked.particle_count, trusted.particle_count);
            let mut checked_frame = Frame::new(4, 2);
            let mut trusted_frame = Frame::new(4, 2);
            checked.write_frame(&mut checked_frame);
            trusted.write_frame(&mut trusted_frame);
            assert_eq!(checked_frame, trusted_frame);
            particles.scatter();
        }
    }

    #[test]
    fn trusted_endpoint_coordinates_map_to_the_final_subpixel() {
        let mut checked = canvas(&RenderConfig::default());
        let mut trusted = canvas(&RenderConfig::default());
        checked.rasterize(&[0.0, 1.0], &[0.0, 1.0]);
        trusted.rasterize_positions::<false>(&[0.0, 1.0], &[0.0, 1.0]);
        assert_eq!(checked.counts(), trusted.counts());
    }

    fn canvas_for(glyphs: GlyphSet, width: u16, height: u16) -> Canvas {
        let layout = PlateLayout::new(
            width,
            height,
            &LayoutConfig {
                padding: [0, 0],
                fit: crate::config::schema::Fit::Stretch,
                ..LayoutConfig::default()
            },
            0.5,
            glyphs,
            Margins::default(),
        )
        .unwrap();
        Canvas::new(layout, &RenderConfig::default(), glyphs)
    }

    #[test]
    fn quadrant_and_half_cells_light_the_sub_pixels_they_contain() {
        // A 2x2-cell plate of quadrants is 4x4 sub-pixels.
        let mut quadrant = canvas_for(GlyphSet::Quadrant, 2, 2);
        // Cell (0,0): top-left and bottom-right. Cell (1,0): top-right.
        quadrant.rasterize(&[0.1, 0.4, 0.9], &[0.1, 0.4, 0.1]);
        let mut frame = Frame::new(2, 2);
        quadrant.write_frame(&mut frame);
        assert_eq!(frame.plain_text(), "▚▝\n  \n");
        // Half blocks make 2x4 sub-pixels: one column per cell, two rows each.
        let mut half = canvas_for(GlyphSet::Half, 2, 2);
        // Cell (0,0) top; cell (0,1) bottom; cell (1,1) top and bottom.
        half.rasterize(&[0.1, 0.1, 0.9, 0.9], &[0.1, 0.9, 0.6, 0.9]);
        let mut frame = Frame::new(2, 2);
        half.write_frame(&mut frame);
        assert_eq!(frame.plain_text(), "▀ \n▄█\n");
    }

    #[test]
    fn the_colorizer_sees_only_non_empty_cells_with_their_density() {
        let mut canvas = canvas_for(GlyphSet::Braille, 4, 2);
        // Four grains in cell (1,0), one in cell (3,0).
        canvas.rasterize(&[0.3, 0.3, 0.3, 0.3, 0.9], &[0.1, 0.2, 0.3, 0.4, 0.1]);
        let mut frame = Frame::new(4, 2);
        let mut seen = Vec::new();
        canvas.write_cells(&mut frame, |cell| {
            seen.push(cell);
            Some(Color::Red)
        });
        assert_eq!(
            seen.iter().map(|cell| (cell.x, cell.y)).collect::<Vec<_>>(),
            [(1, 0), (3, 0)]
        );
        assert!(seen[0].density > seen[1].density);
        assert!(
            seen.iter()
                .all(|cell| (0.0..=1.0).contains(&cell.density) && !cell.ring)
        );
        assert_eq!(frame.cell(1, 0).unwrap().fg, Some(Color::Red));
        // Empty cells stay uncolored and keep their background.
        assert_eq!(frame.cell(0, 0).unwrap().fg, None);
        frame.cell_mut(0, 0).unwrap().bg = Some(Color::Blue);
        canvas.write_cells(&mut frame, |_| Some(Color::Green));
        assert_eq!(frame.cell(0, 0).unwrap().bg, Some(Color::Blue));
        assert_eq!(frame.cell(0, 0).unwrap().character, ' ');
        assert_eq!(frame.cell(0, 0).unwrap().fg, None);
    }

    #[test]
    fn a_ring_adds_its_bits_to_the_glyphs_and_flags_its_cells() {
        let mut canvas = canvas_for(GlyphSet::Braille, 2, 2);
        canvas.set_ring(Some(vec![0x01, 0x80, 0, 0]));
        canvas.rasterize(&[], &[]);
        let mut frame = Frame::new(2, 2);
        let mut rings = Vec::new();
        canvas.write_cells(&mut frame, |cell| {
            rings.push(cell.ring);
            Some(Color::Cyan)
        });
        assert_eq!(frame.plain_text(), "⠁⢀\n  \n");
        assert_eq!(rings, [true, true]);
        // A ring of the wrong size is ignored instead of misaligning cells.
        canvas.set_ring(Some(vec![1; 5]));
        let mut frame = Frame::new(2, 2);
        canvas.write_cells(&mut frame, |_| Some(Color::Cyan));
        assert_eq!(frame.plain_text(), "  \n  \n");
        // In ASCII a bare ring cell is a visible character.
        let mut ascii = canvas_for(GlyphSet::Ascii, 3, 2);
        ascii.set_ring(Some(vec![0, 1, 0, 0, 0, 0]));
        ascii.rasterize(&[], &[]);
        let mut frame = Frame::new(3, 2);
        ascii.write_cells(&mut frame, |_| None);
        assert_eq!(frame.plain_text(), " o \n   \n");
    }
}
