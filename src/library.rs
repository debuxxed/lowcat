use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};

use gpui::{Context, Pixels, Point};

use crate::backend::Backend;
use crate::model::{Category, CategoryState, FileRecord, tag_label};

#[path = "config.rs"]
mod config;

pub struct Library {
    backend: Backend,
    active: Category,
    states: BTreeMap<Category, CategoryState>,
    settings: config::Settings,
    settings_path: PathBuf,
    filters_open: bool,
    internal_file_drag: Option<InternalFileDrag>,
}

#[derive(Clone)]
struct InternalFileDrag {
    category: Category,
    path: PathBuf,
    anchor: Option<Point<Pixels>>,
}

impl Library {
    pub fn new() -> Self {
        Self::new_with_settings_path(config::settings_path())
    }

    pub fn new_with_settings_path(settings_path: PathBuf) -> Self {
        let settings = config::Settings::load(&settings_path);
        let mut this = Self {
            backend: Backend::new(),
            active: Category::Music,
            states: BTreeMap::new(),
            settings,
            settings_path,
            filters_open: false,
            internal_file_drag: None,
        };
        this.init();
        this
    }

    pub fn active(&self) -> Category {
        self.active
    }

    pub fn active_state(&self) -> &CategoryState {
        &self.states[&self.active]
    }

    #[allow(dead_code)]
    pub fn category_folder(&self, category: Category) -> Option<&Path> {
        self.settings.category_folder(category)
    }

    pub fn filters_open(&self) -> bool {
        self.filters_open
    }

    pub fn toggle_filters(&mut self, cx: &mut Context<Self>) {
        self.filters_open = !self.filters_open;
        cx.notify();
    }

    pub fn set_category(&mut self, category: Category, cx: &mut Context<Self>) {
        if self.active != category {
            self.active = category;
            cx.notify();
        }
    }

    pub fn next_category(&mut self, cx: &mut Context<Self>) {
        self.set_category(self.active.next(), cx);
    }

    pub fn previous_category(&mut self, cx: &mut Context<Self>) {
        self.set_category(self.active.previous(), cx);
    }

    pub fn set_search(&mut self, search: String, cx: &mut Context<Self>) {
        let active = self.active;
        if let Some(state) = self.states.get_mut(&active) {
            state.search = search;
        }
        self.refresh(cx);
    }

    pub fn toggle_value(&mut self, key: &str, value: &str, cx: &mut Context<Self>) {
        let active = self.active;
        if let Some(state) = self.states.get_mut(&active) {
            let set = state.selected.entry(key.to_string()).or_default();
            if !set.remove(value) {
                set.insert(value.to_string());
            }
        }
        self.refresh(cx);
    }

    pub fn remove_value(&mut self, key: &str, value: &str, cx: &mut Context<Self>) {
        let active = self.active;
        if let Some(state) = self.states.get_mut(&active) {
            if let Some(set) = state.selected.get_mut(key) {
                set.remove(value);
            }
        }
        self.refresh(cx);
    }

    pub fn add_tag(&mut self, path: PathBuf, key: &str, value: &str, cx: &mut Context<Self>) {
        if self.backend.add_tag(self.active, &path, key, value).is_ok() {
            self.refresh(cx);
        }
    }

    pub fn remove_tag(&mut self, path: PathBuf, key: &str, value: &str, cx: &mut Context<Self>) {
        if self
            .backend
            .remove_tag(self.active, &path, key, value)
            .is_ok()
        {
            self.refresh(cx);
        }
    }

    pub fn begin_internal_file_drag_with_anchor(
        &mut self,
        path: PathBuf,
        anchor: Option<Point<Pixels>>,
        cx: &mut Context<Self>,
    ) {
        self.internal_file_drag = Some(InternalFileDrag {
            category: self.active,
            path: canonical_or_original(path),
            anchor,
        });
        cx.notify();
    }

    #[cfg(test)]
    pub fn begin_internal_file_drag(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.begin_internal_file_drag_with_anchor(path, None, cx);
    }

    pub fn internal_file_drag_active(&self) -> bool {
        self.internal_file_drag.is_some()
    }

    pub fn internal_file_drag_anchor(&self) -> Option<Point<Pixels>> {
        self.internal_file_drag
            .as_ref()
            .and_then(|drag| drag.anchor)
    }

    pub fn clear_internal_file_drag(&mut self, cx: &mut Context<Self>) {
        if self.internal_file_drag.take().is_some() {
            cx.notify();
        }
    }

    pub fn import_files(
        &mut self,
        category: Category,
        paths: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) {
        let internal_origin = self.internal_drag_origin(&paths);
        self.internal_file_drag = None;
        if internal_origin == Some(category) {
            cx.notify();
            return;
        }

        let mut imported = false;
        for path in paths {
            if self.backend.import(category, &path).is_ok() {
                imported = true;
            }
        }
        if imported {
            let _ = self.refresh_category_state(category);
            if let Some(origin) = internal_origin
                && origin != category
            {
                let _ = self.backend.refresh_category(origin);
                let _ = self.refresh_category_state(origin);
            }
            cx.notify();
        } else if internal_origin.is_some() {
            cx.notify();
        }
    }

    pub fn set_category_folder(
        &mut self,
        category: Category,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) -> io::Result<()> {
        let mut settings = self.settings.clone();
        settings.set_category_folder(category, path.clone());
        settings.save(&self.settings_path)?;
        self.settings = settings;
        self.backend.set_category_folder(category, path)?;
        self.refresh_category_state(category)?;
        cx.notify();
        Ok(())
    }

    #[allow(dead_code)]
    pub fn refresh_category(
        &mut self,
        category: Category,
        cx: &mut Context<Self>,
    ) -> io::Result<()> {
        self.backend.refresh_category(category)?;
        self.refresh_category_state(category)?;
        cx.notify();
        Ok(())
    }

    #[allow(dead_code)]
    pub fn refresh_all(&mut self, cx: &mut Context<Self>) -> io::Result<()> {
        self.backend.refresh_all()?;
        for category in Category::ALL {
            self.refresh_category_state(category)?;
        }
        cx.notify();
        Ok(())
    }

    fn init(&mut self) {
        for category in Category::ALL {
            if let Some(path) = self
                .settings
                .category_folder(category)
                .map(Path::to_path_buf)
            {
                let _ = self.backend.set_category_folder(category, path);
            } else {
                let _ = self.backend.refresh_category(category);
            }
        }

        for category in Category::ALL {
            self.states
                .insert(category, self.load_category_state(category));
        }
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let _ = self.refresh_category_state(self.active);
        cx.notify();
    }

    fn refresh_category_state(&mut self, category: Category) -> io::Result<()> {
        let (search, selected) = if let Some(state) = self.states.get(&category) {
            (state.search.clone(), state.selected.clone())
        } else {
            (String::new(), BTreeMap::new())
        };
        let schema = display_schema(self.backend.schema_for(category));
        let results = display_records(self.backend.filter(category, &search, &selected));
        let state = self.states.entry(category).or_default();
        state.schema = schema;
        state.results = results;
        Ok(())
    }

    fn internal_drag_origin(&self, paths: &[PathBuf]) -> Option<Category> {
        let drag = self.internal_file_drag.as_ref()?;
        paths
            .iter()
            .any(|path| paths_equal(&drag.path, path))
            .then_some(drag.category)
    }

    fn load_category_state(&self, category: Category) -> CategoryState {
        let schema = display_schema(self.backend.schema_for(category));
        let results = display_records(self.backend.filter(category, "", &BTreeMap::new()));
        CategoryState {
            schema,
            results,
            ..Default::default()
        }
    }
}

fn canonical_or_original(path: PathBuf) -> PathBuf {
    path.canonicalize().unwrap_or(path)
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    left == right || right.canonicalize().is_ok_and(|right| left == right)
}

fn display_schema(schema: BTreeMap<String, Vec<String>>) -> BTreeMap<String, Vec<String>> {
    schema
        .into_iter()
        .map(|(key, values)| (tag_label(&key).to_string(), values))
        .collect()
}

fn display_records(records: Vec<FileRecord>) -> Vec<FileRecord> {
    records.into_iter().map(display_record).collect()
}

fn display_record(record: FileRecord) -> FileRecord {
    FileRecord {
        name: record.name,
        path: record.path,
        tags: display_schema(record.tags),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;
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
        assert_eq!(schema_keys, vec!["Genre".to_string(), "Mood".to_string()]);
        let has_folder =
            library.read_with(cx, |lib, _| lib.category_folder(Category::Music).is_some());
        assert!(!has_folder);
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

        library.update(cx, |lib, cx| {
            lib.begin_internal_file_drag(music_file.clone(), cx)
        });
        library.update(cx, |lib, cx| {
            lib.import_files(Category::Sfx, vec![music_file.clone()], cx)
        });

        assert!(!music_file.exists());
        assert!(sfx_dir.join("move.flac").is_file());
        let (music_count, sfx_count) = library.read_with(cx, |lib, _| {
            (
                lib.states[&Category::Music].results.len(),
                lib.states[&Category::Sfx].results.len(),
            )
        });
        assert_eq!(music_count, 0);
        assert_eq!(sfx_count, 1);
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

        library.update(cx, |lib, cx| lib.toggle_value("Genre", "Electronic", cx));
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
}
