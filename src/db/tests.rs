use super::*;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::model::FolderTagAssignment;

fn db_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir()
        .join(format!("lowcat-db-{name}-{}-{nanos}", std::process::id()))
        .join("library.sqlite")
}

fn scan(name: &str, tags: &[(&str, &[&str])]) -> FileScanRecord {
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap();
    let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap();
    FileScanRecord {
        path: PathBuf::from(format!("/tmp/{name}")),
        stem: stem.to_string(),
        extension: extension.to_string(),
        size: 1,
        modified: 1,
        tags: tags
            .iter()
            .map(|(key, values)| {
                (
                    key.to_string(),
                    values.iter().map(|value| value.to_string()).collect(),
                )
            })
            .collect(),
    }
}

fn scan_path(path: &str) -> FileScanRecord {
    let file_name = Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap();
    let stem = file_name.rsplit_once('.').map(|(stem, _)| stem).unwrap();
    let extension = file_name.rsplit_once('.').map(|(_, ext)| ext).unwrap();
    FileScanRecord {
        path: PathBuf::from(path),
        stem: stem.to_string(),
        extension: extension.to_string(),
        size: 1,
        modified: 1,
        tags: BTreeMap::new(),
    }
}

fn waveform(value: u8) -> WaveformBinary256 {
    [value; 256]
}

#[test]
fn sync_groups_variants_and_seeds_tags() {
    let db = Database::open(&db_path("group")).unwrap();

    let summary = db
        .sync_category(
            Category::Music,
            vec![
                scan("track.flac", &[("GENRE", &["Ambient"])]),
                scan("track.mp3", &[]),
            ],
        )
        .unwrap();
    assert_eq!(summary.added, 2);

    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "track");
    assert_eq!(
        rows[0]
            .variants
            .iter()
            .map(|variant| variant.extension.as_str())
            .collect::<Vec<_>>(),
        vec!["mp3", "flac"]
    );
    assert_eq!(rows[0].tags["GENRE"], vec!["Ambient"]);
}

#[test]
fn duplicate_same_format_prefixes_newer_row_with_relative_parent() {
    let db = Database::open(&db_path("duplicate-format-prefix")).unwrap();
    db.sync_category(
        Category::Music,
        vec![scan_path("/tmp/music/ambient/song.wav")],
    )
    .unwrap();
    db.sync_category(
        Category::Music,
        vec![
            scan_path("/tmp/music/ambient/song.wav"),
            scan_path("/tmp/music/ambient/alt/song.wav"),
        ],
    )
    .unwrap();

    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            Some(Path::new("/tmp/music")),
        )
        .unwrap();
    let names = rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>();

    assert_eq!(names, vec!["ambient/alt/song", "song"]);
    assert_eq!(rows[0].variants.len(), 1);
    assert_eq!(rows[1].variants.len(), 1);
}

#[test]
fn assign_folder_tags_uses_relative_parent_components_for_stems() {
    let db = Database::open(&db_path("folder-tags")).unwrap();
    db.sync_category(
        Category::Music,
        vec![
            scan_path("/tmp/music/ambient/dark/song.wav"),
            scan_path("/tmp/music/bright/song.wav"),
            scan_path("/tmp/music/root.wav"),
        ],
    )
    .unwrap();

    let values = db
        .folder_tag_values(Category::Music, Path::new("/tmp/music"))
        .unwrap();
    assert_eq!(values, vec!["ambient", "bright", "dark"]);

    let inserted = db
        .assign_folder_tags(
            Category::Music,
            Path::new("/tmp/music"),
            &[
                FolderTagAssignment {
                    value: "ambient".to_string(),
                    key: "GENRE".to_string(),
                    enabled: true,
                },
                FolderTagAssignment {
                    value: "dark".to_string(),
                    key: "MOOD".to_string(),
                    enabled: true,
                },
                FolderTagAssignment {
                    value: "bright".to_string(),
                    key: "GENRE".to_string(),
                    enabled: false,
                },
            ],
        )
        .unwrap();
    assert_eq!(inserted, 2);

    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            Some(Path::new("/tmp/music")),
        )
        .unwrap();
    let song_rows = rows
        .iter()
        .filter(|row| row.stem == "song")
        .collect::<Vec<_>>();
    assert_eq!(song_rows.len(), 2);
    for row in song_rows {
        assert_eq!(row.tags["GENRE"], vec!["ambient"]);
        assert_eq!(row.tags["MOOD"], vec!["dark"]);
    }
    let root = rows.iter().find(|row| row.stem == "root").unwrap();
    assert!(!root.tags.contains_key("GENRE"));

    let inserted = db
        .assign_folder_tags(
            Category::Music,
            Path::new("/tmp/music"),
            &[
                FolderTagAssignment {
                    value: "ambient".to_string(),
                    key: "GENRE".to_string(),
                    enabled: true,
                },
                FolderTagAssignment {
                    value: "dark".to_string(),
                    key: "MOOD".to_string(),
                    enabled: true,
                },
            ],
        )
        .unwrap();
    assert_eq!(inserted, 0);
}

#[test]
fn open_migrates_legacy_files_table_with_first_seen_at() {
    let path = db_path("legacy-first-seen");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    block_on(async {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE files (
                category TEXT NOT NULL,
                path TEXT NOT NULL PRIMARY KEY,
                stem TEXT NOT NULL,
                extension TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified INTEGER NOT NULL,
                search_text TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO files(category, path, stem, extension, size, modified, search_text)
             VALUES ('music', '/tmp/song.wav', 'song', 'wav', 1, 1, 'song wav music')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    });

    let db = Database::open(&path).unwrap();
    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();

    assert_eq!(rows[0].variants[0].first_seen_at, 1);
}

#[test]
fn open_migrates_legacy_files_table_with_preview_waveform() {
    let path = db_path("legacy-preview-waveform");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    block_on(async {
        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .unwrap()
            .create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .unwrap();
        sqlx::query(
            "CREATE TABLE files (
                category TEXT NOT NULL,
                path TEXT NOT NULL PRIMARY KEY,
                stem TEXT NOT NULL,
                extension TEXT NOT NULL,
                size INTEGER NOT NULL,
                modified INTEGER NOT NULL,
                first_seen_at INTEGER NOT NULL DEFAULT 1,
                search_text TEXT NOT NULL
            )",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO files(category, path, stem, extension, size, modified, search_text)
             VALUES ('music', '/tmp/song.wav', 'song', 'wav', 1, 1, 'song wav music')",
        )
        .execute(&pool)
        .await
        .unwrap();
        pool.close().await;
    });

    let db = Database::open(&path).unwrap();
    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert!(rows[0].variants[0].waveform.is_none());
    assert_eq!(
        db.missing_waveform_cache_paths(10).unwrap(),
        vec![PathBuf::from("/tmp/song.wav")]
    );
}

#[test]
fn set_preview_waveform_round_trips_binary256() {
    let db = Database::open(&db_path("waveform-round-trip")).unwrap();
    let path = PathBuf::from("/tmp/song.wav");
    db.sync_category(Category::Music, vec![scan_path("/tmp/song.wav")])
        .unwrap();

    db.set_preview_waveform(&path, waveform(42)).unwrap();
    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();
    assert_eq!(rows[0].variants[0].waveform, Some(waveform(42)));

    db.clear_preview_waveform(&path).unwrap();
    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();
    assert!(rows[0].variants[0].waveform.is_none());
}

#[test]
fn opening_new_waveform_version_invalidates_cached_shapes() {
    let db_path = db_path("waveform-version");
    let db = Database::open(&db_path).unwrap();
    let path = PathBuf::from("/tmp/song.wav");
    db.sync_category(Category::Music, vec![scan_path("/tmp/song.wav")])
        .unwrap();
    db.set_preview_waveform(&path, waveform(42)).unwrap();
    db.set_setting(PREVIEW_WAVEFORM_VERSION_KEY, "1").unwrap();
    drop(db);

    let db = Database::open(&db_path).unwrap();
    assert_eq!(db.missing_waveform_cache_paths(10).unwrap(), vec![path]);
    assert_eq!(
        db.setting(PREVIEW_WAVEFORM_VERSION_KEY).unwrap().as_deref(),
        Some(PREVIEW_WAVEFORM_VERSION)
    );
}

#[test]
fn sync_category_preserves_preview_waveform_when_file_unchanged() {
    let db = Database::open(&db_path("waveform-preserve")).unwrap();
    let path = PathBuf::from("/tmp/song.wav");
    let record = scan_path("/tmp/song.wav");
    db.sync_category(Category::Music, vec![record.clone()])
        .unwrap();
    db.set_preview_waveform(&path, waveform(99)).unwrap();

    let summary = db.sync_category(Category::Music, vec![record]).unwrap();
    assert_eq!(summary, SyncSummary::default());

    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();
    assert_eq!(rows[0].variants[0].waveform, Some(waveform(99)));
}

#[test]
fn sync_category_clears_preview_waveform_when_file_changes() {
    let db = Database::open(&db_path("waveform-invalidate")).unwrap();
    let path = PathBuf::from("/tmp/song.wav");
    let mut record = scan_path("/tmp/song.wav");
    db.sync_category(Category::Music, vec![record.clone()])
        .unwrap();
    db.set_preview_waveform(&path, waveform(99)).unwrap();

    record.size += 1;
    db.sync_category(Category::Music, vec![record]).unwrap();

    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();
    assert!(rows[0].variants[0].waveform.is_none());
}

#[test]
fn missing_waveform_cache_paths_returns_uncached_files() {
    let db = Database::open(&db_path("waveform-missing")).unwrap();
    db.sync_category(
        Category::Music,
        vec![scan_path("/tmp/a.wav"), scan_path("/tmp/b.wav")],
    )
    .unwrap();
    db.set_preview_waveform(Path::new("/tmp/a.wav"), waveform(1))
        .unwrap();

    assert_eq!(
        db.missing_waveform_cache_paths(10).unwrap(),
        vec![PathBuf::from("/tmp/b.wav")]
    );
    assert_eq!(
        db.missing_waveform_cache_paths(0).unwrap(),
        Vec::<PathBuf>::new()
    );
}

#[test]
fn sync_category_preserves_removed_last_tag() {
    let db = Database::open(&db_path("sync-category-removed-last-tag")).unwrap();

    db.sync_category(
        Category::Music,
        vec![scan("song.flac", &[("GENRE", &["sgdsfg"])])],
    )
    .unwrap();
    db.remove_tag(Category::Music, "song", "GENRE", "sgdsfg")
        .unwrap();
    db.sync_category(
        Category::Music,
        vec![scan("song.flac", &[("GENRE", &["sgdsfg"])])],
    )
    .unwrap();

    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();
    assert!(!rows[0].tags.contains_key("GENRE"));
}

#[test]
fn schema_ignores_tags_without_current_file_rows() {
    let db = Database::open(&db_path("schema-orphan-tags")).unwrap();

    db.sync_category(
        Category::Music,
        vec![scan("removed.flac", &[("GENRE", &["aaaaa"])])],
    )
    .unwrap();
    db.sync_category(Category::Music, Vec::new()).unwrap();

    let schema = db.schema_for(Category::Music).unwrap();

    assert_eq!(schema["GENRE"], Vec::<String>::new());
    assert_eq!(schema["MOOD"], Vec::<String>::new());
}

#[test]
fn sync_category_deletes_tags_for_removed_stems() {
    let db = Database::open(&db_path("sync-removes-orphan-tags")).unwrap();

    db.sync_category(
        Category::Music,
        vec![
            scan("removed.flac", &[("GENRE", &["Stale"])]),
            scan("kept.flac", &[("GENRE", &["Current"])]),
        ],
    )
    .unwrap();
    db.sync_category(
        Category::Music,
        vec![scan("kept.flac", &[("GENRE", &["Current"])])],
    )
    .unwrap();

    let schema = db.schema_for(Category::Music).unwrap();
    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();

    assert_eq!(schema["GENRE"], vec!["Current"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "kept");
    assert_eq!(rows[0].tags["GENRE"], vec!["Current"]);
}

#[test]
fn query_visible_rows_filters_search_and_selected_in_sql() {
    let db = Database::open(&db_path("sql-filter")).unwrap();

    db.sync_category(
        Category::Music,
        vec![
            scan("dark.flac", &[("MOOD", &["Dark"])]),
            scan("bright.flac", &[("MOOD", &["Bright"])]),
            scan("dark.mp3", &[]),
        ],
    )
    .unwrap();
    db.add_tag(Category::Music, "dark", "GENRE", "Electronic")
        .unwrap();

    let selected = BTreeMap::from([(
        "Genre".to_string(),
        BTreeSet::from(["Electronic".to_string()]),
    )]);
    let rows = db
        .query_visible_rows(
            Category::Music,
            "dark",
            &selected,
            &default_format_priority(),
            None,
        )
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "dark");
    assert_eq!(
        rows[0]
            .variants
            .iter()
            .map(|variant| variant.extension.as_str())
            .collect::<Vec<_>>(),
        vec!["mp3", "flac"]
    );
    assert_eq!(rows[0].tags["GENRE"], vec!["Electronic"]);
}

#[test]
fn adding_subtag_replaces_existing_sibling_for_stem() {
    let db = Database::open(&db_path("replace-subtag")).unwrap();
    db.sync_category(Category::Music, vec![scan("post.flac", &[])])
        .unwrap();

    db.add_tag(Category::Music, "post", "Use", "shitpost/comedy")
        .unwrap();
    db.add_tag(Category::Music, "post", "Use", "shitpost/meme")
        .unwrap();

    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();
    assert_eq!(rows[0].tags["use"], vec!["shitpost/meme"]);
}

#[test]
fn parent_filter_matches_stored_subtag_path() {
    let db = Database::open(&db_path("filter-subtag-parent")).unwrap();
    db.sync_category(Category::Music, vec![scan("post.flac", &[])])
        .unwrap();
    db.add_tag(Category::Music, "post", "Use", "shitpost/comedy")
        .unwrap();

    let selected = BTreeMap::from([("use".to_string(), BTreeSet::from(["shitpost".to_string()]))]);
    let rows = db
        .query_visible_rows(
            Category::Music,
            "",
            &selected,
            &default_format_priority(),
            None,
        )
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].tags["use"], vec!["shitpost/comedy"]);
}

#[test]
fn query_visible_rows_fuzzy_search_matches_characters_in_order() {
    let db = Database::open(&db_path("sql-filter-fuzzy")).unwrap();

    db.sync_category(
        Category::Music,
        vec![scan("it's me.flac", &[]), scan("mist.flac", &[])],
    )
    .unwrap();

    let rows = db
        .query_visible_rows(
            Category::Music,
            "its m",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "it's me");
}

#[test]
fn query_visible_rows_sorts_search_by_best_name_match() {
    let db = Database::open(&db_path("sql-filter-search-rank")).unwrap();

    db.sync_category(
        Category::Music,
        vec![
            scan("brass loop.flac", &[]),
            scan("bassy.flac", &[]),
            scan("sub bass loop.flac", &[]),
            scan("bass.flac", &[]),
        ],
    )
    .unwrap();

    let rows = db
        .query_visible_rows(
            Category::Music,
            "bass",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();

    assert_eq!(
        rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
        vec!["bass", "sub bass loop", "bassy", "brass loop"]
    );
}

#[test]
fn query_visible_rows_escapes_sql_like_wildcards() {
    let db = Database::open(&db_path("sql-filter-like")).unwrap();

    db.sync_category(
        Category::Music,
        vec![scan("100_percent.flac", &[]), scan("100Xpercent.flac", &[])],
    )
    .unwrap();

    let rows = db
        .query_visible_rows(
            Category::Music,
            "100_",
            &BTreeMap::new(),
            &default_format_priority(),
            None,
        )
        .unwrap();

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].name, "100_percent");
}

#[test]
fn settings_round_trip() {
    let db = Database::open(&db_path("settings")).unwrap();
    assert_eq!(db.format_priority().unwrap(), default_format_priority());

    db.set_format_priority(vec![AudioFormat::Flac, AudioFormat::Mp3])
        .unwrap();
    assert_eq!(
        db.format_priority().unwrap(),
        vec![
            AudioFormat::Flac,
            AudioFormat::Mp3,
            AudioFormat::Wav,
            AudioFormat::Opus
        ]
    );

    db.set_convert_conflict_behavior(ConvertConflictBehavior::Overwrite)
        .unwrap();
    assert_eq!(
        db.convert_conflict_behavior().unwrap(),
        ConvertConflictBehavior::Overwrite
    );
}
