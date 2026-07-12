use super::*;
use std::fs;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_path(name: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("lowcat-{name}-{}-{nanos}", std::process::id()))
}

fn category_dir(name: &str) -> PathBuf {
    let path = unique_path(name);
    fs::create_dir_all(&path).unwrap();
    path
}

fn settings_path(name: &str) -> PathBuf {
    unique_path(name).join("settings.toml")
}

fn settings_with_folders(path: &Path) -> (PathBuf, PathBuf) {
    let music_dir = category_dir("music");
    let sfx_dir = category_dir("sfx");
    let mut settings = config::Settings::default();
    settings.set_category_folder(Category::Music, music_dir.clone());
    settings.set_category_folder(Category::Sfx, sfx_dir.clone());
    settings.save(path).unwrap();
    (music_dir, sfx_dir)
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

#[gpui::test]
fn missing_settings_file_starts_empty(cx: &mut gpui::TestAppContext) {
    let library = cx.new(|_| Library::new_with_settings_path(settings_path("missing")));

    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 0);
    let schema_keys = library.read_with(cx, |lib, _| {
        lib.active_state()
            .schema
            .keys()
            .cloned()
            .collect::<Vec<_>>()
    });
    assert_eq!(schema_keys, vec!["genre".to_string(), "mood".to_string()]);
    let has_folder = library.read_with(cx, |lib, _| lib.category_folder(Category::Music).is_some());
    assert!(!has_folder);
}

#[gpui::test]
fn priority_waveform_cache_fills_missing_visible_row(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("priority-waveform");
    let (music_dir, _) = settings_with_folders(&settings_path);
    let path = fixture(&music_dir, "needs-cache.wav", &[]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, _| {
        lib.backend.clear_preview_waveform(&path).unwrap();
        lib.refresh_category_state(Category::Music);
    });
    let cached = library.read_with(cx, |lib, _| {
        lib.active_state()
            .results
            .iter()
            .find(|record| record.path == path)
            .and_then(|record| record.primary_waveform())
            .is_some()
    });
    assert!(!cached);

    library.update(cx, |lib, cx| {
        lib.maybe_start_priority_waveform_cache(path.clone(), cx);
    });
    cx.run_until_parked();

    let cached = library.read_with(cx, |lib, _| {
        lib.active_state()
            .results
            .iter()
            .find(|record| record.path == path)
            .and_then(|record| record.primary_waveform())
            .is_some()
    });
    assert!(cached);
}

#[gpui::test]
fn invalid_settings_folder_starts_empty(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("invalid");
    let mut settings = config::Settings::default();
    settings.set_category_folder(Category::Music, unique_path("missing-folder"));
    settings.save(&settings_path).unwrap();
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 0);
    let needs_folder = library.read_with(cx, |lib, _| lib.category_needs_folder(Category::Music));
    assert!(needs_folder);
}

#[gpui::test]
fn category_folder_persists_and_refreshes(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("persist");
    let music_dir = category_dir("music-persist");
    fixture(&music_dir, "track.flac", &[("GENRE", "Ambient")]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path.clone()));

    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 0);

    library.update(cx, |lib, cx| {
        lib.set_category_folder(Category::Music, music_dir.clone(), cx)
            .unwrap()
    });
    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 1);

    let restarted = cx.new(|_| Library::new_with_settings_path(settings_path));
    let folder = restarted.read_with(cx, |lib, _| {
        lib.category_folder(Category::Music).map(Path::to_path_buf)
    });
    assert_eq!(folder, Some(music_dir));
    let count = restarted.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 1);
}

#[gpui::test]
fn active_results_group_extension_variants(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("group-variants");
    let (music_dir, _) = settings_with_folders(&settings_path);
    fixture(&music_dir, "aaa.flac", &[("GENRE", "Ambient")]);
    fixture(&music_dir, "aaa.mp3", &[]);

    let library = cx.new(|_| Library::new_with_settings_path(settings_path));
    let results = library.read_with(cx, |lib, _| {
        lib.active_state()
            .results
            .iter()
            .map(|record| {
                (
                    record.name.clone(),
                    record
                        .variants
                        .iter()
                        .map(|variant| variant.extension.clone())
                        .collect::<Vec<_>>(),
                )
            })
            .collect::<Vec<_>>()
    });

    assert_eq!(
        results,
        vec![(
            "aaa".to_string(),
            vec!["mp3".to_string(), "flac".to_string()]
        )]
    );
    let priority = library.read_with(cx, |lib, _| lib.format_priority().to_vec());
    assert_eq!(priority[0], AudioFormat::Mp3);
}

#[gpui::test]
fn folder_tag_assignment_updates_active_category(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("folder-tags");
    let (music_dir, sfx_dir) = settings_with_folders(&settings_path);
    fs::create_dir_all(music_dir.join("ambient/dark")).unwrap();
    fs::write(music_dir.join("ambient/dark/pad.wav"), b"not audio").unwrap();
    fs::create_dir_all(sfx_dir.join("foley/impact")).unwrap();
    fs::write(sfx_dir.join("foley/impact/hit.wav"), b"not audio").unwrap();
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    let values = library.update(cx, |lib, cx| lib.prepare_folder_tag_values(cx));
    assert_eq!(values, vec!["ambient", "dark"]);

    library.update(cx, |lib, cx| {
        lib.assign_folder_tags(
            Category::Music,
            vec![
                FolderTagAssignment {
                    value: "ambient".to_string(),
                    key: "genre".to_string(),
                    enabled: true,
                },
                FolderTagAssignment {
                    value: "dark".to_string(),
                    key: "mood".to_string(),
                    enabled: true,
                },
            ],
            cx,
        );
    });

    let (music_tags, sfx_tags) = library.read_with(cx, |lib, _| {
        (
            lib.states[&Category::Music].results[0].tags.clone(),
            lib.states[&Category::Sfx].results[0].tags.clone(),
        )
    });
    assert_eq!(music_tags["genre"], vec!["ambient"]);
    assert_eq!(music_tags["mood"], vec!["dark"]);
    assert!(!sfx_tags.contains_key("type"));
}

#[gpui::test]
fn trash_files_removes_grouped_variants_and_refreshes(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("trash-grouped");
    let (music_dir, _) = settings_with_folders(&settings_path);
    let aaa_flac = fixture(&music_dir, "aaa.flac", &[("GENRE", "Ambient")]);
    let aaa_mp3 = fixture(&music_dir, "aaa.mp3", &[]);
    let bbb_opus = fixture(&music_dir, "bbb.opus", &[("MOOD", "Dark")]);
    let bbb_wav = fixture(&music_dir, "bbb.wav", &[]);
    let original_paths = vec![aaa_flac, aaa_mp3, bbb_opus, bbb_wav];
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    let variant_paths = library.read_with(cx, |lib, _| {
        assert_eq!(lib.active_state().results.len(), 2);
        lib.active_state()
            .results
            .iter()
            .flat_map(|record| record.variants.iter().map(|variant| variant.path.clone()))
            .collect::<Vec<_>>()
    });

    library.update(cx, |lib, cx| lib.trash_files(variant_paths, cx));
    cx.run_until_parked();

    for path in &original_paths {
        assert!(
            !path.exists(),
            "{} should have moved to Trash",
            path.display()
        );
    }
    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 0);
}

#[gpui::test]
fn format_priority_moves_one_step(cx: &mut gpui::TestAppContext) {
    let library = cx.new(|_| Library::new_with_settings_path(settings_path("priority-step")));

    library.update(cx, |lib, cx| {
        lib.move_format_priority_down(AudioFormat::Mp3, cx);
    });
    let after_down = library.read_with(cx, |lib, _| lib.format_priority().to_vec());
    assert_eq!(
        after_down,
        vec![
            AudioFormat::Wav,
            AudioFormat::Mp3,
            AudioFormat::Opus,
            AudioFormat::Flac,
        ]
    );

    library.update(cx, |lib, cx| {
        lib.move_format_priority_up(AudioFormat::Opus, cx);
    });
    let after_up = library.read_with(cx, |lib, _| lib.format_priority().to_vec());
    assert_eq!(
        after_up,
        vec![
            AudioFormat::Wav,
            AudioFormat::Opus,
            AudioFormat::Mp3,
            AudioFormat::Flac,
        ]
    );
}

#[gpui::test]
fn download_format_persists(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("download-format");
    let library = cx.new(|_| Library::new_with_settings_path(settings_path.clone()));

    let initial = library.read_with(cx, |lib, _| lib.download_format());
    assert_eq!(initial, AudioFormat::Opus);

    library.update(cx, |lib, cx| {
        lib.set_download_format(AudioFormat::Wav, cx);
    });

    let restarted = cx.new(|_| Library::new_with_settings_path(settings_path));
    let persisted = restarted.read_with(cx, |lib, _| lib.download_format());
    assert_eq!(persisted, AudioFormat::Wav);
}

#[gpui::test]
fn preview_volume_defaults_to_full_and_persists(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("preview-volume");
    let library = cx.new(|_| Library::new_with_settings_path(settings_path.clone()));

    assert_eq!(library.read_with(cx, |lib, _| lib.preview_volume()), 1.);

    library.update(cx, |lib, cx| lib.set_preview_volume(0.42, cx));

    let restarted = cx.new(|_| Library::new_with_settings_path(settings_path));
    assert_eq!(restarted.read_with(cx, |lib, _| lib.preview_volume()), 0.42);
}

#[gpui::test]
fn tag_group_visibility_persists(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("tag-group-visibility");
    let library = cx.new(|_| Library::new_with_settings_path(settings_path.clone()));

    library.update(cx, |lib, cx| {
        lib.toggle_tag_group_visibility("genre", cx);
    });

    let restarted = cx.new(|_| Library::new_with_settings_path(settings_path));
    let visible = restarted.read_with(cx, |lib, _| lib.tag_group_is_visible("genre"));
    assert!(!visible);
}

#[gpui::test]
fn tag_column_visibility_persists(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("tag-column-visibility");
    let library = cx.new(|_| Library::new_with_settings_path(settings_path.clone()));

    library.update(cx, |lib, cx| {
        lib.set_hidden_tag_column_keys(BTreeSet::from(["genre".to_string()]), cx);
    });

    let restarted = cx.new(|_| Library::new_with_settings_path(settings_path));
    let hidden = restarted.read_with(cx, |lib, _| lib.hidden_tag_column_keys());
    assert!(hidden.contains("genre"));
}

#[gpui::test]
fn supported_wav_is_indexed_and_searchable(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("supported-wav");
    let (music_dir, _) = settings_with_folders(&settings_path);
    fixture(&music_dir, "native.flac", &[("GENRE", "Ambient")]);
    fixture(&music_dir, "hidden.wav", &[]);

    let library = cx.new(|_| Library::new_with_settings_path(settings_path));
    library.update(cx, |lib, cx| lib.set_search("wav".to_string(), cx));

    let visible = library.read_with(cx, |lib, _| {
        lib.active_state()
            .results
            .iter()
            .map(|record| record.name.clone())
            .collect::<Vec<_>>()
    });

    assert_eq!(visible, vec!["hidden"]);
}

#[gpui::test]
fn search_filters_both_categories_when_switching(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("search-both-categories");
    let (music_dir, sfx_dir) = settings_with_folders(&settings_path);
    fixture(&music_dir, "hit-music.flac", &[("GENRE", "Ambient")]);
    fixture(&music_dir, "miss-music.flac", &[("GENRE", "Ambient")]);
    fixture(&sfx_dir, "hit-sfx.flac", &[("TYPE", "Impact")]);
    fixture(&sfx_dir, "miss-sfx.flac", &[("TYPE", "Impact")]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| lib.set_search("hit".to_string(), cx));
    let (music_count, stale_sfx_count) = library.read_with(cx, |lib, _| {
        (
            lib.active_state().results.len(),
            lib.states[&Category::Sfx].results.len(),
        )
    });
    assert_eq!(music_count, 1);
    assert_eq!(stale_sfx_count, 2);

    library.update(cx, |lib, cx| lib.set_category(Category::Sfx, cx));
    let sfx_count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(sfx_count, 1);

    library.update(cx, |lib, cx| lib.set_category(Category::Music, cx));
    let music_count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(music_count, 1);
}

#[gpui::test]
fn async_search_discards_stale_results(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("async-search-generation");
    let (music_dir, _) = settings_with_folders(&settings_path);
    fixture(&music_dir, "first.flac", &[]);
    fixture(&music_dir, "second.flac", &[]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |library, cx| {
        library.set_search_async("first".to_string(), cx);
        library.set_search_async("second".to_string(), cx);
    });
    cx.run_until_parked();

    let (search, names) = library.read_with(cx, |library, _| {
        (
            library.search().to_string(),
            library
                .active_state()
                .results
                .iter()
                .map(|record| record.name.clone())
                .collect::<Vec<_>>(),
        )
    });
    assert_eq!(search, "second");
    assert_eq!(names, vec!["second".to_string()]);
}

#[gpui::test]
fn unified_search_adds_tag_matches_only_while_filter_panel_is_open(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("unified-name-tag-search");
    let (music_dir, _) = settings_with_folders(&settings_path);
    fixture(&music_dir, "beat.flac", &[("GENRE", "Ambient")]);
    fixture(&music_dir, "ambient-name.flac", &[]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| lib.set_search("ambient".to_string(), cx));
    let names = library.read_with(cx, |lib, _| {
        lib.active_state()
            .results
            .iter()
            .map(|record| record.name.clone())
            .collect::<Vec<_>>()
    });
    assert_eq!(names, vec!["ambient-name".to_string()]);

    library.update(cx, |lib, cx| lib.toggle_filters(cx));
    let (names, autocomplete) = library.read_with(cx, |lib, _| {
        (
            lib.active_state()
                .results
                .iter()
                .map(|record| record.name.clone())
                .collect::<Vec<_>>(),
            lib.single_tag_search_match(),
        )
    });
    assert_eq!(names, vec!["ambient-name".to_string(), "beat".to_string()]);
    assert_eq!(
        autocomplete,
        Some(("genre".to_string(), "Ambient".to_string()))
    );

    let applied = library.update(cx, |lib, cx| lib.apply_single_tag_search_match(cx));
    let (search, names, selected) = library.read_with(cx, |lib, _| {
        (
            lib.search().to_string(),
            lib.active_state()
                .results
                .iter()
                .map(|record| record.name.clone())
                .collect::<Vec<_>>(),
            lib.active_state().selected.clone(),
        )
    });
    assert!(applied);
    assert!(search.is_empty());
    assert_eq!(names, vec!["beat".to_string()]);
    assert_eq!(selected["genre"], BTreeSet::from(["Ambient".to_string()]));
}

#[gpui::test]
fn same_category_internal_drop_is_noop(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("same-category-drop");
    let (music_dir, _) = settings_with_folders(&settings_path);
    let music_file = fixture(&music_dir, "track.flac", &[("GENRE", "Ambient")]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| {
        lib.begin_internal_file_drag(music_file.clone(), cx)
    });
    library.update(cx, |lib, cx| {
        lib.import_files(Category::Music, vec![music_file.clone()], cx)
    });

    assert!(music_file.is_file());
    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 1);
}

#[gpui::test]
fn cross_category_internal_drop_moves_file(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("cross-category-drop");
    let (music_dir, sfx_dir) = settings_with_folders(&settings_path);
    let music_file = fixture(&music_dir, "move.flac", &[("GENRE", "Ambient")]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, _| {
        lib.backend
            .add_tag(Category::Music, &music_file, "CUSTOM", "Favorite")
            .unwrap();
        lib.refresh_category_state(Category::Music);
    });

    library.update(cx, |lib, cx| {
        lib.begin_internal_file_drag(music_file.clone(), cx)
    });
    library.update(cx, |lib, cx| {
        lib.import_files(Category::Sfx, vec![music_file.clone()], cx)
    });
    cx.run_until_parked();

    assert!(!music_file.exists());
    assert!(sfx_dir.join("move.flac").is_file());
    let (music_count, sfx_count, sfx_tags) = library.read_with(cx, |lib, _| {
        (
            lib.states[&Category::Music].results.len(),
            lib.states[&Category::Sfx].results.len(),
            lib.states[&Category::Sfx].results[0].tags.clone(),
        )
    });
    assert_eq!(music_count, 0);
    assert_eq!(sfx_count, 1);
    assert_eq!(sfx_tags["genre"], vec!["Ambient"]);
    assert_eq!(sfx_tags["custom"], vec!["Favorite"]);
}

#[gpui::test]
fn toggling_a_value_filters_active_results(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("filters");
    let (music_dir, sfx_dir) = settings_with_folders(&settings_path);
    fixture(
        &music_dir,
        "dark.flac",
        &[("GENRE", "Electronic"), ("MOOD", "Dark")],
    );
    fixture(
        &music_dir,
        "calm.flac",
        &[("GENRE", "Ambient"), ("MOOD", "Calm")],
    );
    fixture(&sfx_dir, "hit.flac", &[("TYPE", "Impact")]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 2);

    library.update(cx, |lib, cx| lib.toggle_value("genre", "Electronic", cx));
    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 1);

    library.update(cx, |lib, cx| lib.set_category(Category::Sfx, cx));
    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 1);

    library.update(cx, |lib, cx| lib.set_category(Category::Music, cx));
    let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
    assert_eq!(count, 1);
}

#[gpui::test]
fn tag_panel_only_shows_values_present_in_filtered_results(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("filter-tag-panel-values");
    let (music_dir, _) = settings_with_folders(&settings_path);
    fixture(
        &music_dir,
        "dark.flac",
        &[("GENRE", "Electronic"), ("MOOD", "Dark")],
    );
    fixture(
        &music_dir,
        "calm.flac",
        &[("GENRE", "Ambient"), ("MOOD", "Calm")],
    );
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    let schema = library.read_with(cx, |lib, _| lib.tag_panel_schema());
    assert_eq!(schema["genre"], vec!["Ambient", "Electronic"]);
    assert_eq!(schema["mood"], vec!["Calm", "Dark"]);

    library.update(cx, |lib, cx| lib.toggle_value("genre", "Electronic", cx));
    let schema = library.read_with(cx, |lib, _| lib.tag_panel_schema());
    assert_eq!(schema["genre"], vec!["Electronic"]);
    assert_eq!(schema["mood"], vec!["Dark"]);
}

#[gpui::test]
fn clearing_selected_filters_restores_active_results(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("clear-filters");
    let (music_dir, _) = settings_with_folders(&settings_path);
    fixture(&music_dir, "dark.flac", &[("GENRE", "Electronic")]);
    fixture(&music_dir, "calm.flac", &[("GENRE", "Ambient")]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| lib.toggle_value("genre", "Electronic", cx));
    let (filtered_count, selected_count) = library.read_with(cx, |lib, _| {
        let state = lib.active_state();
        (
            state.results.len(),
            state.selected.values().map(BTreeSet::len).sum::<usize>(),
        )
    });
    assert_eq!(filtered_count, 1);
    assert_eq!(selected_count, 1);

    let cleared = library.update(cx, |lib, cx| lib.clear_selected_filters(cx));
    let (result_count, selected_empty) = library.read_with(cx, |lib, _| {
        let state = lib.active_state();
        (state.results.len(), state.selected.is_empty())
    });
    assert!(cleared);
    assert_eq!(result_count, 2);
    assert!(selected_empty);
}

#[gpui::test]
fn tag_search_prioritizes_single_exact_match(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("tag-search-exact-priority");
    let (music_dir, _) = settings_with_folders(&settings_path);
    let path = fixture(&music_dir, "tagged.flac", &[]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| lib.add_tag(path.clone(), "mood", "Hype", cx));
    library.update(cx, |lib, cx| {
        lib.add_tag(path.clone(), "genre", "Hyper", cx)
    });
    library.update(cx, |lib, cx| lib.set_search("hype".to_string(), cx));

    let single_match = library.read_with(cx, |lib, _| lib.single_tag_search_match());
    assert_eq!(single_match, Some(("mood".to_string(), "Hype".to_string())));
}

#[gpui::test]
fn tag_search_autocomplete_counts_collapsed_subtag_parent_once(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("tag-search-collapsed-subtags");
    let (music_dir, _) = settings_with_folders(&settings_path);
    let comedy = fixture(&music_dir, "comedy.flac", &[]);
    let meme = fixture(&music_dir, "meme.flac", &[]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| {
        lib.add_tag(comedy, "use", "shitpost/comedy", cx);
        lib.add_tag(meme, "use", "shitpost/meme", cx);
        lib.set_search("shi".to_string(), cx);
    });

    let single_match = library.read_with(cx, |lib, _| lib.single_tag_search_match());
    assert_eq!(
        single_match,
        Some(("use".to_string(), "shitpost".to_string()))
    );
}

#[gpui::test]
fn tag_search_autocomplete_targets_subtag_before_and_after_parent_selection(
    cx: &mut gpui::TestAppContext,
) {
    let settings_path = settings_path("tag-search-subtag-match");
    let (music_dir, _) = settings_with_folders(&settings_path);
    let comedy = fixture(&music_dir, "comedy.flac", &[]);
    let meme = fixture(&music_dir, "meme.flac", &[]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| {
        lib.add_tag(comedy, "use", "shitpost/comedy", cx);
        lib.add_tag(meme, "use", "shitpost/meme", cx);
        lib.set_search("com".to_string(), cx);
    });
    assert_eq!(
        library.read_with(cx, |lib, _| lib.single_tag_search_match()),
        Some(("use".to_string(), "shitpost/comedy".to_string()))
    );

    library.update(cx, |lib, cx| {
        lib.set_search(String::new(), cx);
        lib.toggle_value("use", "shitpost", cx);
        lib.set_search("mem".to_string(), cx);
    });
    assert_eq!(
        library.read_with(cx, |lib, _| lib.single_tag_search_match()),
        Some(("use".to_string(), "shitpost/meme".to_string()))
    );
}

#[test]
fn tag_search_orders_exact_matches_before_fuzzy_matches() {
    let mut values = vec!["Hyper", "Hypr"];

    values.sort_by_key(|value| tag_search_match_sort_key(value, "hypr"));

    assert_eq!(values, vec!["Hypr", "Hyper"]);
}

#[test]
fn tag_search_orders_exact_groups_before_fuzzy_groups() {
    let mut groups = [("genre", vec!["Hyper"]), ("mood", vec!["Hypr"])];

    groups.sort_by_key(|(key, values)| {
        tag_search_group_sort_key(key, values.iter().copied(), "hypr")
    });

    assert_eq!(groups[0].0, "mood");
    assert_eq!(groups[1].0, "genre");
}

#[gpui::test]
fn removing_last_tag_value_clears_selected_filter(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("remove-selected-filter");
    let (music_dir, _) = settings_with_folders(&settings_path);
    let path = fixture(&music_dir, "tagged.flac", &[]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| lib.add_tag(path.clone(), "genre", "Hype", cx));
    library.update(cx, |lib, cx| lib.toggle_value("genre", "Hype", cx));

    let selected = library.read_with(cx, |lib, _| {
        lib.active_state().selected["genre"].contains("Hype")
    });
    assert!(selected);

    library.update(cx, |lib, cx| {
        lib.remove_tag(path.clone(), "genre", "Hype", cx)
    });

    let (selected_empty, schema_values, result_count) = library.read_with(cx, |lib, _| {
        let state = lib.active_state();
        (
            state.selected.is_empty(),
            state.schema["genre"].clone(),
            state.results.len(),
        )
    });
    assert!(selected_empty);
    assert!(!schema_values.contains(&"Hype".to_string()));
    assert_eq!(result_count, 1);
}

#[gpui::test]
fn intersection_tag_visibility_persists_and_clears_when_orphaned(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("intersection-visibility");
    let (music_dir, _) = settings_with_folders(&settings_path);
    let shared = fixture(&music_dir, "shared.flac", &[]);
    let target_only = fixture(&music_dir, "target-only.flac", &[]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path.clone()));

    library.update(cx, |lib, cx| {
        lib.add_tag(shared.clone(), "genre", "Hype", cx);
        lib.add_tag(shared.clone(), "mood", "Dark", cx);
        lib.add_tag(target_only, "genre", "Hype", cx);
        lib.toggle_tag_intersection_visibility("genre", "Hype", cx);
    });

    assert!(library.read_with(cx, |lib, _| {
        lib.tag_shows_on_intersection("genre", "Hype")
            && !lib.tag_is_visible_in_panel("genre", "Hype")
    }));

    library.update(cx, |lib, cx| lib.set_search("Hype".to_string(), cx));
    assert!(library.read_with(cx, |lib, _| {
        lib.tag_is_visible_in_panel("genre", "Hype")
    }));
    library.update(cx, |lib, cx| lib.set_search(String::new(), cx));

    library.update(cx, |lib, cx| lib.toggle_value("mood", "Dark", cx));
    assert!(library.read_with(cx, |lib, _| {
        lib.tag_is_visible_in_panel("genre", "Hype")
    }));

    let restarted = cx.new(|_| Library::new_with_settings_path(settings_path));
    assert!(restarted.read_with(cx, |lib, _| {
        lib.tag_shows_on_intersection("genre", "Hype")
    }));

    restarted.update(cx, |lib, cx| {
        lib.remove_tag(shared, "mood", "Dark", cx);
    });
    assert!(restarted.read_with(cx, |lib, _| {
        !lib.tag_shows_on_intersection("genre", "Hype")
            && lib.tag_is_visible_in_panel("genre", "Hype")
    }));
}

#[gpui::test]
fn renaming_last_tag_value_renames_selected_filter(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("rename-selected-filter");
    let (music_dir, _) = settings_with_folders(&settings_path);
    let path = fixture(&music_dir, "tagged.flac", &[]);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| lib.add_tag(path.clone(), "genre", "Hype", cx));
    library.update(cx, |lib, cx| lib.toggle_value("genre", "Hype", cx));
    library.update(cx, |lib, cx| {
        lib.rename_tag(path.clone(), "genre", "Hype", "Mad", cx)
    });

    let (has_old_selection, has_new_selection, schema_values, result_tags) =
        library.read_with(cx, |lib, _| {
            let state = lib.active_state();
            let selected = state.selected.get("genre");
            (
                selected.is_some_and(|values| values.contains("Hype")),
                selected.is_some_and(|values| values.contains("Mad")),
                state.schema["genre"].clone(),
                state.results[0].tags["genre"].clone(),
            )
        });
    assert!(!has_old_selection);
    assert!(has_new_selection);
    assert!(!schema_values.contains(&"Hype".to_string()));
    assert!(schema_values.contains(&"Mad".to_string()));
    assert_eq!(result_tags, vec!["Mad"]);
}

#[gpui::test]
fn category_navigation_wraps(cx: &mut gpui::TestAppContext) {
    let settings_path = settings_path("navigation");
    settings_with_folders(&settings_path);
    let library = cx.new(|_| Library::new_with_settings_path(settings_path));

    library.update(cx, |lib, cx| lib.next_category(cx));
    let active = library.read_with(cx, |lib, _| lib.active());
    assert_eq!(active, Category::Sfx);

    library.update(cx, |lib, cx| lib.next_category(cx));
    let active = library.read_with(cx, |lib, _| lib.active());
    assert_eq!(active, Category::Music);

    library.update(cx, |lib, cx| lib.previous_category(cx));
    let active = library.read_with(cx, |lib, _| lib.active());
    assert_eq!(active, Category::Sfx);
}
