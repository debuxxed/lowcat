use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::{StreamExt as _, channel::mpsc};
use gpui::{AppContext as _, Context, EventEmitter};

use crate::backend::{Backend, RenameRecord, import_to_folder};
use crate::downloader::{
    self, DownloadCancel, DownloadError, DownloadErrorKind, DownloadOutput, DownloadProgressEvent,
    DownloadRequest, DownloadState, DownloadStatus,
};
use crate::model::{
    AudioFormat, Category, CategoryState, ConvertConflictBehavior, FileRecord, FolderTagAssignment,
    default_format_priority, fuzzy_search_match, normalize_tag_key, record_matches_scoped,
    record_search_sort_key_scoped, tag_label,
};
use crate::preview_player::{PreviewPlayer, PreviewPosition};

#[path = "config.rs"]
mod config;

pub struct Library {
    backend: Backend,
    active: Category,
    states: BTreeMap<Category, CategoryState>,
    settings: config::Settings,
    settings_path: PathBuf,
    format_priority: Vec<AudioFormat>,
    download_format: AudioFormat,
    preview_volume: f32,
    convert_conflict_behavior: ConvertConflictBehavior,
    filters_open: bool,
    hidden_tag_keys: BTreeSet<String>,
    hidden_tag_column_keys: BTreeSet<String>,
    intersection_tags: BTreeMap<Category, BTreeMap<String, BTreeSet<String>>>,
    tag_intersections: BTreeMap<Category, BTreeMap<(String, String), BTreeSet<(String, String)>>>,
    downloader_open: bool,
    download_state: DownloadState,
    download_cancel: Option<DownloadCancel>,
    internal_file_drag: Option<InternalFileDrag>,
    importing: bool,
    import_progress: Option<ImportProgress>,
    last_focus_rescan: Option<Instant>,
    focus_rescan_in_flight: bool,
    waveform_cache_in_flight: bool,
    waveform_cache_skipped_paths: BTreeSet<PathBuf>,
    waveform_priority_cache_in_flight: BTreeSet<PathBuf>,
    preview_player: Option<PreviewPlayer>,
    preview_current_path: Option<PathBuf>,
    preview_last_stopped: Option<PreviewPosition>,
    preview_playhead_watch_running: bool,
    table_revision: u64,
    filter_panel_revision: u64,
    search_generation: u64,
}

#[derive(Clone)]
pub enum LibraryEvent {
    TagEdited { path: PathBuf },
    PreviewAdvanced,
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
    moved_files: Vec<(PathBuf, PathBuf)>,
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

struct DownloadBatchResult {
    category: Category,
    result: Result<DownloadOutput, DownloadError>,
}

struct WaveformCacheBatchResult {
    processed: usize,
    changed: bool,
    skipped_paths: Vec<PathBuf>,
}

enum ImportProgressEvent {
    Start { file_name: String },
    Progress(f32),
    Finish,
}

impl Library {
    pub fn new_for_app(cx: &mut Context<Self>) -> Self {
        let mut this = Self::new_uninitialized(config::settings_path());
        let mut preview_player = PreviewPlayer::new(this.preview_volume);
        if let Err(error) = preview_player.warm_up() {
            eprintln!("lowcat preview player warm-up failed error={error}");
        }
        this.preview_player = Some(preview_player);
        this.init_cached();
        this.start_initial_rescan(cx);
        this
    }

    #[cfg(test)]
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
        let download_format = settings.download_format();
        let preview_volume = settings.preview_volume();
        let hidden_tag_keys = settings.hidden_tag_groups();
        let hidden_tag_column_keys = settings.hidden_tag_columns();
        let intersection_tags = Category::ALL
            .into_iter()
            .map(|category| (category, settings.intersection_tags(category)))
            .collect();
        Self {
            backend,
            active: Category::Music,
            states: BTreeMap::new(),
            settings,
            settings_path,
            format_priority,
            download_format,
            preview_volume,
            convert_conflict_behavior,
            filters_open: false,
            hidden_tag_keys,
            hidden_tag_column_keys,
            intersection_tags,
            tag_intersections: BTreeMap::new(),
            downloader_open: false,
            download_state: DownloadState::Idle,
            download_cancel: None,
            internal_file_drag: None,
            importing: false,
            import_progress: None,
            last_focus_rescan: None,
            focus_rescan_in_flight: false,
            waveform_cache_in_flight: false,
            waveform_cache_skipped_paths: BTreeSet::new(),
            waveform_priority_cache_in_flight: BTreeSet::new(),
            preview_player: None,
            preview_current_path: None,
            preview_last_stopped: None,
            preview_playhead_watch_running: false,
            table_revision: 0,
            filter_panel_revision: 0,
            search_generation: 0,
        }
    }

    pub fn active(&self) -> Category {
        self.active
    }

    pub fn active_state(&self) -> &CategoryState {
        &self.states[&self.active]
    }

    pub(crate) fn tag_panel_schema(&self) -> BTreeMap<String, Vec<String>> {
        let state = self.active_state();
        if state.selected.is_empty() {
            return state.schema.clone();
        }

        let present: BTreeMap<&str, BTreeSet<&str>> = state
            .results
            .iter()
            .flat_map(|record| &record.tags)
            .fold(BTreeMap::new(), |mut present, (key, values)| {
                present
                    .entry(key.as_str())
                    .or_default()
                    .extend(values.iter().map(String::as_str));
                present
            });

        state
            .schema
            .iter()
            .filter_map(|(key, values)| {
                let present_values = present.get(key.as_str())?;
                let values: Vec<_> = values
                    .iter()
                    .filter(|value| present_values.contains(value.as_str()))
                    .cloned()
                    .collect();
                (!values.is_empty()).then(|| (key.clone(), values))
            })
            .collect()
    }

    #[cfg(test)]
    pub fn category_folder(&self, category: Category) -> Option<&Path> {
        self.settings.category_folder(category)
    }

    pub fn category_needs_folder(&self, category: Category) -> bool {
        self.settings
            .category_folder(category)
            .is_none_or(|path| !path.is_dir())
    }

    pub fn filters_open(&self) -> bool {
        self.filters_open
    }

    pub fn search(&self) -> &str {
        &self.active_state().search
    }

    pub(crate) fn table_revision(&self) -> u64 {
        self.table_revision
    }

    pub(crate) fn filter_panel_revision(&self) -> u64 {
        self.filter_panel_revision
    }

    fn bump_results_revision(&mut self) {
        self.table_revision = self.table_revision.wrapping_add(1);
        self.filter_panel_revision = self.filter_panel_revision.wrapping_add(1);
    }

    fn bump_filter_panel_revision(&mut self) {
        self.filter_panel_revision = self.filter_panel_revision.wrapping_add(1);
    }

    pub fn tag_group_is_visible(&self, key: &str) -> bool {
        !self.hidden_tag_keys.contains(key)
    }

    pub fn hidden_tag_column_keys(&self) -> BTreeSet<String> {
        self.hidden_tag_column_keys.clone()
    }

    pub fn tag_shows_on_intersection(&self, key: &str, value: &str) -> bool {
        self.intersection_tags
            .get(&self.active)
            .and_then(|tags| tags.get(key))
            .is_some_and(|values| values.contains(value))
    }

    pub fn tag_is_visible_in_panel(&self, key: &str, value: &str) -> bool {
        if !self.search().is_empty() || !self.tag_shows_on_intersection(key, value) {
            return true;
        }
        let Some(intersections) = self
            .tag_intersections
            .get(&self.active)
            .and_then(|by_tag| by_tag.get(&(key.to_string(), value.to_string())))
        else {
            return true;
        };
        self.active_state()
            .selected
            .iter()
            .any(|(selected_key, values)| {
                values.iter().any(|selected_value| {
                    intersections.contains(&(selected_key.clone(), selected_value.clone()))
                })
            })
    }

    pub fn toggle_tag_intersection_visibility(
        &mut self,
        key: &str,
        value: &str,
        cx: &mut Context<Self>,
    ) {
        let category = self.active;
        let mut tags = self
            .intersection_tags
            .get(&category)
            .cloned()
            .unwrap_or_default();
        let values = tags.entry(key.to_string()).or_default();
        if !values.remove(value) {
            values.insert(value.to_string());
        }
        if values.is_empty() {
            tags.remove(key);
        }
        self.save_intersection_tags(category, tags);
        self.bump_filter_panel_revision();
        cx.notify();
    }

    fn save_intersection_tags(
        &mut self,
        category: Category,
        tags: BTreeMap<String, BTreeSet<String>>,
    ) {
        let mut settings = self.settings.clone();
        settings.set_intersection_tags(category, tags.clone());
        if settings.save(&self.settings_path).is_ok() {
            self.settings = settings;
            self.intersection_tags.insert(category, tags);
        }
    }

    pub fn downloader_open(&self) -> bool {
        self.downloader_open
    }

    pub fn download_state(&self) -> DownloadState {
        self.download_state.clone()
    }

    pub fn import_progress(&self) -> Option<&ImportProgress> {
        self.import_progress.as_ref()
    }

    pub fn format_priority(&self) -> &[AudioFormat] {
        &self.format_priority
    }

    pub fn download_format(&self) -> AudioFormat {
        self.download_format
    }

    pub fn convert_conflict_behavior(&self) -> ConvertConflictBehavior {
        self.convert_conflict_behavior
    }

    pub fn toggle_filters(&mut self, cx: &mut Context<Self>) {
        self.filters_open = !self.filters_open;
        if self.filters_open {
            self.downloader_open = false;
        }
        let _ = self.refresh_search_results(self.active);
        cx.notify();
    }

    pub fn close_filters(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.filters_open {
            return false;
        }
        self.filters_open = false;
        let _ = self.refresh_search_results(self.active);
        cx.notify();
        true
    }

    pub fn toggle_downloader(&mut self, cx: &mut Context<Self>) {
        self.downloader_open = !self.downloader_open;
        if self.downloader_open {
            let filters_were_open = self.filters_open;
            self.filters_open = false;
            if filters_were_open {
                let _ = self.refresh_search_results(self.active);
            }
        }
        debug_downloader_interaction(|| format!("panel_open={}", self.downloader_open));
        cx.notify();
    }

    pub fn set_category(&mut self, category: Category, cx: &mut Context<Self>) {
        if self.active != category {
            self.active = category;
            let _ = self.refresh_search_results(category);
            debug_library_interaction(|| {
                let results = self.active_state().results.len();
                format!("category={} results={results}", category.label())
            });
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
        self.search_generation = self.search_generation.wrapping_add(1);
        self.set_search_query(&search);
        let _ = self.refresh_search_results(self.active);
        self.log_search(&search);
        cx.notify();
    }

    pub fn set_search_async(&mut self, search: String, cx: &mut Context<Self>) {
        self.search_generation = self.search_generation.wrapping_add(1);
        let generation = self.search_generation;
        self.set_search_query(&search);
        let category = self.active;
        let source_revision = self.table_revision;
        let (records, selected) = {
            let state = &self.states[&category];
            (state.all_records.clone(), state.selected.clone())
        };
        let include_tags = self.filters_open;
        let search_for_task = search.clone();
        let task = cx.background_spawn(async move {
            let start = crate::perf::start();
            let results =
                filter_cached_records(&records, &search_for_task, &selected, include_tags);
            crate::perf::finish("library.search.background", start, || {
                format!(
                    "results={} search_len={} selected_keys={}",
                    results.len(),
                    search_for_task.len(),
                    selected.len()
                )
            });
            results
        });
        cx.spawn(async move |this, cx| {
            let results = task.await;
            this.update(cx, |library, cx| {
                if library.search_generation != generation
                    || library.table_revision != source_revision
                    || library.states[&category].search != search
                {
                    return;
                }
                library.states.entry(category).or_default().results = results;
                library.bump_results_revision();
                library.log_search(&search);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn set_search_query(&mut self, search: &str) {
        for category in Category::ALL {
            let state = self.states.entry(category).or_default();
            state.search = search.to_string();
        }
    }

    fn log_search(&self, search: &str) {
        debug_library_interaction(|| {
            let active = self.active;
            let results = self.active_state().results.len();
            format!(
                "search_len={} active={} results={results}",
                search.len(),
                active.label()
            )
        });
    }

    pub fn clear_search(&mut self, cx: &mut Context<Self>) -> bool {
        if self.states.values().all(|state| state.search.is_empty()) {
            return false;
        }

        self.set_search(String::new(), cx);
        true
    }

    pub fn toggle_tag_group_visibility(&mut self, key: &str, cx: &mut Context<Self>) {
        let mut keys = self.hidden_tag_keys.clone();
        if !keys.remove(key) {
            keys.insert(key.to_string());
        }
        self.set_hidden_tag_groups(keys, cx);
    }

    pub fn set_hidden_tag_groups(&mut self, keys: BTreeSet<String>, cx: &mut Context<Self>) {
        let mut settings = self.settings.clone();
        settings.set_hidden_tag_groups(keys.clone());
        if settings.save(&self.settings_path).is_ok() {
            self.settings = settings;
            self.hidden_tag_keys = keys;
            self.bump_filter_panel_revision();
        }
        cx.notify();
    }

    pub fn show_all_tag_groups(&mut self, cx: &mut Context<Self>) {
        if self.hidden_tag_keys.is_empty() {
            return;
        }
        self.set_hidden_tag_groups(BTreeSet::new(), cx);
    }

    pub fn hide_all_tag_groups(&mut self, keys: &[String], cx: &mut Context<Self>) {
        self.set_hidden_tag_groups(keys.iter().cloned().collect(), cx);
    }

    pub fn set_hidden_tag_column_keys(&mut self, keys: BTreeSet<String>, cx: &mut Context<Self>) {
        let mut settings = self.settings.clone();
        settings.set_hidden_tag_columns(keys.clone());
        if settings.save(&self.settings_path).is_ok() {
            self.settings = settings;
            self.hidden_tag_column_keys = keys;
        }
        cx.notify();
    }

    pub fn single_tag_search_match(&self) -> Option<(String, String)> {
        let schema = self.tag_panel_schema();
        self.single_tag_search_match_in(&schema)
    }

    pub(crate) fn single_tag_search_match_in(
        &self,
        schema: &BTreeMap<String, Vec<String>>,
    ) -> Option<(String, String)> {
        let search = self.search();
        let include_hidden_groups = !search.is_empty();
        let matches: Vec<(String, String)> = schema
            .iter()
            .flat_map(|(key, values)| {
                let mut candidates = BTreeSet::new();
                for value in values {
                    if let Some((parent, child)) = crate::model::split_subtag(value) {
                        if tag_matches_search(parent, search) {
                            candidates.insert(parent.to_string());
                        }
                        if tag_matches_search(child, search)
                            || (search.contains('/') && tag_matches_search(value, search))
                        {
                            candidates.insert(value.clone());
                        }
                    } else if tag_matches_search(value, search) {
                        candidates.insert(value.clone());
                    }
                }
                candidates
                    .into_iter()
                    .filter(|_| include_hidden_groups || self.tag_group_is_visible(key))
                    .map(move |value| (key.clone(), value))
                    .collect::<Vec<_>>()
            })
            .collect();

        let exact_search = search.trim();
        if !exact_search.is_empty() {
            let mut exact_matches = matches
                .iter()
                .filter(|(_, value)| {
                    tag_exactly_matches_search(value, exact_search)
                        || crate::model::split_subtag(value).is_some_and(|(_, child)| {
                            tag_exactly_matches_search(child, exact_search)
                        })
                })
                .cloned();
            if let Some(first) = exact_matches.next() {
                return exact_matches.next().is_none().then_some(first);
            }
        }

        let mut matches = matches.into_iter();
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    pub fn apply_single_tag_search_match(&mut self, cx: &mut Context<Self>) -> bool {
        let Some((key, value)) = self.single_tag_search_match() else {
            return false;
        };
        self.set_search(String::new(), cx);
        self.toggle_value(&key, &value, cx);
        true
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

    pub fn clear_selected_filters(&mut self, cx: &mut Context<Self>) -> bool {
        let active = self.active;
        let Some(state) = self.states.get_mut(&active) else {
            return false;
        };
        if state.selected.values().all(BTreeSet::is_empty) {
            return false;
        }

        state.selected.clear();
        self.refresh(cx);
        true
    }

    pub fn download_from_clipboard(
        &mut self,
        category: Category,
        clipboard_text: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.downloader_open = true;
        self.filters_open = false;

        if matches!(self.download_state, DownloadState::Running(_)) {
            debug_downloader_interaction(|| {
                format!("paste_ignored=running category={}", category.label())
            });
            cx.notify();
            return;
        }

        let Some(clipboard_text) = clipboard_text else {
            debug_downloader_interaction(|| {
                format!(
                    "paste_rejected=empty_clipboard category={}",
                    category.label()
                )
            });
            self.set_download_error(DownloadError::clipboard_empty(), cx);
            return;
        };
        let url = match downloader::extract_youtube_url(&clipboard_text) {
            Ok(url) => url,
            Err(error) => {
                debug_downloader_interaction(|| {
                    format!("paste_rejected=invalid_url category={}", category.label())
                });
                self.set_download_error(error, cx);
                return;
            }
        };
        let Some(folder) = self
            .settings
            .category_folder(category)
            .map(Path::to_path_buf)
        else {
            debug_downloader_interaction(|| {
                format!(
                    "paste_rejected=missing_folder category={}",
                    category.label()
                )
            });
            self.set_download_error(DownloadError::missing_category_folder(category), cx);
            return;
        };

        let request = DownloadRequest {
            category,
            url,
            folder,
            format: self.download_format,
        };
        let cancel = DownloadCancel::default();
        self.download_cancel = Some(cancel.clone());
        self.download_state = DownloadState::Running(DownloadStatus {
            category,
            label: "Preparing download".to_string(),
            progress: 0.,
        });
        debug_downloader_interaction(|| format!("download_start category={}", category.label()));
        cx.notify();

        let (progress_tx, mut progress_rx) = mpsc::unbounded();
        cx.spawn(async move |this, cx| {
            while let Some(event) = progress_rx.next().await {
                this.update(cx, |lib, cx| {
                    lib.apply_download_progress(event, cx);
                })
                .ok();
            }
        })
        .detach();

        let download_task = cx.background_spawn(async move {
            let result = downloader::download_audio(request, cancel, |event| {
                let _ = progress_tx.unbounded_send(event);
            });
            DownloadBatchResult { category, result }
        });

        cx.spawn(async move |this, cx| {
            let result = download_task.await;
            this.update(cx, |lib, cx| {
                lib.finish_download(result, cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn cancel_download(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.download_cancel {
            cancel.cancel();
            if let DownloadState::Running(status) = &mut self.download_state {
                status.label = "Canceling".to_string();
            }
            debug_downloader_interaction(|| "cancel_requested".to_string());
        }
        cx.notify();
    }

    pub fn dismiss_download_error(&mut self, cx: &mut Context<Self>) {
        if matches!(self.download_state, DownloadState::Error(_)) {
            self.download_state = DownloadState::Idle;
            cx.notify();
        }
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

    pub fn add_tag_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let category = self.active;
        match self.backend.add_tag_key(category, key) {
            Ok(Some(key)) => {
                let _ = self.refresh_category_state(category);
                debug_library_interaction(|| {
                    format!("add_tag_key category={} key={key}", category.label())
                });
                cx.notify();
            }
            Ok(None) => cx.notify(),
            Err(error) => {
                eprintln!(
                    "lowcat tag key add failed category={} key={key} error={error}",
                    category.label()
                );
                cx.notify();
            }
        }
    }

    pub fn remove_tag_key(&mut self, key: &str, cx: &mut Context<Self>) {
        let category = self.active;
        match self.backend.remove_tag_key(category, key) {
            Ok(removed) => {
                let _ = self.refresh_category_state(category);
                debug_library_interaction(|| {
                    format!(
                        "remove_tag_key category={} key={key} removed={removed}",
                        category.label()
                    )
                });
                cx.notify();
            }
            Err(error) => {
                eprintln!(
                    "lowcat tag key remove failed category={} key={key} error={error}",
                    category.label()
                );
                cx.notify();
            }
        }
    }

    pub fn rename_tag_key(&mut self, old_key: &str, new_key: &str, cx: &mut Context<Self>) {
        let category = self.active;
        match self.backend.rename_tag_key(category, old_key, new_key) {
            Ok(()) => {
                let _ =
                    self.refresh_category_state_after_tag_key_rename(category, old_key, new_key);
                debug_library_interaction(|| {
                    format!(
                        "rename_tag_key category={} old={old_key} new={new_key}",
                        category.label()
                    )
                });
                cx.notify();
            }
            Err(error) => {
                eprintln!(
                    "lowcat tag key rename failed category={} old={old_key} error={error}",
                    category.label()
                );
                cx.notify();
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

    pub fn rename_tag(
        &mut self,
        path: PathBuf,
        key: &str,
        old_value: &str,
        new_value: &str,
        cx: &mut Context<Self>,
    ) {
        let category = self.active;
        match self
            .backend
            .rename_tag(category, &path, key, old_value, new_value)
        {
            Ok(()) => {
                let _ = self
                    .refresh_category_state_after_tag_rename(category, key, old_value, new_value);
                debug_library_interaction(|| {
                    format!("rename_tag key={key} old={old_value} new={new_value}")
                });
                cx.emit(LibraryEvent::TagEdited { path });
                cx.notify();
            }
            Err(error) => {
                eprintln!(
                    "tag rename failed path={} key={key} old={old_value} error={error}",
                    path.display()
                );
            }
        }
    }

    pub fn rename_records(
        &mut self,
        records: Vec<RenameRecord>,
        new_stem: &str,
        cx: &mut Context<Self>,
    ) {
        let category = self.active;
        match self.backend.rename_records(category, &records, new_stem) {
            Ok(file_count) => {
                let _ = self.refresh_category_state(category);
                debug_library_interaction(|| {
                    format!(
                        "rename_records category={} records={} files={file_count}",
                        category.label(),
                        records.len()
                    )
                });
                cx.notify();
            }
            Err(error) => {
                eprintln!(
                    "lowcat rename failed category={} records={} error={error}",
                    category.label(),
                    records.len()
                );
                cx.notify();
            }
        }
    }

    pub fn rename_tag_value(
        &mut self,
        key: &str,
        old_value: &str,
        new_value: &str,
        cx: &mut Context<Self>,
    ) {
        let category = self.active;
        match self
            .backend
            .rename_tag_value(category, key, old_value, new_value)
        {
            Ok(()) => {
                let _ = self
                    .refresh_category_state_after_tag_rename(category, key, old_value, new_value);
                debug_library_interaction(|| {
                    format!(
                        "rename_tag_value category={} key={key} old={old_value} new={new_value}",
                        category.label()
                    )
                });
                cx.notify();
            }
            Err(error) => {
                eprintln!(
                    "lowcat tag rename failed category={} key={key} old={old_value} error={error}",
                    category.label()
                );
                cx.notify();
            }
        }
    }

    pub fn prepare_folder_tag_values(&mut self, cx: &mut Context<Self>) -> Vec<String> {
        let category = self.active;
        if let Err(error) = self.backend.refresh_category(category) {
            eprintln!(
                "lowcat folder tag refresh failed category={} error={error}",
                category.label()
            );
            return Vec::new();
        }
        if let Err(error) = self.refresh_category_state(category) {
            eprintln!(
                "lowcat folder tag state refresh failed category={} error={error}",
                category.label()
            );
        }
        cx.notify();

        match self.backend.folder_tag_values(category) {
            Ok(values) => values,
            Err(error) => {
                eprintln!(
                    "lowcat folder tag preview failed category={} error={error}",
                    category.label()
                );
                Vec::new()
            }
        }
    }

    pub fn assign_folder_tags(
        &mut self,
        category: Category,
        assignments: Vec<FolderTagAssignment>,
        cx: &mut Context<Self>,
    ) {
        let inserted = match self.backend.assign_folder_tags(category, &assignments) {
            Ok(count) => count,
            Err(error) => {
                eprintln!(
                    "lowcat folder tag assignment failed category={} error={error}",
                    category.label()
                );
                0
            }
        };
        if let Err(error) = self.refresh_category_state(category) {
            eprintln!(
                "lowcat folder tag state refresh failed category={} error={error}",
                category.label()
            );
        }
        debug_library_interaction(|| {
            format!(
                "folder_tags category={} assignments={} inserted={inserted}",
                category.label(),
                assignments.len()
            )
        });
        cx.notify();
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

    pub fn set_download_format(&mut self, format: AudioFormat, cx: &mut Context<Self>) {
        if self.download_format == format {
            cx.notify();
            return;
        }

        let mut settings = self.settings.clone();
        settings.set_download_format(format);
        if settings.save(&self.settings_path).is_ok() {
            self.settings = settings;
            self.download_format = format;
        }
        cx.notify();
    }

    pub fn preview_volume(&self) -> f32 {
        self.preview_volume
    }

    pub fn set_preview_volume(&mut self, volume: f32, cx: &mut Context<Self>) {
        let volume = volume.clamp(0., 1.);
        if self.preview_volume == volume {
            return;
        }

        let mut settings = self.settings.clone();
        settings.set_preview_volume(volume);
        if settings.save(&self.settings_path).is_ok() {
            self.settings = settings;
            self.preview_volume = volume;
            if let Some(player) = self.preview_player.as_mut() {
                player.set_volume(volume);
            }
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
                    moved_files: Vec::new(),
                },
                cx,
            );
            return;
        };
        let import_task = cx.background_spawn(async move {
            let mut imported = false;
            let mut moved_files = Vec::new();
            for path in paths {
                if let Ok(destination) = import_to_folder(&folder, &path, |_| {}) {
                    imported = true;
                    moved_files.push((path, destination));
                }
            }
            ImportBatchResult {
                category,
                imported,
                moved_from: internal_origin,
                moved_files,
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
        self.stop_preview_if_paths_match(&paths, cx);

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
        self.maybe_start_waveform_cache(cx);
        cx.notify();
        Ok(())
    }

    pub fn play_preview_from_start(&mut self, path: PathBuf, cx: &mut Context<Self>) -> bool {
        self.play_preview_from_offset(path, Duration::ZERO, cx)
    }

    pub fn play_preview_from_ratio(
        &mut self,
        path: PathBuf,
        ratio: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        self.stop_active_preview();
        self.ensure_preview_player();
        let Some(player) = self.preview_player.as_mut() else {
            return false;
        };
        match player.play_from_ratio(path.clone(), ratio) {
            Ok(()) => {
                let offset = player
                    .current_position()
                    .map_or(Duration::ZERO, |position| position.offset);
                self.preview_started(path, offset, cx)
            }
            Err(error) => {
                eprintln!(
                    "lowcat preview play failed path={} error={error}",
                    path.display()
                );
                self.preview_current_path = None;
                false
            }
        }
    }

    pub fn stop_preview(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(player) = self.preview_player.as_mut() else {
            return false;
        };
        self.preview_last_stopped = player.stop();
        self.preview_current_path = None;
        self.preview_playhead_watch_running = false;
        cx.emit(LibraryEvent::PreviewAdvanced);
        cx.notify();
        true
    }

    pub fn preview_playhead_ratio_for_path(&self, path: &Path) -> Option<f32> {
        let player = self.preview_player.as_ref()?;
        let position = player.current_position()?;
        if !paths_equal(&position.path, path) {
            return None;
        }
        let duration = player.current_duration()?;
        if duration.is_zero() {
            return None;
        }
        Some((position.offset.as_secs_f32() / duration.as_secs_f32()).clamp(0., 1.))
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

    #[cfg(test)]
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
        self.maybe_start_waveform_cache(cx);
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

    fn set_download_error(&mut self, error: DownloadError, cx: &mut Context<Self>) {
        self.download_cancel = None;
        self.download_state = DownloadState::Error(error);
        cx.notify();
    }

    fn apply_download_progress(&mut self, event: DownloadProgressEvent, cx: &mut Context<Self>) {
        let DownloadState::Running(status) = &mut self.download_state else {
            return;
        };

        match event {
            DownloadProgressEvent::Label(label) => {
                if !label.is_empty() {
                    status.label = label;
                }
            }
            DownloadProgressEvent::Progress(progress) => {
                status.progress = progress.clamp(0., 100.);
            }
        }
        cx.notify();
    }

    fn finish_download(&mut self, result: DownloadBatchResult, cx: &mut Context<Self>) {
        self.download_cancel = None;
        match result.result {
            Ok(output) => {
                let _ = output.path;
                self.download_state = DownloadState::Idle;
                let _ = self.backend.refresh_category(result.category);
                let _ = self.refresh_category_state(result.category);
                self.maybe_start_waveform_cache(cx);
                debug_downloader_interaction(|| {
                    format!("download_finished category={}", result.category.label())
                });
            }
            Err(error) if error.kind == DownloadErrorKind::Canceled => {
                self.download_state = DownloadState::Idle;
                debug_downloader_interaction(|| "download_canceled".to_string());
            }
            Err(error) => {
                debug_downloader_interaction(|| {
                    format!(
                        "download_failed category={} reason={}",
                        result.category.label(),
                        error.message
                    )
                });
                self.download_state = DownloadState::Error(error);
            }
        }
        cx.notify();
    }

    fn finish_import(&mut self, result: ImportBatchResult, cx: &mut Context<Self>) {
        self.importing = false;
        self.import_progress = None;
        if result.imported {
            let _ = self.backend.refresh_category(result.category);
            if let Some(origin) = result.moved_from
                && origin != result.category
            {
                for (source, destination) in &result.moved_files {
                    if let Err(error) = self.backend.copy_tags_between_categories(
                        origin,
                        result.category,
                        source,
                        destination,
                    ) {
                        eprintln!(
                            "lowcat cross-category metadata move failed source={} destination={} error={error}",
                            source.display(),
                            destination.display()
                        );
                    }
                }
            }
            let _ = self.refresh_category_state(result.category);
            self.maybe_start_waveform_cache(cx);
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
        self.refresh_category_state_with_tag_rename(category, None, None)
    }

    fn refresh_category_state_after_tag_rename(
        &mut self,
        category: Category,
        key: &str,
        old_value: &str,
        new_value: &str,
    ) -> io::Result<()> {
        self.refresh_category_state_with_tag_rename(
            category,
            Some((key, old_value, new_value)),
            None,
        )
    }

    fn refresh_category_state_after_tag_key_rename(
        &mut self,
        category: Category,
        old_key: &str,
        new_key: &str,
    ) -> io::Result<()> {
        self.refresh_category_state_with_tag_rename(category, None, Some((old_key, new_key)))
    }

    fn refresh_category_state_with_tag_rename(
        &mut self,
        category: Category,
        renamed_tag: Option<(&str, &str, &str)>,
        renamed_key: Option<(&str, &str)>,
    ) -> io::Result<()> {
        let total_start = crate::perf::start();
        let (search, mut selected) = if let Some(state) = self.states.get(&category) {
            (state.search.clone(), state.selected.clone())
        } else {
            (String::new(), BTreeMap::new())
        };
        let schema_start = crate::perf::start();
        let schema = display_schema(self.backend.schema_for(category));
        let all_records = display_records(self.backend.filter(category, "", &BTreeMap::new()));
        let intersections = tag_intersections(&all_records);
        let mut configured = self
            .intersection_tags
            .get(&category)
            .cloned()
            .unwrap_or_default();
        configured.retain(|key, values| {
            values.retain(|value| {
                intersections
                    .get(&(key.clone(), value.clone()))
                    .is_some_and(|others| !others.is_empty())
            });
            !values.is_empty()
        });
        if self.intersection_tags.get(&category) != Some(&configured) {
            self.save_intersection_tags(category, configured);
        }
        self.tag_intersections.insert(category, intersections);
        crate::perf::finish("library.schema", schema_start, || {
            format!("category={} keys={}", category.label(), schema.len())
        });
        reconcile_selected_filter_keys(&mut selected, &schema, renamed_key);
        reconcile_selected_filters(&mut selected, &schema, renamed_tag);
        let filter_start = crate::perf::start();
        let results = filter_cached_records(&all_records, &search, &selected, self.filters_open);
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
        state.selected = selected;
        state.schema = schema;
        state.all_records = Arc::new(all_records);
        state.results = results;
        crate::perf::finish("library.refresh_category_state", total_start, || {
            format!(
                "category={} results={}",
                category.label(),
                state.results.len()
            )
        });
        self.bump_results_revision();
        Ok(())
    }

    fn refresh_search_results(&mut self, category: Category) -> io::Result<()> {
        let total_start = crate::perf::start();
        let (search, selected, all_records) = if let Some(state) = self.states.get(&category) {
            (
                state.search.clone(),
                state.selected.clone(),
                state.all_records.clone(),
            )
        } else {
            (String::new(), BTreeMap::new(), Arc::default())
        };
        let results = filter_cached_records(&all_records, &search, &selected, self.filters_open);
        let results_len = results.len();
        self.states.entry(category).or_default().results = results;
        self.bump_results_revision();
        crate::perf::finish("library.refresh_search_results", total_start, || {
            format!(
                "category={} results={results_len} search_len={} selected_keys={}",
                category.label(),
                search.len(),
                selected.len()
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
            self.maybe_start_waveform_cache(cx);
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
        self.stop_preview_if_missing(cx);
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
                self.stop_preview_if_missing(cx);
                self.maybe_start_waveform_cache(cx);
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

    fn maybe_start_waveform_cache(&mut self, cx: &mut Context<Self>) {
        const BATCH_SIZE: usize = 16;
        if self.waveform_cache_in_flight {
            return;
        }
        self.waveform_cache_in_flight = true;
        let db_path = database_path_for_settings(&self.settings_path);
        let skipped_paths = self.waveform_cache_skipped_paths.clone();
        let fetch_limit = BATCH_SIZE + skipped_paths.len();
        let task = cx.background_spawn(async move {
            let backend = Backend::new(db_path)?;
            let paths = backend.missing_waveform_cache_paths(fetch_limit)?;
            let mut processed = 0usize;
            let mut changed = false;
            let mut skipped = Vec::new();
            for path in paths {
                if skipped_paths.contains(&path) {
                    continue;
                }
                if processed >= BATCH_SIZE {
                    break;
                }
                if !path.is_file() {
                    skipped.push(path);
                    continue;
                }
                processed += 1;
                match crate::preview_waveform::generate_waveform_binary256(&path) {
                    Ok(waveform) => {
                        backend.set_preview_waveform(&path, waveform)?;
                        changed = true;
                    }
                    Err(error) => {
                        if lowcat_debug_enabled() {
                            eprintln!(
                                "[lowcat:preview] waveform failed path={} error={error}",
                                path.display()
                            );
                        }
                        skipped.push(path);
                    }
                }
            }
            Ok::<_, io::Error>(WaveformCacheBatchResult {
                processed,
                changed,
                skipped_paths: skipped,
            })
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |lib, cx| {
                lib.finish_waveform_cache(result, cx);
            })
            .ok();
        })
        .detach();
    }

    pub fn maybe_start_priority_waveform_cache(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.waveform_priority_cache_in_flight.contains(&path) || !path.is_file() {
            return;
        }

        self.waveform_cache_skipped_paths.remove(&path);
        self.waveform_priority_cache_in_flight.insert(path.clone());
        let db_path = database_path_for_settings(&self.settings_path);
        let task = cx.background_spawn(async move {
            let backend = Backend::new(db_path)?;
            let result = crate::preview_waveform::generate_waveform_binary256(&path)
                .and_then(|waveform| backend.set_preview_waveform(&path, waveform));
            Ok::<_, io::Error>((path, result))
        });

        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |lib, cx| {
                lib.finish_priority_waveform_cache(result, cx);
            })
            .ok();
        })
        .detach();
    }

    fn finish_priority_waveform_cache(
        &mut self,
        result: io::Result<(PathBuf, io::Result<()>)>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((path, Ok(()))) => {
                self.waveform_priority_cache_in_flight.remove(&path);
                for category in Category::ALL {
                    let _ = self.refresh_category_state(category);
                }
                cx.notify();
            }
            Ok((path, Err(error))) => {
                self.waveform_priority_cache_in_flight.remove(&path);
                if lowcat_debug_enabled() {
                    eprintln!(
                        "[lowcat:preview] priority waveform failed path={} error={error}",
                        path.display()
                    );
                }
                cx.notify();
            }
            Err(error) => {
                self.waveform_priority_cache_in_flight.clear();
                eprintln!("lowcat priority waveform cache failed error={error}");
                cx.notify();
            }
        }
    }

    fn finish_waveform_cache(
        &mut self,
        result: io::Result<WaveformCacheBatchResult>,
        cx: &mut Context<Self>,
    ) {
        self.waveform_cache_in_flight = false;
        match result {
            Ok(result) => {
                let skipped_count = result.skipped_paths.len();
                self.waveform_cache_skipped_paths
                    .extend(result.skipped_paths);
                if result.changed {
                    for category in Category::ALL {
                        let _ = self.refresh_category_state(category);
                    }
                    cx.notify();
                }
                if result.processed >= 16 || skipped_count > 0 {
                    self.maybe_start_waveform_cache(cx);
                }
            }
            Err(error) => {
                eprintln!("lowcat waveform cache failed error={error}");
                cx.notify();
            }
        }
    }

    fn play_preview_from_offset(
        &mut self,
        path: PathBuf,
        offset: Duration,
        cx: &mut Context<Self>,
    ) -> bool {
        self.stop_active_preview();
        self.ensure_preview_player();
        let Some(player) = self.preview_player.as_mut() else {
            return false;
        };
        match player.play_from(path.clone(), offset) {
            Ok(()) => self.preview_started(path, offset, cx),
            Err(error) => {
                eprintln!(
                    "lowcat preview play failed path={} error={error}",
                    path.display()
                );
                self.preview_current_path = None;
                false
            }
        }
    }

    fn ensure_preview_player(&mut self) {
        if self.preview_player.is_none() {
            self.preview_player = Some(PreviewPlayer::new(self.preview_volume));
        }
    }

    fn stop_active_preview(&mut self) {
        if let Some(player) = self.preview_player.as_mut()
            && player.is_playing()
        {
            self.preview_last_stopped = player.stop();
        }
    }

    fn preview_started(&mut self, path: PathBuf, offset: Duration, cx: &mut Context<Self>) -> bool {
        self.preview_current_path = Some(path.clone());
        self.preview_last_stopped = Some(PreviewPosition { path, offset });
        self.start_preview_playhead_watch(cx);
        cx.emit(LibraryEvent::PreviewAdvanced);
        cx.notify();
        true
    }

    fn start_preview_playhead_watch(&mut self, cx: &mut Context<Self>) {
        if self.preview_playhead_watch_running {
            return;
        }
        self.preview_playhead_watch_running = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(33))
                    .await;
                let should_continue = this
                    .update(cx, |lib, cx| {
                        let ended_position = lib
                            .preview_player
                            .as_mut()
                            .and_then(|player| player.finish_if_ended());
                        if let Some(position) = ended_position {
                            lib.preview_last_stopped = Some(position);
                            lib.preview_current_path = None;
                            lib.preview_playhead_watch_running = false;
                            cx.emit(LibraryEvent::PreviewAdvanced);
                            cx.notify();
                            return false;
                        }
                        let playing = lib
                            .preview_player
                            .as_ref()
                            .is_some_and(|player| player.is_playing());
                        if !playing {
                            lib.preview_playhead_watch_running = false;
                            return false;
                        }
                        cx.emit(LibraryEvent::PreviewAdvanced);
                        true
                    })
                    .unwrap_or(false);
                if !should_continue {
                    break;
                }
            }
        })
        .detach();
    }

    fn stop_preview_if_paths_match(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let Some(current_path) = self.preview_current_path.as_ref() else {
            return;
        };
        if paths.iter().any(|path| paths_equal(path, current_path)) {
            self.stop_preview(cx);
        }
    }

    fn stop_preview_if_missing(&mut self, cx: &mut Context<Self>) {
        let Some(current_path) = self.preview_current_path.as_ref() else {
            return;
        };
        if !current_path.is_file() {
            self.stop_preview(cx);
        }
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
        let all_records = Arc::new(display_records(self.backend.filter(
            category,
            "",
            &BTreeMap::new(),
        )));
        let results = all_records.as_ref().clone();
        CategoryState {
            schema,
            all_records,
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

fn tag_intersections(
    records: &[FileRecord],
) -> BTreeMap<(String, String), BTreeSet<(String, String)>> {
    let mut intersections = BTreeMap::new();
    for record in records {
        let tags: BTreeSet<(String, String)> = record
            .tags
            .iter()
            .flat_map(|(key, values)| {
                values.iter().flat_map(move |value| {
                    let mut identities = vec![(key.clone(), value.clone())];
                    if let Some((parent, _)) = crate::model::split_subtag(value) {
                        identities.push((key.clone(), parent.to_string()));
                    }
                    identities
                })
            })
            .collect();
        for tag in &tags {
            intersections
                .entry(tag.clone())
                .or_insert_with(BTreeSet::new)
                .extend(tags.iter().filter(|other| *other != tag).cloned());
        }
    }
    intersections
}

fn filter_cached_records(
    records: &[FileRecord],
    search: &str,
    selected: &BTreeMap<String, BTreeSet<String>>,
    include_tags: bool,
) -> Vec<FileRecord> {
    let mut results: Vec<_> = records
        .iter()
        .filter(|record| record_matches_scoped(record, search, selected, include_tags))
        .cloned()
        .collect();
    results.sort_by_key(|record| record_search_sort_key_scoped(record, search, include_tags));
    results
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
        .filter_map(|(key, values)| display_tag_key(&key).map(|key| (key, values)))
        .collect()
}

fn display_tag_key(key: &str) -> Option<String> {
    normalize_tag_key(key).map(|key| tag_label(&key).to_string())
}

pub(crate) fn tag_matches_search(value: &str, search: &str) -> bool {
    fuzzy_search_match(value, search)
}

pub(crate) fn tag_exactly_matches_search(value: &str, search: &str) -> bool {
    let search = search.trim();
    !search.is_empty() && value.trim().eq_ignore_ascii_case(search)
}

pub(crate) fn tag_search_match_sort_key(value: &str, search: &str) -> (bool, String) {
    (
        !tag_exactly_matches_search(value, search),
        value.to_lowercase(),
    )
}

pub(crate) fn tag_search_group_sort_key<'a, I>(key: &str, values: I, search: &str) -> (bool, String)
where
    I: IntoIterator<Item = &'a str>,
{
    let has_exact_match = values
        .into_iter()
        .any(|value| tag_exactly_matches_search(value, search));
    (!has_exact_match, key.to_lowercase())
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

fn reconcile_selected_filters(
    selected: &mut BTreeMap<String, BTreeSet<String>>,
    schema: &BTreeMap<String, Vec<String>>,
    renamed_tag: Option<(&str, &str, &str)>,
) {
    if let Some((key, old_value, new_value)) = renamed_tag
        && let Some(key) = display_tag_key(key)
    {
        let old_exists = schema
            .get(&key)
            .is_some_and(|values| values.iter().any(|value| value == old_value));
        let new_exists = schema
            .get(&key)
            .is_some_and(|values| values.iter().any(|value| value == new_value));
        if !old_exists
            && new_exists
            && let Some(values) = selected.get_mut(&key)
            && values.remove(old_value)
        {
            values.insert(new_value.to_string());
        }
    }

    selected.retain(|key, values| {
        let Some(available) = schema.get(key) else {
            return false;
        };
        values.retain(|filter| {
            available
                .iter()
                .any(|value| crate::model::tag_value_matches_filter(value, filter))
        });
        !values.is_empty()
    });
}

fn reconcile_selected_filter_keys(
    selected: &mut BTreeMap<String, BTreeSet<String>>,
    schema: &BTreeMap<String, Vec<String>>,
    renamed_key: Option<(&str, &str)>,
) {
    if let Some((old_key, new_key)) = renamed_key
        && let (Some(old_key), Some(new_key)) = (display_tag_key(old_key), display_tag_key(new_key))
        && !schema.contains_key(&old_key)
        && schema.contains_key(&new_key)
        && let Some(values) = selected.remove(&old_key)
    {
        selected.entry(new_key).or_default().extend(values);
    }
}

fn debug_downloader_interaction(details: impl FnOnce() -> String) {
    if lowcat_debug_enabled() {
        eprintln!("[lowcat:downloader] {}", details());
    }
}

fn debug_library_interaction(details: impl FnOnce() -> String) {
    if lowcat_debug_enabled() {
        eprintln!("[lowcat:library] {}", details());
    }
}

fn lowcat_debug_enabled() -> bool {
    std::env::var("LOWCAT_DEBUG")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false)
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
        assert_eq!(schema_keys, vec!["genre".to_string(), "mood".to_string()]);
        let has_folder =
            library.read_with(cx, |lib, _| lib.category_folder(Category::Music).is_some());
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
            lib.refresh_category_state(Category::Music).unwrap();
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
        let needs_folder =
            library.read_with(cx, |lib, _| lib.category_needs_folder(Category::Music));
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
    fn unified_search_adds_tag_matches_only_while_filter_panel_is_open(
        cx: &mut gpui::TestAppContext,
    ) {
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
            lib.refresh_category_state(Category::Music).unwrap();
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
        let mut groups = vec![("genre", vec!["Hyper"]), ("mood", vec!["Hypr"])];

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
    fn intersection_tag_visibility_persists_and_clears_when_orphaned(
        cx: &mut gpui::TestAppContext,
    ) {
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
}
