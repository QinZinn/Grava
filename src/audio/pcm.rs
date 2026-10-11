//! Raw PCM decoding to mono `f32`, shared by the stdin and FIFO inputs.

use std::{
    io::{self, Read},
    sync::atomic::{AtomicBool, Ordering},
};

use rtrb::Producer;

use crate::config::schema::{ChannelMix, InputConfig, SampleFormat};

use super::{AudioError, SampleSource};

/// Bytes read from the pipe at a time.
const READ_SIZE: usize = 16 * 1024;

/// How to turn one interleaved frame into one mono sample.
#[derive(Debug, Clone, Copy)]
struct Layout {
    format: SampleFormat,
    channels: usize,
    sample_bytes: usize,
    mix: ChannelMix,
}

impl Layout {
    /// `frame` holds exactly one sample per channel.
    fn mono(self, frame: &[u8]) -> f32 {
        let sample = |channel: usize| -> f32 {
            let bytes = &frame[channel * self.sample_bytes..(channel + 1) * self.sample_bytes];
            match self.format {
                SampleFormat::S16le => {
                    f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32_768.0
                }
                SampleFormat::S32le => {
                    i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) as f32
                        / 2_147_483_648.0
                }
                SampleFormat::F32le => {
                    let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                    if value.is_finite() {
                        value.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    }
                }
            }
        };
        match self.mix {
            ChannelMix::Left => sample(0),
            ChannelMix::Right => sample(1.min(self.channels - 1)),
            ChannelMix::Average => {
                let mut sum = 0.0;
                for channel in 0..self.channels {
                    sum += sample(channel);
                }
                sum / self.channels as f32
            }
        }
    }
}

/// Mix interleaved `f32` frames down to mono, calling `sink` once per frame.
/// A trailing partial frame is ignored; non-finite samples count as silence.
/// Backends that receive native samples (ALSA, PipeWire) use this; byte
/// streams go through [`PcmDecoder`].
pub fn mix_interleaved(
    samples: &[f32],
    channels: usize,
    mix: ChannelMix,
    mut sink: impl FnMut(f32),
) {
    let channels = channels.max(1);
    let clean = |value: f32| {
        if value.is_finite() {
            value.clamp(-1.0, 1.0)
        } else {
            0.0
        }
    };
    for frame in samples.chunks_exact(channels) {
        sink(match mix {
            ChannelMix::Left => clean(frame[0]),
            ChannelMix::Right => clean(frame[1.min(channels - 1)]),
            ChannelMix::Average => frame.iter().map(|&v| clean(v)).sum::<f32>() / channels as f32,
        });
    }
}

/// Decodes a byte stream of interleaved little-endian PCM, which may be cut at
/// any byte: a partial frame is carried over to the next call.
#[derive(Debug, Clone)]
pub struct PcmDecoder {
    layout: Layout,
    frame_bytes: usize,
    partial: Vec<u8>,
}

impl PcmDecoder {
    pub fn new(format: SampleFormat, channels: u16, mix: ChannelMix) -> Self {
        let sample_bytes = match format {
            SampleFormat::S16le => 2,
            SampleFormat::S32le | SampleFormat::F32le => 4,
        };
        let channels = usize::from(channels.max(1));
        let frame_bytes = sample_bytes * channels;
        Self {
            layout: Layout {
                format,
                channels,
                sample_bytes,
                mix,
            },
            frame_bytes,
            partial: Vec::with_capacity(frame_bytes),
        }
    }

    pub fn from_config(config: &InputConfig) -> Self {
        Self::new(config.sample_format, config.channels, config.channel_mix)
    }

    /// Bytes per interleaved frame (all channels of one instant).
    pub fn frame_bytes(&self) -> usize {
        self.frame_bytes
    }

    /// Decode `bytes`, calling `sink` once per complete frame.
    pub fn decode(&mut self, mut bytes: &[u8], mut sink: impl FnMut(f32)) {
        if !self.partial.is_empty() {
            let take = (self.frame_bytes - self.partial.len()).min(bytes.len());
            self.partial.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.partial.len() < self.frame_bytes {
                return;
            }
            sink(self.layout.mono(&self.partial));
            self.partial.clear();
        }
        let mut frames = bytes.chunks_exact(self.frame_bytes);
        for frame in &mut frames {
            sink(self.layout.mono(frame));
        }
        self.partial.extend_from_slice(frames.remainder());
    }
}

/// Read `reader` until it ends or `stop` is set, pushing mono samples into the
/// ring buffer. A full ring drops samples; this function never waits for room.
pub fn pump(
    reader: &mut impl Read,
    decoder: &mut PcmDecoder,
    producer: &mut Producer<f32>,
    stop: &AtomicBool,
) -> io::Result<()> {
    let mut bytes = vec![0_u8; READ_SIZE];
    let mut mono = Vec::with_capacity(READ_SIZE / decoder.frame_bytes() + 1);
    while !stop.load(Ordering::Relaxed) {
        let count = match reader.read(&mut bytes) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        mono.clear();
        decoder.decode(&bytes[..count], |sample| mono.push(sample));
        let _ = producer.push_partial_slice(&mono);
    }
    Ok(())
}

/// Raw PCM read in lock-step with simulated time, for deterministic headless
/// runs: each pull reads exactly the bytes of `dt` seconds, so the same input
/// always gives the same output.
pub struct SyncPcm<R> {
    reader: R,
    decoder: PcmDecoder,
    sample_rate: u32,
    carry: f64,
    bytes: Vec<u8>,
    ended: bool,
}

impl<R: Read> SyncPcm<R> {
    pub fn new(reader: R, sample_rate: u32, decoder: PcmDecoder) -> Self {
        Self {
            reader,
            decoder,
            sample_rate,
            carry: 0.0,
            bytes: vec![0; READ_SIZE],
            ended: false,
        }
    }
}

impl<R: Read> SampleSource for SyncPcm<R> {
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn pull(&mut self, dt: f32, out: &mut Vec<f32>) -> Result<(), AudioError> {
        if dt.is_finite() && dt > 0.0 {
            self.carry += f64::from(dt) * f64::from(self.sample_rate);
        }
        // `dt` arrives as f32; the epsilon keeps 10 ms at 1 kHz at 10 samples.
        let wanted = (self.carry + 1e-4).floor() as usize;
        self.carry -= wanted as f64;
        let start = out.len();
        let frame_bytes = self.decoder.frame_bytes();
        while !self.ended && out.len() - start < wanted {
            let missing = wanted - (out.len() - start);
            let request = (missing * frame_bytes).min(self.bytes.len());
            match self.reader.read(&mut self.bytes[..request]) {
                Ok(0) => self.ended = true,
                Ok(count) => self
                    .decoder
                    .decode(&self.bytes[..count], |sample| out.push(sample)),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(AudioError::io("cannot read PCM input", error)),
            }
        }
        // Once the input ends, time goes on in silence.
        out.resize(start + wanted, 0.0);
        Ok(())
    }

    fn connected(&self) -> bool {
        !self.ended
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::io::Cursor;

    use super::*;

    fn decode_all(decoder: &mut PcmDecoder, chunks: &[&[u8]]) -> Vec<f32> {
        let mut out = Vec::new();
        for chunk in chunks {
            decoder.decode(chunk, |sample| out.push(sample));
        }
        out
    }

    #[test]
    fn s16_stereo_is_averaged_or_selected() {
        // Frames: (16384, -16384), (32767, 0), (-32768, -32768).
        let bytes: Vec<u8> = [16384_i16, -16384, 32767, 0, -32768, -32768]
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect();
        let average = decode_all(
            &mut PcmDecoder::new(SampleFormat::S16le, 2, ChannelMix::Average),
            &[&bytes],
        );
        assert_eq!(average.len(), 3);
        assert_eq!(average[0], 0.0);
        assert!((average[1] - 0.499_98).abs() < 1e-4);
        assert_eq!(average[2], -1.0);
        let left = decode_all(
            &mut PcmDecoder::new(SampleFormat::S16le, 2, ChannelMix::Left),
            &[&bytes],
        );
        assert_eq!(left, [0.5, 32767.0 / 32768.0, -1.0]);
        let right = decode_all(
            &mut PcmDecoder::new(SampleFormat::S16le, 2, ChannelMix::Right),
            &[&bytes],
        );
        assert_eq!(right, [-0.5, 0.0, -1.0]);
    }

    #[test]
    fn s32_f32_and_mono_formats() {
        let s32: Vec<u8> = [i32::MIN, 0, 1 << 30]
            .into_iter()
            .flat_map(i32::to_le_bytes)
            .collect();
        let decoded = decode_all(
            &mut PcmDecoder::new(SampleFormat::S32le, 1, ChannelMix::Average),
            &[&s32],
        );
        assert_eq!(decoded, [-1.0, 0.0, 0.5]);
        let floats: Vec<u8> = [0.25_f32, -2.0, f32::NAN, f32::INFINITY]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .collect();
        let decoded = decode_all(
            &mut PcmDecoder::new(SampleFormat::F32le, 1, ChannelMix::Right),
            &[&floats],
        );
        // `right` on a mono stream falls back to the only channel; bad values
        // are clamped or silenced.
        assert_eq!(decoded, [0.25, -1.0, 0.0, 0.0]);
    }

    #[test]
    fn interleaved_floats_mix_like_encoded_bytes() {
        let samples = [0.5_f32, -0.5, 1.0, 0.0, f32::NAN, 0.25, 7.0, -7.0, 0.1];
        let collect = |mix| {
            let mut out = Vec::new();
            mix_interleaved(&samples, 2, mix, |sample| out.push(sample));
            out
        };
        // The odd ninth sample is a partial frame and is dropped.
        assert_eq!(collect(ChannelMix::Left), [0.5, 1.0, 0.0, 1.0]);
        assert_eq!(collect(ChannelMix::Right), [-0.5, 0.0, 0.25, -1.0]);
        assert_eq!(collect(ChannelMix::Average), [0.0, 0.5, 0.125, 0.0]);
        // Bytes and native floats agree.
        let bytes: Vec<u8> = samples.iter().flat_map(|v| v.to_le_bytes()).collect();
        let mut decoded = Vec::new();
        PcmDecoder::new(SampleFormat::F32le, 2, ChannelMix::Average)
            .decode(&bytes, |sample| decoded.push(sample));
        assert_eq!(decoded, collect(ChannelMix::Average));
        let mut mono = Vec::new();
        mix_interleaved(&[0.3, 0.6], 1, ChannelMix::Right, |s| mono.push(s));
        assert_eq!(mono, [0.3, 0.6]);
    }

    #[test]
    fn arbitrary_chunk_boundaries_do_not_change_the_result() {
        let bytes: Vec<u8> = (0..60_i16)
            .map(|i| i * 500 - 15_000)
            .flat_map(i16::to_le_bytes)
            .collect();
        let whole = decode_all(
            &mut PcmDecoder::new(SampleFormat::S16le, 3, ChannelMix::Average),
            &[&bytes],
        );
        assert_eq!(whole.len(), 20);
        for size in 1..=17 {
            let mut decoder = PcmDecoder::new(SampleFormat::S16le, 3, ChannelMix::Average);
            let chunks: Vec<&[u8]> = bytes.chunks(size).collect();
            assert_eq!(decode_all(&mut decoder, &chunks), whole, "chunks of {size}");
        }
        // A dangling partial frame is never emitted.
        let mut decoder = PcmDecoder::new(SampleFormat::S16le, 2, ChannelMix::Average);
        assert!(decode_all(&mut decoder, &[&[1, 2, 3]]).is_empty());
        assert_eq!(decode_all(&mut decoder, &[&[4]]).len(), 1);
    }

    #[test]
    fn pump_stops_at_end_of_input_and_drops_on_overflow() {
        let mut decoder = PcmDecoder::new(SampleFormat::S16le, 1, ChannelMix::Average);
        let bytes: Vec<u8> = [0x00, 0x40].repeat(10_000);
        let (mut producer, mut consumer) = rtrb::RingBuffer::new(1_000);
        pump(
            &mut Cursor::new(bytes),
            &mut decoder,
            &mut producer,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(consumer.slots(), 1_000);
        assert_eq!(consumer.pop().unwrap(), 0.5);
        // A stop flag that is already set reads nothing.
        let (mut producer, consumer) = rtrb::RingBuffer::new(100);
        pump(
            &mut Cursor::new(vec![0_u8; 100]),
            &mut decoder,
            &mut producer,
            &AtomicBool::new(true),
        )
        .unwrap();
        assert_eq!(consumer.slots(), 0);
    }

    #[test]
    fn sync_pcm_reads_exactly_the_requested_time_and_pads_after_the_end() {
        // 1 kHz mono s16 input: 10 samples per 10 ms step.
        let bytes: Vec<u8> = (0..25_i16).flat_map(|i| (i * 1000).to_le_bytes()).collect();
        let mut source = SyncPcm::new(
            Cursor::new(bytes),
            1_000,
            PcmDecoder::new(SampleFormat::S16le, 1, ChannelMix::Average),
        );
        let mut out = Vec::new();
        source.pull(0.010, &mut out).unwrap();
        assert_eq!(out.len(), 10);
        assert!(source.connected());
        source.pull(0.010, &mut out).unwrap();
        source.pull(0.010, &mut out).unwrap();
        assert_eq!(out.len(), 30);
        assert!(!source.connected());
        assert!((out[24] - 24_000.0 / 32_768.0).abs() < 1e-6);
        assert_eq!(&out[25..], [0.0; 5]);
        // Fractional sample counts carry over instead of drifting.
        let mut fractional = SyncPcm::new(
            Cursor::new(vec![0_u8; 20_000]),
            48_000,
            PcmDecoder::new(SampleFormat::S16le, 1, ChannelMix::Average),
        );
        let mut total = Vec::new();
        for _ in 0..60 {
            fractional.pull(1.0 / 60.0, &mut total).unwrap();
        }
        assert!((total.len() as i64 - 48_000).abs() <= 1, "{}", total.len());
    }

    #[test]
    fn sync_pcm_handles_short_reads() {
        /// A reader that returns one byte at a time.
        struct Dribble(Cursor<Vec<u8>>);
        impl Read for Dribble {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let end = buffer.len().min(1);
                self.0.read(&mut buffer[..end])
            }
        }
        let bytes: Vec<u8> = (0..40_i16).flat_map(i16::to_le_bytes).collect();
        let mut source = SyncPcm::new(
            Dribble(Cursor::new(bytes)),
            1_000,
            PcmDecoder::new(SampleFormat::S16le, 2, ChannelMix::Average),
        );
        let mut out = Vec::new();
        source.pull(0.020, &mut out).unwrap();
        assert_eq!(out.len(), 20);
        assert!((out[1] - 2.5 / 32_768.0).abs() < 1e-9);
    }
}
