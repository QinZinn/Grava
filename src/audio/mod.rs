//! Audio sources.
//!
//! Everything downstream of this module consumes mono `f32` samples in -1..1
//! at any sample rate through the [`SampleSource`] trait. Live backends run a
//! capture thread (see [`Capture`]) that decodes to mono and pushes into a
//! lock-free ring buffer; on overflow samples are dropped, never waited for.
//!
//! Sources: the built-in demo synth, raw PCM from stdin or a FIFO, and the
//! native backends `pulse` (PulseAudio and pipewire-pulse), `pipewire` and
//! `alsa`, each behind its own Cargo feature. [`select`] chooses a backend
//! (`method = "auto"` tries pipewire, then pulse, then alsa) and lists the
//! sources of the chosen one.

pub mod demo;
pub mod fifo;
pub mod pcm;
pub mod select;
pub mod stdin;

#[cfg(feature = "alsa")]
pub mod alsa;
#[cfg(feature = "pipewire")]
pub mod pipewire;
#[cfg(feature = "pulse")]
pub mod pulse;

use std::{
    io::{self, IsTerminal},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use rtrb::{Consumer, Producer, RingBuffer};

use crate::config::schema::{InputConfig, InputMethod};

/// The negotiated format of a capture stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioFormat {
    pub sample_rate: u32,
    pub channels: u16,
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("{context}: {source}")]
    Io {
        context: String,
        #[source]
        source: io::Error,
    },
    #[error("{0}")]
    Unsupported(String),
}

impl AudioError {
    pub fn io(context: impl Into<String>, source: io::Error) -> Self {
        Self::Io {
            context: context.into(),
            source,
        }
    }
}

/// A producer of mono samples that the main loop pulls once per frame.
pub trait SampleSource {
    fn sample_rate(&self) -> u32;

    /// Append the mono samples for the next `dt` seconds to `out`.
    ///
    /// Deterministic sources (demo, headless PCM) produce exactly
    /// `dt * sample_rate` samples, padding with silence once their input ends.
    /// Live sources append whatever has arrived since the last call.
    fn pull(&mut self, dt: f32, out: &mut Vec<f32>) -> Result<(), AudioError>;

    /// False once a live input has ended or failed; the plate then rests.
    fn connected(&self) -> bool {
        true
    }
}

/// Whether a live capture currently has audio flowing from its server.
///
/// The capture thread sets it; the main loop only reads it, so a vanished
/// server is visible without any locking.
#[derive(Debug, Clone)]
pub struct Link(Arc<AtomicBool>);

impl Link {
    pub fn new(up: bool) -> Self {
        Self(Arc::new(AtomicBool::new(up)))
    }

    pub fn set(&self, up: bool) {
        self.0.store(up, Ordering::Relaxed);
    }

    pub fn is_up(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }
}

/// A live capture backend running on its own thread.
pub trait Capture: Send + 'static {
    fn format(&self) -> AudioFormat;

    /// What the HUD calls this input, such as `pulse: alsa_output...monitor`.
    fn label(&self) -> String {
        "capture".to_owned()
    }

    /// The connection state, for backends that reconnect. Captures without
    /// one (a pipe has nothing to reconnect to) are connected until they end.
    fn link(&self) -> Option<Link> {
        None
    }

    /// Block, writing mono samples into `producer`, until `stop` is set or the
    /// input ends. Never block on a full ring: drop the samples instead.
    fn run(&mut self, producer: &mut Producer<f32>, stop: &AtomicBool) -> Result<(), AudioError>;
}

impl Capture for Box<dyn Capture> {
    fn format(&self) -> AudioFormat {
        (**self).format()
    }

    fn link(&self) -> Option<Link> {
        (**self).link()
    }

    fn label(&self) -> String {
        (**self).label()
    }

    fn run(&mut self, producer: &mut Producer<f32>, stop: &AtomicBool) -> Result<(), AudioError> {
        (**self).run(producer, stop)
    }
}

/// How long a reconnecting backend waits before trying again.
pub const RETRY_INTERVAL: Duration = Duration::from_secs(2);

/// Sleep for up to `duration`, returning early (true) once `stop` is set.
pub fn sleep_unless_stopped(duration: Duration, stop: &AtomicBool) -> bool {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::Relaxed) {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return false;
        }
        std::thread::sleep(remaining.min(Duration::from_millis(50)));
    }
    true
}

/// Keep a live backend running across server restarts.
///
/// `first` is the session opened at startup, where failures were reported to
/// the user. Whenever `record` returns, with an error or because the server
/// closed the stream, the link goes down and, after `interval`, `connect`
/// builds a new session; failed attempts are retried every `interval`. This
/// returns only when `stop` is set.
pub fn supervise<S>(
    stop: &AtomicBool,
    link: &Link,
    interval: Duration,
    first: Option<S>,
    mut connect: impl FnMut() -> Result<S, AudioError>,
    mut record: impl FnMut(&mut S) -> Result<(), AudioError>,
) -> Result<(), AudioError> {
    let mut pending = first;
    while !stop.load(Ordering::Relaxed) {
        let mut session = match pending.take() {
            Some(session) => session,
            None => match connect() {
                Ok(session) => session,
                Err(_) => {
                    link.set(false);
                    if sleep_unless_stopped(interval, stop) {
                        break;
                    }
                    continue;
                }
            },
        };
        link.set(true);
        // Any end of a session but our own stop request means the server went
        // away or moved on; the reason is for the next attempt to rediscover.
        let _ = record(&mut session);
        link.set(false);
        drop(session);
        if sleep_unless_stopped(interval, stop) {
            break;
        }
    }
    Ok(())
}

/// A [`Capture`] running on a thread, drained through a ring buffer.
pub struct StreamSource {
    consumer: Consumer<f32>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result<(), AudioError>>>,
    link: Option<Link>,
    sample_rate: u32,
    ended: bool,
}

impl StreamSource {
    /// Start `capture` on a new thread with one second of ring buffer.
    pub fn spawn(mut capture: impl Capture) -> io::Result<Self> {
        let sample_rate = capture.format().sample_rate;
        let link = capture.link();
        let (mut producer, consumer) = RingBuffer::new((sample_rate as usize).max(4096));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("grava-capture".to_owned())
            .spawn(move || capture.run(&mut producer, &thread_stop))?;
        Ok(Self {
            consumer,
            stop,
            thread: Some(thread),
            link,
            sample_rate,
            ended: false,
        })
    }

    /// Collect the thread's result once it has finished.
    fn finish(&mut self) -> Result<(), AudioError> {
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(thread) = self.thread.take()
        {
            self.ended = true;
            return thread.join().unwrap_or_else(|_| {
                Err(AudioError::Unsupported(
                    "the capture thread panicked".to_owned(),
                ))
            });
        }
        Ok(())
    }
}

impl SampleSource for StreamSource {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn pull(&mut self, _dt: f32, out: &mut Vec<f32>) -> Result<(), AudioError> {
        let available = self.consumer.slots();
        if available > 0
            && let Ok(chunk) = self.consumer.read_chunk(available)
        {
            let (first, second) = chunk.as_slices();
            out.extend_from_slice(first);
            out.extend_from_slice(second);
            chunk.commit_all();
        }
        // Samples queued before the thread ended are delivered first.
        if available == 0 {
            self.finish()?;
        }
        Ok(())
    }

    fn connected(&self) -> bool {
        !self.ended && self.link.as_ref().is_none_or(Link::is_up)
    }
}

impl Drop for StreamSource {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // A thread blocked in a read cannot be woken portably, and the process
        // is about to exit, so only a finished thread is joined.
        if self.thread.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(thread) = self.thread.take()
        {
            let _ = thread.join();
        }
    }
}

/// How the main loop consumes the source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pacing {
    /// Pull exactly `dt * sample_rate` samples per call (headless runs).
    Deterministic,
    /// Let a capture thread fill the ring buffer as audio arrives.
    Realtime,
}

/// An opened input and the name the HUD shows for it.
pub struct OpenedSource {
    pub source: Box<dyn SampleSource>,
    pub label: String,
}

impl OpenedSource {
    fn new(source: impl SampleSource + 'static, label: impl Into<String>) -> Self {
        Self {
            source: Box::new(source),
            label: label.into(),
        }
    }
}

/// Open the configured input.
pub fn open_source(
    config: &InputConfig,
    seed: u64,
    pacing: Pacing,
) -> Result<OpenedSource, AudioError> {
    match config.method {
        InputMethod::Demo => Ok(OpenedSource::new(demo::DemoSynth::new(seed), "demo")),
        InputMethod::Stdin => {
            if io::stdin().is_terminal() {
                return Err(AudioError::Unsupported(
                    "--input stdin reads raw PCM from a pipe, for example: \
                     parec -d @DEFAULT_MONITOR@ --format=s16le --rate=48000 --channels=2 | grava -i stdin"
                        .to_owned(),
                ));
            }
            match pacing {
                Pacing::Deterministic => Ok(OpenedSource::new(
                    pcm::SyncPcm::new(
                        io::stdin(),
                        config.sample_rate,
                        pcm::PcmDecoder::from_config(config),
                    ),
                    "stdin",
                )),
                Pacing::Realtime => {
                    let capture = stdin::StdinCapture::new(config);
                    spawn(capture)
                }
            }
        }
        InputMethod::Fifo => {
            let path = fifo::resolve_path(&config.source);
            fifo::check(&path)?;
            match pacing {
                Pacing::Deterministic => {
                    let file = std::fs::File::open(&path).map_err(|source| {
                        AudioError::io(format!("cannot open {}", path.display()), source)
                    })?;
                    Ok(OpenedSource::new(
                        pcm::SyncPcm::new(
                            file,
                            config.sample_rate,
                            pcm::PcmDecoder::from_config(config),
                        ),
                        format!("fifo: {}", path.display()),
                    ))
                }
                Pacing::Realtime => spawn(fifo::FifoCapture::new(path, config)),
            }
        }
        InputMethod::Auto | InputMethod::Pulse | InputMethod::Pipewire | InputMethod::Alsa => {
            if pacing == Pacing::Deterministic {
                // Live audio cannot be replayed, and a headless run must be.
                return Err(AudioError::Unsupported(
                    "headless runs need a reproducible input: use --input demo, stdin or fifo"
                        .to_owned(),
                ));
            }
            spawn(select::open_capture(config)?)
        }
    }
}

fn spawn(capture: impl Capture) -> Result<OpenedSource, AudioError> {
    let label = capture.label();
    let source = StreamSource::spawn(capture)
        .map_err(|source| AudioError::io("cannot start the capture thread", source))?;
    Ok(OpenedSource::new(source, label))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    /// Writes a fixed sequence, then ends (or fails).
    struct Scripted {
        samples: Vec<f32>,
        fail: bool,
    }

    impl Capture for Scripted {
        fn format(&self) -> AudioFormat {
            AudioFormat {
                sample_rate: 8_000,
                channels: 1,
            }
        }

        fn run(
            &mut self,
            producer: &mut Producer<f32>,
            _stop: &AtomicBool,
        ) -> Result<(), AudioError> {
            let _ = producer.push_partial_slice(&self.samples);
            if self.fail {
                Err(AudioError::Unsupported("scripted failure".to_owned()))
            } else {
                Ok(())
            }
        }
    }

    fn drain(source: &mut StreamSource) -> Vec<f32> {
        let mut out = Vec::new();
        let mut result = Ok(());
        for _ in 0..200 {
            result = source.pull(0.016, &mut out);
            if !source.connected() || result.is_err() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        result.unwrap();
        out
    }

    #[test]
    fn stream_source_delivers_queued_samples_then_reports_the_end() {
        let samples: Vec<f32> = (0..1000).map(|i| i as f32 / 1000.0).collect();
        let mut source = StreamSource::spawn(Scripted {
            samples: samples.clone(),
            fail: false,
        })
        .unwrap();
        assert_eq!(source.sample_rate(), 8_000);
        assert_eq!(drain(&mut source), samples);
        assert!(!source.connected());
    }

    #[test]
    fn stream_source_surfaces_a_capture_failure_once_the_samples_are_read() {
        let mut source = StreamSource::spawn(Scripted {
            samples: vec![0.5; 100],
            fail: true,
        })
        .unwrap();
        let mut received = 0;
        let mut error = None;
        for _ in 0..200 {
            let mut out = Vec::new();
            if let Err(failure) = source.pull(0.016, &mut out) {
                error = Some(failure);
                break;
            }
            received += out.len();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(received, 100);
        assert!(error.unwrap().to_string().contains("scripted failure"));
        assert!(!source.connected());
    }

    #[test]
    fn a_full_ring_drops_samples_instead_of_blocking() {
        let mut source = StreamSource::spawn(Scripted {
            samples: vec![0.1; 100_000],
            fail: false,
        })
        .unwrap();
        let received = drain(&mut source).len();
        assert!((4_096..100_000).contains(&received), "{received}");
    }

    #[test]
    fn headless_runs_refuse_live_backends_and_say_what_works() {
        for method in [
            InputMethod::Auto,
            InputMethod::Pulse,
            InputMethod::Pipewire,
            InputMethod::Alsa,
        ] {
            let config = InputConfig {
                method,
                ..InputConfig::default()
            };
            let error = open_source(&config, 0, Pacing::Deterministic)
                .err()
                .unwrap()
                .to_string();
            assert!(error.contains("demo") && error.contains("stdin"), "{error}");
        }
    }

    #[test]
    fn supervision_retries_after_failures_and_stops_on_request() {
        use std::sync::atomic::AtomicUsize;

        let stop = AtomicBool::new(false);
        let link = Link::new(false);
        let connects = AtomicUsize::new(0);
        let records = AtomicUsize::new(0);
        let observed_up = AtomicBool::new(false);
        // The first session ends at once (the server went away), the next two
        // connection attempts fail, and the fourth succeeds and records until
        // the stop request.
        supervise(
            &stop,
            &link,
            Duration::from_millis(5),
            Some(0_u32),
            || {
                let attempt = connects.fetch_add(1, Ordering::Relaxed);
                if attempt < 2 {
                    Err(AudioError::Unsupported("server gone".to_owned()))
                } else {
                    Ok(attempt as u32)
                }
            },
            |session| {
                let count = records.fetch_add(1, Ordering::Relaxed);
                if count == 0 {
                    assert!(link.is_up(), "the link is up while recording");
                    return Err(AudioError::Unsupported("stream killed".to_owned()));
                }
                assert_eq!(*session, 2);
                observed_up.store(link.is_up(), Ordering::Relaxed);
                stop.store(true, Ordering::Relaxed);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(connects.load(Ordering::Relaxed), 3);
        assert_eq!(records.load(Ordering::Relaxed), 2);
        assert!(observed_up.load(Ordering::Relaxed));
        assert!(!link.is_up(), "the link goes down when recording ends");
    }

    #[test]
    fn supervision_returns_promptly_when_stopped_while_waiting() {
        let stop = AtomicBool::new(false);
        let link = Link::new(true);
        let started = Instant::now();
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(60));
                stop.store(true, Ordering::Relaxed);
            });
            supervise(
                &stop,
                &link,
                Duration::from_secs(30),
                None::<()>,
                || Err(AudioError::Unsupported("no server".to_owned())),
                |_| Ok(()),
            )
            .unwrap();
        });
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!link.is_up());
    }

    #[test]
    fn a_stream_source_reports_its_captures_link() {
        struct Flaky(Link);
        impl Capture for Flaky {
            fn format(&self) -> AudioFormat {
                AudioFormat {
                    sample_rate: 8_000,
                    channels: 1,
                }
            }
            fn link(&self) -> Option<Link> {
                Some(self.0.clone())
            }
            fn run(
                &mut self,
                _producer: &mut Producer<f32>,
                stop: &AtomicBool,
            ) -> Result<(), AudioError> {
                while !stop.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Ok(())
            }
        }
        let link = Link::new(true);
        let source = StreamSource::spawn(Flaky(link.clone())).unwrap();
        assert!(source.connected());
        link.set(false);
        assert!(!source.connected());
        link.set(true);
        assert!(source.connected());
    }
}
