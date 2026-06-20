mod native_drag;

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, DismissEvent, Entity, FocusHandle,
    Focusable, InteractiveElement as _, IntoElement, KeyDownEvent, Keystroke, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render,
    SharedString, Size, StatefulInteractiveElement as _, Styled, Window, div, hsla,
    prelude::FluentBuilder as _, px, red, size,
};
use gpui_component::{
    ActiveTheme as _, Sizable, StyledExt, VirtualListScrollHandle,
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenuItem},
    scroll::{ScrollableElement, ScrollbarAxis},
    table::*,
    v_virtual_list,
};

use crate::ui::CONTENT_PX;
use crate::{
    library::{Library, LibraryEvent},
    model::{AudioFormat, FileRecord},
};

const TAG_CELL_X_PADDING_WIDTH: f32 = 24.;
const TAG_CHIP_X_PADDING_WIDTH: f32 = 12.;
const TAG_ADD_BUTTON_WIDTH: f32 = 19.;
const TAG_COLUMN_MIN_WIDTH: f32 = TAG_CELL_X_PADDING_WIDTH + TAG_ADD_BUTTON_WIDTH;
const TAG_GAP_WIDTH: f32 = 4.;
const TAG_TEXT_WIDTH: f32 = 7.;
const TAG_EDITOR_WIDTH: f32 = 90.;
const FILE_DRAG_THRESHOLD_PX: f32 = 4.;
const CONVERT_MENU_PANE_WIDTH: f32 = 160.;
const ROW_HEIGHT: Pixels = px(32.);

struct PendingFileDrag {
    extension: String,
    paths: Vec<PathBuf>,
    origin: Point<Pixels>,
}

struct TagWidthCache {
    keys: Vec<String>,
    editing: Option<(PathBuf, String)>,
    row_count: usize,
    widths: Vec<Pixels>,
}

#[derive(Clone)]
struct ConversionAction {
    target: AudioFormat,
    sources: Vec<PathBuf>,
}

#[derive(Clone)]
struct DeleteTarget {
    row_count: usize,
    paths: Vec<PathBuf>,
}

#[derive(Clone)]
struct PendingDelete {
    target: DeleteTarget,
}

pub struct FileTable {
    library: Entity<Library>,
    tag_input: Entity<InputState>,
    editing: Option<(PathBuf, String)>,
    focus_handle: FocusHandle,
    alt_down: bool,
    hovered_row: Option<PathBuf>,
    hovered_tag_chip: Option<PathBuf>,
    hovered_delete_row: Option<PathBuf>,
    pending_delete: Option<PendingDelete>,
    pending_context_menu_delete: Option<DeleteTarget>,
    row_context_menu_open: bool,
    pending_drag: Option<PendingFileDrag>,
    selected: BTreeSet<PathBuf>,
    selection_anchor: Option<PathBuf>,
    row_scroll_handle: VirtualListScrollHandle,
    row_sizes: Rc<Vec<Size<Pixels>>>,
    row_sizes_len: usize,
    tag_width_cache: Option<TagWidthCache>,
}

impl FileTable {
    fn tag_values_width(values: &[String]) -> f32 {
        values
            .iter()
            .map(|value| value.chars().count() as f32 * TAG_TEXT_WIDTH + TAG_CHIP_X_PADDING_WIDTH)
            .sum::<f32>()
            + values.len().saturating_sub(1) as f32 * TAG_GAP_WIDTH
    }

    fn tag_column_width(
        state: &crate::model::CategoryState,
        key: &str,
        editing: Option<(&PathBuf, &str)>,
    ) -> Pixels {
        let header_width = key.chars().count() as f32 * TAG_TEXT_WIDTH + TAG_CELL_X_PADDING_WIDTH;
        let mut width = header_width;

        for record in &state.results {
            let value_width = record
                .tags
                .get(key)
                .map_or(0., |values| Self::tag_values_width(values));
            let is_editing = editing
                .is_some_and(|(path, editing_key)| path == &record.path && editing_key == key);
            let gap_width = if value_width > 0. { TAG_GAP_WIDTH } else { 0. };
            let action_width = if is_editing {
                TAG_EDITOR_WIDTH
            } else if record.is_convertible() {
                0.
            } else {
                TAG_ADD_BUTTON_WIDTH
            };
            let row_width = value_width + gap_width + action_width;
            width = width.max(row_width + TAG_CELL_X_PADDING_WIDTH);
        }

        px(width.max(TAG_COLUMN_MIN_WIDTH))
    }

    pub fn new(library: Entity<Library>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |this, _, cx| {
            this.tag_width_cache = None;
            cx.notify();
        })
        .detach();
        cx.subscribe(&library, |this, _, event: &LibraryEvent, cx| match event {
            LibraryEvent::TagEdited { path } => {
                this.tag_width_cache = None;
                this.hovered_tag_chip = None;
                if this.hovered_row.as_ref() == Some(path) {
                    this.hovered_delete_row = Some(path.clone());
                }
                cx.notify();
            }
        })
        .detach();

        let tag_input = cx.new(|cx| InputState::new(window, cx));

        cx.subscribe_in(
            &tag_input,
            window,
            |this, state, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let value = state.read(cx).value().to_string();
                    this.commit_tag(value, window, cx);
                }
                InputEvent::Blur => this.cancel_tag(window, cx),
                _ => {}
            },
        )
        .detach();

        Self {
            library,
            tag_input,
            editing: None,
            focus_handle: cx.focus_handle(),
            alt_down: false,
            hovered_row: None,
            hovered_tag_chip: None,
            hovered_delete_row: None,
            pending_delete: None,
            pending_context_menu_delete: None,
            row_context_menu_open: false,
            pending_drag: None,
            selected: BTreeSet::new(),
            selection_anchor: None,
            row_scroll_handle: VirtualListScrollHandle::new(),
            row_sizes: Rc::new(Vec::new()),
            row_sizes_len: 0,
            tag_width_cache: None,
        }
    }

    fn table_columns(&mut self, cx: &mut Context<Self>) -> (Vec<String>, Vec<Pixels>, usize) {
        let editing = self.editing.clone();
        let (keys, row_count, widths) = {
            let state = self.library.read(cx).active_state();
            let keys: Vec<String> = state.schema.keys().cloned().collect();
            let row_count = state.results.len();

            if let Some(cache) = self.tag_width_cache.as_ref()
                && cache.keys == keys
                && cache.editing == editing
                && cache.row_count == row_count
            {
                return (cache.keys.clone(), cache.widths.clone(), cache.row_count);
            }

            let width_start = crate::perf::start();
            let editing_ref = editing.as_ref().map(|(path, key)| (path, key.as_str()));
            let widths: Vec<Pixels> = keys
                .iter()
                .map(|key| Self::tag_column_width(state, key, editing_ref))
                .collect();
            crate::perf::finish("table.widths", width_start, || {
                format!(
                    "rows={} keys={} cached=false",
                    state.results.len(),
                    keys.len()
                )
            });

            (keys, row_count, widths)
        };

        self.tag_width_cache = Some(TagWidthCache {
            keys: keys.clone(),
            editing,
            row_count,
            widths: widths.clone(),
        });

        (keys, widths, row_count)
    }

    fn row_sizes(&mut self, len: usize) -> Rc<Vec<Size<Pixels>>> {
        if self.row_sizes_len != len {
            self.row_sizes = Rc::new((0..len).map(|_| size(px(0.), ROW_HEIGHT)).collect());
            self.row_sizes_len = len;
        }
        self.row_sizes.clone()
    }

    pub(crate) fn set_alt_down(&mut self, alt_down: bool, cx: &mut Context<Self>) {
        if self.alt_down != alt_down {
            self.alt_down = alt_down;
            cx.notify();
        }
    }

    fn start_editing(
        &mut self,
        path: PathBuf,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing = Some((path, key));
        self.tag_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn commit_tag(&mut self, raw: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some((path, key)) = self.editing.take() else {
            return;
        };
        let value = raw.trim();
        if !value.is_empty() {
            self.library
                .update(cx, |lib, cx| lib.add_tag(path, &key, value, cx));
        }
        self.focus_handle.focus(window, cx);
        window.refresh();
        cx.notify();
    }

    fn cancel_tag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn start_pending_drag(
        &mut self,
        record: &FileRecord,
        extension: String,
        origin: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let start = crate::perf::start();
        let state = self.library.read(cx);
        let paths = self.selected_paths_for_extension(
            &state.active_state().results,
            &record.path,
            &extension,
            state.format_priority(),
        );
        let path_count = paths.len();
        self.pending_drag = Some(PendingFileDrag {
            extension,
            paths,
            origin,
        });
        crate::perf::finish("table.pending_drag", start, || {
            format!("paths={path_count}")
        });
    }

    fn clear_pending_drag(&mut self) {
        self.pending_drag = None;
    }

    fn select_row(
        &mut self,
        path: PathBuf,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = crate::perf::start();
        let paths: Vec<PathBuf> = self
            .library
            .read(cx)
            .active_state()
            .results
            .iter()
            .map(|record| record.path.clone())
            .collect();

        if extend
            && let Some(anchor) = self.selection_anchor.as_ref()
            && let (Some(anchor_ix), Some(path_ix)) = (
                paths.iter().position(|candidate| candidate == anchor),
                paths.iter().position(|candidate| candidate == &path),
            )
        {
            let (start, end) = if anchor_ix <= path_ix {
                (anchor_ix, path_ix)
            } else {
                (path_ix, anchor_ix)
            };
            self.selected = paths[start..=end].iter().cloned().collect();
        } else {
            self.selected.clear();
            self.selected.insert(path.clone());
            self.selection_anchor = Some(path);
        }

        self.focus_handle.focus(window, cx);
        cx.notify();
        crate::perf::finish("table.select_row", start, || {
            format!("rows={} selected={}", paths.len(), self.selected.len())
        });
    }

    fn selected_paths_for_extension(
        &self,
        records: &[FileRecord],
        path: &Path,
        extension: &str,
        priority: &[AudioFormat],
    ) -> Vec<PathBuf> {
        if self.selected.len() > 1 && self.selected.contains(path) {
            records
                .iter()
                .filter(|record| self.selected.contains(record.path.as_path()))
                .filter_map(|record| {
                    if let Some(variant) = record.variant_for_extension(extension) {
                        return Some(variant.path.clone());
                    }

                    let fallback = priority
                        .iter()
                        .filter_map(|format| record.variant_for_extension(format.extension()))
                        .next()
                        .or_else(|| record.variants.first());
                    if let Some(variant) = fallback {
                        Some(variant.path.clone())
                    } else {
                        None
                    }
                })
                .collect()
        } else {
            records
                .iter()
                .find(|record| record.path.as_path() == path)
                .and_then(|record| record.variant_for_extension(extension))
                .map(|variant| vec![variant.path.clone()])
                .unwrap_or_default()
        }
    }

    fn conversion_actions(
        &self,
        record: &FileRecord,
        selected_records: &[FileRecord],
    ) -> Vec<ConversionAction> {
        if self.selected.len() > 1 && self.selected.contains(record.path.as_path()) {
            return AudioFormat::ALL
                .into_iter()
                .filter_map(|target| {
                    let mut sources = Vec::new();
                    for selected in selected_records {
                        if !selected.has_extension(target.extension()) {
                            sources.push(selected.path.clone());
                        }
                    }
                    (!sources.is_empty()).then_some(ConversionAction { target, sources })
                })
                .collect();
        }

        record
            .conversion_targets()
            .into_iter()
            .map(|target| ConversionAction {
                target,
                sources: vec![record.path.clone()],
            })
            .collect()
    }

    fn delete_target(&self, record: &FileRecord, selected_records: &[FileRecord]) -> DeleteTarget {
        let records: Vec<&FileRecord> =
            if self.selected.len() > 1 && self.selected.contains(record.path.as_path()) {
                selected_records.iter().collect()
            } else {
                vec![record]
            };

        let mut seen = BTreeSet::new();
        let paths = records
            .iter()
            .flat_map(|record| record.variants.iter().map(|variant| variant.path.clone()))
            .filter(|path| seen.insert(path.clone()))
            .collect();

        DeleteTarget {
            row_count: records.len(),
            paths,
        }
    }

    fn selected_delete_target(&self, cx: &mut Context<Self>) -> Option<DeleteTarget> {
        if self.selected.is_empty() {
            return None;
        }

        let state = self.library.read(cx);
        let selected_records: Vec<&FileRecord> = state
            .active_state()
            .results
            .iter()
            .filter(|record| self.selected.contains(record.path.as_path()))
            .collect();

        if selected_records.is_empty() {
            return None;
        }

        let mut seen = BTreeSet::new();
        let paths = selected_records
            .iter()
            .flat_map(|record| record.variants.iter().map(|variant| variant.path.clone()))
            .filter(|path| seen.insert(path.clone()))
            .collect();

        Some(DeleteTarget {
            row_count: selected_records.len(),
            paths,
        })
    }

    fn confirm_delete_target(
        &mut self,
        target: DeleteTarget,
        _source: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if target.paths.is_empty() {
            return;
        }

        self.pending_delete = Some(PendingDelete { target });
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn confirm_selected_delete(
        &mut self,
        source: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.selected_delete_target(cx) else {
            return false;
        };
        self.confirm_delete_target(target, source, window, cx);
        true
    }

    fn request_context_menu_delete(&mut self, target: DeleteTarget) {
        self.pending_context_menu_delete = Some(target);
    }

    fn confirm_pending_context_menu_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.pending_context_menu_delete.take() else {
            return;
        };

        self.confirm_delete_target(target, "context-menu", window, cx);
    }

    pub fn pending_delete_counts(&self) -> Option<(usize, usize)> {
        let pending = self.pending_delete.as_ref()?;
        Some((pending.target.row_count, pending.target.paths.len()))
    }

    pub fn cancel_delete(&mut self, cx: &mut Context<Self>) -> bool {
        if self.pending_delete.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.selected.is_empty() && self.selection_anchor.is_none() {
            return false;
        }

        self.selected.clear();
        self.selection_anchor = None;
        cx.notify();
        true
    }

    pub fn confirm_pending_delete(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_delete.take() else {
            return;
        };

        let paths = pending.target.paths;
        self.selected.clear();
        self.selection_anchor = None;
        self.hovered_row = None;
        self.hovered_tag_chip = None;
        self.hovered_delete_row = None;
        self.library
            .update(cx, |lib, cx| lib.trash_files(paths, cx));
        cx.notify();
    }

    fn set_row_hovered(&mut self, path: PathBuf, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.hovered_row.as_ref() != Some(&path) {
                self.hovered_row = Some(path.clone());
                self.hovered_delete_row = Some(path);
                cx.notify();
            }
        } else if self.hovered_row.as_ref() == Some(&path) {
            self.hovered_row = None;
            if self.hovered_delete_row.as_ref() == Some(&path) {
                self.hovered_delete_row = None;
            }
            cx.notify();
        }
    }

    fn set_tag_chip_hovered(&mut self, path: PathBuf, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.hovered_tag_chip.as_ref() != Some(&path) {
                self.hovered_tag_chip = Some(path);
                cx.notify();
            }
        } else if self.hovered_tag_chip.as_ref() == Some(&path) {
            self.hovered_tag_chip = None;
            cx.notify();
        }
    }

    pub(crate) fn cancel_file_drag(&mut self, cx: &mut Context<Self>) {
        self.clear_pending_drag();
        self.library
            .update(cx, |lib, cx| lib.clear_internal_file_drag(cx));
    }

    fn maybe_start_file_drag(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            self.clear_pending_drag();
            return;
        }

        let Some(pending) = self.pending_drag.as_ref() else {
            return;
        };

        let move_start = crate::perf::start();
        let dx = event.position.x.as_f32() - pending.origin.x.as_f32();
        let dy = event.position.y.as_f32() - pending.origin.y.as_f32();
        if dx.hypot(dy) < FILE_DRAG_THRESHOLD_PX {
            crate::perf::finish("table.drag_move", move_start, || {
                "below_threshold".to_string()
            });
            return;
        }

        let drag_start = crate::perf::start();
        let extension = pending.extension.clone();
        let drag_paths = pending.paths.clone();
        let drag_path_count = drag_paths.len();
        self.clear_pending_drag();
        self.library.update(cx, |lib, cx| {
            if drag_paths.len() == 1 {
                lib.begin_internal_file_drag(drag_paths[0].clone(), cx);
            } else {
                lib.begin_internal_file_drag_files(drag_paths.clone(), cx);
            }
        });
        let (drag_finished_tx, mut drag_finished_rx) = mpsc::unbounded::<()>();
        let library = self.library.clone();
        cx.spawn(async move |_, cx| {
            if drag_finished_rx.next().await.is_some() {
                library.update(cx, |lib, cx| lib.clear_internal_file_drag(cx));
            }
        })
        .detach();

        let library = self.library.clone();
        window.on_next_frame(move |window, cx| {
            let native_start = crate::perf::start();
            let drag_finished_tx = drag_finished_tx.clone();
            let ok = native_drag::start_file_drag(drag_paths.clone(), window, move || {
                let _ = drag_finished_tx.unbounded_send(());
            });
            crate::perf::finish("table.native_file_drag", native_start, || {
                format!("paths={} ok={ok}", drag_paths.len())
            });
            if !ok {
                library.update(cx, |lib, cx| lib.clear_internal_file_drag(cx));
            }
        });
        window.refresh();
        crate::perf::finish("table.drag_start", drag_start, || {
            format!("extension={extension} paths={drag_path_count}")
        });
        crate::perf::finish("table.drag_move", move_start, || {
            format!("started extension={extension} paths={drag_path_count}")
        });
    }

    fn render_rows(
        &mut self,
        range: Range<usize>,
        keys: Vec<String>,
        tag_widths: Vec<Pixels>,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        crate::perf::sample("table.rows.rate");
        let rows_start = crate::perf::start();
        let (records, selected_records, total_rows, visible_start, visible_end) = {
            let state = self.library.read(cx).active_state();
            let total_rows = state.results.len();
            let visible_start = range.start.min(total_rows);
            let visible_end = range.end.min(total_rows).max(visible_start);
            let selected_records = if self.selected.len() > 1 {
                state
                    .results
                    .iter()
                    .filter(|record| self.selected.contains(record.path.as_path()))
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };

            (
                state.results[visible_start..visible_end].to_vec(),
                selected_records,
                total_rows,
                visible_start,
                visible_end,
            )
        };

        let rows = records
            .iter()
            .enumerate()
            .map(|(offset, record)| {
                self.render_record(
                    visible_start + offset,
                    record,
                    &keys,
                    &tag_widths,
                    &selected_records,
                    cx,
                )
            })
            .collect();

        crate::perf::finish("table.rows", rows_start, || {
            format!(
                "visible={}..{} total={} keys={} selected={}",
                visible_start,
                visible_end,
                total_rows,
                keys.len(),
                self.selected.len()
            )
        });
        rows
    }

    fn render_record(
        &self,
        row_ix: usize,
        record: &FileRecord,
        keys: &[String],
        tag_widths: &[Pixels],
        selected_records: &[FileRecord],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = record.path.clone();
        let convertible = record.is_convertible();
        let selected = self.selected.contains(path.as_path());
        let delete_target = self.delete_target(record, selected_records);
        let row_hover_bg = if convertible {
            hsla(0.095, 1., 0.55, 0.2)
        } else {
            cx.theme().table_hover
        };
        let convertible_bg = hsla(0.095, 1., 0.55, 0.12);
        let chip_delete_bg = red().opacity(0.18);
        let row_delete_bg = red().opacity(0.18);
        let chip_hovered = self.hovered_tag_chip.as_ref() == Some(&path);
        let row_hovered = !chip_hovered && self.hovered_row.as_ref() == Some(&path);
        let row_delete_hover_enabled = self.hovered_tag_chip.is_none();
        let delete_hovered = row_delete_hover_enabled
            && self.alt_down
            && self.hovered_delete_row.as_ref().is_some_and(|hovered| {
                if self.selected.len() > 1 && self.selected.contains(hovered.as_path()) {
                    selected
                } else {
                    hovered == &path
                }
            });
        let name = div()
            .w_full()
            .min_w_0()
            .truncate()
            .child(record.name.clone());
        let open_path = record.path.clone();
        let select_path = record.path.clone();
        let hover_path = record.path.clone();
        let delete_click_target = delete_target.clone();
        let conversion_actions = self.conversion_actions(record, selected_records);
        let table = cx.entity();
        let menu_action_context = self.focus_handle.clone();
        let row_group = SharedString::from(format!("row-group:{}", path.display()));
        let mut row = div()
            .id(SharedString::from(format!("row:{}", path.display())))
            .group(row_group.clone())
            .h(ROW_HEIGHT)
            .flex()
            .flex_row()
            .w_full()
            .text_sm()
            .when(row_ix > 0, |s| {
                s.border_t_1().border_color(cx.theme().table_row_border)
            })
            .when(convertible, |s| s.bg(convertible_bg))
            .when(selected || row_hovered, |s| s.bg(row_hover_bg))
            .when(delete_hovered, |s| s.bg(row_delete_bg))
            .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                this.set_row_hovered(hover_path.clone(), *hovered, cx);
            }))
            .on_click(cx.listener(move |_, event: &ClickEvent, _, _| {
                if event.modifiers().alt {
                    return;
                }
                if event.click_count() == 2 {
                    open_in_default_app(&open_path);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    if event.click_count == 1 {
                        if event.modifiers.alt {
                            this.clear_pending_drag();
                            this.confirm_delete_target(
                                delete_click_target.clone(),
                                "opt-click",
                                window,
                                cx,
                            );
                            cx.stop_propagation();
                            return;
                        }
                        if event.modifiers.shift || !this.selected.contains(&select_path) {
                            this.select_row(select_path.clone(), event.modifiers.shift, window, cx);
                        }
                    }
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                this.maybe_start_file_drag(event, window, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| {
                    this.clear_pending_drag();
                }),
            )
            .context_menu(move |menu, window, menu_cx| {
                let table = table.clone();
                let actions = conversion_actions.clone();
                let target = delete_target.clone();
                let row_count = target.row_count;
                menu_cx.update_entity(&table, |this, cx| {
                    this.row_context_menu_open = true;
                    cx.notify();
                });
                let popup = menu_cx.entity().clone();
                window
                    .subscribe(&popup, menu_cx, {
                        let table = table.clone();
                        move |_, _: &DismissEvent, window, cx| {
                            cx.update_entity(&table, |this, cx| {
                                this.row_context_menu_open = false;
                                this.confirm_pending_context_menu_delete(window, cx);
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                let mut menu = menu
                    .max_w(px(CONVERT_MENU_PANE_WIDTH))
                    .action_context(menu_action_context.clone());
                for action in actions {
                    let table = table.clone();
                    let target = action.target;
                    let label = SharedString::from(format!("Convert to {}", target.label()));
                    menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        let sources = action.sources.clone();
                        cx.update_entity(&table, |this, cx| {
                            this.library.update(cx, |lib, cx| {
                                lib.convert_files_to_format(sources, target, cx);
                            });
                        });
                    }));
                }
                if !conversion_actions.is_empty() {
                    menu = menu.separator();
                }
                let table = table.clone();
                let label = if row_count == 1 {
                    SharedString::from("Delete")
                } else {
                    SharedString::from(format!("Delete {row_count} Rows"))
                };
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    cx.update_entity(&table, |this, _| {
                        this.request_context_menu_delete(target.clone());
                    });
                }));
                menu
            })
            .child(
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .px(CONTENT_PX)
                    .py(px(4.))
                    .relative()
                    .child(
                        div()
                            .absolute()
                            .left(CONTENT_PX)
                            .right(CONTENT_PX)
                            .top_0()
                            .bottom_0()
                            .h_flex()
                            .items_center()
                            .gap_1()
                            .opacity(0.)
                            .group_hover(row_group.clone(), |style| style.opacity(1.))
                            .children(record.variants.iter().map(|variant| {
                                let record = record.clone();
                                let extension = variant.extension.clone();
                                let label = extension.to_ascii_uppercase();
                                div()
                                    .id(SharedString::from(format!(
                                        "extension-chip:{}:{extension}",
                                        record.path.display()
                                    )))
                                    .w(px(56.))
                                    .h_full()
                                    .flex_shrink_0()
                                    .h_flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .text_base()
                                    .bg(cx.theme().muted)
                                    .text_color(cx.theme().muted_foreground)
                                    .cursor_pointer()
                                    .child(SharedString::from(label))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(
                                            move |this, event: &MouseDownEvent, window, cx| {
                                                if event.click_count == 1 {
                                                    if event.modifiers.shift
                                                        || !this.selected.contains(&record.path)
                                                    {
                                                        this.select_row(
                                                            record.path.clone(),
                                                            event.modifiers.shift,
                                                            window,
                                                            cx,
                                                        );
                                                    }
                                                    this.start_pending_drag(
                                                        &record,
                                                        extension.clone(),
                                                        event.position,
                                                        cx,
                                                    );
                                                }
                                            },
                                        ),
                                    )
                            })),
                    )
                    .child(name.group_hover(row_group.clone(), |style| style.opacity(0.))),
            );

        for (key, tag_width) in keys.iter().zip(tag_widths) {
            let group = SharedString::from(format!("cell:{}:{key}", path.display()));
            let is_editing = self
                .editing
                .as_ref()
                .is_some_and(|(p, k)| p == &path && k == key);

            let mut cell = div()
                .id(group.clone())
                .group(group.clone())
                .absolute()
                .left(CONTENT_PX)
                .right(CONTENT_PX)
                .top_0()
                .bottom_0()
                .h_flex()
                .flex_nowrap()
                .items_center()
                .gap_1();

            if let Some(values) = record.tags.get(key) {
                for value in values {
                    let (key, value, path) = (key.clone(), value.clone(), path.clone());
                    let chip_hover_path = path.clone();
                    let chip_group =
                        SharedString::from(format!("chip-group:{}:{key}:{value}", path.display()));

                    cell = cell.child(
                        div()
                            .id(SharedString::from(format!(
                                "chip:{}:{key}:{value}",
                                path.display()
                            )))
                            .group(chip_group.clone())
                            .relative()
                            .h(ROW_HEIGHT)
                            .h_flex()
                            .items_center()
                            .child(
                                div()
                                    .px_1p5()
                                    .rounded_md()
                                    .text_xs()
                                    .whitespace_nowrap()
                                    .bg(cx.theme().muted)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(value.clone()))
                                    .when(self.alt_down, |this| {
                                        this.group_hover(chip_group, move |this| {
                                            this.bg(chip_delete_bg)
                                        })
                                    }),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!(
                                        "chip-hitbox:{}:{key}:{value}",
                                        path.display()
                                    )))
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .top(px(-1.))
                                    .bottom(px(-1.))
                                    .bg(cx.theme().transparent)
                                    .cursor_pointer()
                                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                                        this.set_tag_chip_hovered(
                                            chip_hover_path.clone(),
                                            *hovered,
                                            cx,
                                        );
                                    }))
                                    .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                                    .on_mouse_up(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_mouse_move(|_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, window, cx| {
                                            if event.modifiers().alt {
                                                let path = path.clone();
                                                this.library.update(cx, |lib, cx| {
                                                    lib.remove_tag(path, &key, &value, cx)
                                                });
                                                window.refresh();
                                                cx.notify();
                                            }
                                            cx.stop_propagation();
                                        },
                                    )),
                            ),
                    );
                }
            }

            if is_editing {
                cell = cell.child(
                    div()
                        .h_flex()
                        .items_center()
                        .w(px(TAG_EDITOR_WIDTH))
                        .flex_shrink_0()
                        .px_1p5()
                        .rounded_md()
                        .text_xs()
                        .bg(cx.theme().muted)
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                this.cancel_tag(window, cx);
                            }
                        }))
                        .child(Input::new(&self.tag_input).appearance(false).xsmall()),
                );
            } else if !convertible {
                let (key, path) = (key.clone(), path.clone());
                cell = cell.child(
                    div()
                        .id(SharedString::from(format!("add:{}:{key}", path.display())))
                        .px_1p5()
                        .rounded_md()
                        .text_xs()
                        .whitespace_nowrap()
                        .text_color(cx.theme().muted_foreground)
                        .cursor_pointer()
                        .opacity(0.)
                        .group_hover(group.clone(), |s| s.opacity(1.))
                        .hover(|s| s.text_color(cx.theme().foreground))
                        .child("+")
                        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_mouse_move(|_, _, cx| cx.stop_propagation())
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_editing(path.clone(), key.clone(), window, cx);
                            cx.stop_propagation();
                        })),
                );
            }

            row = row.child(
                div()
                    .h_full()
                    .relative()
                    .w(*tag_width)
                    .min_w(*tag_width)
                    .flex_shrink_0()
                    .child(cell),
            );
        }

        row.into_any_element()
    }
}

impl Focusable for FileTable {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileTable {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::sample("table.render.rate");
        let render_start = crate::perf::start();
        let (keys, tag_widths, row_count) = self.table_columns(cx);

        let mut header_row = TableRow::new().child(
            TableHead::new()
                .flex_1()
                .min_w_0()
                .px(CONTENT_PX)
                .child(div().w_full().min_w_0().truncate().child("name")),
        );
        for (key, tag_width) in keys.iter().zip(&tag_widths) {
            header_row = header_row.child(
                TableHead::new()
                    .w(*tag_width)
                    .min_w(*tag_width)
                    .flex_shrink_0()
                    .px(CONTENT_PX)
                    .child(SharedString::from(key.clone())),
            );
        }

        let virtual_keys = keys.clone();
        let virtual_tag_widths = tag_widths.clone();
        let row_sizes = self.row_sizes(row_count);
        let row_scroll_handle = self.row_scroll_handle.clone();
        let rows = div()
            .size_full()
            .child(
                v_virtual_list(
                    cx.entity().clone(),
                    "file-table-rows",
                    row_sizes,
                    move |this, range, _, cx| {
                        this.render_rows(
                            range,
                            virtual_keys.clone(),
                            virtual_tag_widths.clone(),
                            cx,
                        )
                    },
                )
                .track_scroll(&row_scroll_handle),
            )
            .scrollbar(&row_scroll_handle, ScrollbarAxis::Vertical);

        let table = div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        if this.cancel_delete(cx) || this.clear_selection(cx) {
                            cx.stop_propagation();
                        }
                    }
                    "backspace" | "delete" => {
                        if this.row_context_menu_open {
                            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
                        }
                        if this.confirm_selected_delete("keyboard", window, cx) {
                            cx.stop_propagation();
                        }
                    }
                    _ => {}
                }
            }))
            .relative()
            .flex_1()
            .min_h_0()
            .v_flex()
            .child(
                Table::new()
                    .flex_shrink_0()
                    .child(TableHeader::new().child(header_row)),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(div().size_full().child(rows)),
            );

        crate::perf::finish("table.render", render_start, || {
            format!(
                "rows={} keys={} editing={}",
                row_count,
                keys.len(),
                self.editing.is_some()
            )
        });
        table
    }
}

fn open_in_default_app(path: &Path) {
    if let Err(err) = open::that(path) {
        eprintln!("failed to open {}: {err}", path.display());
    }
}
