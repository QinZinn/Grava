//! Raw PCM from a named pipe, such as the MPD `fifo` audio output:
//!
//! ```text
//! audio_output {
//!     type   "fifo"
//!     name   "grava"
//!     path   "/tmp/mpd.fifo"
//!     format "48000:16:2"
//! }
//! ```
//!
//! Then set `[input] method = "fifo"` with `sample_rate = 48000`,
//! `sample_format = "s16le"` and `channels = 2`.

use std::{
    fs::File,
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use rtrb::Producer;

use crate::config::schema::InputConfig;

use super::{AudioError, AudioFormat, Capture, pcm};

/// The path used when `source = "auto"`.
pub const DEFAULT_PATH: &str = "/tmp/mpd.fifo";

pub fn resolve_path(source: &str) -> PathBuf {
    if source == "auto" {
        PathBuf::from(DEFAULT_PATH)
    } else {
        PathBuf::from(source)
    }
}

/// Fail early, with advice, when the path cannot be a PCM source.
pub fn check(path: &Path) -> Result<(), AudioError> {
    let metadata = std::fs::metadata(path).map_err(|source| {
        AudioError::io(
            format!(
                "cannot use {} as a PCM source (create a pipe with `mkfifo {}` or set input.source)",
                path.display(),
                path.display()
            ),
            source,
        )
    })?;
    let kind = metadata.file_type();
    if kind.is_fifo() || kind.is_file() {
        Ok(())
    } else {
        Err(AudioError::Unsupported(format!(
            "{} is neither a named pipe nor a regular file",
            path.display()
        )))
    }
}

pub struct FifoCapture {
    path: PathBuf,
    format: AudioFormat,
    decoder: pcm::PcmDecoder,
}

impl FifoCapture {
    pub fn new(path: PathBuf, config: &InputConfig) -> Self {
        Self {
            path,
            format: AudioFormat {
                sample_rate: config.sample_rate,
                channels: config.channels,
            },
            decoder: pcm::PcmDecoder::from_config(config),
        }
    }
}

impl Capture for FifoCapture {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn label(&self) -> String {
        format!("fifo: {}", self.path.display())
    }

    /// Opening a pipe blocks until a writer appears, and reading ends when the
    /// last writer leaves. Reopening in a loop therefore survives restarts of
    /// the player; a regular file is simply read once.
    fn run(&mut self, producer: &mut Producer<f32>, stop: &AtomicBool) -> Result<(), AudioError> {
        let open_error =
            |source| AudioError::io(format!("cannot open {}", self.path.display()), source);
        while !stop.load(Ordering::Relaxed) {
            let mut file = File::open(&self.path).map_err(open_error)?;
            let is_pipe = file
                .metadata()
                .map(|metadata| metadata.file_type().is_fifo())
                .unwrap_or(false);
            pcm::pump(&mut file, &mut self.decoder, producer, stop).map_err(|source| {
                AudioError::io(format!("cannot read {}", self.path.display()), source)
            })?;
            if !is_pipe {
                break;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn auto_selects_the_mpd_default_and_explicit_paths_are_kept() {
        assert_eq!(resolve_path("auto"), PathBuf::from("/tmp/mpd.fifo"));
        assert_eq!(
            resolve_path("/run/audio.pcm"),
            PathBuf::from("/run/audio.pcm")
        );
    }

    #[test]
    fn missing_paths_and_directories_are_reported_with_advice() {
        let error = check(Path::new("/definitely/not/here.fifo")).unwrap_err();
        assert!(error.to_string().contains("mkfifo"), "{error}");
        let error = check(Path::new("/tmp")).unwrap_err();
        assert!(error.to_string().contains("named pipe"), "{error}");
        check(Path::new("/dev/null")).unwrap_err();
        check(Path::new("/proc/self/exe")).unwrap();
    }

    #[test]
    fn a_regular_file_is_read_once_through_the_capture_thread() {
        let directory = std::env::temp_dir().join(format!("grava-fifo-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("tone.pcm");
        let bytes: Vec<u8> = (0..500_i16).flat_map(i16::to_le_bytes).collect();
        std::fs::write(&path, bytes).unwrap();
        let config = InputConfig {
            sample_rate: 8_000,
            channels: 1,
            ..InputConfig::default()
        };
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(4096);
        FifoCapture::new(path, &config)
            .run(&mut producer, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(consumer.slots(), 500);
        assert_eq!(consumer.pop().unwrap(), 0.0);
        assert!((consumer.pop().unwrap() - 1.0 / 32_768.0).abs() < 1e-9);
        std::fs::remove_dir_all(&directory).unwrap();
    }

    /// Create a named pipe with the system `mkfifo`, or skip where it is missing.
    fn make_fifo(path: &Path) -> bool {
        std::process::Command::new("mkfifo")
            .arg(path)
            .status()
            .is_ok_and(|status| status.success())
    }

    #[test]
    fn a_named_pipe_survives_its_writer_leaving_and_returning() {
        use std::io::Write;

        use crate::audio::{SampleSource, StreamSource};

        let directory = std::env::temp_dir().join(format!("grava-pipe-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("pcm.fifo");
        if !make_fifo(&path) {
            std::fs::remove_dir_all(&directory).unwrap();
            return;
        }
        check(&path).unwrap();
        let config = InputConfig {
            sample_rate: 8_000,
            channels: 1,
            ..InputConfig::default()
        };
        let mut source = StreamSource::spawn(FifoCapture::new(path.clone(), &config)).unwrap();
        let mut received = Vec::new();
        for round in 0..2 {
            // Each writer opens the pipe, sends 100 samples, and leaves.
            let bytes: Vec<u8> = (0..100_i16)
                .flat_map(|i| (i + round * 1000).to_le_bytes())
                .collect();
            let mut writer = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
            writer.write_all(&bytes).unwrap();
            drop(writer);
            for _ in 0..400 {
                source.pull(0.016, &mut received).unwrap();
                if received.len() >= 100 * (round as usize + 1) {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        }
        assert_eq!(received.len(), 200);
        assert!((received[99] - 99.0 / 32_768.0).abs() < 1e-9);
        assert!((received[100] - 1000.0 / 32_768.0).abs() < 1e-9);
        assert!(source.connected());
        // The reader thread is blocked in `open` waiting for a third writer;
        // dropping the source must not hang on it.
        drop(source);
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
