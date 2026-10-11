//! A minimal in-process PulseAudio server for tests.
//!
//! It speaks just enough of the protocol for the capture client: handshake,
//! source and sink lookup, subscriptions and one record stream per connection
//! that carries a 440 Hz tone. Tests steer it with [`Event`]s to kill streams,
//! hang up, or change the default output.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::{
    collections::VecDeque,
    ffi::CString,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread::JoinHandle,
    time::{Duration, Instant},
};

use pulseaudio::protocol::{
    self, ChannelVolume, Command, RecordStreamParams, ServerInfo, SinkInfo, SourceInfo,
    SubscriptionEvent, SubscriptionEventFacility, SubscriptionEventType,
};

use super::Frames;

const VERSION: u16 = protocol::MAX_VERSION;

/// A source of the fake server; monitors name the sink they belong to.
#[derive(Debug, Clone)]
pub struct SourceSpec {
    pub name: &'static str,
    pub description: &'static str,
    pub monitor_of: Option<&'static str>,
    pub rate: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Tell the client its record stream was removed.
    KillStream,
    /// Announce a change of the server's defaults.
    DefaultChanged,
    /// Close the connection abruptly.
    Hangup,
}

pub struct Shared {
    /// Whether `@DEFAULT_MONITOR@` is understood as a source name.
    pub alias_supported: AtomicBool,
    pub sources: Mutex<Vec<SourceSpec>>,
    pub default_sink: Mutex<String>,
    pub connections: AtomicUsize,
    /// The parameters of every record stream created, in order.
    pub streams: Mutex<Vec<RecordStreamParams>>,
    mailboxes: Mutex<Vec<Arc<Mutex<VecDeque<Event>>>>>,
}

pub struct FakeServer {
    pub path: PathBuf,
    pub shared: Arc<Shared>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeServer {
    pub fn start(path: &Path, sources: Vec<SourceSpec>, default_sink: &str) -> Self {
        let shared = Arc::new(Shared {
            alias_supported: AtomicBool::new(true),
            sources: Mutex::new(sources),
            default_sink: Mutex::new(default_sink.to_owned()),
            connections: AtomicUsize::new(0),
            streams: Mutex::new(Vec::new()),
            mailboxes: Mutex::new(Vec::new()),
        });
        let _ = std::fs::remove_file(path);
        let listener = UnixListener::bind(path).expect("the test socket can be bound");
        listener
            .set_nonblocking(true)
            .expect("the listener can be non-blocking");
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
            std::thread::spawn(move || accept(&listener, &shared, &stop))
        };
        Self {
            path: path.to_owned(),
            shared,
            stop,
            thread: Some(thread),
        }
    }

    /// Deliver `event` to every open connection.
    pub fn send(&self, event: Event) {
        for mailbox in self.shared.mailboxes.lock().expect("mailboxes").iter() {
            mailbox.lock().expect("mailbox").push_back(event);
        }
    }

    /// Change the default output and announce it, as a desktop would.
    pub fn set_default_sink(&self, name: &str) {
        *self.shared.default_sink.lock().expect("default sink") = name.to_owned();
        self.send(Event::DefaultChanged);
    }

    pub fn connections(&self) -> usize {
        self.shared.connections.load(Ordering::Relaxed)
    }

    pub fn streams_created(&self) -> Vec<RecordStreamParams> {
        self.shared.streams.lock().expect("streams").clone()
    }

    /// Stop serving: hang up on everyone and remove the socket.
    pub fn shutdown(mut self) {
        self.send(Event::Hangup);
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn accept(listener: &UnixListener, shared: &Arc<Shared>, stop: &Arc<AtomicBool>) {
    let mut handlers = Vec::new();
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((socket, _)) => {
                shared.connections.fetch_add(1, Ordering::Relaxed);
                let mailbox = Arc::new(Mutex::new(VecDeque::new()));
                shared
                    .mailboxes
                    .lock()
                    .expect("mailboxes")
                    .push(Arc::clone(&mailbox));
                let (shared, stop) = (Arc::clone(shared), Arc::clone(stop));
                handlers.push(std::thread::spawn(move || {
                    let _ = socket.set_nonblocking(false);
                    serve(socket, &shared, &stop, &mailbox);
                }));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(2)),
        }
    }
    for handler in handlers {
        let _ = handler.join();
    }
}

struct Tone {
    channel: u32,
    rate: u32,
    frames_per_chunk: usize,
    sent: u64,
    next: Instant,
}

fn serve(
    mut socket: UnixStream,
    shared: &Shared,
    stop: &AtomicBool,
    mailbox: &Mutex<VecDeque<Event>>,
) {
    let _ = socket.set_read_timeout(Some(Duration::from_millis(4)));
    let mut frames = Frames::new();
    let mut tone: Option<Tone> = None;
    while !stop.load(Ordering::Relaxed) {
        let event = mailbox.lock().expect("mailbox").pop_front();
        match event {
            Some(Event::Hangup) => return,
            Some(Event::KillStream) => {
                if let Some(tone) = tone.take() {
                    let _ = protocol::write_command_message(
                        &mut socket,
                        u32::MAX,
                        &Command::RecordStreamKilled(tone.channel),
                        VERSION,
                    );
                }
            }
            Some(Event::DefaultChanged) => {
                let _ = protocol::write_command_message(
                    &mut socket,
                    u32::MAX,
                    &Command::SubscribeEvent(SubscriptionEvent {
                        event_facility: SubscriptionEventFacility::Server,
                        event_type: SubscriptionEventType::Changed,
                        index: None,
                    }),
                    VERSION,
                );
            }
            None => {}
        }
        if frames.fill(&mut socket).is_err() {
            return;
        }
        while let Ok(Some((_, total))) = frames.peek() {
            let payload = frames.payload(total).to_vec();
            frames.advance(total);
            if let Ok((sequence, command)) = Command::read_tag_prefixed(&mut &payload[..], VERSION)
            {
                answer(&mut socket, shared, sequence, command, &mut tone);
            }
        }
        if let Some(tone) = &mut tone
            && Instant::now() >= tone.next
        {
            send_tone(&mut socket, tone);
        }
    }
}

fn answer(
    socket: &mut UnixStream,
    shared: &Shared,
    sequence: u32,
    command: Command,
    tone: &mut Option<Tone>,
) {
    let sources = shared.sources.lock().expect("sources").clone();
    let default_sink = shared.default_sink.lock().expect("default sink").clone();
    let info_of = |position: usize| source_info(position, &sources[position]);
    let find = |request: &protocol::GetSourceInfo| -> Option<SourceInfo> {
        let by_name = |name: &str| sources.iter().position(|source| source.name == name);
        let position = match (&request.index, &request.name) {
            (Some(index), _) => (*index as usize)
                .checked_sub(100)
                .filter(|p| *p < sources.len()),
            (None, Some(name)) => {
                let name = name.to_string_lossy();
                if name == "@DEFAULT_MONITOR@" {
                    shared
                        .alias_supported
                        .load(Ordering::Relaxed)
                        .then(|| {
                            sources
                                .iter()
                                .position(|source| source.monitor_of == Some(default_sink.as_str()))
                        })
                        .flatten()
                } else {
                    by_name(&name)
                }
            }
            _ => None,
        };
        position.map(info_of)
    };
    let missing = |socket: &mut UnixStream| {
        let _ = protocol::write_error(socket, sequence, &protocol::PulseError::NoEntity);
    };
    match command {
        Command::Auth(_) => {
            let reply = protocol::AuthReply {
                version: VERSION,
                use_memfd: false,
                use_shm: false,
            };
            let _ = protocol::write_reply_message(socket, sequence, &reply, VERSION);
        }
        Command::SetClientName(_) => {
            let reply = protocol::SetClientNameReply { client_id: 1 };
            let _ = protocol::write_reply_message(socket, sequence, &reply, VERSION);
        }
        Command::GetServerInfo => {
            let reply = ServerInfo {
                server_name: Some(CString::new("fake pulse").expect("no nul")),
                default_sink_name: Some(CString::new(default_sink).expect("no nul")),
                ..Default::default()
            };
            let _ = protocol::write_reply_message(socket, sequence, &reply, VERSION);
        }
        Command::GetSinkInfo(request) => {
            let wanted = request.name.map(|name| name.to_string_lossy().into_owned());
            match sources
                .iter()
                .position(|source| source.monitor_of.map(str::to_owned) == wanted)
            {
                Some(position) => {
                    let reply = SinkInfo {
                        index: position as u32,
                        name: CString::new(wanted.unwrap_or_default()).expect("no nul"),
                        monitor_source_index: Some(100 + position as u32),
                        monitor_source_name: Some(
                            CString::new(sources[position].name).expect("no nul"),
                        ),
                        ..Default::default()
                    };
                    let _ = protocol::write_reply_message(socket, sequence, &reply, VERSION);
                }
                None => missing(socket),
            }
        }
        Command::GetSourceInfo(request) => match find(&request) {
            Some(reply) => {
                let _ = protocol::write_reply_message(socket, sequence, &reply, VERSION);
            }
            None => missing(socket),
        },
        Command::GetSourceInfoList => {
            let list: protocol::SourceInfoList = (0..sources.len()).map(info_of).collect();
            let _ = protocol::write_reply_message(socket, sequence, &list, VERSION);
        }
        Command::Subscribe(_) => {
            let _ = protocol::write_ack_message(socket, sequence);
        }
        Command::CreateRecordStream(params) => {
            shared.streams.lock().expect("streams").push(params.clone());
            let channel = 3;
            let reply = protocol::CreateRecordStreamReply {
                channel,
                stream_index: 7,
                buffer_attr: params.buffer_attr,
                sample_spec: params.sample_spec,
                channel_map: params.channel_map,
                ..Default::default()
            };
            let _ = protocol::write_reply_message(socket, sequence, &reply, VERSION);
            let rate = params.sample_spec.sample_rate;
            *tone = Some(Tone {
                channel,
                rate,
                frames_per_chunk: (rate / 100) as usize,
                sent: 0,
                next: Instant::now(),
            });
        }
        Command::DeleteRecordStream(_) => {
            *tone = None;
            let _ = protocol::write_ack_message(socket, sequence);
        }
        _ => {
            let _ = protocol::write_error(socket, sequence, &protocol::PulseError::Command);
        }
    }
}

fn source_info(position: usize, spec: &SourceSpec) -> SourceInfo {
    SourceInfo {
        index: 100 + position as u32,
        name: CString::new(spec.name).expect("no nul"),
        description: Some(CString::new(spec.description).expect("no nul")),
        sample_spec: protocol::SampleSpec {
            format: protocol::SampleFormat::S32Le,
            channels: 2,
            sample_rate: spec.rate,
        },
        channel_map: protocol::ChannelMap::stereo(),
        cvolume: ChannelVolume::norm(2),
        monitor_of_sink_index: spec.monitor_of.map(|_| position as u32),
        monitor_of_sink_name: spec
            .monitor_of
            .map(|sink| CString::new(sink).expect("no nul")),
        ..Default::default()
    }
}

/// One 10 ms data frame of a 440 Hz sine at amplitude 0.3, stereo float.
fn send_tone(socket: &mut UnixStream, tone: &mut Tone) {
    let mut payload = Vec::with_capacity(tone.frames_per_chunk * 8);
    for _ in 0..tone.frames_per_chunk {
        let phase = std::f32::consts::TAU * 440.0 * tone.sent as f32 / tone.rate as f32;
        let sample = (0.3 * phase.sin()).to_le_bytes();
        payload.extend_from_slice(&sample);
        payload.extend_from_slice(&sample);
        tone.sent += 1;
    }
    let descriptor = protocol::Descriptor {
        length: payload.len() as u32,
        channel: tone.channel,
        offset: 0,
        flags: protocol::DescriptorFlags::empty(),
    };
    use std::io::Write;
    let _ = protocol::write_descriptor(socket, &descriptor);
    let _ = socket.write_all(&payload);
    tone.next += Duration::from_millis(10);
}
