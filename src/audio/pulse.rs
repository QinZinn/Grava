//! PulseAudio capture, and PipeWire through its pipewire-pulse server.
//!
//! This is a pure-Rust client of the PulseAudio native protocol, built on the
//! `pulseaudio` crate's wire format, so it needs no system library at build or
//! run time. The capture thread owns one blocking Unix socket: the protocol is
//! a plain stream of length-prefixed frames, and a short read timeout lets the
//! thread notice the stop flag without ever losing half a frame.
//!
//! `source = "auto"` records the monitor of the default output (what you hear)
//! and follows the default output when it changes. Every stream carries
//! `node.virtual = true`, which pipewire-pulse forwards to the PipeWire node;
//! desktop shells use it to tell a visualizer from a recording application.

use std::{
    ffi::{CStr, CString},
    io::{self, Cursor, Read},
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[cfg(test)]
mod fake;

use pulseaudio::protocol::{
    self, ChannelMap, Command, CommandReply, Descriptor, Prop, Props, ProtocolError,
    RecordStreamParams, SampleFormat, SampleSpec, ServerInfo, SinkInfo, SourceInfo,
    SubscriptionEventFacility, SubscriptionMask,
    stream::{BufferAttr, StreamFlags},
};
use rtrb::Producer;

use crate::config::schema::{ChannelMix, InputConfig, SampleFormat as RawFormat};

use super::{
    AudioError, AudioFormat, Capture, Link, RETRY_INTERVAL, pcm::PcmDecoder, select::SourceEntry,
    supervise,
};

const CLIENT_NAME: &CStr = c"grava";
/// How long a read waits before the loop checks the stop flag again.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long the server gets to answer a command during setup.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// The audio the server batches into one data frame.
const FRAGMENT_MS: u32 = 10;
/// Frames larger than this are not valid protocol traffic.
const MAX_FRAME: usize = protocol::MAX_MEMBLOCKQ_LENGTH + protocol::DESCRIPTOR_SIZE + 4096;
/// Captured audio is always stereo float; the server remixes and converts.
const CHANNELS: u8 = 2;

#[derive(Debug, thiserror::Error)]
enum PulseError {
    #[error(
        "no PulseAudio server socket found (looked at $PULSE_SERVER, $PULSE_RUNTIME_PATH/native \
         and $XDG_RUNTIME_DIR/pulse/native); start pipewire-pulse or pulseaudio"
    )]
    NoServer,
    #[error("PULSE_SERVER names a remote server ({0}); only local sockets are supported")]
    RemoteServer(String),
    #[error("cannot connect to the PulseAudio server at {path}: {source}")]
    Connect { path: PathBuf, source: io::Error },
    #[error("connection to the server failed: {0}")]
    Io(#[from] io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
    #[error("the server closed the connection")]
    Closed,
    #[error("no reply from the server within {} s", REPLY_TIMEOUT.as_secs())]
    NoReply,
    #[error("no source named `{0}` (see --list-sources)")]
    NoSuchSource(String),
    #[error("the record stream was removed by the server")]
    StreamKilled,
    #[error("the default output changed")]
    DefaultChanged,
}

impl From<PulseError> for AudioError {
    fn from(error: PulseError) -> Self {
        AudioError::Unsupported(error.to_string())
    }
}

/// Candidate server sockets, in the order they are tried: each local entry of
/// `$PULSE_SERVER`, then `$PULSE_RUNTIME_PATH/native`, then
/// `$XDG_RUNTIME_DIR/pulse/native`.
fn socket_candidates(
    pulse_server: Option<&str>,
    runtime_path: Option<&Path>,
    xdg_runtime_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for entry in pulse_server.unwrap_or_default().split_whitespace() {
        // Entries may start with a `{machine-id}` selector and a `unix:` tag.
        let entry = entry.rsplit_once('}').map_or(entry, |(_, rest)| rest);
        let entry = entry.strip_prefix("unix:").unwrap_or(entry);
        if entry.starts_with('/') {
            candidates.push(PathBuf::from(entry));
        }
    }
    if let Some(path) = runtime_path {
        candidates.push(path.join("native"));
    }
    if let Some(path) = xdg_runtime_dir {
        candidates.push(path.join("pulse/native"));
    }
    candidates
}

/// The first candidate socket that exists.
fn server_socket() -> Result<PathBuf, PulseError> {
    let environment = |name: &str| std::env::var_os(name).filter(|value| !value.is_empty());
    let server = environment("PULSE_SERVER");
    let candidates = socket_candidates(
        server.as_deref().and_then(|value| value.to_str()),
        environment("PULSE_RUNTIME_PATH").as_deref().map(Path::new),
        environment("XDG_RUNTIME_DIR").as_deref().map(Path::new),
    );
    if let Some(path) = candidates.into_iter().find(|path| path.exists()) {
        return Ok(path);
    }
    match server {
        // Only unix sockets are supported; say so rather than "not found".
        Some(value) if !value.to_string_lossy().contains('/') => Err(PulseError::RemoteServer(
            value.to_string_lossy().into_owned(),
        )),
        _ => Err(PulseError::NoServer),
    }
}

/// Accumulates socket bytes into whole protocol frames. A read that times out
/// never discards anything, which is why this exists instead of `read_exact`.
struct Frames {
    bytes: Vec<u8>,
    start: usize,
}

impl Frames {
    fn new() -> Self {
        Self {
            bytes: Vec::with_capacity(64 * 1024),
            start: 0,
        }
    }

    /// Read what the socket has, waiting up to its read timeout. Returns
    /// whether anything arrived.
    fn fill(&mut self, socket: &mut UnixStream) -> Result<bool, PulseError> {
        if self.start > 0 {
            self.bytes.drain(..self.start);
            self.start = 0;
        }
        let mut chunk = [0_u8; 16 * 1024];
        loop {
            return match socket.read(&mut chunk) {
                Ok(0) => Err(PulseError::Closed),
                Ok(count) => {
                    self.bytes.extend_from_slice(&chunk[..count]);
                    Ok(true)
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    Ok(false)
                }
                Err(error) => Err(error.into()),
            };
        }
    }

    /// The next complete frame, as its descriptor and total length in bytes
    /// (descriptor included), without consuming it.
    fn peek(&self) -> Result<Option<(Descriptor, usize)>, PulseError> {
        let pending = &self.bytes[self.start..];
        if pending.len() < protocol::DESCRIPTOR_SIZE {
            return Ok(None);
        }
        let descriptor = protocol::read_descriptor(&mut &pending[..])?;
        let total = protocol::DESCRIPTOR_SIZE + descriptor.length as usize;
        if total > MAX_FRAME {
            return Err(ProtocolError::Invalid(format!(
                "frame of {} bytes exceeds the protocol limit",
                descriptor.length
            ))
            .into());
        }
        Ok((pending.len() >= total).then_some((descriptor, total)))
    }

    /// The whole peeked frame, descriptor first.
    fn frame(&self, total: usize) -> &[u8] {
        &self.bytes[self.start..self.start + total]
    }

    fn payload(&self, total: usize) -> &[u8] {
        &self.frame(total)[protocol::DESCRIPTOR_SIZE..]
    }

    fn advance(&mut self, total: usize) {
        self.start += total;
    }
}

/// A connected, authenticated client socket.
struct Connection {
    socket: UnixStream,
    version: u16,
    sequence: u32,
    frames: Frames,
}

impl Connection {
    /// Connect to `socket`, or to the server the environment points at.
    fn connect(socket: Option<&Path>) -> Result<Self, PulseError> {
        let path = match socket {
            Some(path) => path.to_owned(),
            None => server_socket()?,
        };
        let socket = UnixStream::connect(&path).map_err(|source| PulseError::Connect {
            path: path.clone(),
            source,
        })?;
        socket.set_read_timeout(Some(POLL_INTERVAL))?;
        socket.set_write_timeout(Some(REPLY_TIMEOUT))?;
        let mut connection = Self {
            socket,
            version: protocol::MAX_VERSION,
            sequence: 0,
            frames: Frames::new(),
        };
        let cookie = pulseaudio::cookie_path_from_env()
            .and_then(|path| std::fs::read(path).ok())
            .unwrap_or_default();
        let auth: protocol::AuthReply =
            connection.roundtrip(Command::Auth(protocol::AuthParams {
                version: protocol::MAX_VERSION,
                supports_shm: false,
                supports_memfd: false,
                cookie,
            }))?;
        connection.version = protocol::MAX_VERSION.min(auth.version);
        let mut props = Props::new();
        props.set(Prop::ApplicationName, CLIENT_NAME);
        let _: protocol::SetClientNameReply =
            connection.roundtrip(Command::SetClientName(props))?;
        Ok(connection)
    }

    /// Send a command and return its sequence number.
    fn send(&mut self, command: &Command) -> Result<u32, PulseError> {
        let sequence = self.sequence;
        self.sequence += 1;
        protocol::write_command_message(&mut self.socket, sequence, command, self.version)?;
        Ok(sequence)
    }

    /// Send a command and wait for its reply. Other frames that arrive
    /// meanwhile are dropped: nothing else is expected during setup.
    fn roundtrip<T: CommandReply>(&mut self, command: Command) -> Result<T, PulseError> {
        let sequence = self.send(&command)?;
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            while let Some((descriptor, total)) = self.frames.peek()? {
                let is_command = descriptor.channel == u32::MAX;
                let reply = is_command.then(|| {
                    protocol::read_reply_message::<T>(
                        &mut Cursor::new(self.frames.frame(total)),
                        self.version,
                    )
                });
                self.frames.advance(total);
                match reply {
                    Some(Ok((answered, value))) if answered == sequence => return Ok(value),
                    // A notification that is not a reply is none of our business.
                    Some(Err(ProtocolError::UnexpectedCommand(_))) => {}
                    Some(Err(error)) => return Err(error.into()),
                    _ => {}
                }
            }
            if Instant::now() >= deadline {
                return Err(PulseError::NoReply);
            }
            self.frames.fill(&mut self.socket)?;
        }
    }

    fn source_by_name(&mut self, name: &CStr) -> Result<SourceInfo, PulseError> {
        self.roundtrip(Command::GetSourceInfo(protocol::GetSourceInfo {
            index: None,
            name: Some(name.to_owned()),
        }))
    }

    /// The monitor of the default output, the source `auto` records.
    fn default_monitor(&mut self) -> Result<SourceInfo, PulseError> {
        // Servers resolve this alias themselves ...
        if let Ok(source) = self.source_by_name(c"@DEFAULT_MONITOR@") {
            return Ok(source);
        }
        // ... and where one does not, ask for the default sink's monitor.
        let server: ServerInfo = self.roundtrip(Command::GetServerInfo)?;
        let sink_name = server
            .default_sink_name
            .ok_or_else(|| PulseError::NoSuchSource("@DEFAULT_MONITOR@".to_owned()))?;
        let sink: SinkInfo = self.roundtrip(Command::GetSinkInfo(protocol::GetSinkInfo {
            index: None,
            name: Some(sink_name),
        }))?;
        let monitor = sink
            .monitor_source_name
            .ok_or_else(|| PulseError::NoSuchSource("@DEFAULT_MONITOR@".to_owned()))?;
        self.source_by_name(&monitor)
    }

    /// The source named by `source`: `auto`, a name, or a numeric index.
    fn resolve(&mut self, source: &str) -> Result<SourceInfo, PulseError> {
        if source == "auto" {
            return self.default_monitor();
        }
        let missing = || PulseError::NoSuchSource(source.to_owned());
        let name = CString::new(source).map_err(|_| missing())?;
        match self.source_by_name(&name) {
            Ok(info) => Ok(info),
            Err(PulseError::Protocol(ProtocolError::ServerError(_))) => {
                let index: u32 = source.parse().map_err(|_| missing())?;
                self.roundtrip(Command::GetSourceInfo(protocol::GetSourceInfo {
                    index: Some(index),
                    name: None,
                }))
                .map_err(|_| missing())
            }
            Err(other) => Err(other),
        }
    }
}

/// What a session needs to be rebuilt after the server goes away.
#[derive(Debug, Clone)]
struct Settings {
    source: String,
    mix: ChannelMix,
    /// Fixed after the first connection, so that the analyzer's sample rate
    /// stays valid whichever output the stream later follows.
    rate: Option<u32>,
    /// Overrides the environment's server, for tests.
    socket: Option<PathBuf>,
}

/// One connection with a record stream running on it.
struct Session {
    connection: Connection,
    channel: u32,
    rate: u32,
    /// The name of the source being recorded.
    source_name: String,
    /// The output whose monitor is recorded, when following the default.
    followed_sink: Option<CString>,
    /// An outstanding `GetServerInfo` asked after a server event.
    pending_info: Option<u32>,
}

impl Session {
    fn open(settings: &Settings) -> Result<Self, PulseError> {
        let mut connection = Connection::connect(settings.socket.as_deref())?;
        let source = connection.resolve(&settings.source)?;
        let rate = settings
            .rate
            .unwrap_or(source.sample_spec.sample_rate)
            .max(1);
        let following = settings.source == "auto";

        let mut props = Props::new();
        props.set(Prop::ApplicationName, CLIENT_NAME);
        props.set(Prop::MediaName, c"Audio visualizer");
        props.set_bytes(c"node.virtual", b"true\0");
        let fragment_bytes = rate / 1000 * FRAGMENT_MS * u32::from(CHANNELS) * 4;
        let created: protocol::CreateRecordStreamReply =
            connection.roundtrip(Command::CreateRecordStream(RecordStreamParams {
                sample_spec: SampleSpec {
                    format: SampleFormat::Float32Le,
                    channels: CHANNELS,
                    sample_rate: rate,
                },
                channel_map: ChannelMap::stereo(),
                source_index: Some(source.index),
                buffer_attr: BufferAttr {
                    fragment_size: fragment_bytes,
                    ..Default::default()
                },
                flags: StreamFlags {
                    adjust_latency: true,
                    ..Default::default()
                },
                props,
                ..Default::default()
            }))?;
        if following {
            // Learn when the default output changes. Failing to subscribe
            // only costs the following, so the answer is not awaited.
            connection.send(&Command::Subscribe(SubscriptionMask::SERVER))?;
        }
        Ok(Self {
            connection,
            channel: created.channel,
            rate: created.sample_spec.sample_rate.max(1),
            source_name: source.name.to_string_lossy().into_owned(),
            followed_sink: following.then_some(source.monitor_of_sink_name).flatten(),
            pending_info: None,
        })
    }

    /// Receive audio until the stream or connection ends or `stop` is set.
    fn record(
        &mut self,
        settings: &Settings,
        producer: &mut Producer<f32>,
        stop: &AtomicBool,
    ) -> Result<(), PulseError> {
        let mut decoder = PcmDecoder::new(RawFormat::F32le, u16::from(CHANNELS), settings.mix);
        let mut mono = Vec::with_capacity(8192);
        while !stop.load(Ordering::Relaxed) {
            self.connection.frames.fill(&mut self.connection.socket)?;
            while let Some((descriptor, total)) = self.connection.frames.peek()? {
                if descriptor.channel == self.channel {
                    mono.clear();
                    decoder.decode(self.connection.frames.payload(total), |sample| {
                        mono.push(sample);
                    });
                    // A full ring drops samples; it never makes the server wait.
                    let _ = producer.push_partial_slice(&mono);
                } else if descriptor.channel == u32::MAX {
                    self.handle_command(total)?;
                }
                self.connection.frames.advance(total);
            }
        }
        Ok(())
    }

    /// React to a control frame of the peeked length.
    fn handle_command(&mut self, total: usize) -> Result<(), PulseError> {
        let version = self.connection.version;
        let frame = self.connection.frames.payload(total);
        let (sequence, command) = match Command::read_tag_prefixed(&mut &frame[..], version) {
            Ok(parsed) => parsed,
            // Unknown or unparsable notices are not worth dying for.
            Err(_) => return Ok(()),
        };
        match command {
            Command::RecordStreamKilled(channel) if channel == self.channel => {
                Err(PulseError::StreamKilled)
            }
            Command::Exit => Err(PulseError::Closed),
            Command::SubscribeEvent(event)
                if event.event_facility == SubscriptionEventFacility::Server
                    && self.followed_sink.is_some()
                    && self.pending_info.is_none() =>
            {
                self.pending_info = Some(self.connection.send(&Command::GetServerInfo)?);
                Ok(())
            }
            Command::Reply if self.pending_info == Some(sequence) => {
                self.pending_info = None;
                let info = protocol::read_reply_message::<ServerInfo>(
                    &mut Cursor::new(self.connection.frames.frame(total)),
                    version,
                );
                match info {
                    Ok((_, info)) if info.default_sink_name != self.followed_sink => {
                        Err(PulseError::DefaultChanged)
                    }
                    _ => Ok(()),
                }
            }
            Command::Error(_) if self.pending_info == Some(sequence) => {
                self.pending_info = None;
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

/// Capture from a PulseAudio-compatible server.
pub struct PulseCapture {
    settings: Settings,
    label: String,
    format: AudioFormat,
    session: Option<Session>,
    link: Link,
    retry: Duration,
}

impl PulseCapture {
    /// Connect, pick the source and start the record stream. Problems are
    /// reported here, so that they appear before the terminal is taken over.
    pub fn open(config: &InputConfig) -> Result<Self, AudioError> {
        Self::open_at(config, None, RETRY_INTERVAL)
    }

    fn open_at(
        config: &InputConfig,
        socket: Option<PathBuf>,
        retry: Duration,
    ) -> Result<Self, AudioError> {
        let mut settings = Settings {
            source: config.source.clone(),
            mix: config.channel_mix,
            rate: None,
            socket,
        };
        let session = Session::open(&settings)?;
        settings.rate = Some(session.rate);
        Ok(Self {
            label: format!("pulse: {}", session.source_name),
            format: AudioFormat {
                sample_rate: session.rate,
                channels: u16::from(CHANNELS),
            },
            settings,
            session: Some(session),
            link: Link::new(true),
            retry,
        })
    }
}

impl Capture for PulseCapture {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn label(&self) -> String {
        self.label.clone()
    }

    fn link(&self) -> Option<Link> {
        Some(self.link.clone())
    }

    /// Record until stopped. When the server goes away, or the default output
    /// moves, wait two seconds and reconnect, over and over.
    fn run(&mut self, producer: &mut Producer<f32>, stop: &AtomicBool) -> Result<(), AudioError> {
        let settings = self.settings.clone();
        supervise(
            stop,
            &self.link,
            self.retry,
            self.session.take(),
            || Session::open(&settings).map_err(AudioError::from),
            |session| {
                session
                    .record(&settings, producer, stop)
                    .map_err(AudioError::from)
            },
        )
    }
}

/// The sources of the server, for `--list-sources`.
pub fn list_sources() -> Result<Vec<SourceEntry>, AudioError> {
    list_sources_at(None)
}

fn list_sources_at(socket: Option<&Path>) -> Result<Vec<SourceEntry>, AudioError> {
    let mut connection = Connection::connect(socket)?;
    let sources: protocol::SourceInfoList = connection.roundtrip(Command::GetSourceInfoList)?;
    let default = connection.default_monitor().ok().map(|source| source.name);
    Ok(sources
        .into_iter()
        .map(|source| SourceEntry {
            default: default.as_ref() == Some(&source.name),
            monitor: source.monitor_of_sink_index.is_some(),
            description: source
                .description
                .as_deref()
                .map(|text| text.to_string_lossy().into_owned())
                .unwrap_or_default(),
            name: source.name.to_string_lossy().into_owned(),
        })
        .collect())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn socket_candidates_follow_the_documented_order() {
        let candidates = socket_candidates(
            Some("unix:/tmp/custom.sock"),
            Some(Path::new("/run/pulse-runtime")),
            Some(Path::new("/run/user/1000")),
        );
        assert_eq!(
            candidates,
            [
                PathBuf::from("/tmp/custom.sock"),
                PathBuf::from("/run/pulse-runtime/native"),
                PathBuf::from("/run/user/1000/pulse/native"),
            ]
        );
        assert_eq!(
            socket_candidates(None, None, Some(Path::new("/run/user/1"))),
            [PathBuf::from("/run/user/1/pulse/native")]
        );
        assert!(socket_candidates(None, None, None).is_empty());
    }

    #[test]
    fn pulse_server_syntax_is_parsed_leniently() {
        let candidates = socket_candidates(
            Some("tcp:host:4713 {abc123}unix:/a/native /b/native unix:relative"),
            None,
            None,
        );
        // Remote entries and relative paths are skipped; selectors stripped.
        assert_eq!(
            candidates,
            [PathBuf::from("/a/native"), PathBuf::from("/b/native")]
        );
    }

    // --- Against the in-process fake server ------------------------------

    use super::fake::{Event, FakeServer, SourceSpec};
    use crate::audio::{SampleSource, StreamSource};
    use crate::config::schema::InputMethod;

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(label: &str) -> Self {
            let path =
                std::env::temp_dir().join(format!("grava-pulse-{}-{label}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn socket(&self) -> PathBuf {
            self.0.join("native")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn spec(
        name: &'static str,
        description: &'static str,
        monitor_of: Option<&'static str>,
        rate: u32,
    ) -> SourceSpec {
        SourceSpec {
            name,
            description,
            monitor_of,
            rate,
        }
    }

    /// Speakers (48 kHz), headphones (44.1 kHz) and a microphone.
    fn devices() -> Vec<SourceSpec> {
        vec![
            spec(
                "speakers.monitor",
                "Monitor of Speakers",
                Some("speakers"),
                48_000,
            ),
            spec(
                "headphones.monitor",
                "Monitor of Headphones",
                Some("headphones"),
                44_100,
            ),
            spec("mic", "Microphone", None, 44_100),
        ]
    }

    fn config(source: &str) -> InputConfig {
        InputConfig {
            method: InputMethod::Pulse,
            source: source.to_owned(),
            ..InputConfig::default()
        }
    }

    fn open(server: &FakeServer, source: &str) -> Result<PulseCapture, AudioError> {
        PulseCapture::open_at(
            &config(source),
            Some(server.path.clone()),
            Duration::from_millis(30),
        )
    }

    fn wait_for(timeout: Duration, mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if condition() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        condition()
    }

    /// Pull until `count` samples arrived or the time runs out.
    fn collect(source: &mut StreamSource, count: usize, timeout: Duration) -> Vec<f32> {
        let mut samples = Vec::new();
        wait_for(timeout, || {
            source.pull(0.0, &mut samples).unwrap();
            samples.len() >= count
        });
        samples
    }

    #[test]
    fn records_the_default_monitor_and_hears_the_tone() {
        use crate::{config::Config, dsp::Analyzer};

        let directory = TempDir::new("tone");
        let server = FakeServer::start(&directory.socket(), devices(), "speakers");
        let capture = open(&server, "auto").unwrap();
        assert_eq!(capture.format().sample_rate, 48_000);
        let stream = server.streams_created().remove(0);
        assert_eq!(stream.source_index, Some(100));
        assert_eq!(stream.sample_spec.format, SampleFormat::Float32Le);
        assert_eq!(
            (stream.sample_spec.channels, stream.sample_spec.sample_rate),
            (2, 48_000)
        );
        assert!(stream.flags.adjust_latency);
        // 10 ms of 48 kHz stereo float.
        assert_eq!(stream.buffer_attr.fragment_size, 480 * 2 * 4);
        assert_eq!(
            stream.props.get_bytes(c"node.virtual"),
            Some(&b"true\0"[..])
        );
        assert_eq!(
            stream.props.get(Prop::ApplicationName),
            Some(&b"grava\0"[..])
        );

        let mut source = StreamSource::spawn(capture).unwrap();
        let samples = collect(&mut source, 48_000, Duration::from_secs(5));
        assert!(samples.len() >= 48_000, "{} samples", samples.len());
        let defaults = Config::default();
        let mut analyzer = Analyzer::new(&defaults.analysis, &defaults.general, 48_000);
        let mut features = crate::dsp::AudioFeatures::default();
        for chunk in samples.chunks(800) {
            analyzer.push(chunk);
            features = *analyzer.analyze(1.0 / 60.0);
        }
        assert_eq!((features.pitch_class, features.octave), (Some(9), 4));
        assert!(source.connected());
    }

    #[test]
    fn falls_back_to_the_default_sinks_monitor_when_the_alias_is_unknown() {
        let directory = TempDir::new("fallback");
        let server = FakeServer::start(&directory.socket(), devices(), "headphones");
        server
            .shared
            .alias_supported
            .store(false, Ordering::Relaxed);
        let capture = open(&server, "auto").unwrap();
        // Headphones are the default, so their 44.1 kHz monitor is recorded.
        assert_eq!(capture.format().sample_rate, 44_100);
        assert_eq!(server.streams_created()[0].source_index, Some(101));
    }

    #[test]
    fn names_and_indices_select_sources_and_unknown_ones_are_rejected() {
        let directory = TempDir::new("names");
        let server = FakeServer::start(&directory.socket(), devices(), "speakers");
        assert_eq!(open(&server, "mic").unwrap().format().sample_rate, 44_100);
        assert_eq!(server.streams_created()[0].source_index, Some(102));
        // A number that is not a name selects by index.
        open(&server, "101").unwrap();
        assert_eq!(server.streams_created()[1].source_index, Some(101));
        for missing in ["nonexistent", "999", "-1"] {
            let error = open(&server, missing).err().unwrap().to_string();
            assert!(
                error.contains(&format!("no source named `{missing}`"))
                    && error.contains("--list-sources"),
                "{error}"
            );
        }
    }

    #[test]
    fn a_missing_server_is_reported_with_the_path_and_what_to_start() {
        let directory = TempDir::new("absent");
        let error =
            PulseCapture::open_at(&config("auto"), Some(directory.socket()), RETRY_INTERVAL)
                .err()
                .unwrap()
                .to_string();
        assert!(
            error.contains("cannot connect") && error.contains("native"),
            "{error}"
        );
        let none = PulseError::NoServer.to_string();
        assert!(
            none.contains("pipewire-pulse") && none.contains("XDG_RUNTIME_DIR"),
            "{none}"
        );
        assert!(
            PulseError::RemoteServer("tcp:host".to_owned())
                .to_string()
                .contains("remote")
        );
    }

    #[test]
    fn survives_the_server_restarting() {
        let directory = TempDir::new("restart");
        let server = FakeServer::start(&directory.socket(), devices(), "speakers");
        let mut source = StreamSource::spawn(open(&server, "auto").unwrap()).unwrap();
        assert!(collect(&mut source, 4_800, Duration::from_secs(5)).len() >= 4_800);
        assert!(source.connected());

        server.shutdown();
        assert!(
            wait_for(Duration::from_secs(5), || {
                let mut sink = Vec::new();
                source.pull(0.0, &mut sink).unwrap();
                !source.connected()
            }),
            "the link should report the lost server"
        );

        let server = FakeServer::start(&directory.socket(), devices(), "speakers");
        let samples = collect(&mut source, 4_800, Duration::from_secs(5));
        assert!(samples.len() >= 4_800, "no audio after the restart");
        assert!(source.connected());
        assert_eq!(server.connections(), 1);
    }

    #[test]
    fn a_stream_killed_by_the_server_is_recreated() {
        let directory = TempDir::new("killed");
        let server = FakeServer::start(&directory.socket(), devices(), "speakers");
        let mut source = StreamSource::spawn(open(&server, "auto").unwrap()).unwrap();
        collect(&mut source, 4_800, Duration::from_secs(5));
        server.send(Event::KillStream);
        assert!(wait_for(Duration::from_secs(5), || server
            .streams_created()
            .len()
            == 2));
        assert!(collect(&mut source, 4_800, Duration::from_secs(5)).len() >= 4_800);
        assert_eq!(server.connections(), 2);
    }

    #[test]
    fn auto_follows_the_default_output_and_keeps_its_sample_rate() {
        let directory = TempDir::new("follow");
        let server = FakeServer::start(&directory.socket(), devices(), "speakers");
        let mut source = StreamSource::spawn(open(&server, "auto").unwrap()).unwrap();
        assert_eq!(source.sample_rate(), 48_000);
        collect(&mut source, 4_800, Duration::from_secs(5));

        server.set_default_sink("headphones");
        assert!(
            wait_for(Duration::from_secs(5), || server.streams_created().len()
                == 2),
            "the stream should move to the new default output"
        );
        let streams = server.streams_created();
        assert_eq!(streams[1].source_index, Some(101));
        // The analyzer was built for 48 kHz; the 44.1 kHz monitor is resampled.
        assert_eq!(streams[1].sample_spec.sample_rate, 48_000);
        assert!(collect(&mut source, 4_800, Duration::from_secs(5)).len() >= 4_800);

        // A default change that is not a change of output is ignored.
        server.send(Event::DefaultChanged);
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(server.streams_created().len(), 2);
    }

    #[test]
    fn a_named_source_stays_put_when_the_default_changes() {
        let directory = TempDir::new("pinned");
        let server = FakeServer::start(&directory.socket(), devices(), "speakers");
        let mut source = StreamSource::spawn(open(&server, "speakers.monitor").unwrap()).unwrap();
        collect(&mut source, 4_800, Duration::from_secs(5));
        server.set_default_sink("headphones");
        std::thread::sleep(Duration::from_millis(400));
        assert_eq!(server.streams_created().len(), 1);
        assert_eq!(server.connections(), 1);
        assert!(source.connected());
    }

    #[test]
    fn lists_sources_with_monitors_and_the_default_marked() {
        let directory = TempDir::new("list");
        let server = FakeServer::start(&directory.socket(), devices(), "headphones");
        let sources = list_sources_at(Some(&server.path)).unwrap();
        assert_eq!(sources.len(), 3);
        let by_name = |name: &str| sources.iter().find(|source| source.name == name).unwrap();
        assert!(by_name("speakers.monitor").monitor && !by_name("speakers.monitor").default);
        assert!(by_name("headphones.monitor").monitor && by_name("headphones.monitor").default);
        assert!(!by_name("mic").monitor && !by_name("mic").default);
        assert_eq!(by_name("mic").description, "Microphone");
        assert_eq!(sources.iter().filter(|source| source.default).count(), 1);
    }

    #[test]
    fn the_capture_thread_stops_promptly_when_asked() {
        let directory = TempDir::new("stop");
        let server = FakeServer::start(&directory.socket(), devices(), "speakers");
        let mut capture = open(&server, "auto").unwrap();
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(48_000);
        let stop = AtomicBool::new(false);
        let started = Instant::now();
        std::thread::scope(|scope| {
            let thread = scope.spawn(|| capture.run(&mut producer, &stop));
            assert!(wait_for(Duration::from_secs(5), || consumer.slots() > 0));
            stop.store(true, Ordering::Relaxed);
            thread.join().unwrap().unwrap();
        });
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(!capture.link.is_up());
        // Everything that arrived was mono, so the ring holds frames, not pairs.
        assert!(consumer.pop().is_ok());
    }

    #[test]
    fn frames_are_reassembled_from_arbitrary_splits_and_bad_frames_are_rejected() {
        use std::io::Write;

        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        reader
            .set_read_timeout(Some(Duration::from_millis(1)))
            .unwrap();
        let payload: Vec<u8> = (0..100).collect();
        let mut frame = Vec::new();
        protocol::write_descriptor(
            &mut frame,
            &Descriptor {
                length: payload.len() as u32,
                channel: 5,
                offset: 0,
                flags: protocol::DescriptorFlags::empty(),
            },
        )
        .unwrap();
        frame.extend_from_slice(&payload);
        let mut frames = Frames::new();
        let mut got = None;
        for byte in &frame {
            // Nothing is complete before the last byte; timeouts lose nothing.
            assert!(got.is_none());
            writer.write_all(&[*byte]).unwrap();
            let _ = frames.fill(&mut reader).unwrap();
            if let Some((descriptor, total)) = frames.peek().unwrap() {
                got = Some((descriptor, frames.payload(total).to_vec()));
            }
        }
        let (descriptor, bytes) = got.unwrap();
        assert_eq!((descriptor.channel, descriptor.length), (5, 100));
        assert_eq!(bytes, payload);
        // Two frames in one read come out one by one.
        let mut pair = frame.clone();
        pair.extend_from_slice(&frame);
        let mut frames = Frames::new();
        writer.write_all(&pair).unwrap();
        frames.fill(&mut reader).unwrap();
        let (_, total) = frames.peek().unwrap().unwrap();
        frames.advance(total);
        assert!(frames.peek().unwrap().is_some());
        // An absurd length is a protocol error, not an allocation.
        let mut huge = Vec::new();
        protocol::write_descriptor(
            &mut huge,
            &Descriptor {
                length: u32::MAX,
                channel: 0,
                offset: 0,
                flags: protocol::DescriptorFlags::empty(),
            },
        )
        .unwrap();
        let mut frames = Frames::new();
        writer.write_all(&huge).unwrap();
        frames.fill(&mut reader).unwrap();
        assert!(matches!(frames.peek(), Err(PulseError::Protocol(_))));
        // A closed peer is reported, not mistaken for a timeout.
        drop(writer);
        assert!(matches!(
            Frames::new().fill(&mut reader),
            Err(PulseError::Closed)
        ));
    }

    // --- Live checks against a real server -------------------------------
    //
    // These need a running PulseAudio or pipewire-pulse and are ignored by
    // default. `GRAVA_LIVE_SOURCE` names the source to record ("auto" for the
    // default output's monitor). To hear nothing and record nothing private,
    // create a null sink, play a 440 Hz tone into it and record its monitor:
    //
    //   pactl load-module module-null-sink sink_name=grava_test
    //   paplay -d grava_test tone.wav &
    //   GRAVA_LIVE_SOURCE=grava_test.monitor cargo test --lib live_ -- --ignored --nocapture

    fn live_config() -> Option<InputConfig> {
        Some(InputConfig {
            method: crate::config::schema::InputMethod::Pulse,
            source: std::env::var("GRAVA_LIVE_SOURCE").ok()?,
            ..InputConfig::default()
        })
    }

    #[test]
    #[ignore = "needs a PulseAudio server; see the comment above"]
    fn live_capture_hears_the_tone_as_a4() {
        use crate::{
            audio::{SampleSource, StreamSource},
            config::Config,
            dsp::Analyzer,
        };

        let Some(config) = live_config() else { return };
        let capture = PulseCapture::open(&config).unwrap();
        let rate = capture.format().sample_rate;
        let mut source = StreamSource::spawn(capture).unwrap();
        let defaults = Config::default();
        let mut analyzer = Analyzer::new(&defaults.analysis, &defaults.general, rate);
        let mut samples = Vec::new();
        let mut received = 0;
        let mut features = crate::dsp::AudioFeatures::default();
        for _ in 0..120 {
            samples.clear();
            source.pull(1.0 / 60.0, &mut samples).unwrap();
            received += samples.len();
            analyzer.push(&samples);
            features = *analyzer.analyze(1.0 / 60.0);
            std::thread::sleep(Duration::from_millis(16));
        }
        println!(
            "rate {rate} Hz, {received} samples in 2 s, energy {:.2}, pitch {:?} octave {} at {:?} Hz",
            features.energy, features.pitch_class, features.octave, features.dominant_hz
        );
        assert!(source.connected());
        assert!(received > rate as usize, "only {received} samples arrived");
        assert_eq!((features.pitch_class, features.octave), (Some(9), 4));
    }

    #[test]
    #[ignore = "needs a PulseAudio server; see the comment above"]
    fn live_capture_stays_open_for_inspection() {
        use crate::audio::{SampleSource, StreamSource};

        let Some(config) = live_config() else { return };
        let seconds: u64 = std::env::var("GRAVA_LIVE_HOLD_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(4);
        let capture = PulseCapture::open(&config).unwrap();
        let mut source = StreamSource::spawn(capture).unwrap();
        let mut samples = Vec::new();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(seconds) {
            samples.clear();
            source.pull(0.0, &mut samples).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(source.connected());
    }

    #[test]
    #[ignore = "needs a PulseAudio server"]
    fn live_default_monitor_resolution() {
        let mut connection = Connection::connect(None).unwrap();
        let alias = connection.source_by_name(c"@DEFAULT_MONITOR@");
        println!(
            "alias @DEFAULT_MONITOR@: {:?}",
            alias
                .as_ref()
                .map(|s| s.name.clone())
                .map_err(ToString::to_string)
        );
        let resolved = connection.default_monitor().unwrap();
        println!(
            "default monitor: {:?} of sink {:?}",
            resolved.name, resolved.monitor_of_sink_name
        );
        assert!(resolved.monitor_of_sink_index.is_some());
    }
}
