use super::*;
use std::fs::File;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use lofty::config::ParseOptions;
use lofty::file::AudioFile;
use lofty::flac::FlacFile;

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
fn scans_supported_audio_recursively_and_groups_variants() {
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
    assert_eq!(
        names(records.clone()),
        vec!["clip", "nested", "skip", "top"]
    );
    assert_eq!(
        records[3]
            .variants
            .iter()
            .map(|variant| variant.extension.as_str())
            .collect::<Vec<_>>(),
        vec!["mp3", "opus", "flac"]
    );
}

#[test]
fn preserves_tags_when_file_moves_within_category_folder() {
    let dir = unique_dir("move-tags");
    let path = fixture(&dir, "song.flac", &[]);

    let mut backend = backend("move-tags");
    backend
        .set_category_folder(Category::Sfx, dir.clone())
        .unwrap();
    backend
        .add_tag(Category::Sfx, &path, "Type", "Foley")
        .unwrap();

    let nested = dir.join("nested");
    fs::create_dir_all(&nested).unwrap();
    let nested_path = nested.join("song.flac");
    fs::rename(&path, &nested_path).unwrap();
    backend.refresh_category(Category::Sfx).unwrap();

    let records = backend.filter(Category::Sfx, "", &BTreeMap::new());
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].path, nested_path);
    assert_eq!(records[0].tags["TYPE"], vec!["Foley"]);
}

#[test]
fn rename_records_moves_variants_and_preserves_tags() {
    let dir = unique_dir("rename-records");
    fs::write(dir.join("song.wav"), b"not actually audio").unwrap();
    fs::write(dir.join("song.mp3"), b"not actually audio").unwrap();
    let paths = vec![dir.join("song.wav"), dir.join("song.mp3")];

    let mut backend = backend("rename-records");
    backend
        .set_category_folder(Category::Music, dir.clone())
        .unwrap();
    backend
        .add_tag(Category::Music, &paths[0], "Genre", "Ambient")
        .unwrap();

    backend
        .rename_records(
            Category::Music,
            &[RenameRecord {
                stem: "song".to_string(),
                paths,
            }],
            "renamed",
        )
        .unwrap();

    assert!(!dir.join("song.wav").exists());
    assert!(!dir.join("song.mp3").exists());
    assert!(dir.join("renamed.wav").exists());
    assert!(dir.join("renamed.mp3").exists());
    let records = backend.filter(Category::Music, "", &BTreeMap::new());
    assert_eq!(names(records.clone()), vec!["renamed"]);
    assert_eq!(records[0].tags["GENRE"], vec!["Ambient"]);
}

#[test]
fn rename_tag_value_updates_all_matching_stems() {
    let dir = unique_dir("rename-tag-value");
    let first = dir.join("first.wav");
    let second = dir.join("second.wav");
    fs::write(&first, b"not actually audio").unwrap();
    fs::write(&second, b"not actually audio").unwrap();

    let mut backend = backend("rename-tag-value");
    backend.set_category_folder(Category::Music, dir).unwrap();
    backend
        .add_tag(Category::Music, &first, "Genre", "Ambient")
        .unwrap();
    backend
        .add_tag(Category::Music, &second, "Genre", "Ambient")
        .unwrap();

    backend
        .rename_tag_value(Category::Music, "Genre", "Ambient", "Drone")
        .unwrap();

    let records = backend.filter(Category::Music, "", &BTreeMap::new());
    assert_eq!(records[0].tags["GENRE"], vec!["Drone"]);
    assert_eq!(records[1].tags["GENRE"], vec!["Drone"]);
}

#[test]
fn refresh_indexes_matching_extensions_without_probe() {
    let dir = unique_dir("extension-only");
    fs::write(dir.join("renamed.wav"), b"not actually audio").unwrap();

    let mut backend = backend("extension-only");
    backend.set_category_folder(Category::Music, dir).unwrap();
    let records = backend.filter(Category::Music, "", &BTreeMap::new());

    assert_eq!(names(records), vec!["renamed"]);
}

#[test]
fn unchanged_refresh_preserves_existing_tags_without_rereading_file_tags() {
    let dir = unique_dir("unchanged-tags");
    let path = fixture(&dir, "song.flac", &[("genre", "Embedded")]);

    let mut backend = backend("unchanged-tags");
    backend
        .set_category_folder(Category::Music, dir.clone())
        .unwrap();
    backend
        .remove_tag(Category::Music, &path, "genre", "Embedded")
        .unwrap();
    backend
        .add_tag(Category::Music, &path, "genre", "Manual")
        .unwrap();

    backend.refresh_category(Category::Music).unwrap();

    let records = backend.filter(Category::Music, "", &BTreeMap::new());
    assert_eq!(records[0].tags["GENRE"], vec!["Manual"]);
}

#[test]
fn unparsable_matching_extension_is_indexed_without_tags() {
    let dir = unique_dir("unparsable");
    fs::write(dir.join("renamed.opus"), b"not actually ogg opus").unwrap();
    fixture(&dir, "valid.flac", &[("GENRE", "Ambient")]);

    let mut backend = backend("unparsable");
    backend.set_category_folder(Category::Music, dir).unwrap();
    let records = backend.filter(Category::Music, "", &BTreeMap::new());

    assert_eq!(names(records.clone()), vec!["renamed", "valid"]);
    assert!(!records[0].tags.contains_key("GENRE"));
    assert_eq!(records[1].tags["GENRE"], vec!["Ambient"]);
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
        &[
            ("genre", "Ambient"),
            ("MoOd", "Calm"),
            ("TYPE", "Ignored"),
            ("Shitpost", "Yes"),
        ],
    );

    let mut backend = backend("read-tags");
    backend.set_category_folder(Category::Music, dir).unwrap();
    let records = backend.filter(Category::Music, "", &BTreeMap::new());

    assert_eq!(records[0].tags["GENRE"], vec!["Ambient"]);
    assert_eq!(records[0].tags["MOOD"], vec!["Calm"]);
    assert!(!records[0].tags.contains_key("shitpost"));
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
