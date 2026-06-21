use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use futures::{StreamExt as _, channel::mpsc};
use gpui::{AppContext as _, Context, EventEmitter};

use crate::backend::{Backend, import_to_folder};
use crate::model::{
    AudioFormat, Category, CategoryState, ConvertConflictBehavior, FileRecord, canonical_tag_key,
    default_format_priority, tag_label,
};

#[path = "config.rs"]
mod config;

pub struct Library {
    backend: Backend,
    active: Category,
    states: BTreeMap<Category, CategoryState>,
    settings: config::Settings,
    settings_path: PathBuf,
    format_priority: Vec<AudioFormat>,
    convert_conflict_behavior: ConvertConflictBehavior,
    filters_open: bool,
    internal_file_drag: Option<InternalFileDrag>,
    importing: bool,
    import_progress: Option<ImportProgress>,
    last_focus_rescan: Option<Instant>,
    focus_rescan_in_flight: bool,
}

#[derive(Clone)]
pub enum LibraryEvent {
    TagEdited { path: PathBuf },
}

impl EventEmitter<LibraryEvent> for Library {}

#[derive(Clone)]
struct InternalFileDrag {
    category: Category,
    paths: Vec<PathBuf>,
}

#[derive(Clone)]
pub struct ImportProgress {
    pub file_name: String,
    pub progress: f32,
}

struct ImportBatchResult {
    category: Category,
    imported: bool,
    moved_from: Option<Category>,
}

struct ConvertBatchResult {
    category: Category,
    converted: bool,
}

struct TrashBatchResult {
    category: Category,
    file_count: usize,
    result: io::Result<usize>,
}

enum ImportProgressEvent {
    Start { file_name: String },
    Progress(f32),
    Finish,
}

impl Library {
    #[allow(dead_code)]
    pub fn new() -> Self {
        Self::new_with_settings_path(config::settings_path())
    }

    pub fn new_for_app(cx: &mut Context<Self>) -> Self {
        let mut this = Self::new_uninitialized(config::settings_path());
        this.init_cached();
        this.start_initial_rescan(cx);
        this
    }

    #[allow(dead_code)]
    pub fn new_with_settings_path(settings_path: PathBuf) -> Self {
        let mut this = Self::new_uninitialized(settings_path);
        this.init();
        this
    }

    fn new_uninitialized(settings_path: PathBuf) -> Self {
        let settings = config::Settings::load(&settings_path);
        let backend = Backend::new(database_path_for_settings(&settings_path))
            .expect("failed to initialize Lowcat SQLite database");
        let format_priority = backend
            .format_priority()
            .unwrap_or_else(|_| default_format_priority());
        let convert_conflict_behavior = backend
            .convert_conflict_behavior()
            .unwrap_or(ConvertConflictBehavior::AddCopy);
        Self {
            backend,
            active: Category::Music,
            states: BTreeMap::new(),
            settings,
            settings_path,
            format_priority,
            convert_conflict_behavior,
            filters_open: false,
            internal_file_drag: None,
            importing: false,
            import_progress: None,
            last_focus_rescan: None,
            focus_rescan_in_flight: false,
        }
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

    pub fn import_progress(&self) -> Option<&ImportProgress> {
        self.import_progress.as_ref()
    }

    pub fn format_priority(&self) -> &[AudioFormat] {
        &self.format_priority
    }

    pub fn convert_conflict_behavior(&self) -> ConvertConflictBehavior {
        self.convert_conflict_behavior
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
        match self.backend.add_tag(self.active, &path, key, value) {
            Ok(()) => {
                self.apply_tag_add(&path, key, value);
                let _ = self.refresh_category_state(self.active);
                cx.emit(LibraryEvent::TagEdited { path });
                cx.notify();
            }
            Err(error) => {
                eprintln!(
                    "tag add failed path={} key={key} error={error}",
                    path.display()
                );
            }
        }
    }

    pub fn remove_tag(&mut self, path: PathBuf, key: &str, value: &str, cx: &mut Context<Self>) {
        match self.backend.remove_tag(self.active, &path, key, value) {
            Ok(()) => {
                self.apply_tag_remove(&path, key, value);
                let _ = self.refresh_category_state(self.active);
                cx.emit(LibraryEvent::TagEdited { path });
                cx.notify();
            }
            Err(error) => {
                eprintln!(
                    "tag remove failed path={} key={key} error={error}",
                    path.display()
                );
            }
        }
    }

    pub fn move_format_priority_up(&mut self, format: AudioFormat, cx: &mut Context<Self>) {
        let Some(index) = self.format_priority.iter().position(|item| *item == format) else {
            return;
        };
        self.move_format_priority_to_index(format, index.saturating_sub(1), cx);
    }

    pub fn move_format_priority_down(&mut self, format: AudioFormat, cx: &mut Context<Self>) {
        let Some(index) = self.format_priority.iter().position(|item| *item == format) else {
            return;
        };
        self.move_format_priority_to_index(format, index.saturating_add(1), cx);
    }

    fn move_format_priority_to_index(
        &mut self,
        format: AudioFormat,
        new_index: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(index) = self.format_priority.iter().position(|item| *item == format) else {
            return;
        };
        let new_index = new_index.min(self.format_priority.len().saturating_sub(1));
        if index == new_index {
            cx.notify();
            return;
        }
        let format = self.format_priority.remove(index);
        self.format_priority.insert(new_index, format);
        if self
            .backend
            .set_format_priority(self.format_priority.clone())
            .is_ok()
        {
            self.refresh(cx);
        } else {
            cx.notify();
        }
    }

    pub fn set_convert_conflict_behavior(
        &mut self,
        behavior: ConvertConflictBehavior,
        cx: &mut Context<Self>,
    ) {
        if self.convert_conflict_behavior == behavior {
            cx.notify();
            return;
        }
        if self.backend.set_convert_conflict_behavior(behavior).is_ok() {
            self.convert_conflict_behavior = behavior;
        }
        cx.notify();
    }

    pub fn begin_internal_file_drag(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.begin_internal_file_drag_files(vec![path], cx);
    }

    pub fn begin_internal_file_drag_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = paths.into_iter().map(canonical_or_original).collect();
        self.internal_file_drag = Some(InternalFileDrag {
            category: self.active,
            paths,
        });
        cx.notify();
    }

    pub fn internal_file_drag_active(&self) -> bool {
        self.internal_file_drag.is_some()
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
        if paths.is_empty() || self.importing {
            cx.notify();
            return;
        }

        self.importing = true;
        self.import_progress = None;
        cx.notify();

        let Some(folder) = self
            .settings
            .category_folder(category)
            .map(Path::to_path_buf)
        else {
            self.finish_import(
                ImportBatchResult {
                    category,
                    imported: false,
                    moved_from: internal_origin,
                },
                cx,
            );
            return;
        };
        let import_task = cx.background_spawn(async move {
            let mut imported = false;
            for path in paths {
                if import_to_folder(&folder, &path, |_| {}).is_ok() {
                    imported = true;
                }
            }
            ImportBatchResult {
                category,
                imported,
                moved_from: internal_origin,
            }
        });

        cx.spawn(async move |this, cx| {
            let result = import_task.await;
            this.update(cx, |lib, cx| {
                lib.finish_import(result, cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn convert_files_to_format(
        &mut self,
        sources: Vec<PathBuf>,
        target: AudioFormat,
        cx: &mut Context<Self>,
    ) {
        if self.importing {
            cx.notify();
            return;
        }

        let mut seen = BTreeSet::new();
        let sources: Vec<PathBuf> = sources
            .into_iter()
            .filter(|source| seen.insert(source.clone()))
            .collect();
        if sources.is_empty() {
            cx.notify();
            return;
        }

        let category = self.active;
        let behavior = self.convert_conflict_behavior;
        let db_path = database_path_for_settings(&self.settings_path);
        self.importing = true;
        self.import_progress = Some(ImportProgress {
            file_name: file_name(&sources[0]),
            progress: 0.,
        });
        cx.notify();

        let (progress_tx, mut progress_rx) = mpsc::unbounded();
        cx.spawn(async move |this, cx| {
            while let Some(event) = progress_rx.next().await {
                this.update(cx, |lib, cx| {
                    lib.apply_import_progress(event, cx);
                })
                .ok();
            }
        })
        .detach();

        let convert_task = cx.background_spawn(async move {
            let mut converted = false;
            match Backend::new(db_path) {
                Ok(backend) => {
                    for source in sources {
                        let file_name = file_name(&source);
                        let _ = progress_tx.unbounded_send(ImportProgressEvent::Start {
                            file_name: file_name.clone(),
                        });
                        let result =
                            backend.convert_file_to_format(&source, target, behavior, |progress| {
                                let _ = progress_tx
                                    .unbounded_send(ImportProgressEvent::Progress(progress));
                            });
                        match result {
                            Ok(_) => {
                                converted = true;
                            }
                            Err(error) => {
                                eprintln!(
                                    "lowcat convert failed source={} target={} error={error}",
                                    source.display(),
                                    target.extension()
                                );
                            }
                        }
                    }
                }
                Err(error) => {
                    eprintln!(
                        "lowcat convert batch failed target={} error={error}",
                        target.extension()
                    );
                }
            }
            let _ = progress_tx.unbounded_send(ImportProgressEvent::Finish);
            ConvertBatchResult {
                category,
                converted,
            }
        });

        cx.spawn(async move |this, cx| {
            let result = convert_task.await;
            this.update(cx, |lib, cx| {
                lib.finish_convert_unsupported(result, cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn trash_files(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        let mut seen = BTreeSet::new();
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .collect();
        if paths.is_empty() {
            cx.notify();
            return;
        }

        let category = self.active;
        let file_count = paths.len();

        let trash_task = cx.background_spawn(async move {
            let result = Backend::trash_files(paths);
            TrashBatchResult {
                category,
                file_count,
                result,
            }
        });

        cx.spawn(async move |this, cx| {
            let result = trash_task.await;
            this.update(cx, |lib, cx| {
                lib.finish_trash_files(result, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
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

    pub fn rescan_after_focus(&mut self, cx: &mut Context<Self>) {
        let now = Instant::now();
        if self.focus_rescan_in_flight {
            return;
        }
        if self
            .last_focus_rescan
            .is_some_and(|last| now.duration_since(last) < Duration::from_millis(750))
        {
            return;
        }
        self.last_focus_rescan = Some(now);
        self.focus_rescan_in_flight = true;
        let settings = self.settings.clone();
        let db_path = database_path_for_settings(&self.settings_path);
        let started_at = Instant::now();

        let rescan_task = cx.background_spawn(async move {
            let mut backend = Backend::new(db_path)?;
            for category in Category::ALL {
                if let Some(path) = settings.category_folder(category).map(Path::to_path_buf) {
                    backend.set_category_folder(category, path)?;
                } else {
                    backend.refresh_category(category)?;
                }
            }
            Ok::<(), io::Error>(())
        });

        cx.spawn(async move |this, cx| {
            let result = rescan_task.await;
            this.update(cx, |lib, cx| {
                lib.finish_focus_rescan(result, started_at, cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    #[allow(dead_code)]
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

        self.load_all_category_states();
    }

    fn init_cached(&mut self) {
        for category in Category::ALL {
            if let Some(path) = self
                .settings
                .category_folder(category)
                .map(Path::to_path_buf)
            {
                self.backend.remember_category_folder(category, path);
            }
        }

        self.load_all_category_states();
    }

    fn load_all_category_states(&mut self) {
        for category in Category::ALL {
            self.states
                .insert(category, self.load_category_state(category));
        }
    }

    fn start_initial_rescan(&mut self, cx: &mut Context<Self>) {
        self.rescan_after_focus(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let _ = self.refresh_category_state(self.active);
        cx.notify();
    }

    fn apply_import_progress(&mut self, event: ImportProgressEvent, cx: &mut Context<Self>) {
        match event {
            ImportProgressEvent::Start { file_name } => {
                self.import_progress = Some(ImportProgress {
                    file_name,
                    progress: 0.,
                });
            }
            ImportProgressEvent::Progress(progress) => {
                if let Some(import_progress) = self.import_progress.as_mut() {
                    import_progress.progress = progress;
                }
            }
            ImportProgressEvent::Finish => {
                self.import_progress = None;
            }
        }
        cx.notify();
    }

    fn finish_import(&mut self, result: ImportBatchResult, cx: &mut Context<Self>) {
        self.importing = false;
        self.import_progress = None;
        if result.imported {
            let _ = self.backend.refresh_category(result.category);
            let _ = self.refresh_category_state(result.category);
            if let Some(origin) = result.moved_from
                && origin != result.category
            {
                let _ = self.backend.refresh_category(origin);
                let _ = self.refresh_category_state(origin);
            }
        } else if result.moved_from.is_some() {
            cx.notify();
            return;
        }
        cx.notify();
    }

    fn refresh_category_state(&mut self, category: Category) -> io::Result<()> {
        let total_start = crate::perf::start();
        let (search, selected) = if let Some(state) = self.states.get(&category) {
            (state.search.clone(), state.selected.clone())
        } else {
            (String::new(), BTreeMap::new())
        };
        let schema_start = crate::perf::start();
        let schema = display_schema(self.backend.schema_for(category));
        crate::perf::finish("library.schema", schema_start, || {
            format!("category={} keys={}", category.label(), schema.len())
        });
        let filter_start = crate::perf::start();
        let results = display_records(self.backend.filter(category, &search, &selected));
        crate::perf::finish("library.filter", filter_start, || {
            format!(
                "category={} results={} search_len={} selected_keys={}",
                category.label(),
                results.len(),
                search.len(),
                selected.len()
            )
        });
        let state = self.states.entry(category).or_default();
        state.schema = schema;
        state.results = results;
        crate::perf::finish("library.refresh_category_state", total_start, || {
            format!(
                "category={} results={}",
                category.label(),
                state.results.len()
            )
        });
        Ok(())
    }

    fn finish_convert_unsupported(&mut self, result: ConvertBatchResult, cx: &mut Context<Self>) {
        self.importing = false;
        self.import_progress = None;
        if result.converted {
            let _ = self.backend.refresh_category(result.category);
            let _ = self.refresh_category_state(result.category);
        }
        cx.notify();
    }

    fn finish_trash_files(&mut self, result: TrashBatchResult, cx: &mut Context<Self>) {
        match result.result {
            Ok(_) => {}
            Err(error) => {
                eprintln!(
                    "lowcat trash batch failed category={} requested={} error={error}",
                    result.category.label(),
                    result.file_count
                );
            }
        }
        if let Err(error) = self.backend.refresh_category(result.category) {
            eprintln!(
                "lowcat trash refresh failed category={} error={error}",
                result.category.label()
            );
        }
        if let Err(error) = self.refresh_category_state(result.category) {
            eprintln!(
                "lowcat trash state refresh failed category={} error={error}",
                result.category.label()
            );
        }
        cx.notify();
    }

    fn finish_focus_rescan(
        &mut self,
        result: io::Result<()>,
        started_at: Instant,
        cx: &mut Context<Self>,
    ) {
        self.focus_rescan_in_flight = false;
        match result {
            Ok(()) => {
                for category in Category::ALL {
                    if let Err(error) = self.refresh_category_state(category) {
                        eprintln!(
                            "lowcat focus rescan state refresh failed category={} error={error}",
                            category.label()
                        );
                    }
                }
            }
            Err(error) => {
                eprintln!(
                    "lowcat focus rescan failed elapsed_ms={} error={error}",
                    started_at.elapsed().as_millis()
                );
            }
        }
        cx.notify();
    }

    fn internal_drag_origin(&self, paths: &[PathBuf]) -> Option<Category> {
        let drag = self.internal_file_drag.as_ref()?;
        drag.paths
            .iter()
            .any(|drag_path| paths.iter().any(|path| paths_equal(drag_path, path)))
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

    fn apply_tag_add(&mut self, path: &Path, key: &str, value: &str) {
        let Some(key) = display_tag_key(key) else {
            return;
        };
        let value = value.to_string();
        let Some(state) = self.states.get_mut(&self.active) else {
            return;
        };
        if let Some(record) = state.results.iter_mut().find(|record| record.path == path) {
            let values = record.tags.entry(key.clone()).or_default();
            if !values.contains(&value) {
                values.push(value.clone());
                values.sort_by_key(|value| value.to_lowercase());
            }
        }
        let values = state.schema.entry(key).or_default();
        if !values.contains(&value) {
            values.push(value);
            values.sort_by_key(|value| value.to_lowercase());
        }
    }

    fn apply_tag_remove(&mut self, path: &Path, key: &str, value: &str) {
        let Some(key) = display_tag_key(key) else {
            return;
        };
        let Some(state) = self.states.get_mut(&self.active) else {
            return;
        };
        if let Some(record) = state.results.iter_mut().find(|record| record.path == path)
            && let Some(values) = record.tags.get_mut(&key)
        {
            values.retain(|existing| existing != value);
            if values.is_empty() {
                record.tags.remove(&key);
            }
        }
        let value = value.to_string();
        let still_used = state.results.iter().any(|record| {
            record
                .tags
                .get(&key)
                .is_some_and(|values| values.contains(&value))
        });
        if !still_used && let Some(values) = state.schema.get_mut(&key) {
            values.retain(|existing| existing != &value);
        }
    }
}

fn database_path_for_settings(settings_path: &Path) -> PathBuf {
    settings_path
        .parent()
        .map(|parent| parent.join("library.sqlite"))
        .unwrap_or_else(|| PathBuf::from("library.sqlite"))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("import")
        .to_string()
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

fn display_tag_key(key: &str) -> Option<String> {
    canonical_tag_key(key).map(tag_label).map(str::to_string)
}

fn display_records(records: Vec<FileRecord>) -> Vec<FileRecord> {
    records.into_iter().map(display_record).collect()
}

fn display_record(record: FileRecord) -> FileRecord {
    FileRecord {
        name: record.name,
        path: record.path,
        support: record.support,
        stem: record.stem,
        variants: record.variants,
        tags: display_schema(record.tags),
    }
}

#[cfg(test)]
mod tests {
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
        cx.run_until_parked();

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
