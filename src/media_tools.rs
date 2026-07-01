use std::{
    env,
    path::{Path, PathBuf},
    process::Command,
};

const REQUIRED_TOOLS: &[&str] = &["ffmpeg", "ffprobe", "yt-dlp"];

#[derive(Debug, Clone)]
pub struct MissingTool {
    pub name: &'static str,
    pub search_locations: Vec<SearchLocation>,
}

#[derive(Debug, Clone)]
pub enum SearchLocation {
    Path,
    Directory(PathBuf),
}

pub fn command(tool: &str) -> Command {
    Command::new(resolve(tool).unwrap_or_else(|| PathBuf::from(tool)))
}

pub fn available(tool: &str) -> bool {
    command(tool)
        .arg(version_arg(tool))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn missing_required_tools() -> Vec<MissingTool> {
    REQUIRED_TOOLS
        .iter()
        .copied()
        .filter(|tool| !available(tool))
        .map(|tool| MissingTool {
            name: tool,
            search_locations: display_search_locations(),
        })
        .collect()
}

pub fn resolve(tool: &str) -> Option<PathBuf> {
    search_path(tool).or_else(|| search_common_dirs(tool))
}

fn display_search_locations() -> Vec<SearchLocation> {
    let mut locations = Vec::new();
    if env::var_os("PATH").is_some() {
        locations.push(SearchLocation::Path);
    }
    for dir in common_tool_dirs() {
        push_unique_location(&mut locations, dir.clone());
    }
    locations
}

fn search_path(tool: &str) -> Option<PathBuf> {
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|dir| dir.join(tool))
        .find(|candidate| is_executable(candidate))
}

fn search_common_dirs(tool: &str) -> Option<PathBuf> {
    common_tool_dirs()
        .iter()
        .map(|dir| dir.join(tool))
        .find(|candidate| is_executable(candidate))
}

#[cfg(target_os = "macos")]
fn common_tool_dirs() -> &'static [PathBuf] {
    use std::sync::OnceLock;

    static DIRS: OnceLock<Vec<PathBuf>> = OnceLock::new();
    DIRS.get_or_init(|| {
        vec![
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/bin"),
            PathBuf::from("/bin"),
        ]
    })
}

#[cfg(not(target_os = "macos"))]
fn common_tool_dirs() -> &'static [PathBuf] {
    &[]
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        path.metadata()
            .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }

    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn push_unique_location(locations: &mut Vec<SearchLocation>, path: PathBuf) {
    if !locations.iter().any(
        |location| matches!(location, SearchLocation::Directory(existing) if existing == &path),
    ) {
        locations.push(SearchLocation::Directory(path));
    }
}

fn version_arg(tool: &str) -> &'static str {
    match tool {
        "yt-dlp" => "--version",
        _ => "-version",
    }
}
