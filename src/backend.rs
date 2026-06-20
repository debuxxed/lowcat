use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::config::ParseOptions;
use lofty::file::AudioFile;
use lofty::flac::FlacFile;
use lofty::ogg::{OpusFile, VorbisComments};

use crate::db::{Database, FileScanRecord};
use crate::model::{
    AudioFormat, Category, ConvertConflictBehavior, FileRecord, canonical_tag_key,
    supported_audio_extension,
};

pub struct Backend {
    db: Database,
    folders: BTreeMap<Category, PathBuf>,
}

impl Backend {
    pub fn new(db_path: PathBuf) -> io::Result<Self> {
        Ok(Self {
            db: Database::open(&db_path)?,
            folders: BTreeMap::new(),
        })
    }

    pub fn set_category_folder(&mut self, category: Category, path: PathBuf) -> io::Result<()> {
        self.folders.insert(category, path);
        self.refresh_category(category)
    }

    pub fn refresh_all(&mut self) -> io::Result<()> {
        for category in Category::ALL {
            self.refresh_category(category)?;
        }
        Ok(())
    }

    pub fn refresh_category(&mut self, category: Category) -> io::Result<()> {
        let refresh_start = crate::perf::start();
        let Some(folder) = self.folders.get(&category) else {
            let summary = self.db.sync_category(category, Vec::new())?;
            crate::perf::finish("backend.refresh_category", refresh_start, || {
                format!(
                    "category={} records=0 missing_folder=true removed={}",
                    category.label(),
                    summary.removed
                )
            });
            return Ok(());
        };

        if !folder.is_dir() {
            let summary = self.db.sync_category(category, Vec::new())?;
            crate::perf::finish("backend.refresh_category", refresh_start, || {
                format!(
                    "category={} records=0 invalid_folder=true removed={}",
                    category.label(),
                    summary.removed
                )
            });
            return Ok(());
        }

        let mut records = Vec::new();
        let mut files_seen = 0usize;
        let mut skipped_seen = 0usize;
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file() {
                continue;
            }
            files_seen += 1;

            if !is_library_file(&path) || !probe_is_audio(&path) {
                skipped_seen += 1;
                continue;
            }

            if let Ok(record) = read_scan_record(category, &path) {
                records.push(record);
            }
        }

        let records_len = records.len();
        let summary = self.db.sync_category(category, records)?;
        crate::perf::finish("backend.refresh_category", refresh_start, || {
            format!(
                "category={} records={records_len} files_seen={files_seen} skipped={skipped_seen} added={} updated={} removed={}",
                category.label(),
                summary.added,
                summary.updated,
                summary.removed
            )
        });
        Ok(())
    }

    pub fn filter(
        &self,
        category: Category,
        search: &str,
        selected: &BTreeMap<String, BTreeSet<String>>,
    ) -> Vec<FileRecord> {
        self.db
            .query_visible_rows(
                category,
                search,
                selected,
                &self
                    .format_priority()
                    .unwrap_or_else(|_| crate::model::default_format_priority()),
            )
            .unwrap_or_default()
    }

    pub fn schema_for(&self, category: Category) -> BTreeMap<String, Vec<String>> {
        self.db.schema_for(category).unwrap_or_else(|_| {
            category
                .tag_keys()
                .iter()
                .map(|key| ((*key).to_string(), Vec::new()))
                .collect()
        })
    }

    pub fn add_tag(
        &mut self,
        category: Category,
        path: &Path,
        key: &str,
        value: &str,
    ) -> io::Result<()> {
        let Some(key) = canonical_category_key(category, key) else {
            return Ok(());
        };
        let stem = file_stem(path);
        self.db.add_tag(category, &stem, key, value)
    }

    pub fn remove_tag(
        &mut self,
        category: Category,
        path: &Path,
        key: &str,
        value: &str,
    ) -> io::Result<()> {
        let Some(key) = canonical_category_key(category, key) else {
            return Ok(());
        };
        let stem = file_stem(path);
        self.db.remove_tag(category, &stem, key, value)
    }

    /// Import `source` into `category`'s folder. Supported formats are copied as-is.
    /// The source is only removed
    /// after the destination has been written and verified, so a failed import
    /// never deletes the source. Returns an error if the category has no folder.
    #[allow(dead_code)]
    pub fn import(&mut self, category: Category, source: &Path) -> io::Result<()> {
        let folder = self.folders.get(&category).cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "category has no configured folder")
        })?;
        import_to_folder(&folder, source, |_| {})?;
        self.refresh_category(category)
    }

    pub fn format_priority(&self) -> io::Result<Vec<AudioFormat>> {
        self.db.format_priority()
    }

    pub fn set_format_priority(&self, priority: Vec<AudioFormat>) -> io::Result<()> {
        self.db.set_format_priority(priority)
    }

    pub fn convert_conflict_behavior(&self) -> io::Result<ConvertConflictBehavior> {
        self.db.convert_conflict_behavior()
    }

    pub fn set_convert_conflict_behavior(
        &self,
        behavior: ConvertConflictBehavior,
    ) -> io::Result<()> {
        self.db.set_convert_conflict_behavior(behavior)
    }

    pub fn convert_file_to_format(
        &self,
        source: &Path,
        target: AudioFormat,
        behavior: ConvertConflictBehavior,
        on_progress: impl FnMut(f32),
    ) -> io::Result<PathBuf> {
        let folder = source.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "source has no parent folder")
        })?;
        convert_file_to_format(source, folder, target, behavior, on_progress)
    }

    pub fn trash_files(paths: Vec<PathBuf>) -> io::Result<usize> {
        trash_files(paths)
    }
}

fn trash_files(paths: Vec<PathBuf>) -> io::Result<usize> {
    let mut seen = BTreeSet::new();
    let paths: Vec<PathBuf> = paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect();
    let path_count = paths.len();
    if path_count == 0 {
        return Ok(0);
    }

    trash::delete_all(&paths).map_err(io::Error::other)?;
    Ok(path_count)
}

pub fn import_to_folder(
    folder: &Path,
    source: &Path,
    mut on_conversion_progress: impl FnMut(f32),
) -> io::Result<()> {
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
    Ok(())
}

fn probe_is_audio(path: &Path) -> bool {
    Command::new("ffprobe")
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

fn convert_file_to_format(
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
    let result = match target {
        AudioFormat::Mp3 => convert_to_mp3(source, &temp_path, on_progress),
        AudioFormat::Wav => convert_to_wav(source, &temp_path, on_progress),
        AudioFormat::Opus => convert_to_opus(source, &temp_path, on_progress),
        AudioFormat::Flac => convert_to_flac(source, &temp_path, on_progress),
    };
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

fn convert_to_mp3(source: &Path, dest: &Path, on_progress: impl FnMut(f32)) -> io::Result<()> {
    convert_media(
        source,
        dest,
        &["-vn", "-c:a", "libmp3lame", "-q:a", "2", "-y"],
        on_progress,
    )
}

fn convert_to_wav(source: &Path, dest: &Path, on_progress: impl FnMut(f32)) -> io::Result<()> {
    convert_media(
        source,
        dest,
        &["-vn", "-c:a", "pcm_s16le", "-y"],
        on_progress,
    )
}

fn convert_to_opus(source: &Path, dest: &Path, on_progress: impl FnMut(f32)) -> io::Result<()> {
    convert_media(source, dest, &["-vn", "-c:a", "libopus", "-y"], on_progress)
}

fn convert_to_flac(source: &Path, dest: &Path, on_progress: impl FnMut(f32)) -> io::Result<()> {
    convert_media(source, dest, &["-vn", "-c:a", "flac", "-y"], on_progress)
}

fn convert_media(
    source: &Path,
    dest: &Path,
    output_args: &[&str],
    mut on_progress: impl FnMut(f32),
) -> io::Result<()> {
    let duration_us = media_duration_us(source);
    on_progress(0.);
    let mut child = Command::new("ffmpeg")
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
    let output = Command::new("ffprobe")
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

fn canonical_category_key(category: Category, key: &str) -> Option<&'static str> {
    let key = canonical_tag_key(key)?;
    category.tag_keys().contains(&key).then_some(key)
}

fn is_library_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .and_then(supported_audio_extension)
        .is_some()
}

fn read_scan_record(category: Category, path: &Path) -> io::Result<FileScanRecord> {
    let metadata = fs::metadata(path)?;
    let tags = read_tags(category, path).unwrap_or_default();
    Ok(FileScanRecord {
        path: path.to_path_buf(),
        stem: file_stem(path),
        extension: extension(path).unwrap_or_default(),
        size: metadata.len(),
        modified: metadata
            .modified()
            .ok()
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs() as i64)
            .unwrap_or_default(),
        tags,
    })
}

fn file_stem(path: &Path) -> String {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_string()
}

#[allow(dead_code)]
fn read_record(category: Category, path: &Path) -> io::Result<FileRecord> {
    let scan = read_scan_record(category, path)?;
    Ok(FileRecord {
        name: scan.stem.clone(),
        path: path.to_path_buf(),
        support: crate::model::FileSupport::Native,
        stem: scan.stem,
        variants: vec![crate::model::FileVariant {
            path: scan.path,
            extension: scan.extension,
            size: scan.size,
            modified: scan.modified,
        }],
        tags: scan.tags,
    })
}

fn is_taggable_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("opus") || ext.eq_ignore_ascii_case("flac"))
        .unwrap_or(false)
}

fn read_tags(category: Category, path: &Path) -> io::Result<BTreeMap<String, Vec<String>>> {
    let mut tags = BTreeMap::new();
    if !is_taggable_file(path) {
        return Ok(tags);
    }
    match extension(path).as_deref() {
        Some("opus") => {
            let mut file = File::open(path)?;
            let opus = OpusFile::read_from(&mut file, ParseOptions::new()).map_err(lofty_error)?;
            collect_vorbis_tags(category, opus.vorbis_comments(), &mut tags);
        }
        Some("flac") => {
            let mut file = File::open(path)?;
            let flac = FlacFile::read_from(&mut file, ParseOptions::new()).map_err(lofty_error)?;
            if let Some(comments) = flac.vorbis_comments() {
                collect_vorbis_tags(category, comments, &mut tags);
            }
        }
        _ => {}
    }
    Ok(tags)
}

fn collect_vorbis_tags(
    category: Category,
    comments: &VorbisComments,
    tags: &mut BTreeMap<String, Vec<String>>,
) {
    for (key, value) in comments.items() {
        let Some(key) = canonical_category_key(category, key) else {
            continue;
        };
        let values = tags.entry(key.to_string()).or_default();
        push_unique(values, value);
    }
}

fn push_unique(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_lowercase())
}

fn lofty_error(error: lofty::error::LoftyError) -> io::Error {
    io::Error::other(error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "lowcat-backend-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn unique_db(name: &str) -> PathBuf {
        unique_dir(name).join("library.sqlite")
    }

    fn backend(name: &str) -> Backend {
        Backend::new(unique_db(name)).unwrap()
    }

    fn fixture(dir: &Path, name: &str, tags: &[(&str, &str)]) -> PathBuf {
        let path = dir.join(name);
        let mut command = Command::new("ffmpeg");
        command
            .arg("-hide_banner")
            .arg("-loglevel")
            .arg("error")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("anullsrc=r=48000:cl=mono")
            .arg("-t")
            .arg("0.02");
        for (key, value) in tags {
            command.arg("-metadata").arg(format!("{key}={value}"));
        }
        command.arg("-y").arg(&path);
        let status = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .expect("ffmpeg must be available for audio fixture tests");
        assert!(
            status.success(),
            "ffmpeg failed to create {}",
            path.display()
        );
        path
    }

    fn names(records: Vec<FileRecord>) -> Vec<String> {
        records.into_iter().map(|record| record.name).collect()
    }

    #[test]
    fn scans_supported_audio_non_recursively_and_groups_variants() {
        let dir = unique_dir("extensions");
        fixture(&dir, "top.opus", &[]);
        fixture(&dir, "top.flac", &[]);
        fixture(&dir, "top.mp3", &[]);
        fixture(&dir, "clip.wav", &[]);
        fs::write(dir.join("skip.wav"), b"not audio").unwrap();
        fs::create_dir_all(dir.join("nested")).unwrap();
        fixture(&dir.join("nested"), "nested.flac", &[]);

        let mut backend = backend("extensions");
        backend.set_category_folder(Category::Music, dir).unwrap();

        let records = backend.filter(Category::Music, "", &BTreeMap::new());
        assert_eq!(names(records.clone()), vec!["clip", "top"]);
        assert_eq!(
            records[1]
                .variants
                .iter()
                .map(|variant| variant.extension.as_str())
                .collect::<Vec<_>>(),
            vec!["mp3", "opus", "flac"]
        );
    }

    #[test]
    fn unparsable_matching_extension_is_skipped() {
        let dir = unique_dir("unparsable");
        fs::write(dir.join("renamed.opus"), b"not actually ogg opus").unwrap();
        fixture(&dir, "valid.flac", &[("GENRE", "Ambient")]);

        let mut backend = backend("unparsable");
        backend.set_category_folder(Category::Music, dir).unwrap();
        let records = backend.filter(Category::Music, "", &BTreeMap::new());

        assert_eq!(names(records.clone()), vec!["valid"]);
        assert_eq!(records[0].tags["GENRE"], vec!["Ambient"]);
    }

    #[test]
    fn missing_or_unset_folders_return_empty_results() {
        let mut backend = backend("missing");
        backend.refresh_all().unwrap();
        assert!(
            backend
                .filter(Category::Music, "", &BTreeMap::new())
                .is_empty()
        );

        backend
            .set_category_folder(Category::Music, unique_dir("missing").join("nope"))
            .unwrap();
        assert!(
            backend
                .filter(Category::Music, "", &BTreeMap::new())
                .is_empty()
        );
    }

    #[test]
    fn records_sort_by_grouped_display_name() {
        let dir = unique_dir("sorting");
        fixture(&dir, "zeta.flac", &[]);
        fixture(&dir, "alpha.flac", &[]);
        fixture(&dir, "Beta.opus", &[]);
        fixture(&dir, "omega.wav", &[]);
        fixture(&dir, "delta.wav", &[]);

        let mut backend = backend("sorting");
        backend.set_category_folder(Category::Music, dir).unwrap();

        assert_eq!(
            names(backend.filter(Category::Music, "", &BTreeMap::new())),
            vec!["alpha", "Beta", "delta", "omega", "zeta"]
        );
    }

    #[test]
    fn reads_vorbis_comments_case_insensitively_into_canonical_keys() {
        let dir = unique_dir("read-tags");
        fixture(
            &dir,
            "tagged.flac",
            &[("genre", "Ambient"), ("MoOd", "Calm"), ("TYPE", "Ignored")],
        );

        let mut backend = backend("read-tags");
        backend.set_category_folder(Category::Music, dir).unwrap();
        let records = backend.filter(Category::Music, "", &BTreeMap::new());

        assert_eq!(records[0].tags["GENRE"], vec!["Ambient"]);
        assert_eq!(records[0].tags["MOOD"], vec!["Calm"]);
        assert!(!records[0].tags.contains_key("TYPE"));
    }

    #[test]
    fn schemas_are_fixed_and_values_sorted() {
        let dir = unique_dir("schema");
        fixture(&dir, "one.flac", &[("genre", "Rock")]);
        fixture(&dir, "two.flac", &[("genre", "Ambient")]);

        let mut backend = backend("schema");
        backend.set_category_folder(Category::Music, dir).unwrap();
        let schema = backend.schema_for(Category::Music);

        assert_eq!(schema["GENRE"], vec!["Ambient", "Rock"]);
        assert_eq!(schema["MOOD"], Vec::<String>::new());
        assert_eq!(
            backend
                .schema_for(Category::Sfx)
                .keys()
                .cloned()
                .collect::<Vec<_>>(),
            vec!["TYPE".to_string()]
        );
    }

    #[test]
    fn filtering_uses_existing_record_match_behavior() {
        let dir = unique_dir("filtering");
        fixture(
            &dir,
            "dark.flac",
            &[("GENRE", "Electronic"), ("MOOD", "Dark")],
        );
        fixture(
            &dir,
            "bright.flac",
            &[("GENRE", "Electronic"), ("MOOD", "Bright")],
        );

        let mut backend = backend("filtering");
        backend.set_category_folder(Category::Music, dir).unwrap();
        let selected = BTreeMap::from([(
            "Genre".to_string(),
            BTreeSet::from(["Electronic".to_string()]),
        )]);

        assert_eq!(
            names(backend.filter(Category::Music, "dark", &selected)),
            vec!["dark"]
        );
    }

    #[test]
    fn tag_edits_are_persisted_in_sqlite() {
        let dir = unique_dir("write-tags");
        let path = fixture(&dir, "hit.flac", &[("type", "Impact")]);

        let mut backend = backend("write-tags");
        backend.set_category_folder(Category::Sfx, dir).unwrap();
        backend
            .add_tag(Category::Sfx, &path, "Type", "Foley")
            .unwrap();
        backend
            .remove_tag(Category::Sfx, &path, "type", "Impact")
            .unwrap();

        let records = backend.filter(Category::Sfx, "", &BTreeMap::new());
        assert_eq!(records[0].tags["TYPE"], vec!["Foley"]);
    }

    #[test]
    fn import_without_folder_fails_cleanly() {
        let source_dir = unique_dir("import-no-folder-src");
        let source = fixture(&source_dir, "song.flac", &[]);

        let mut backend = backend("import-no-folder");
        assert!(backend.import(Category::Music, &source).is_err());
        assert!(source.is_file(), "source must survive a failed import");
    }

    #[test]
    fn import_supported_formats_without_conversion() {
        let dir = unique_dir("import-passthrough");
        let source_dir = unique_dir("import-passthrough-src");
        let source = fixture(&source_dir, "track.wav", &[]);

        let mut backend = backend("import-passthrough");
        backend
            .set_category_folder(Category::Music, dir.clone())
            .unwrap();
        backend.import(Category::Music, &source).unwrap();

        assert!(!source.exists(), "source is moved into the library");
        assert!(dir.join("track.wav").is_file());
        let records = backend.filter(Category::Music, "", &BTreeMap::new());
        assert_eq!(names(records.clone()), vec!["track"]);
    }

    #[test]
    fn import_rejects_unsupported_audio_without_conversion() {
        let dir = unique_dir("import-reject-unsupported");
        let source_dir = unique_dir("import-reject-unsupported-src");
        let source = source_dir.join("clip.ogg");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "anullsrc=r=48000:cl=mono",
                "-t",
                "0.05",
                "-y",
            ])
            .arg(&source)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());

        let mut backend = backend("import-reject-unsupported");
        backend
            .set_category_folder(Category::Music, dir.clone())
            .unwrap();
        assert!(backend.import(Category::Music, &source).is_err());

        assert!(source.exists());
        assert!(!dir.join("clip.opus").exists());
    }

    #[test]
    fn import_rejects_non_audio() {
        let dir = unique_dir("import-reject");
        let source_dir = unique_dir("import-reject-src");
        let source = source_dir.join("notes.txt");
        fs::write(&source, b"just some text").unwrap();

        let mut backend = backend("import-reject");
        backend
            .set_category_folder(Category::Music, dir.clone())
            .unwrap();
        assert!(backend.import(Category::Music, &source).is_err());
        assert!(source.is_file(), "rejected source is left untouched");
        assert!(
            backend
                .filter(Category::Music, "", &BTreeMap::new())
                .is_empty()
        );
    }

    #[test]
    fn import_uses_conflict_names_and_never_overwrites() {
        let dir = unique_dir("import-conflict");
        let source_dir = unique_dir("import-conflict-src");
        let existing = fixture(&dir, "song.flac", &[("GENRE", "Existing")]);

        let mut backend = backend("import-conflict");
        backend
            .set_category_folder(Category::Music, dir.clone())
            .unwrap();

        let first = fixture(&source_dir, "song.flac", &[("GENRE", "First")]);
        backend.import(Category::Music, &first).unwrap();
        let second = fixture(&source_dir, "song.flac", &[("GENRE", "Second")]);
        backend.import(Category::Music, &second).unwrap();

        assert!(dir.join("song 2.flac").is_file());
        assert!(dir.join("song 3.flac").is_file());

        let mut file = File::open(&existing).unwrap();
        let flac = FlacFile::read_from(&mut file, ParseOptions::new()).unwrap();
        let genre: Vec<_> = flac.vorbis_comments().unwrap().get_all("GENRE").collect();
        assert_eq!(
            genre,
            vec!["Existing"],
            "existing file is never overwritten"
        );
    }

    #[test]
    fn explicit_conversion_writes_requested_target() {
        let dir = unique_dir("convert-explicit");
        let source = fixture(&dir, "voice.wav", &[]);
        let backend = backend("convert-explicit");

        let out = backend
            .convert_file_to_format(
                &source,
                AudioFormat::Opus,
                ConvertConflictBehavior::AddCopy,
                |_| {},
            )
            .unwrap();

        assert_eq!(out, dir.join("voice.opus"));
        assert!(out.is_file());
        assert!(source.is_file());
    }

    #[test]
    fn overwrite_conversion_replaces_source_format() {
        let dir = unique_dir("convert-overwrite-replaces-source");
        let source = fixture(&dir, "voice.wav", &[]);
        let backend = backend("convert-overwrite-replaces-source");

        let out = backend
            .convert_file_to_format(
                &source,
                AudioFormat::Opus,
                ConvertConflictBehavior::Overwrite,
                |_| {},
            )
            .unwrap();

        assert_eq!(out, dir.join("voice.opus"));
        assert!(out.is_file());
        assert!(!source.exists());
    }
}
