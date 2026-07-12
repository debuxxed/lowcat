use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use rodio::{ChannelCount, Sample, SampleRate, Source, source::SeekError};
use symphonia::core::audio::{AudioBufferRef, SampleBuffer, SignalSpec};
use symphonia::core::codecs::{
    CODEC_TYPE_NULL, CodecParameters, CodecRegistry, Decoder, DecoderOptions,
};
use symphonia::core::errors::Error;
use symphonia::core::formats::{FormatOptions, FormatReader, SeekMode, SeekTo, SeekedTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units;
use symphonia::default::get_probe;
use symphonia_adapter_libopus::OpusDecoder;

pub struct OpusSource {
    codec_params: CodecParameters,
    decoder: Box<dyn Decoder>,
    format: Box<dyn FormatReader>,
    track_id: u32,
    total_duration: Option<Duration>,
    buffer: SampleBuffer<Sample>,
    buffer_offset: usize,
    spec: SignalSpec,
}

impl OpusSource {
    pub fn open(path: &Path) -> io::Result<Self> {
        let file = File::open(path)?;
        let stream = MediaSourceStream::new(Box::new(file), Default::default());
        let mut hint = Hint::new();
        hint.with_extension("opus");
        let mut probed = get_probe()
            .format(
                &hint,
                stream,
                &FormatOptions {
                    enable_gapless: true,
                    ..Default::default()
                },
                &MetadataOptions::default(),
            )
            .map_err(symphonia_error)?;
        let track = probed
            .format
            .tracks()
            .iter()
            .find(|track| track.codec_params.codec != CODEC_TYPE_NULL)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "no Opus track"))?;
        let track_id = track.id;
        let codec_params = track.codec_params.clone();
        let total_duration = codec_params
            .time_base
            .zip(codec_params.n_frames)
            .map(|(base, frames)| Duration::from(base.calc_time(frames)))
            .filter(|duration| !duration.is_zero());
        let mut decoder = make_decoder(&codec_params).map_err(symphonia_error)?;

        let (spec, buffer) = decode_next_buffer(&mut *probed.format, &mut *decoder, track_id)
            .map_err(symphonia_error)?;

        Ok(Self {
            codec_params,
            decoder,
            format: probed.format,
            track_id,
            total_duration,
            buffer,
            buffer_offset: 0,
            spec,
        })
    }

    fn refine_position(&mut self, seeked: SeekedTo) -> Result<(), SeekError> {
        let time_base = self
            .codec_params
            .time_base
            .ok_or_else(|| other_seek_error("Opus stream has no time base"))?;
        let duration = Duration::from(
            time_base.calc_time(seeked.required_ts.saturating_sub(seeked.actual_ts)),
        );
        let mut samples_to_skip = (duration.as_secs_f64()
            * self.sample_rate().get() as f64
            * self.channels().get() as f64)
            .ceil() as usize;
        samples_to_skip -= samples_to_skip % self.channels().get() as usize;
        for _ in 0..samples_to_skip {
            if self.next().is_none() {
                break;
            }
        }
        Ok(())
    }
}

impl Iterator for OpusSource {
    type Item = Sample;

    fn next(&mut self) -> Option<Self::Item> {
        if self.buffer_offset >= self.buffer.len() {
            let (spec, buffer) =
                decode_next_buffer(&mut *self.format, &mut *self.decoder, self.track_id).ok()?;
            self.spec = spec;
            self.buffer = buffer;
            self.buffer_offset = 0;
        }

        let sample = *self.buffer.samples().get(self.buffer_offset)?;
        self.buffer_offset += 1;
        Some(sample)
    }
}

impl Source for OpusSource {
    fn current_span_len(&self) -> Option<usize> {
        Some(self.buffer.len())
    }

    fn channels(&self) -> ChannelCount {
        ChannelCount::new(self.spec.channels.count() as u16)
            .expect("Opus streams have at least one channel")
    }

    fn sample_rate(&self) -> SampleRate {
        SampleRate::new(self.spec.rate).expect("Opus streams have a nonzero sample rate")
    }

    fn total_duration(&self) -> Option<Duration> {
        self.total_duration
    }

    fn try_seek(&mut self, position: Duration) -> Result<(), SeekError> {
        let target = self
            .total_duration
            .map_or(position, |duration| position.min(duration));
        let seeked = self
            .format
            .seek(
                SeekMode::Accurate,
                SeekTo::Time {
                    time: target.into(),
                    track_id: Some(self.track_id),
                },
            )
            .map_err(|error| other_seek_error(error.to_string()))?;
        self.decoder = make_decoder(&self.codec_params)
            .map_err(|error| other_seek_error(error.to_string()))?;
        self.buffer_offset = usize::MAX;
        self.refine_position(seeked)
    }
}

fn make_decoder(params: &CodecParameters) -> Result<Box<dyn Decoder>, Error> {
    let mut registry = CodecRegistry::new();
    registry.register_all::<OpusDecoder>();
    registry.make(params, &DecoderOptions::default())
}

fn decode_next_buffer(
    format: &mut dyn FormatReader,
    decoder: &mut dyn Decoder,
    track_id: u32,
) -> Result<(SignalSpec, SampleBuffer<Sample>), Error> {
    loop {
        let packet = format.next_packet()?;
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(decoded) if decoded.frames() > 0 => {
                let spec = *decoded.spec();
                return Ok((spec, sample_buffer(decoded, &spec)));
            }
            Ok(_) | Err(Error::DecodeError(_)) => continue,
            Err(error) => return Err(error),
        }
    }
}

fn sample_buffer(decoded: AudioBufferRef<'_>, spec: &SignalSpec) -> SampleBuffer<Sample> {
    let mut buffer = SampleBuffer::new(units::Duration::from(decoded.capacity() as u64), *spec);
    buffer.copy_interleaved_ref(decoded);
    buffer
}

fn symphonia_error(error: Error) -> io::Error {
    io::Error::other(error.to_string())
}

fn other_seek_error(error: impl Into<String>) -> SeekError {
    SeekError::Other(Arc::new(io::Error::other(error.into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Stdio;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn decodes_and_seeks_ogg_opus() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("lowcat-opus-source-{unique}.opus"));
        let status = crate::media_tools::command("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-t",
                "0.2",
                "-c:a",
                "libopus",
                "-y",
            ])
            .arg(&path)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());

        let mut source = OpusSource::open(&path).unwrap();
        assert!(source.total_duration().is_some_and(|duration| {
            duration >= Duration::from_millis(190) && duration <= Duration::from_millis(210)
        }));
        assert!(source.by_ref().take(1_000).any(|sample| sample != 0.));
        source.try_seek(Duration::from_millis(100)).unwrap();
        assert!(source.by_ref().take(1_000).any(|sample| sample != 0.));

        fs::remove_file(path).unwrap();
    }
}
