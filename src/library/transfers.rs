use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use futures::{StreamExt as _, channel::mpsc};
use gpui::{AppContext as _, Context};

use crate::backend::{Backend, import_to_folder};
use crate::downloader::{
    self, DownloadCancel, DownloadError, DownloadErrorKind, DownloadProgressEvent, DownloadRequest,
    DownloadState, DownloadStatus,
};
use crate::model::{AudioFormat, Category};

use super::{ImportProgress, Library, database_path_for_settings, debug_downloader_interaction};

pub(super) struct ImportBatchResult {
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
    result: Result<(), DownloadError>,
}

enum ImportProgressEvent {
    Start { file_name: String },
    Progress(f32),
    Finish,
}

impl Library {
    pub fn downloader_open(&self) -> bool {
        self.downloader_open
    }

    pub fn download_state(&self) -> DownloadState {
        self.download_state.clone()
    }

    pub fn import_progress(&self) -> Option<&ImportProgress> {
        self.import_progress.as_ref()
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
        crate::diagnostics::debug("import", || {
            format!(
                "started category={} folder={} paths={paths:?}",
                category.label(),
                folder.display()
            )
        });
        let import_task = cx.background_spawn(async move {
            let mut imported = false;
            let mut moved_files = Vec::new();
            for path in paths {
                match import_to_folder(&folder, &path, |_| {}) {
                    Ok(destination) => {
                        crate::diagnostics::debug("import", || {
                            format!(
                                "moved source={} destination={}",
                                path.display(),
                                destination.display()
                            )
                        });
                        imported = true;
                        moved_files.push((path, destination));
                    }
                    Err(error) => {
                        eprintln!(
                            "lowcat import failed source={} destination_folder={} error={error}",
                            path.display(),
                            folder.display()
                        );
                    }
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
            Ok(()) => {
                self.download_state = DownloadState::Idle;
                let _ = self.backend.refresh_category(result.category);
                self.refresh_category_state(result.category);
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

    pub(super) fn finish_import(&mut self, result: ImportBatchResult, cx: &mut Context<Self>) {
        if self.focus_rescan_in_flight {
            crate::diagnostics::debug("import", || {
                format!(
                    "deferring refresh until focus rescan finishes category={} imported={}",
                    result.category.label(),
                    result.imported
                )
            });
            self.deferred_import_result = Some(result);
            cx.notify();
            return;
        }

        self.importing = false;
        self.import_progress = None;
        let moved_from = result
            .moved_from
            .filter(|origin| *origin != result.category);
        if result.imported {
            if let Err(error) = self.backend.refresh_category(result.category) {
                eprintln!(
                    "lowcat import refresh failed category={} error={error}",
                    result.category.label()
                );
            }
            if let Some(origin) = moved_from {
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
            self.refresh_category_state(result.category);
            crate::diagnostics::debug("import", || {
                let state = &self.states[&result.category];
                let destinations = result
                    .moved_files
                    .iter()
                    .map(|(_, destination)| destination)
                    .collect::<Vec<_>>();
                let indexed = state.all_records.iter().any(|record| {
                    record
                        .variants
                        .iter()
                        .any(|variant| destinations.iter().any(|path| *path == &variant.path))
                });
                let visible = state.results.iter().any(|record| {
                    record
                        .variants
                        .iter()
                        .any(|variant| destinations.iter().any(|path| *path == &variant.path))
                });
                format!(
                    "refreshed category={} indexed={} visible={} all_records={} results={} search={:?} selected={:?}",
                    result.category.label(),
                    indexed,
                    visible,
                    state.all_records.len(),
                    state.results.len(),
                    state.search,
                    state.selected
                )
            });
            self.maybe_start_waveform_cache(cx);
            if let Some(origin) = moved_from {
                let _ = self.backend.refresh_category(origin);
                self.refresh_category_state(origin);
            }
        }
        cx.notify();
    }

    fn finish_convert_unsupported(&mut self, result: ConvertBatchResult, cx: &mut Context<Self>) {
        self.importing = false;
        self.import_progress = None;
        if result.converted {
            let _ = self.backend.refresh_category(result.category);
            self.refresh_category_state(result.category);
            self.maybe_start_waveform_cache(cx);
        }
        cx.notify();
    }

    fn finish_trash_files(&mut self, result: TrashBatchResult, cx: &mut Context<Self>) {
        if let Err(error) = result.result {
            eprintln!(
                "lowcat trash batch failed category={} requested={} error={error}",
                result.category.label(),
                result.file_count
            );
        }
        if let Err(error) = self.backend.refresh_category(result.category) {
            eprintln!(
                "lowcat trash refresh failed category={} error={error}",
                result.category.label()
            );
        }
        self.refresh_category_state(result.category);
        self.stop_preview_if_missing(cx);
        cx.notify();
    }
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("import")
        .to_string()
}
