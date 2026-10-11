//! Raw PCM on standard input, for example
//! `parec -d @DEFAULT_MONITOR@ --format=s16le --rate=48000 --channels=2 | grava -i stdin`.

use std::{io, sync::atomic::AtomicBool};

use rtrb::Producer;

use crate::config::schema::InputConfig;

use super::{AudioError, AudioFormat, Capture, pcm};

pub struct StdinCapture {
    format: AudioFormat,
    decoder: pcm::PcmDecoder,
}

impl StdinCapture {
    pub fn new(config: &InputConfig) -> Self {
        Self {
            format: AudioFormat {
                sample_rate: config.sample_rate,
                channels: config.channels,
            },
            decoder: pcm::PcmDecoder::from_config(config),
        }
    }
}

impl Capture for StdinCapture {
    fn format(&self) -> AudioFormat {
        self.format
    }

    fn label(&self) -> String {
        "stdin".to_owned()
    }

    fn run(&mut self, producer: &mut Producer<f32>, stop: &AtomicBool) -> Result<(), AudioError> {
        // Locking inside the thread keeps the non-`Send` guard on one thread.
        // A read blocked on a silent pipe cannot be interrupted portably; the
        // process simply exits around it.
        let mut stdin = io::stdin().lock();
        pcm::pump(&mut stdin, &mut self.decoder, producer, stop)
            .map_err(|source| AudioError::io("cannot read PCM from stdin", source))
    }
}
