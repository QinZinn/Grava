//! Native PipeWire capture.
//!
//! `source = "auto"` records the monitor of the default output through a
//! capture stream with `stream.capture.sink = true` and no target, so the
//! session manager keeps it on whichever output is the default. A named source
//! (`node.name` or `object.serial`) is linked directly; naming an output
//! records its monitor. Every stream is marked `node.virtual = true`, the way
//! cava does it, so that desktops do not announce it as a recording app.
//!
//! PipeWire's objects are not `Send`, so each session is built, run and torn
//! down on the capture thread. `open` only probes the server, which makes bad
//! source names and missing servers fail before the terminal is taken over.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use pipewire as pw;
use pw::{
    keys,
    metadata::{Metadata, MetadataListener},
    properties::{PropertiesBox, properties},
    spa,
    stream::{StreamFlags, StreamState},
    types::ObjectType,
};
use rtrb::Producer;
use spa::{
    param::{
        audio::{AudioFormat as SpaFormat, AudioInfoRaw},
        format::{MediaSubtype, MediaType},
        format_utils,
    },
    pod::Pod,
};

use crate::config::schema::{ChannelMix, InputConfig, SampleFormat as RawFormat};

use super::{
    AudioError, AudioFormat, Capture, Link, RETRY_INTERVAL, pcm::PcmDecoder, select::SourceEntry,
    supervise,
};

const CLIENT_NAME: &str = "grava";
/// The rate and channel count requested from the graph, which converts as
/// needed. A fixed rate keeps the analyzer valid across reconnects.
const RATE: u32 = 48_000;
const CHANNELS: u32 = 2;
/// Frames per processing quantum asked for: 10 ms.
const LATENCY_FRAMES: u32 = 480;
/// How long one turn of the event loop waits before the stop flag is checked.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// How long the server gets to answer while probing.
const ANSWER_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, thiserror::Error)]
enum PipewireError {
    #[error(
        "cannot connect to the PipeWire server: {0}; is pipewire running? \
         (`systemctl --user status pipewire`)"
    )]
    Connect(String),
    #[error("the PipeWire server did not answer within {} s", ANSWER_TIMEOUT.as_secs())]
    NoAnswer,
    #[error("no audio output was found to record; enable a sound device (see --list-sources)")]
    NoOutput,
    #[error("no source named `{0}` (see --list-sources)")]
    NoSuchSource(String),
    #[error("{0}")]
    Failed(String),
}

impl From<PipewireError> for AudioError {
    fn from(error: PipewireError) -> Self {
        AudioError::Unsupported(error.to_string())
    }
}

fn connect_error(error: impl std::fmt::Display) -> PipewireError {
    PipewireError::Connect(error.to_string())
}

/// An audio node of the graph, as announced by the registry.
#[derive(Debug, Clone, PartialEq, Eq)]
struct NodeInfo {
    serial: String,
    name: String,
    description: String,
    class: String,
}

impl NodeInfo {
    fn is_sink(&self) -> bool {
        self.class == "Audio/Sink"
    }

    /// Microphones and other inputs; sinks are listed as monitors instead.
    fn is_input(&self) -> bool {
        self.class == "Audio/Source" || self.class == "Audio/Duplex"
    }
}

#[derive(Debug, Default)]
struct Snapshot {
    nodes: Vec<NodeInfo>,
    default_sink: Option<String>,
}

/// What the stream links to.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// The default output's monitor, wherever the default goes.
    DefaultMonitor,
    Node {
        name: String,
        is_sink: bool,
    },
}

/// The node name inside a `default.audio.sink` metadata value,
/// `{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}`.
fn parse_default_name(value: &str) -> Option<String> {
    serde_json::from_str::<serde_json::Value>(value)
        .ok()?
        .get("name")?
        .as_str()
        .map(str::to_owned)
}

/// Pick the target for `source` among the nodes found.
fn resolve_target(snapshot: &Snapshot, source: &str) -> Result<Target, PipewireError> {
    if source == "auto" {
        return if snapshot.nodes.iter().any(NodeInfo::is_sink) {
            Ok(Target::DefaultMonitor)
        } else {
            Err(PipewireError::NoOutput)
        };
    }
    snapshot
        .nodes
        .iter()
        .find(|node| {
            (node.is_sink() || node.is_input()) && (node.name == source || node.serial == source)
        })
        .map(|node| Target::Node {
            name: node.name.clone(),
            is_sink: node.is_sink(),
        })
        .ok_or_else(|| PipewireError::NoSuchSource(source.to_owned()))
}

fn entries(snapshot: &Snapshot) -> Vec<SourceEntry> {
    snapshot
        .nodes
        .iter()
        .filter(|node| node.is_sink() || node.is_input())
        .map(|node| SourceEntry {
            name: node.name.clone(),
            description: if node.is_sink() {
                format!("Monitor of {}", node.description)
            } else {
                node.description.clone()
            },
            monitor: node.is_sink(),
            default: node.is_sink() && snapshot.default_sink.as_deref() == Some(&node.name),
        })
        .collect()
}

/// Connect to the server, collect its audio nodes and default output, and
/// disconnect again.
fn snapshot() -> Result<Snapshot, PipewireError> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(connect_error)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(connect_error)?;
    let core = context.connect_rc(None).map_err(connect_error)?;
    let registry = core.get_registry_rc().map_err(connect_error)?;

    let failure: Rc<RefCell<Option<String>>> = Rc::default();
    let nodes: Rc<RefCell<Vec<NodeInfo>>> = Rc::default();
    let default_sink: Rc<RefCell<Option<String>>> = Rc::default();
    let metadata: Rc<RefCell<Vec<(Metadata, MetadataListener)>>> = Rc::default();

    let _core_listener = core
        .add_listener_local()
        .error({
            let failure = Rc::clone(&failure);
            move |id, _sequence, _result, message| {
                if id == pw::core::PW_ID_CORE {
                    *failure.borrow_mut() = Some(message.to_owned());
                }
            }
        })
        .register();
    let _registry_listener = registry
        .add_listener_local()
        .global({
            let registry = registry.clone();
            let nodes = Rc::clone(&nodes);
            let default_sink = Rc::clone(&default_sink);
            let metadata = Rc::clone(&metadata);
            move |global| {
                let Some(props) = global.props else { return };
                match global.type_ {
                    ObjectType::Node => {
                        let class = props.get("media.class").unwrap_or_default();
                        if class.starts_with("Audio/") {
                            let name = props.get("node.name").unwrap_or_default();
                            let description = props
                                .get("node.description")
                                .or_else(|| props.get("node.nick"))
                                .unwrap_or(name);
                            nodes.borrow_mut().push(NodeInfo {
                                serial: props.get("object.serial").unwrap_or_default().to_owned(),
                                name: name.to_owned(),
                                description: description.to_owned(),
                                class: class.to_owned(),
                            });
                        }
                    }
                    ObjectType::Metadata if props.get("metadata.name") == Some("default") => {
                        if let Ok(bound) = registry.bind::<Metadata, _>(global) {
                            let default_sink = Rc::clone(&default_sink);
                            let listener = bound
                                .add_listener_local()
                                .property(move |_subject, key, _type, value| {
                                    if key == Some("default.audio.sink") {
                                        *default_sink.borrow_mut() =
                                            value.and_then(parse_default_name);
                                    }
                                    0
                                })
                                .register();
                            metadata.borrow_mut().push((bound, listener));
                        }
                    }
                    _ => {}
                }
            }
        })
        .register();

    // The first round trip delivers the globals, the second the metadata
    // properties that binding the default metadata object triggered.
    for _ in 0..2 {
        roundtrip(&mainloop, &core, &failure)?;
    }
    drop(metadata);
    let snapshot = Snapshot {
        nodes: nodes.take(),
        default_sink: default_sink.take(),
    };
    Ok(snapshot)
}

/// Wait until the server has processed everything sent so far.
fn roundtrip(
    mainloop: &pw::main_loop::MainLoopRc,
    core: &pw::core::CoreRc,
    failure: &RefCell<Option<String>>,
) -> Result<(), PipewireError> {
    let pending = core.sync(0).map_err(connect_error)?;
    let done = Rc::new(Cell::new(false));
    let _listener = core
        .add_listener_local()
        .done({
            let done = Rc::clone(&done);
            move |id, sequence| {
                if id == pw::core::PW_ID_CORE && sequence == pending {
                    done.set(true);
                }
            }
        })
        .register();
    let deadline = Instant::now() + ANSWER_TIMEOUT;
    while !done.get() {
        if let Some(message) = failure.borrow().as_ref() {
            return Err(PipewireError::Failed(message.clone()));
        }
        if Instant::now() >= deadline {
            return Err(PipewireError::NoAnswer);
        }
        mainloop.loop_().iterate(Duration::from_millis(50));
    }
    Ok(())
}

fn stream_properties(target: &Target) -> PropertiesBox {
    let mut props = properties! {
        *keys::MEDIA_TYPE => "Audio",
        *keys::MEDIA_CATEGORY => "Capture",
        *keys::MEDIA_ROLE => "Music",
        *keys::MEDIA_NAME => "Audio visualizer",
        *keys::APP_NAME => CLIENT_NAME,
        *keys::NODE_NAME => CLIENT_NAME,
        *keys::NODE_VIRTUAL => "true",
        *keys::NODE_LATENCY => format!("{LATENCY_FRAMES}/{RATE}"),
    };
    match target {
        Target::DefaultMonitor => props.insert(*keys::STREAM_CAPTURE_SINK, "true"),
        Target::Node { name, is_sink } => {
            props.insert(*keys::TARGET_OBJECT, name.as_str());
            if *is_sink {
                props.insert(*keys::STREAM_CAPTURE_SINK, "true");
            }
        }
    }
    props
}

/// The state the stream callbacks share.
struct Capturing<'a> {
    producer: &'a mut Producer<f32>,
    mix: ChannelMix,
    /// Created once the graph has settled on a format.
    decoder: Option<PcmDecoder>,
    mono: Vec<f32>,
}

impl Capturing<'_> {
    /// Interleaved little-endian `f32` frames from one PipeWire buffer.
    fn push(&mut self, bytes: &[u8]) {
        let Some(decoder) = &mut self.decoder else {
            return;
        };
        self.mono.clear();
        let mono = &mut self.mono;
        decoder.decode(bytes, |sample| mono.push(sample));
        // A full ring drops samples; it never stalls the graph.
        let _ = self.producer.push_partial_slice(&self.mono);
    }
}

/// One connection with a capture stream, run until stopped or failed.
fn record(
    mix: ChannelMix,
    target: &Target,
    producer: &mut Producer<f32>,
    stop: &AtomicBool,
) -> Result<(), PipewireError> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(connect_error)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(connect_error)?;
    let core = context.connect_rc(None).map_err(connect_error)?;

    let failure: Rc<RefCell<Option<String>>> = Rc::default();
    let _core_listener = core
        .add_listener_local()
        .error({
            let failure = Rc::clone(&failure);
            move |id, _sequence, _result, message| {
                // Errors of the core itself mean the connection is unusable.
                if id == pw::core::PW_ID_CORE {
                    *failure.borrow_mut() = Some(message.to_owned());
                }
            }
        })
        .register();

    let stream = pw::stream::StreamRc::new(core.clone(), CLIENT_NAME, stream_properties(target))
        .map_err(connect_error)?;
    let capturing = Capturing {
        producer,
        mix,
        decoder: None,
        mono: Vec::with_capacity(16 * 1024),
    };
    let _listener = stream
        .add_local_listener_with_user_data(capturing)
        .state_changed({
            let failure = Rc::clone(&failure);
            move |_, _, _, state| {
                if let StreamState::Error(message) = state {
                    *failure.borrow_mut() = Some(message);
                }
            }
        })
        .param_changed(|_, capturing, id, param| {
            let Some(param) = param else { return };
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Ok((media_type, media_subtype)) = format_utils::parse_format(param) else {
                return;
            };
            if media_type != MediaType::Audio || media_subtype != MediaSubtype::Raw {
                return;
            }
            let mut info = AudioInfoRaw::default();
            if info.parse(param).is_ok() {
                let channels = u16::try_from(info.channels()).unwrap_or(1).max(1);
                capturing.decoder =
                    Some(PcmDecoder::new(RawFormat::F32le, channels, capturing.mix));
            }
        })
        .process(|stream, capturing| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let (offset, size) = {
                let chunk = data.chunk();
                (chunk.offset() as usize, chunk.size() as usize)
            };
            let Some(bytes) = data.data() else { return };
            let end = offset.saturating_add(size).min(bytes.len());
            capturing.push(&bytes[offset.min(end)..end]);
        })
        .register()
        .map_err(connect_error)?;

    let mut format = AudioInfoRaw::new();
    format.set_format(SpaFormat::F32LE);
    format.set_rate(RATE);
    format.set_channels(CHANNELS);
    let object = spa::pod::Object {
        type_: spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
        id: spa::param::ParamType::EnumFormat.as_raw(),
        properties: format.into(),
    };
    let values = spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &spa::pod::Value::Object(object),
    )
    .map_err(|error| PipewireError::Failed(format!("cannot describe the audio format: {error}")))?
    .0
    .into_inner();
    let pod = Pod::from_bytes(&values)
        .ok_or_else(|| PipewireError::Failed("the audio format description is invalid".into()))?;
    stream
        .connect(
            spa::utils::Direction::Input,
            None,
            StreamFlags::AUTOCONNECT | StreamFlags::MAP_BUFFERS,
            &mut [pod],
        )
        .map_err(connect_error)?;

    while !stop.load(Ordering::Relaxed) {
        if let Some(message) = failure.borrow().as_ref() {
            return Err(PipewireError::Failed(message.clone()));
        }
        mainloop.loop_().iterate(POLL_INTERVAL);
    }
    Ok(())
}

/// Capture through the PipeWire native protocol.
pub struct PipewireCapture {
    mix: ChannelMix,
    target: Target,
    link: Link,
    retry: Duration,
}

impl PipewireCapture {
    /// Probe the server and resolve `source`. Problems are reported here,
    /// before the terminal is taken over.
    pub fn open(config: &InputConfig) -> Result<Self, AudioError> {
        let target = resolve_target(&snapshot()?, &config.source)?;
        Ok(Self {
            mix: config.channel_mix,
            target,
            link: Link::new(true),
            retry: RETRY_INTERVAL,
        })
    }
}

impl Capture for PipewireCapture {
    fn format(&self) -> AudioFormat {
        AudioFormat {
            sample_rate: RATE,
            channels: CHANNELS as u16,
        }
    }

    fn label(&self) -> String {
        match &self.target {
            Target::DefaultMonitor => "pipewire: default output".to_owned(),
            Target::Node { name, .. } => format!("pipewire: {name}"),
        }
    }

    fn link(&self) -> Option<Link> {
        Some(self.link.clone())
    }

    /// Record until stopped; when the server goes away, retry every two seconds.
    fn run(&mut self, producer: &mut Producer<f32>, stop: &AtomicBool) -> Result<(), AudioError> {
        let (mix, target) = (self.mix, self.target.clone());
        supervise(
            stop,
            &self.link,
            self.retry,
            Some(()),
            || Ok(()),
            |()| record(mix, &target, producer, stop).map_err(AudioError::from),
        )
    }
}

/// The audio sources of the server, for `--list-sources`.
pub fn list_sources() -> Result<Vec<SourceEntry>, AudioError> {
    Ok(entries(&snapshot()?))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn node(serial: &str, name: &str, description: &str, class: &str) -> NodeInfo {
        NodeInfo {
            serial: serial.to_owned(),
            name: name.to_owned(),
            description: description.to_owned(),
            class: class.to_owned(),
        }
    }

    fn graph() -> Snapshot {
        Snapshot {
            nodes: vec![
                node("41", "alsa_output.speakers", "Speakers", "Audio/Sink"),
                node("42", "alsa_output.headphones", "Headphones", "Audio/Sink"),
                node("43", "alsa_input.mic", "Microphone", "Audio/Source"),
                node("44", "player", "A player", "Stream/Output/Audio"),
            ],
            default_sink: Some("alsa_output.headphones".to_owned()),
        }
    }

    #[test]
    fn default_metadata_values_are_json_with_a_name() {
        assert_eq!(
            parse_default_name(r#"{"name":"alsa_output.pci-0000_00_1f.3.analog-stereo"}"#),
            Some("alsa_output.pci-0000_00_1f.3.analog-stereo".to_owned())
        );
        for bad in ["", "not json", r#"{"id":3}"#, r#"{"name":7}"#, "null"] {
            assert_eq!(parse_default_name(bad), None, "{bad}");
        }
    }

    #[test]
    fn auto_follows_the_default_and_needs_an_output() {
        assert_eq!(
            resolve_target(&graph(), "auto").unwrap(),
            Target::DefaultMonitor
        );
        let no_outputs = Snapshot {
            nodes: vec![node("43", "alsa_input.mic", "Microphone", "Audio/Source")],
            default_sink: None,
        };
        let error = resolve_target(&no_outputs, "auto").unwrap_err().to_string();
        assert!(error.contains("no audio output"), "{error}");
    }

    #[test]
    fn names_and_serials_resolve_and_outputs_record_their_monitor() {
        let by_name = resolve_target(&graph(), "alsa_output.speakers").unwrap();
        assert_eq!(
            by_name,
            Target::Node {
                name: "alsa_output.speakers".to_owned(),
                is_sink: true
            }
        );
        // A serial resolves to the node's stable name.
        assert_eq!(resolve_target(&graph(), "41").unwrap(), by_name);
        assert_eq!(
            resolve_target(&graph(), "alsa_input.mic").unwrap(),
            Target::Node {
                name: "alsa_input.mic".to_owned(),
                is_sink: false
            }
        );
        // Streams are not sources, and unknown names are reported.
        for missing in ["player", "44", "nonexistent"] {
            let error = resolve_target(&graph(), missing).unwrap_err().to_string();
            assert!(
                error.contains(&format!("no source named `{missing}`")),
                "{error}"
            );
        }
    }

    #[test]
    fn listing_shows_outputs_as_monitors_and_marks_the_default() {
        let listed = entries(&graph());
        assert_eq!(listed.len(), 3, "streams are not listed");
        let speakers = &listed[0];
        assert!(speakers.monitor && !speakers.default);
        assert_eq!(speakers.description, "Monitor of Speakers");
        assert!(listed[1].monitor && listed[1].default);
        assert!(!listed[2].monitor && !listed[2].default);
        assert_eq!(listed[2].description, "Microphone");
    }

    #[test]
    fn stream_properties_mark_the_node_virtual_and_pick_the_link() {
        let default = stream_properties(&Target::DefaultMonitor);
        assert_eq!(default.get("node.virtual"), Some("true"));
        assert_eq!(default.get("stream.capture.sink"), Some("true"));
        assert_eq!(default.get("target.object"), None);
        assert_eq!(default.get("node.latency"), Some("480/48000"));
        assert_eq!(default.get("media.category"), Some("Capture"));
        let sink = stream_properties(&Target::Node {
            name: "alsa_output.speakers".to_owned(),
            is_sink: true,
        });
        assert_eq!(sink.get("target.object"), Some("alsa_output.speakers"));
        assert_eq!(sink.get("stream.capture.sink"), Some("true"));
        let input = stream_properties(&Target::Node {
            name: "alsa_input.mic".to_owned(),
            is_sink: false,
        });
        assert_eq!(input.get("target.object"), Some("alsa_input.mic"));
        assert_eq!(input.get("stream.capture.sink"), None);
    }

    #[test]
    fn captured_buffers_are_mixed_to_mono_once_a_format_is_known() {
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(64);
        let mut capturing = Capturing {
            producer: &mut producer,
            mix: ChannelMix::Average,
            decoder: None,
            mono: Vec::with_capacity(16),
        };
        let bytes: Vec<u8> = [0.5_f32, -0.5, 1.0, 0.0]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        // Before the graph negotiated a format nothing is decoded.
        capturing.push(&bytes);
        capturing.decoder = Some(PcmDecoder::new(RawFormat::F32le, 2, ChannelMix::Average));
        capturing.push(&bytes);
        assert_eq!(consumer.slots(), 2);
        assert_eq!(consumer.pop().unwrap(), 0.0);
        assert_eq!(consumer.pop().unwrap(), 0.5);
    }

    // --- Live checks against a real server -------------------------------
    //
    // Ignored by default. `GRAVA_LIVE_SOURCE` names the node to record. To
    // record nothing private, create a null sink, play a 440 Hz tone into it
    // and name it:
    //
    //   pactl load-module module-null-sink sink_name=grava_test
    //   paplay -d grava_test tone.wav &
    //   GRAVA_LIVE_SOURCE=grava_test cargo test --all-features --lib pipewire::tests::live_ -- --ignored --nocapture

    fn live_config() -> Option<InputConfig> {
        Some(InputConfig {
            method: crate::config::schema::InputMethod::Pipewire,
            source: std::env::var("GRAVA_LIVE_SOURCE").ok()?,
            ..InputConfig::default()
        })
    }

    #[test]
    #[ignore = "needs a PipeWire server; see the comment above"]
    fn live_capture_hears_the_tone_as_a4() {
        use crate::{
            audio::{SampleSource, StreamSource},
            config::Config,
            dsp::Analyzer,
        };

        let Some(config) = live_config() else { return };
        let capture = PipewireCapture::open(&config).unwrap();
        let rate = capture.format().sample_rate;
        let mut source = StreamSource::spawn(capture).unwrap();
        let defaults = Config::default();
        let mut analyzer = Analyzer::new(&defaults.analysis, &defaults.general, rate);
        let mut samples = Vec::new();
        let mut received = 0;
        let mut features = crate::dsp::AudioFeatures::default();
        for _ in 0..180 {
            samples.clear();
            source.pull(1.0 / 60.0, &mut samples).unwrap();
            received += samples.len();
            analyzer.push(&samples);
            features = *analyzer.analyze(1.0 / 60.0);
            std::thread::sleep(Duration::from_millis(16));
        }
        println!(
            "rate {rate} Hz, {received} samples in 3 s, energy {:.2}, pitch {:?} octave {} at {:?} Hz",
            features.energy, features.pitch_class, features.octave, features.dominant_hz
        );
        assert!(source.connected());
        assert!(received > rate as usize, "only {received} samples arrived");
        assert_eq!((features.pitch_class, features.octave), (Some(9), 4));
    }

    #[test]
    #[ignore = "needs a PipeWire server"]
    fn live_listing_finds_outputs_and_exactly_one_default() {
        let sources = list_sources().unwrap();
        for source in &sources {
            println!(
                "{} {:?} monitor={} default={}",
                source.name, source.description, source.monitor, source.default
            );
        }
        assert!(sources.iter().any(|source| source.monitor));
        assert_eq!(sources.iter().filter(|source| source.default).count(), 1);
    }

    #[test]
    #[ignore = "needs a PipeWire server"]
    fn live_capture_stays_open_for_inspection() {
        use crate::audio::{SampleSource, StreamSource};

        let Some(config) = live_config() else { return };
        let seconds: u64 = std::env::var("GRAVA_LIVE_HOLD_SECS")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(4);
        let mut source = StreamSource::spawn(PipewireCapture::open(&config).unwrap()).unwrap();
        let mut samples = Vec::new();
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(seconds) {
            samples.clear();
            source.pull(0.0, &mut samples).unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(source.connected());
    }
}
