use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{AppContext as _, Context, EventEmitter};

use crate::backend::{Backend, RenameRecord};
use crate::downloader::{DownloadCancel, DownloadState};
use crate::model::{
    AudioFormat, Category, CategoryState, ConvertConflictBehavior, FileRecord, FolderTagAssignment,
    default_format_priority, fuzzy_search_match, normalize_tag_key, record_matches_scoped,
    record_search_sort_key_scoped, tag_label,
};
use crate::preview_player::{PreviewPlayer, PreviewPosition};

#[path = "config.rs"]
mod config;
mod preview;
mod transfers;

type TagIdentity = (String, String);
type TagIntersections = BTreeMap<Category, BTreeMap<TagIdentity, BTreeSet<TagIdentity>>>;

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
    tag_intersections: TagIntersections,
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
        self.refresh_search_results(self.active);
        cx.notify();
    }

    pub fn close_filters(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.filters_open {
            return false;
        }
        self.filters_open = false;
        self.refresh_search_results(self.active);
        cx.notify();
        true
    }

    pub fn toggle_downloader(&mut self, cx: &mut Context<Self>) {
        self.downloader_open = !self.downloader_open;
        if self.downloader_open {
            let filters_were_open = self.filters_open;
            self.filters_open = false;
            if filters_were_open {
                self.refresh_search_results(self.active);
            }
        }
        debug_downloader_interaction(|| format!("panel_open={}", self.downloader_open));
        cx.notify();
    }

    pub fn set_category(&mut self, category: Category, cx: &mut Context<Self>) {
        if self.active != category {
            self.active = category;
            self.refresh_search_results(category);
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
        self.refresh_search_results(self.active);
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
        if let Some(state) = self.states.get_mut(&active)
            && let Some(set) = state.selected.get_mut(key)
        {
            set.remove(value);
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

    pub fn add_tag(&mut self, path: PathBuf, key: &str, value: &str, cx: &mut Context<Self>) {
        match self.backend.add_tag(self.active, &path, key, value) {
            Ok(()) => {
                self.refresh_category_state(self.active);
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
                self.refresh_category_state(category);
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
                self.refresh_category_state(category);
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
                self.refresh_category_state(self.active);
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
                self.refresh_category_state_after_tag_rename(category, key, old_value, new_value);
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
                self.refresh_category_state(category);
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
                self.refresh_category_state_after_tag_rename(category, key, old_value, new_value);
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
        self.refresh_category_state(category);
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
        self.refresh_category_state(category);
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
        self.refresh_category_state(category);
        self.maybe_start_waveform_cache(cx);
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
        self.refresh_category_state(self.active);
        cx.notify();
    }

    fn refresh_category_state(&mut self, category: Category) {
        self.refresh_category_state_with_tag_rename(category, None, None)
    }

    fn refresh_category_state_after_tag_rename(
        &mut self,
        category: Category,
        key: &str,
        old_value: &str,
        new_value: &str,
    ) {
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
    ) {
        self.refresh_category_state_with_tag_rename(category, None, Some((old_key, new_key)))
    }

    fn refresh_category_state_with_tag_rename(
        &mut self,
        category: Category,
        renamed_tag: Option<(&str, &str, &str)>,
        renamed_key: Option<(&str, &str)>,
    ) {
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
    }

    fn refresh_search_results(&mut self, category: Category) {
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
                    self.refresh_category_state(category);
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
    crate::diagnostics::debug("downloader", details);
}

fn debug_library_interaction(details: impl FnOnce() -> String) {
    crate::diagnostics::debug("library", details);
}

#[cfg(test)]
mod tests;
