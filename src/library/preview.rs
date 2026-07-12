use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{AppContext as _, Context};

use crate::backend::Backend;
use crate::model::Category;
use crate::preview_player::{PreviewPlayer, PreviewPosition};

use super::{Library, LibraryEvent, database_path_for_settings, paths_equal};

struct WaveformCacheBatchResult {
    processed: usize,
    changed: bool,
    skipped_paths: Vec<PathBuf>,
}

impl Library {
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

    pub(super) fn maybe_start_waveform_cache(&mut self, cx: &mut Context<Self>) {
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
                        crate::diagnostics::debug("preview", || {
                            format!("waveform failed path={} error={error}", path.display())
                        });
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
                    self.refresh_category_state(category);
                }
                cx.notify();
            }
            Ok((path, Err(error))) => {
                self.waveform_priority_cache_in_flight.remove(&path);
                crate::diagnostics::debug("preview", || {
                    format!(
                        "priority waveform failed path={} error={error}",
                        path.display()
                    )
                });
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
                        self.refresh_category_state(category);
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

    pub(super) fn stop_preview_if_paths_match(
        &mut self,
        paths: &[PathBuf],
        cx: &mut Context<Self>,
    ) {
        let Some(current_path) = self.preview_current_path.as_ref() else {
            return;
        };
        if paths.iter().any(|path| paths_equal(path, current_path)) {
            self.stop_preview(cx);
        }
    }

    pub(super) fn stop_preview_if_missing(&mut self, cx: &mut Context<Self>) {
        let Some(current_path) = self.preview_current_path.as_ref() else {
            return;
        };
        if !current_path.is_file() {
            self.stop_preview(cx);
        }
    }
}
