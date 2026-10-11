//! Terminal ownership and a single-write renderer for changed cells.

use std::{
    io::{self, Write},
    sync::atomic::{AtomicBool, Ordering},
};

use crossterm::{
    QueueableCommand,
    cursor::{Hide, MoveTo, Show},
    execute,
    style::{Color, ResetColor},
    terminal::{
        BeginSynchronizedUpdate, Clear, ClearType, EndSynchronizedUpdate, EnterAlternateScreen,
        LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    },
};

use super::{
    color::write_sgr,
    frame::{Cell, Frame},
};

static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);
static ALTERNATE_SCREEN: AtomicBool = AtomicBool::new(true);

/// Owns the alternate screen and raw mode, including partially failed startup.
pub struct TerminalGuard;

impl TerminalGuard {
    /// Take over the terminal. Without `alternate_screen` (the Linux console
    /// has none) the visible screen is cleared instead, and cleared again on
    /// the way out so that no picture is left behind.
    pub fn enter(alternate_screen: bool) -> io::Result<Self> {
        enable_raw_mode()?;
        ALTERNATE_SCREEN.store(alternate_screen, Ordering::SeqCst);
        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);
        let guard = Self;
        let mut stdout = io::stdout();
        if alternate_screen {
            stdout.queue(EnterAlternateScreen)?;
        }
        execute!(
            stdout,
            Hide,
            ResetColor,
            Clear(ClearType::All),
            MoveTo(0, 0)
        )?;
        Ok(guard)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore();
    }
}

/// Restore before the previous panic hook prints its diagnostic. The guard's
/// later Drop is harmless because restoration takes the active flag once.
pub fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore();
        previous(info);
    }));
}

fn restore() {
    if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        let mut stdout = io::stdout();
        let _ = execute!(stdout, EndSynchronizedUpdate, ResetColor, Show);
        if ALTERNATE_SCREEN.load(Ordering::SeqCst) {
            let _ = execute!(stdout, LeaveAlternateScreen);
        } else {
            let _ = execute!(stdout, Clear(ClearType::All), MoveTo(0, 0));
        }
        let _ = disable_raw_mode();
    }
}

/// Reuses its frame and byte buffers until terminal geometry changes.
pub struct DiffRenderer {
    previous: Vec<Cell>,
    width: u16,
    height: u16,
    bytes: Vec<u8>,
    foreground: Option<Color>,
    background: Option<Color>,
    cursor: Option<(u16, u16)>,
    invalidated: bool,
}

impl Default for DiffRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl DiffRenderer {
    pub fn new() -> Self {
        Self {
            previous: Vec::new(),
            width: 0,
            height: 0,
            bytes: Vec::new(),
            foreground: None,
            background: None,
            cursor: None,
            invalidated: true,
        }
    }

    /// Force a complete repaint after resize or external terminal damage.
    pub fn invalidate(&mut self) {
        self.invalidated = true;
        self.cursor = None;
    }

    /// Returns false without writing when the frame is unchanged.
    pub fn draw(
        &mut self,
        frame: &Frame,
        writer: &mut impl Write,
        synchronized: bool,
    ) -> io::Result<bool> {
        if self.width != frame.width() || self.height != frame.height() {
            self.width = frame.width();
            self.height = frame.height();
            self.previous.resize(frame.cells().len(), Cell::default());
            // Worst case: cursor movement, two truecolor SGRs, and a UTF-8 glyph
            // for every cell. Geometry changes may allocate; steady frames do not.
            let capacity = frame.cells().len().saturating_mul(80).saturating_add(128);
            self.bytes
                .reserve(capacity.saturating_sub(self.bytes.len()));
            self.invalidate();
        }
        if !self.invalidated && self.previous == frame.cells() {
            return Ok(false);
        }
        self.bytes.clear();
        if synchronized {
            self.bytes.queue(BeginSynchronizedUpdate)?;
        }
        if self.invalidated {
            self.bytes.queue(ResetColor)?.queue(Clear(ClearType::All))?;
            self.foreground = None;
            self.background = None;
            self.cursor = None;
        }
        for (index, cell) in frame.cells().iter().enumerate() {
            if !self.invalidated && *cell == self.previous[index] {
                continue;
            }
            let x = (index % usize::from(self.width)) as u16;
            let y = (index / usize::from(self.width)) as u16;
            if self.cursor != Some((x, y)) {
                self.bytes.queue(MoveTo(x, y))?;
            }
            if self.foreground != cell.fg {
                write_sgr(&mut self.bytes, cell.fg, true)?;
                self.foreground = cell.fg;
            }
            if self.background != cell.bg {
                write_sgr(&mut self.bytes, cell.bg, false)?;
                self.background = cell.bg;
            }
            let mut encoded = [0; 4];
            self.bytes
                .extend_from_slice(cell.character.encode_utf8(&mut encoded).as_bytes());
            self.cursor = if x + 1 < self.width {
                Some((x + 1, y))
            } else {
                None
            };
        }
        if synchronized {
            self.bytes.queue(EndSynchronizedUpdate)?;
        }
        if let Err(error) = writer.write_all(&self.bytes).and_then(|()| writer.flush()) {
            self.invalidate();
            return Err(error);
        }
        self.previous.copy_from_slice(frame.cells());
        self.invalidated = false;
        Ok(true)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Output {
        bytes: Vec<u8>,
        writes: usize,
    }

    impl Write for Output {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn writes_once_and_skips_identical_frames() {
        let mut renderer = DiffRenderer::new();
        let mut output = Output::default();
        let mut frame = Frame::new(4, 2);
        frame.cells_mut()[0].character = 'A';
        assert!(renderer.draw(&frame, &mut output, true).unwrap());
        assert_eq!(output.writes, 1);
        assert!(output.bytes.starts_with(b"\x1b[?2026h"));
        assert!(output.bytes.ends_with(b"\x1b[?2026l"));
        assert!(!renderer.draw(&frame, &mut output, true).unwrap());
        assert_eq!(output.writes, 1);

        output.bytes.clear();
        frame.cells_mut()[6].character = 'B';
        renderer.draw(&frame, &mut output, false).unwrap();
        assert_eq!(output.writes, 2);
        assert_eq!(output.bytes, b"\x1b[2;3HB");
    }

    #[test]
    fn style_changes_are_tracked_without_background_painting() {
        let mut renderer = DiffRenderer::new();
        let mut output = Output::default();
        let mut frame = Frame::new(3, 1);
        frame.cells_mut().fill(Cell {
            character: 'x',
            fg: Some(Color::Red),
            bg: None,
        });
        renderer.draw(&frame, &mut output, false).unwrap();
        let text = String::from_utf8(output.bytes.clone()).unwrap();
        // One compact SGR for the whole run of equally colored cells, and no
        // background command at all: transparent terminals stay transparent.
        assert_eq!(text.matches("\x1b[91m").count(), 1);
        assert!(!text.contains("\x1b[48;") && !text.contains("\x1b[49m"));
        let capacity = renderer.bytes.capacity();
        output.bytes.clear();
        frame.cells_mut()[0].character = 'y';
        renderer.draw(&frame, &mut output, false).unwrap();
        assert_eq!(renderer.bytes.capacity(), capacity);
        assert!(
            !String::from_utf8(output.bytes)
                .unwrap()
                .contains("\x1b[91m")
        );
    }

    #[test]
    fn resize_repaints_and_failed_writes_can_be_retried() {
        struct Failing;
        impl Write for Failing {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::other("disconnected"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut renderer = DiffRenderer::new();
        let mut frame = Frame::new(3, 2);
        let mut output = Output::default();
        assert!(renderer.draw(&frame, &mut output, true).unwrap());
        frame.cells_mut()[0].fg = Some(Color::Red);
        assert!(renderer.draw(&frame, &mut Failing, true).is_err());
        output.bytes.clear();
        assert!(renderer.draw(&frame, &mut output, true).unwrap());
        assert!(output.bytes.windows(4).any(|bytes| bytes == b"\x1b[2J"));
        output.bytes.clear();
        renderer
            .draw(&Frame::new(1, 1), &mut output, false)
            .unwrap();
        assert!(output.bytes.windows(4).any(|bytes| bytes == b"\x1b[2J"));
    }
}
