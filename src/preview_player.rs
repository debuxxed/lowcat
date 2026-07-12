use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rodio::{Decoder, DeviceSinkBuilder, MixerDeviceSink, Player, Source, cpal};

use crate::opus_source::OpusSource;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PreviewPosition {
    pub path: PathBuf,
    pub offset: Duration,
}

pub struct PreviewPlayer {
    output: Option<MixerDeviceSink>,
    active: Option<ActivePlayback>,
    current_path: Option<PathBuf>,
    current_duration: Option<Duration>,
    volume: f32,
}

enum ActivePlayback {
    Rodio {
        player: Player,
        started_offset: Duration,
    },
}

impl PreviewPlayer {
    pub fn new(volume: f32) -> Self {
        Self {
            output: None,
            active: None,
            current_path: None,
            current_duration: None,
            volume: volume.clamp(0., 1.),
        }
    }

    pub fn warm_up(&mut self) -> io::Result<()> {
        self.ensure_output().map(|_| ())
    }

    pub fn play_from(&mut self, path: PathBuf, offset: Duration) -> io::Result<()> {
        self.stop();
        self.play_rodio(path, PreviewStart::Offset(offset))
    }

    pub fn play_from_ratio(&mut self, path: PathBuf, ratio: f32) -> io::Result<()> {
        self.stop();
        self.play_rodio(path, PreviewStart::Ratio(ratio))
    }

    fn play_rodio(&mut self, path: PathBuf, start: PreviewStart) -> io::Result<()> {
        if is_opus(&path) {
            let source = OpusSource::open(&path)?;
            self.play_source(path, start, source)
        } else {
            let file = File::open(&path)?;
            let source = Decoder::try_from(file).map_err(io::Error::other)?;
            self.play_source(path, start, source)
        }
    }

    fn play_source<S>(&mut self, path: PathBuf, start: PreviewStart, source: S) -> io::Result<()>
    where
        S: Source + Send + 'static,
    {
        let duration = source.total_duration();
        let offset = match start {
            PreviewStart::Offset(offset) => offset,
            PreviewStart::Ratio(ratio) => {
                let duration = duration.ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "audio duration unavailable")
                })?;
                offset_for_ratio(duration, ratio)
            }
        };

        let player = Player::connect_new(self.ensure_output()?.mixer());
        player.set_volume(self.volume);
        player.pause();
        player.append(source);
        if !offset.is_zero() {
            player.try_seek(offset).map_err(io::Error::other)?;
        }
        player.play();

        self.active = Some(ActivePlayback::Rodio {
            player,
            started_offset: offset,
        });
        self.current_path = Some(path);
        self.current_duration = duration;
        Ok(())
    }

    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0., 1.);
        if let Some(ActivePlayback::Rodio { player, .. }) = self.active.as_ref() {
            player.set_volume(self.volume);
        }
    }

    fn ensure_output(&mut self) -> io::Result<&MixerDeviceSink> {
        if self.output.is_none() {
            let mut output = DeviceSinkBuilder::from_default_device()
                .map_err(io::Error::other)?
                .with_buffer_size(cpal::BufferSize::Fixed(256))
                .open_sink_or_fallback()
                .map_err(io::Error::other)?;
            output.log_on_drop(false);
            self.output = Some(output);
        }
        self.output
            .as_ref()
            .ok_or_else(|| io::Error::other("audio output unavailable"))
    }

    pub fn pause_or_stop(&mut self) -> Option<PreviewPosition> {
        self.stop()
    }

    pub fn stop(&mut self) -> Option<PreviewPosition> {
        let position = self.current_position();
        if let Some(active) = self.active.take() {
            match active {
                ActivePlayback::Rodio { player, .. } => player.stop(),
            }
        }
        self.current_path = None;
        self.current_duration = None;
        position
    }

    pub fn is_playing(&self) -> bool {
        self.active.is_some()
    }

    pub fn current_position(&self) -> Option<PreviewPosition> {
        let path = self.current_path.clone()?;
        let output_delay = self.output_delay();
        let offset = match self.active.as_ref()? {
            ActivePlayback::Rodio {
                player,
                started_offset,
            } => audible_position(player.get_pos(), *started_offset, output_delay),
        };
        Some(PreviewPosition { path, offset })
    }

    fn output_delay(&self) -> Duration {
        let Some(output) = self.output.as_ref() else {
            return Duration::ZERO;
        };
        let cpal::BufferSize::Fixed(frames) = output.config().buffer_size() else {
            return Duration::from_millis(50);
        };
        let sample_rate = output.config().sample_rate().get();
        Duration::from_secs_f64((*frames as f64 * 2.) / sample_rate as f64)
    }

    pub fn current_duration(&self) -> Option<Duration> {
        self.current_duration
    }

    pub fn finish_if_ended(&mut self) -> Option<PreviewPosition> {
        let ended = match self.active.as_mut()? {
            ActivePlayback::Rodio { player, .. } => player.empty(),
        };
        if !ended {
            return None;
        }

        self.active = None;
        let path = self.current_path.take()?;
        self.current_duration = None;
        Some(PreviewPosition {
            path,
            offset: Duration::ZERO,
        })
    }
}

impl Drop for PreviewPlayer {
    fn drop(&mut self) {
        self.stop();
    }
}

enum PreviewStart {
    Offset(Duration),
    Ratio(f32),
}

fn is_opus(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("opus"))
}

fn audible_position(raw: Duration, started_offset: Duration, output_delay: Duration) -> Duration {
    raw.saturating_sub(output_delay).max(started_offset)
}

pub fn offset_for_ratio(duration: Duration, ratio: f32) -> Duration {
    duration.mul_f32(ratio.clamp(0., 1.))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_offset_is_clamped() {
        let duration = Duration::from_secs(100);
        assert_eq!(offset_for_ratio(duration, -1.), Duration::ZERO);
        assert_eq!(offset_for_ratio(duration, 0.75), Duration::from_secs(75));
        assert_eq!(offset_for_ratio(duration, 2.), duration);
    }

    #[test]
    fn opus_extension_selects_opus_decoder() {
        assert!(is_opus(Path::new("preview.opus")));
        assert!(is_opus(Path::new("preview.OPUS")));
        assert!(!is_opus(Path::new("preview.ogg")));
        assert!(!is_opus(Path::new("preview.wav")));
    }

    #[test]
    fn audible_position_waits_for_queued_audio() {
        let start = Duration::from_secs(30);
        let delay = Duration::from_millis(12);
        assert_eq!(audible_position(start, start, delay), start);
        assert_eq!(
            audible_position(start + Duration::from_millis(20), start, delay),
            start + Duration::from_millis(8)
        );
    }
}
