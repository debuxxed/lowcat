use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::config::{ParseOptions, WriteOptions};
use lofty::file::AudioFile;
use lofty::flac::FlacFile;
use lofty::ogg::{OpusFile, VorbisComments};

use crate::model::{Category, FileRecord, canonical_tag_key, record_matches};

#[derive(Default)]
pub struct Backend {
    folders: BTreeMap<Category, PathBuf>,
    files: BTreeMap<Category, Vec<FileRecord>>,
}

impl Backend {
    pub fn new() -> Self {
        Self::default()
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
        let Some(folder) = self.folders.get(&category) else {
            self.files.insert(category, Vec::new());
            return Ok(());
        };

        if !folder.is_dir() {
            self.files.insert(category, Vec::new());
            return Ok(());
        }

        let mut records = Vec::new();
        for entry in fs::read_dir(folder)? {
            let entry = entry?;
            let path = entry.path();
            if !entry.file_type()?.is_file() || !is_library_file(&path) {
                continue;
            }
            if let Ok(record) = read_record(category, &path) {
                records.push(record);
            }
        }

        records.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
                .then_with(|| a.path.cmp(&b.path))
        });
        self.files.insert(category, records);
        Ok(())
    }

    pub fn filter(
        &self,
        category: Category,
        search: &str,
        selected: &BTreeMap<String, BTreeSet<String>>,
    ) -> Vec<FileRecord> {
        let selected = canonical_selected(selected);
        self.files
            .get(&category)
            .map(|recs| {
                recs.iter()
                    .filter(|r| record_matches(r, search, &selected))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn schema_for(&self, category: Category) -> BTreeMap<String, Vec<String>> {
        let mut schema: BTreeMap<String, BTreeSet<String>> = category
            .tag_keys()
            .iter()
            .map(|key| ((*key).to_string(), BTreeSet::new()))
            .collect();

        if let Some(recs) = self.files.get(&category) {
            for rec in recs {
                for key in category.tag_keys() {
                    if let Some(values) = rec.tags.get(*key) {
                        let entry = schema.entry((*key).to_string()).or_default();
                        for value in values {
                            entry.insert(value.clone());
                        }
                    }
                }
            }
        }

        schema
            .into_iter()
            .map(|(key, values)| (key, values.into_iter().collect()))
            .collect()
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
        update_audio_tag(path, key, Some(value))?;
        self.refresh_category(category)
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
        remove_audio_tag_value(path, key, value)?;
        self.refresh_category(category)
    }

    /// Import `source` into `category`'s folder. `.opus`/`.flac` are copied as-is;
    /// other readable audio is converted to `.opus`. The source is only removed
    /// after the destination has been written and verified, so a failed import
    /// never deletes the source. Returns an error if the category has no folder.
    pub fn import(&mut self, category: Category, source: &Path) -> io::Result<()> {
        let folder = self.folders.get(&category).cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "category has no configured folder")
        })?;
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

        let convert = !is_library_file(source);
        let extension = if convert {
            "opus".to_string()
        } else {
            extension(source).unwrap_or_else(|| "opus".to_string())
        };
        let stem = source
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("import");

        let final_path = unique_destination(&folder, stem, &extension);
        let temp_path = temp_destination(&folder, &extension);

        let produced = if convert {
            convert_to_opus(source, &temp_path)
        } else {
            fs::copy(source, &temp_path).map(|_| ())
        };
        if let Err(error) = produced.and_then(|()| verify_exists(&temp_path)) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }

        if let Err(error) = fs::rename(&temp_path, &final_path) {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }

        fs::remove_file(source)?;
        self.refresh_category(category)
    }
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

fn convert_to_opus(source: &Path, dest: &Path) -> io::Result<()> {
    let status = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i"])
        .arg(source)
        .args(["-vn", "-c:a", "libopus", "-y"])
        .arg(dest)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other("ffmpeg conversion failed"))
    }
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

fn canonical_selected(
    selected: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for (key, values) in selected {
        if let Some(key) = canonical_tag_key(key) {
            out.entry(key.to_string())
                .or_insert_with(BTreeSet::new)
                .extend(values.iter().cloned());
        }
    }
    out
}

fn is_library_file(path: &Path) -> bool {
    path.extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.eq_ignore_ascii_case("opus") || ext.eq_ignore_ascii_case("flac"))
        .unwrap_or(false)
}

fn read_record(category: Category, path: &Path) -> io::Result<FileRecord> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let tags = read_tags(category, path)?;
    Ok(FileRecord {
        name,
        path: path.to_path_buf(),
        tags,
    })
}

fn read_tags(category: Category, path: &Path) -> io::Result<BTreeMap<String, Vec<String>>> {
    let mut tags = BTreeMap::new();
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

fn update_audio_tag(path: &Path, key: &str, value_to_add: Option<&str>) -> io::Result<()> {
    rewrite_audio_tag(path, key, |values| {
        if let Some(value) = value_to_add {
            push_unique(values, value);
        }
    })
}

fn remove_audio_tag_value(path: &Path, key: &str, value_to_remove: &str) -> io::Result<()> {
    rewrite_audio_tag(path, key, |values| {
        values.retain(|value| value != value_to_remove);
    })
}

fn rewrite_audio_tag<F>(path: &Path, key: &str, update: F) -> io::Result<()>
where
    F: FnOnce(&mut Vec<String>),
{
    match extension(path).as_deref() {
        Some("opus") => {
            let mut source = File::open(path)?;
            let mut opus =
                OpusFile::read_from(&mut source, ParseOptions::new()).map_err(lofty_error)?;
            rewrite_vorbis_key(opus.vorbis_comments_mut(), key, update);
            opus.save_to_path(path, WriteOptions::new())
                .map_err(lofty_error)
        }
        Some("flac") => {
            let mut source = File::open(path)?;
            let mut flac =
                FlacFile::read_from(&mut source, ParseOptions::new()).map_err(lofty_error)?;
            let mut comments = flac
                .remove_vorbis_comments()
                .unwrap_or_else(VorbisComments::new);
            rewrite_vorbis_key(&mut comments, key, update);
            flac.set_vorbis_comments(comments);
            flac.save_to_path(path, WriteOptions::new())
                .map_err(lofty_error)
        }
        _ => Ok(()),
    }
}

fn rewrite_vorbis_key<F>(comments: &mut VorbisComments, key: &str, update: F)
where
    F: FnOnce(&mut Vec<String>),
{
    let mut matching = Vec::new();
    let mut other = Vec::new();

    for (item_key, value) in comments.take_items() {
        if item_key.eq_ignore_ascii_case(key) {
            push_unique(&mut matching, &value);
        } else {
            other.push((item_key, value));
        }
    }

    update(&mut matching);

    for (item_key, value) in other {
        comments.push(item_key, value);
    }

    for value in matching {
        comments.push(key.to_string(), value);
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
    fn scans_only_opus_and_flac_non_recursively() {
        let dir = unique_dir("extensions");
        fixture(&dir, "top.opus", &[]);
        fixture(&dir, "top.flac", &[]);
        fs::write(dir.join("skip.wav"), b"not audio").unwrap();
        fs::create_dir_all(dir.join("nested")).unwrap();
        fixture(&dir.join("nested"), "nested.flac", &[]);

        let mut backend = Backend::new();
        backend.set_category_folder(Category::Music, dir).unwrap();

        assert_eq!(
            names(backend.filter(Category::Music, "", &BTreeMap::new())),
            vec!["top.flac", "top.opus"]
        );
    }

    #[test]
    fn renamed_or_unparsable_matching_extension_is_skipped() {
        let dir = unique_dir("unparsable");
        fs::write(dir.join("renamed.opus"), b"not actually ogg opus").unwrap();
        fixture(&dir, "valid.flac", &[("GENRE", "Ambient")]);

        let mut backend = Backend::new();
        backend.set_category_folder(Category::Music, dir).unwrap();
        let records = backend.filter(Category::Music, "", &BTreeMap::new());

        assert_eq!(names(records.clone()), vec!["valid.flac"]);
        assert_eq!(records[0].tags["GENRE"], vec!["Ambient"]);
    }

    #[test]
    fn missing_or_unset_folders_return_empty_results() {
        let mut backend = Backend::new();
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
    fn records_are_sorted_by_display_name() {
        let dir = unique_dir("sorting");
        fixture(&dir, "zeta.flac", &[]);
        fixture(&dir, "alpha.flac", &[]);
        fixture(&dir, "Beta.opus", &[]);

        let mut backend = Backend::new();
        backend.set_category_folder(Category::Music, dir).unwrap();

        assert_eq!(
            names(backend.filter(Category::Music, "", &BTreeMap::new())),
            vec!["alpha.flac", "Beta.opus", "zeta.flac"]
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

        let mut backend = Backend::new();
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

        let mut backend = Backend::new();
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

        let mut backend = Backend::new();
        backend.set_category_folder(Category::Music, dir).unwrap();
        let selected = BTreeMap::from([(
            "Genre".to_string(),
            BTreeSet::from(["Electronic".to_string()]),
        )]);

        assert_eq!(
            names(backend.filter(Category::Music, "dark", &selected)),
            vec!["dark.flac"]
        );
    }

    #[test]
    fn tag_edits_are_written_back_with_canonical_keys() {
        let dir = unique_dir("write-tags");
        let path = fixture(&dir, "hit.flac", &[("type", "Impact")]);

        let mut backend = Backend::new();
        backend.set_category_folder(Category::Sfx, dir).unwrap();
        backend
            .add_tag(Category::Sfx, &path, "Type", "Foley")
            .unwrap();
        backend
            .remove_tag(Category::Sfx, &path, "type", "Impact")
            .unwrap();

        let records = backend.filter(Category::Sfx, "", &BTreeMap::new());
        assert_eq!(records[0].tags["TYPE"], vec!["Foley"]);

        let mut file = File::open(path).unwrap();
        let flac = FlacFile::read_from(&mut file, ParseOptions::new()).unwrap();
        let comments = flac.vorbis_comments().unwrap();
        assert_eq!(comments.get_all("TYPE").collect::<Vec<_>>(), vec!["Foley"]);
        assert!(
            comments
                .items()
                .any(|(key, value)| key == "TYPE" && value == "Foley")
        );
        assert!(!comments.items().any(|(key, _)| key == "type"));
    }

    #[test]
    fn import_without_folder_fails_cleanly() {
        let source_dir = unique_dir("import-no-folder-src");
        let source = fixture(&source_dir, "song.flac", &[]);

        let mut backend = Backend::new();
        assert!(backend.import(Category::Music, &source).is_err());
        assert!(source.is_file(), "source must survive a failed import");
    }

    #[test]
    fn import_library_formats_without_conversion() {
        let dir = unique_dir("import-passthrough");
        let source_dir = unique_dir("import-passthrough-src");
        let source = fixture(&source_dir, "track.flac", &[("GENRE", "Ambient")]);

        let mut backend = Backend::new();
        backend
            .set_category_folder(Category::Music, dir.clone())
            .unwrap();
        backend.import(Category::Music, &source).unwrap();

        assert!(!source.exists(), "source is moved into the library");
        assert!(dir.join("track.flac").is_file());
        let records = backend.filter(Category::Music, "", &BTreeMap::new());
        assert_eq!(names(records.clone()), vec!["track.flac"]);
        assert_eq!(records[0].tags["GENRE"], vec!["Ambient"]);
    }

    #[test]
    fn import_converts_other_audio_to_opus() {
        let dir = unique_dir("import-convert");
        let source_dir = unique_dir("import-convert-src");
        let source = source_dir.join("clip.wav");
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

        let mut backend = Backend::new();
        backend
            .set_category_folder(Category::Music, dir.clone())
            .unwrap();
        backend.import(Category::Music, &source).unwrap();

        assert!(!source.exists());
        assert!(dir.join("clip.opus").is_file());
        assert!(!dir.join("clip.wav").exists());
        assert_eq!(
            names(backend.filter(Category::Music, "", &BTreeMap::new())),
            vec!["clip.opus"]
        );
    }

    #[test]
    fn import_rejects_non_audio() {
        let dir = unique_dir("import-reject");
        let source_dir = unique_dir("import-reject-src");
        let source = source_dir.join("notes.txt");
        fs::write(&source, b"just some text").unwrap();

        let mut backend = Backend::new();
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

        let mut backend = Backend::new();
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
}
