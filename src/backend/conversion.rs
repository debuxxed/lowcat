use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::{AudioFormat, ConvertConflictBehavior};

use super::scanner::{extension, is_library_file};

pub(crate) fn import_to_folder(
    folder: &Path,
    source: &Path,
    mut on_conversion_progress: impl FnMut(f32),
) -> io::Result<PathBuf> {
    if !folder.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "category folder does not exist",
        ));
    }
    if !source.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "source is not a file",
        ));
    }
    if !probe_is_audio(source) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source is not readable audio",
        ));
    }

    if !is_library_file(source) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported audio format",
        ));
    }

    let extension = extension(source).unwrap_or_else(|| "opus".to_string());
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("import");

    let final_path = unique_destination(folder, stem, &extension);
    let temp_path = temp_destination(folder, &extension);

    on_conversion_progress(100.);
    let produced = fs::copy(source, &temp_path).map(|_| ());
    if let Err(error) = produced.and_then(|()| verify_exists(&temp_path)) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    if let Err(error) = fs::rename(&temp_path, &final_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }

    fs::remove_file(source)?;
    Ok(final_path)
}

fn probe_is_audio(path: &Path) -> bool {
    crate::media_tools::command("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .map(|output| output.status.success() && !output.stdout.is_empty())
        .unwrap_or(false)
}

pub(super) fn convert_file_to_format(
    source: &Path,
    folder: &Path,
    target: AudioFormat,
    behavior: ConvertConflictBehavior,
    on_progress: impl FnMut(f32),
) -> io::Result<PathBuf> {
    if !source.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "source is not a file",
        ));
    }
    if !probe_is_audio(source) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "source is not readable audio",
        ));
    }

    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("converted");
    let final_path = conversion_destination(folder, stem, target.extension(), behavior);
    let temp_path = temp_destination(folder, target.extension());
    let output_args: &[&str] = match target {
        AudioFormat::Mp3 => &["-vn", "-c:a", "libmp3lame", "-q:a", "2", "-y"],
        AudioFormat::Wav => &["-vn", "-c:a", "pcm_s16le", "-y"],
        AudioFormat::Opus => &["-vn", "-c:a", "libopus", "-y"],
        AudioFormat::Flac => &["-vn", "-c:a", "flac", "-y"],
    };
    let result = convert_media(source, &temp_path, output_args, on_progress);
    if let Err(error) = result.and_then(|()| verify_exists(&temp_path)) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if behavior == ConvertConflictBehavior::Overwrite && final_path.exists() {
        fs::remove_file(&final_path)?;
    }
    if let Err(error) = fs::rename(&temp_path, &final_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    if behavior == ConvertConflictBehavior::Overwrite && source != final_path {
        fs::remove_file(source)?;
    }
    Ok(final_path)
}

fn convert_media(
    source: &Path,
    dest: &Path,
    output_args: &[&str],
    mut on_progress: impl FnMut(f32),
) -> io::Result<()> {
    let duration_us = media_duration_us(source);
    on_progress(0.);
    let mut child = crate::media_tools::command("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-progress",
            "pipe:1",
            "-nostats",
            "-i",
        ])
        .arg(source)
        .args(output_args)
        .arg(dest)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some(value) = parse_ffmpeg_progress(&line, duration_us) {
                on_progress(value);
            }
        }
    }
    let status = child.wait()?;
    if status.success() {
        on_progress(100.);
        Ok(())
    } else {
        Err(io::Error::other("ffmpeg conversion failed"))
    }
}

fn media_duration_us(path: &Path) -> Option<f64> {
    let output = crate::media_tools::command("ffprobe")
        .args([
            "-v",
            "error",
            "-show_entries",
            "format=duration",
            "-of",
            "default=noprint_wrappers=1:nokey=1",
        ])
        .arg(path)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let seconds = String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<f64>()
        .ok()?;
    (seconds.is_finite() && seconds > 0.).then_some(seconds * 1_000_000.)
}

fn parse_ffmpeg_progress(line: &str, duration_us: Option<f64>) -> Option<f32> {
    let duration_us = duration_us?;
    let (key, raw) = line.split_once('=')?;
    let elapsed_us = match key {
        "out_time_us" | "out_time_ms" => raw.parse::<f64>().ok()?,
        _ => return None,
    };
    Some(((elapsed_us / duration_us) * 100.).clamp(0., 100.) as f32)
}

fn verify_exists(path: &Path) -> io::Result<()> {
    if path.is_file() {
        Ok(())
    } else {
        Err(io::Error::other("import produced no destination file"))
    }
}

fn unique_destination(folder: &Path, stem: &str, extension: &str) -> PathBuf {
    let first = folder.join(format!("{stem}.{extension}"));
    if !first.exists() {
        return first;
    }
    for n in 2.. {
        let candidate = folder.join(format!("{stem} {n}.{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("an unused conflict name always exists")
}

fn conversion_destination(
    folder: &Path,
    stem: &str,
    extension: &str,
    behavior: ConvertConflictBehavior,
) -> PathBuf {
    match behavior {
        ConvertConflictBehavior::Overwrite => folder.join(format!("{stem}.{extension}")),
        ConvertConflictBehavior::AddCopy => unique_destination(folder, stem, extension),
    }
}

fn temp_destination(folder: &Path, extension: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    folder.join(format!(
        ".lowcat-import-{}-{nanos}.{extension}",
        std::process::id()
    ))
}
