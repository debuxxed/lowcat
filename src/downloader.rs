use std::{
    error::Error,
    fmt, fs,
    io::{self, BufRead, BufReader},
    path::PathBuf,
    process::{Child, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use crate::model::{AudioFormat, Category};

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub category: Category,
    pub url: String,
    pub folder: PathBuf,
    pub format: AudioFormat,
}

#[derive(Debug, Clone)]
pub struct DownloadOutput {
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub enum DownloadProgressEvent {
    Label(String),
    Progress(f32),
}

#[derive(Debug, Clone)]
pub enum DownloadState {
    Idle,
    Running(DownloadStatus),
    Error(DownloadError),
}

#[derive(Debug, Clone)]
pub struct DownloadStatus {
    pub category: Category,
    pub label: String,
    pub progress: f32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadErrorKind {
    ClipboardEmpty,
    InvalidUrl,
    MissingCategoryFolder,
    ToolMissing,
    Canceled,
    Unavailable,
    Failed,
}

#[derive(Debug, Clone)]
pub struct DownloadError {
    pub kind: DownloadErrorKind,
    pub message: String,
}

impl DownloadError {
    pub fn clipboard_empty() -> Self {
        Self::new(DownloadErrorKind::ClipboardEmpty, "Clipboard has no text")
    }

    pub fn invalid_url() -> Self {
        Self::new(
            DownloadErrorKind::InvalidUrl,
            "No YouTube link in clipboard",
        )
    }

    pub fn missing_category_folder(category: Category) -> Self {
        Self::new(
            DownloadErrorKind::MissingCategoryFolder,
            format!("{} folder not set", category.label()),
        )
    }

    pub fn tool_missing() -> Self {
        Self::new(DownloadErrorKind::ToolMissing, "yt-dlp not found")
    }

    pub fn canceled() -> Self {
        Self::new(DownloadErrorKind::Canceled, "Download canceled")
    }

    pub fn unavailable() -> Self {
        Self::new(DownloadErrorKind::Unavailable, "Video unavailable")
    }

    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(DownloadErrorKind::Failed, message)
    }

    fn new(kind: DownloadErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
}

impl fmt::Display for DownloadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl Error for DownloadError {}

#[derive(Debug, Clone, Default)]
pub struct DownloadCancel {
    canceled: Arc<AtomicBool>,
}

impl DownloadCancel {
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::SeqCst);
    }

    pub fn is_canceled(&self) -> bool {
        self.canceled.load(Ordering::SeqCst)
    }
}

pub fn extract_youtube_url(text: &str) -> Result<String, DownloadError> {
    if text.trim().is_empty() {
        return Err(DownloadError::clipboard_empty());
    }

    text.split_whitespace()
        .map(trim_url_token)
        .find_map(normalize_youtube_url)
        .ok_or_else(DownloadError::invalid_url)
}

pub fn download_audio(
    request: DownloadRequest,
    cancel: DownloadCancel,
    mut on_progress: impl FnMut(DownloadProgressEvent),
) -> Result<DownloadOutput, DownloadError> {
    let _ = request.category;
    on_progress(DownloadProgressEvent::Progress(0.));

    fs::create_dir_all(&request.folder).map_err(|_| DownloadError::failed("Download failed"))?;
    let temp_dir = temp_download_dir(&request.folder);
    fs::create_dir_all(&temp_dir).map_err(|_| DownloadError::failed("Download failed"))?;

    let mut child = spawn_ytdlp(&request, &temp_dir).map_err(|error| {
        cleanup_temp_dir(&temp_dir);
        if error.kind() == io::ErrorKind::NotFound {
            DownloadError::tool_missing()
        } else {
            DownloadError::failed("Download failed")
        }
    })?;

    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let (line_tx, line_rx) = mpsc::channel();
    let mut reader_threads = Vec::new();
    if let Some(stdout) = stdout {
        reader_threads.push(spawn_line_reader(
            stdout,
            StreamKind::Stdout,
            line_tx.clone(),
        ));
    }
    if let Some(stderr) = stderr {
        reader_threads.push(spawn_line_reader(stderr, StreamKind::Stderr, line_tx));
    }

    let mut output_path = None;
    let mut error_text = String::new();
    loop {
        if cancel.is_canceled() {
            terminate_child(&mut child);
            join_readers(reader_threads);
            cleanup_temp_dir(&temp_dir);
            return Err(DownloadError::canceled());
        }

        match child.try_wait() {
            Ok(Some(status)) => {
                join_readers(reader_threads);
                while let Ok(line) = line_rx.try_recv() {
                    apply_ytdlp_line(&line, &mut on_progress, &mut output_path, &mut error_text);
                }
                cleanup_temp_dir(&temp_dir);
                if cancel.is_canceled() {
                    return Err(DownloadError::canceled());
                }
                if status.success() {
                    let Some(path) = output_path else {
                        return Err(DownloadError::failed("Download failed"));
                    };
                    on_progress(DownloadProgressEvent::Progress(100.));
                    return Ok(DownloadOutput { path });
                }
                return Err(classify_ytdlp_error(&error_text));
            }
            Ok(None) => {}
            Err(_) => {
                terminate_child(&mut child);
                join_readers(reader_threads);
                cleanup_temp_dir(&temp_dir);
                return Err(DownloadError::failed("Download failed"));
            }
        }

        match line_rx.recv_timeout(Duration::from_millis(100)) {
            Ok(line) => {
                apply_ytdlp_line(&line, &mut on_progress, &mut output_path, &mut error_text);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {}
        }
    }
}

fn spawn_ytdlp(request: &DownloadRequest, temp_dir: &PathBuf) -> io::Result<Child> {
    crate::media_tools::command("yt-dlp")
        .args([
            "--newline",
            "--no-playlist",
            "--extract-audio",
            "--audio-format",
            request.format.extension(),
            "--audio-quality",
            "0",
            "--format",
            "bestaudio/best",
            "--no-overwrites",
            "--progress",
            "--progress-template",
            "download:lowcat_progress:%(progress._percent_str)s",
            "--print",
            "before_dl:lowcat_title:%(title)s",
            "--print",
            "after_video:lowcat_file:%(filepath)s",
            "--paths",
        ])
        .arg(&request.folder)
        .arg("--paths")
        .arg(format!("temp:{}", temp_dir.display()))
        .args(["--output", "%(title).200B [%(id)s].%(ext)s"])
        .arg(&request.url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .stdin(Stdio::null())
        .spawn()
}

#[derive(Debug, Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

#[derive(Debug)]
struct ProcessLine {
    stream: StreamKind,
    text: String,
}

fn spawn_line_reader(
    stream: impl io::Read + Send + 'static,
    kind: StreamKind,
    line_tx: mpsc::Sender<ProcessLine>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        for line in BufReader::new(stream).lines().map_while(Result::ok) {
            let _ = line_tx.send(ProcessLine {
                stream: kind,
                text: line,
            });
        }
    })
}

fn join_readers(reader_threads: Vec<thread::JoinHandle<()>>) {
    for reader_thread in reader_threads {
        let _ = reader_thread.join();
    }
}

fn apply_ytdlp_line(
    line: &ProcessLine,
    on_progress: &mut impl FnMut(DownloadProgressEvent),
    output_path: &mut Option<PathBuf>,
    error_text: &mut String,
) {
    match line.stream {
        StreamKind::Stdout => {
            if let Some(title) = parse_ytdlp_title(&line.text) {
                on_progress(DownloadProgressEvent::Label(title.to_string()));
            }
            if let Some(progress) = parse_ytdlp_progress(&line.text) {
                on_progress(DownloadProgressEvent::Progress(progress));
            }
            if let Some(label) = parse_ytdlp_destination_label(&line.text) {
                on_progress(DownloadProgressEvent::Label(label));
            }
            if let Some(path) = parse_ytdlp_output_path(&line.text) {
                *output_path = Some(PathBuf::from(path));
            }
        }
        StreamKind::Stderr => {
            if !error_text.is_empty() {
                error_text.push('\n');
            }
            error_text.push_str(&line.text);
        }
    }
}

fn parse_ytdlp_title(line: &str) -> Option<&str> {
    line.strip_prefix("lowcat_title:")
        .map(str::trim)
        .filter(|title| !title.is_empty())
}

fn parse_ytdlp_output_path(line: &str) -> Option<&str> {
    line.strip_prefix("lowcat_file:")
        .map(str::trim)
        .filter(|path| !path.is_empty())
}

fn parse_ytdlp_destination_label(line: &str) -> Option<String> {
    let raw_path = line
        .strip_prefix("[download] Destination:")
        .or_else(|| line.strip_prefix("[ExtractAudio] Destination:"))?
        .trim();
    PathBuf::from(raw_path)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
        .map(str::to_string)
}

fn parse_ytdlp_progress(line: &str) -> Option<f32> {
    if let Some(raw) = line.strip_prefix("lowcat_progress:") {
        return parse_percent(raw);
    }

    let raw = line.strip_prefix("[download]")?.trim_start();
    parse_percent(raw)
}

fn parse_percent(raw: &str) -> Option<f32> {
    let percent_end = raw.find('%')?;
    raw[..percent_end]
        .trim()
        .parse::<f32>()
        .ok()
        .map(|progress| progress.clamp(0., 100.))
}

fn classify_ytdlp_error(stderr: &str) -> DownloadError {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("private video")
        || lower.contains("video unavailable")
        || lower.contains("this video is unavailable")
        || lower.contains("has been removed")
        || lower.contains("has been deleted")
        || lower.contains("account has been terminated")
    {
        return DownloadError::unavailable();
    }

    DownloadError::failed("Download failed")
}

fn terminate_child(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

fn cleanup_temp_dir(temp_dir: &PathBuf) {
    let _ = fs::remove_dir_all(temp_dir);
}

fn temp_download_dir(folder: &PathBuf) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    folder.join(format!(".lowcat-download-{}-{nanos}", std::process::id()))
}

fn normalize_youtube_url(token: &str) -> Option<String> {
    let token = token.trim();
    if token.is_empty() {
        return None;
    }

    let candidate = if has_http_scheme(token) {
        token.to_string()
    } else if starts_with_youtube_host(token) {
        format!("https://{token}")
    } else {
        return None;
    };

    is_youtube_url(&candidate).then_some(candidate)
}

fn trim_url_token(token: &str) -> &str {
    token.trim_matches(|c: char| {
        matches!(
            c,
            '<' | '>' | '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ','
        )
    })
}

fn has_http_scheme(value: &str) -> bool {
    value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        || value
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
}

fn starts_with_youtube_host(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.starts_with("youtube.com/")
        || lower.starts_with("www.youtube.com/")
        || lower.starts_with("m.youtube.com/")
        || lower.starts_with("music.youtube.com/")
        || lower.starts_with("youtu.be/")
}

fn is_youtube_url(value: &str) -> bool {
    let Some(rest) = strip_http_scheme(value) else {
        return false;
    };
    let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
    let host = host
        .split_once(':')
        .map(|(host, _)| host)
        .unwrap_or(host)
        .to_ascii_lowercase();

    match host.as_str() {
        "youtu.be" => !path.is_empty(),
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" => {
            path.starts_with("watch?")
                || path.starts_with("shorts/")
                || path.starts_with("embed/")
                || path.starts_with("live/")
        }
        _ => false,
    }
}

fn strip_http_scheme(value: &str) -> Option<&str> {
    value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"))
        .then_some(&value[7..])
        .or_else(|| {
            value
                .get(..8)
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
                .then_some(&value[8..])
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_watch_url_from_clipboard_text() {
        assert_eq!(
            extract_youtube_url("watch this https://www.youtube.com/watch?v=abc123").unwrap(),
            "https://www.youtube.com/watch?v=abc123"
        );
    }

    #[test]
    fn normalizes_bare_youtu_be_url() {
        assert_eq!(
            extract_youtube_url("youtu.be/abc123").unwrap(),
            "https://youtu.be/abc123"
        );
    }

    #[test]
    fn rejects_non_youtube_urls() {
        assert_eq!(
            extract_youtube_url("https://example.com/video")
                .unwrap_err()
                .kind,
            DownloadErrorKind::InvalidUrl
        );
    }

    #[test]
    fn parses_ytdlp_download_progress() {
        assert_eq!(
            parse_ytdlp_progress("[download]  42.7% of 3.00MiB at 1.00MiB/s ETA 00:01"),
            Some(42.7)
        );
        assert_eq!(
            parse_ytdlp_progress("[download] 100% of 3.00MiB"),
            Some(100.)
        );
        assert_eq!(parse_ytdlp_progress("lowcat_progress:  8.2%"), Some(8.2));
        assert_eq!(
            parse_ytdlp_progress("[ExtractAudio] Destination: track.opus"),
            None
        );
    }

    #[test]
    fn clamps_ytdlp_download_progress() {
        assert_eq!(
            parse_ytdlp_progress("[download] 100.5% of 3.00MiB"),
            Some(100.)
        );
    }

    #[test]
    fn parses_lowcat_print_markers() {
        assert_eq!(
            parse_ytdlp_title("lowcat_title: Test Track"),
            Some("Test Track")
        );
        assert_eq!(
            parse_ytdlp_output_path("lowcat_file:/tmp/Test Track.opus"),
            Some("/tmp/Test Track.opus")
        );
        assert_eq!(
            parse_ytdlp_destination_label("[download] Destination: /tmp/Test Track.webm"),
            Some("Test Track".to_string())
        );
    }

    #[test]
    fn classifies_unavailable_ytdlp_errors() {
        assert_eq!(
            classify_ytdlp_error("ERROR: [youtube] abc: Video unavailable").kind,
            DownloadErrorKind::Unavailable
        );
        assert_eq!(
            classify_ytdlp_error("ERROR: [youtube] abc: Private video").kind,
            DownloadErrorKind::Unavailable
        );
    }

    #[test]
    fn keeps_generic_ytdlp_errors_short() {
        let error = classify_ytdlp_error("ERROR: a very long implementation detail");
        assert_eq!(error.kind, DownloadErrorKind::Failed);
        assert_eq!(error.message, "Download failed");
    }
}
