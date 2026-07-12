use super::*;

#[test]
fn cmd_preview_does_not_activate_during_row_editing() {
    let path = PathBuf::from("/tmp/preview.wav");

    assert_eq!(
        FileTable::preview_path_for_state(true, Some(&path), false),
        Some(path.clone())
    );
    assert_eq!(
        FileTable::preview_path_for_state(true, Some(&path), true),
        None
    );
}

#[test]
fn preview_scrub_commits_only_when_taken_for_release_path() {
    let path = PathBuf::from("/tmp/preview.wav");
    let mut scrub = Some(PreviewScrub::new(path.clone(), 0.2));

    assert!(scrub.as_mut().unwrap().update(&path, 0.7));
    assert_eq!(scrub.as_ref().map(|scrub| scrub.ratio), Some(0.7));
    assert_eq!(
        PreviewScrub::take_ratio_for_path(&mut scrub, Path::new("/tmp/other.wav")),
        None
    );
    assert!(scrub.is_some());
    assert_eq!(
        PreviewScrub::take_ratio_for_path(&mut scrub, &path),
        Some(0.7)
    );
    assert!(scrub.is_none());
}

#[test]
fn preview_playhead_atomic_round_trips_optional_ratio() {
    let bits = AtomicU32::new(u32::MAX);
    assert_eq!(FileTable::load_preview_playhead(&bits), None);

    FileTable::store_preview_playhead(&bits, Some(1.5));
    assert_eq!(FileTable::load_preview_playhead(&bits), Some(1.));

    FileTable::store_preview_playhead(&bits, None);
    assert_eq!(FileTable::load_preview_playhead(&bits), None);
}

#[test]
fn internal_drag_payload_updates_for_mouse_down_selection() {
    let drag = InternalFileDrag::new_shared(
        "first".to_string(),
        Arc::new(vec![PathBuf::from("/tmp/first.wav")]),
    );
    let drag_value = drag.clone();

    drag.replace(
        "selected".to_string(),
        vec![
            PathBuf::from("/tmp/first.wav"),
            PathBuf::from("/tmp/second.wav"),
        ],
    );

    let data = drag_value.snapshot();
    assert_eq!(data.label, "selected");
    assert_eq!(data.paths.len(), 2);
}

#[test]
fn native_drag_release_maps_to_category_tabs_but_cancel_does_not() {
    let bounds = Bounds::new(point(px(100.), px(200.)), size(px(500.), px(400.)));
    let first_tab = native_drag::DragEnd {
        screen_x: 200.,
        screen_y: 590.,
        released: true,
    };
    let second_tab = native_drag::DragEnd {
        screen_x: 450.,
        screen_y: 590.,
        released: true,
    };
    let canceled = native_drag::DragEnd {
        released: false,
        ..first_tab
    };

    assert_eq!(
        category_for_native_drag_end(first_tab, bounds),
        Some(Category::Music)
    );
    assert_eq!(
        category_for_native_drag_end(second_tab, bounds),
        Some(Category::Sfx)
    );
    assert_eq!(category_for_native_drag_end(canceled, bounds), None);
}

#[test]
fn native_drag_session_rejects_overlap_and_reopens_after_finish() {
    let session = NativeDragSession::default();

    assert!(session.try_start());
    assert!(session.is_active());
    assert!(!session.try_start());

    session.finish();

    assert!(session.try_start());
}
