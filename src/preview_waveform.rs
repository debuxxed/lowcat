use std::io::{self, Read};
use std::path::Path;
use std::process::Stdio;

use crate::model::{WAVEFORM_BAR_COUNT, WaveformBinary256};

const WAVEFORM_IMAGE_HEIGHT: usize = 256;

pub fn generate_waveform_binary256(path: &Path) -> io::Result<WaveformBinary256> {
    generate_waveform_from_image(path).or_else(|_| generate_waveform_from_samples(path))
}

fn generate_waveform_from_image(path: &Path) -> io::Result<WaveformBinary256> {
    let output = crate::media_tools::command("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-filter_complex")
        .arg(format!(
            "showwavespic=s={}x{}:split_channels=0:colors=white:scale=lin:draw=full:filter=average",
            WAVEFORM_BAR_COUNT, WAVEFORM_IMAGE_HEIGHT
        ))
        .arg("-frames:v")
        .arg("1")
        .arg("-pix_fmt")
        .arg("gray")
        .arg("-f")
        .arg("rawvideo")
        .arg("pipe:1")
        .output()?;

    if !output.status.success() {
        return Err(io::Error::other(format!(
            "ffmpeg waveform image failed for {}",
            path.display()
        )));
    }

    waveform_from_image_pixels(&output.stdout, WAVEFORM_IMAGE_HEIGHT)
}

fn generate_waveform_from_samples(path: &Path) -> io::Result<WaveformBinary256> {
    let channels = probe_audio_channels(path).unwrap_or(2).max(1);
    let mut child = crate::media_tools::command("ffmpeg")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-vn")
        .arg("-ac")
        .arg(channels.to_string())
        .arg("-f")
        .arg("f32le")
        .arg("-acodec")
        .arg("pcm_f32le")
        .arg("pipe:1")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;

    let mut bytes = Vec::new();
    if let Some(mut stdout) = child.stdout.take() {
        stdout.read_to_end(&mut bytes)?;
    }
    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!(
            "ffmpeg waveform decode failed for {}",
            path.display()
        )));
    }

    let samples = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    Ok(waveform_from_samples(samples, channels))
}

fn waveform_from_image_pixels(pixels: &[u8], height: usize) -> io::Result<WaveformBinary256> {
    if height == 0 || pixels.len() != WAVEFORM_BAR_COUNT * height {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unexpected waveform image dimensions",
        ));
    }

    let mut counts = [0f32; WAVEFORM_BAR_COUNT];
    for x in 0..WAVEFORM_BAR_COUNT {
        let mut count = 0usize;
        for y in 0..height {
            if pixels[y * WAVEFORM_BAR_COUNT + x] > 0 {
                count += 1;
            }
        }
        counts[x] = count as f32;
    }

    let max_count = counts.iter().copied().fold(0f32, f32::max);
    if max_count <= 1. {
        return Ok([0; WAVEFORM_BAR_COUNT]);
    }

    let mut waveform = [0u8; WAVEFORM_BAR_COUNT];
    for (out, count) in waveform.iter_mut().zip(counts) {
        if count > 0. {
            *out = ((count / max_count).clamp(0., 1.) * 255.).round().max(1.) as u8;
        }
    }
    Ok(waveform)
}

pub(crate) fn waveform_from_samples(
    samples: impl IntoIterator<Item = f32>,
    channels: usize,
) -> WaveformBinary256 {
    let channels = channels.max(1);
    let mut frames = Vec::new();
    let mut current_square_sum = 0f32;
    let mut channel_ix = 0usize;

    for sample in samples {
        let amp = sample.abs();
        if amp.is_finite() {
            current_square_sum += amp * amp;
        }
        channel_ix += 1;
        if channel_ix == channels {
            frames.push((current_square_sum / channels as f32).sqrt());
            current_square_sum = 0.;
            channel_ix = 0;
        }
    }
    if channel_ix > 0 {
        frames.push((current_square_sum / channel_ix as f32).sqrt());
    }

    if frames.is_empty() {
        return [0; WAVEFORM_BAR_COUNT];
    }

    let mut square_sums = [0f32; WAVEFORM_BAR_COUNT];
    let mut frame_counts = [0usize; WAVEFORM_BAR_COUNT];
    for (frame_ix, amp) in frames.iter().enumerate() {
        let bar_ix = (frame_ix * WAVEFORM_BAR_COUNT / frames.len()).min(WAVEFORM_BAR_COUNT - 1);
        square_sums[bar_ix] += amp * amp;
        frame_counts[bar_ix] += 1;
    }

    let rms = std::array::from_fn::<_, WAVEFORM_BAR_COUNT, _>(|ix| {
        if frame_counts[ix] == 0 {
            0.
        } else {
            (square_sums[ix] / frame_counts[ix] as f32).sqrt()
        }
    });
    let max_rms = rms.iter().copied().fold(0f32, f32::max);
    if max_rms <= f32::EPSILON {
        return [0; WAVEFORM_BAR_COUNT];
    }

    let mut waveform = [0u8; WAVEFORM_BAR_COUNT];
    for (out, value) in waveform.iter_mut().zip(rms) {
        *out = ((value / max_rms).clamp(0., 1.) * 255.).round() as u8;
    }
    waveform
}

fn probe_audio_channels(path: &Path) -> io::Result<usize> {
    let output = crate::media_tools::command("ffprobe")
        .arg("-v")
        .arg("error")
        .arg("-select_streams")
        .arg("a:0")
        .arg("-show_entries")
        .arg("stream=channels")
        .arg("-of")
        .arg("default=noprint_wrappers=1:nokey=1")
        .arg(path)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("ffprobe channel probe failed"));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    text.trim()
        .parse::<usize>()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_output_is_256_bytes() {
        let waveform = waveform_from_samples([0.1, 0.2, 0.3, 0.4], 1);
        assert_eq!(waveform.len(), WAVEFORM_BAR_COUNT);
    }

    #[test]
    fn waveform_combines_stereo_channels_for_one_sided_amplitude() {
        let waveform = waveform_from_samples([0.0, -0.5, 0.0, 1.0], 2);
        assert!(waveform.iter().any(|bar| *bar > 0));
        assert_eq!(waveform.iter().copied().max(), Some(255));
    }

    #[test]
    fn waveform_silence_is_zero() {
        let waveform = waveform_from_samples([0.0; 1024], 2);
        assert!(waveform.iter().all(|bar| *bar == 0));
    }

    #[test]
    fn waveform_normalizes_largest_bin_to_255() {
        let waveform = waveform_from_samples([0.1, 0.5, 0.25, 0.75], 1);
        assert_eq!(waveform.iter().copied().max(), Some(255));
    }

    #[test]
    fn waveform_uses_bin_energy_instead_of_single_peaks() {
        let mut samples = vec![0.; WAVEFORM_BAR_COUNT * 100];
        samples[0] = 1.;
        samples[100..200].fill(0.5);

        let waveform = waveform_from_samples(samples, 1);

        assert!(waveform[1] > waveform[0]);
        assert_eq!(waveform[1], 255);
    }

    #[test]
    fn waveform_image_pixels_normalize_column_heights() {
        let height = 4;
        let mut pixels = vec![0u8; WAVEFORM_BAR_COUNT * height];
        pixels[3 * WAVEFORM_BAR_COUNT] = 255;
        for y in 2..4 {
            pixels[y * WAVEFORM_BAR_COUNT + 1] = 255;
        }
        for y in 0..4 {
            pixels[y * WAVEFORM_BAR_COUNT + 2] = 255;
        }

        let waveform = waveform_from_image_pixels(&pixels, height).unwrap();

        assert!(waveform[0] > 0);
        assert!(waveform[1] > waveform[0]);
        assert_eq!(waveform[2], 255);
        assert_eq!(waveform[3], 0);
    }

    #[test]
    fn waveform_image_pixels_treat_single_pixel_line_as_silence() {
        let height = 4;
        let mut pixels = vec![0u8; WAVEFORM_BAR_COUNT * height];
        for x in 0..WAVEFORM_BAR_COUNT {
            pixels[2 * WAVEFORM_BAR_COUNT + x] = 255;
        }

        let waveform = waveform_from_image_pixels(&pixels, height).unwrap();

        assert!(waveform.iter().all(|bar| *bar == 0));
    }
}
