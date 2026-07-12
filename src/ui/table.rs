mod native_drag;
mod preview_waveform;
mod render;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU32, Ordering},
};

use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    Anchor, AnyElement, App, AppContext as _, AsyncApp, Bounds, ClickEvent, Context, CursorStyle,
    DismissEvent, DispatchPhase, Element, ElementId, Entity, FocusHandle, Focusable,
    GlobalElementId, Hitbox, HitboxBehavior, InspectorElementId, InteractiveElement as _,
    IntoElement, KeyDownEvent, Keystroke, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, ParentElement, PathPromptOptions, Pixels, Point, Render, SharedString, Size,
    StatefulInteractiveElement as _, Style, Styled, Window, anchored, deferred, div, fill, hsla,
    point, prelude::FluentBuilder as _, px, red, relative, size, white,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Sizable, StyledExt, VirtualListScrollHandle,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
    menu::{ContextMenuExt as _, PopupMenuItem},
    scroll::{ScrollableElement, ScrollbarAxis},
    table::*,
    v_virtual_list,
};

use crate::ui::CONTENT_PX;
use crate::ui::titlebar::{TITLEBAR_HEIGHT, TITLEBAR_LEFT_OFFSET};
use crate::{
    backend::RenameRecord,
    library::{Library, LibraryEvent},
    model::{AudioFormat, Category, FileRecord, WAVEFORM_BAR_COUNT, WaveformBinary256},
};

const TAG_CELL_LEFT_PADDING_WIDTH: f32 = 12.;
const TAG_CHIP_X_PADDING_WIDTH: f32 = 12.;
const TAG_ADD_BUTTON_WIDTH: f32 = 19.;
const TAG_COLUMN_MIN_WIDTH: f32 = TAG_CELL_LEFT_PADDING_WIDTH + TAG_ADD_BUTTON_WIDTH;
const TAG_GAP_WIDTH: f32 = 4.;
const TAG_TEXT_WIDTH: f32 = 7.;
const TAG_EDITOR_WIDTH: f32 = 90.;
const TAG_KEY_ACTION_WIDTH: f32 = 32.;
const TAG_KEY_EDITOR_WIDTH: f32 = 118.;
const CONVERT_MENU_PANE_WIDTH: f32 = 160.;
const ROW_HEIGHT: Pixels = px(32.);

#[derive(Clone)]
pub(super) struct InternalFileDrag {
    data: Arc<Mutex<InternalFileDragData>>,
}

#[derive(Clone)]
struct InternalFileDragData {
    label: String,
    paths: Arc<Vec<PathBuf>>,
}

impl InternalFileDrag {
    fn new_shared(label: String, paths: Arc<Vec<PathBuf>>) -> Self {
        Self {
            data: Arc::new(Mutex::new(InternalFileDragData { label, paths })),
        }
    }

    fn replace(&self, label: String, paths: Vec<PathBuf>) {
        *self
            .data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = InternalFileDragData {
            label,
            paths: Arc::new(paths),
        };
    }

    fn snapshot(&self) -> InternalFileDragData {
        self.data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    pub(super) fn paths(&self) -> Vec<PathBuf> {
        self.snapshot().paths.as_ref().clone()
    }

    pub(super) fn is_empty(&self) -> bool {
        self.data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .paths
            .is_empty()
    }
}

struct PendingFileDrag;

struct ActiveFileDrag {
    label: String,
    paths: Vec<PathBuf>,
}

struct FileDragPreview {
    label: SharedString,
    cursor_offset: Point<Pixels>,
}

impl FileDragPreview {
    fn new(drag: &InternalFileDrag, cursor_offset: Point<Pixels>) -> Self {
        let data = drag.snapshot();
        let label = if data.paths.len() > 1 {
            format!("{} · {} files", data.label, data.paths.len())
        } else {
            data.label
        };
        Self {
            label: SharedString::from(label),
            cursor_offset,
        }
    }
}

impl Render for FileDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .pl(self.cursor_offset.x + px(8.))
            .pt(self.cursor_offset.y + px(8.))
            .child(
                div()
                    .h(px(28.))
                    .max_w(px(190.))
                    .h_flex()
                    .items_center()
                    .gap_1()
                    .px_2()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().popover)
                    .text_color(cx.theme().foreground)
                    .text_sm()
                    .shadow_md()
                    .child(Icon::new(IconName::File).small())
                    .child(div().min_w_0().truncate().child(self.label.clone())),
            )
    }
}

#[derive(Clone, Default)]
struct NativeDragSession(Arc<AtomicBool>);

impl NativeDragSession {
    fn is_active(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn try_start(&self) -> bool {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    fn finish(&self) {
        self.0.store(false, Ordering::Release);
    }
}

struct TagWidthCache {
    revision: u64,
    keys: Vec<String>,
    editing: Option<TagEditCacheKey>,
    row_count: usize,
    widths: Vec<Pixels>,
}

struct SelectedRowsActions {
    delete_target: Arc<DeleteTarget>,
    rename_target: Arc<RenameTarget>,
    conversion_actions: Arc<Vec<ConversionAction>>,
    row_drag_paths: Arc<Vec<PathBuf>>,
    format_drag_paths: BTreeMap<String, Arc<Vec<PathBuf>>>,
}

struct SelectedRowsActionsCache {
    table_revision: u64,
    actions: Arc<SelectedRowsActions>,
}

#[derive(Clone)]
struct ConversionAction {
    target: AudioFormat,
    sources: Vec<PathBuf>,
}

#[derive(Clone)]
struct DeleteTarget {
    kind: DeleteKind,
    row_count: usize,
    paths: Vec<PathBuf>,
}

#[derive(Clone)]
struct TagChipTarget {
    path: PathBuf,
    key: String,
    value: String,
}

#[derive(Clone)]
enum DeleteKind {
    Rows,
    Format,
    TagKey { key: String },
}

#[derive(Clone)]
struct PendingDelete {
    target: DeleteTarget,
}

#[derive(Clone)]
struct RenameTarget {
    kind: RenameKind,
}

#[derive(Clone)]
enum RenameKind {
    Rows { records: Vec<RowRenameTarget> },
    TagAll { key: String, old_value: String },
    TagKey { old_key: String },
}

#[derive(Clone)]
struct RowRenameTarget {
    name: String,
    stem: String,
    paths: Vec<PathBuf>,
}

#[derive(Clone)]
struct PendingRename {
    target: RenameTarget,
}

#[derive(Clone, PartialEq, Eq)]
enum TagEdit {
    Add {
        path: PathBuf,
        key: String,
    },
    Rename {
        path: PathBuf,
        key: String,
        old_value: String,
    },
}

#[derive(Clone, PartialEq, Eq)]
struct TagEditCacheKey {
    path: PathBuf,
    key: String,
}

pub(crate) struct PendingDeleteCounts {
    pub(crate) kind: PendingDeleteKind,
    pub(crate) row_count: usize,
    pub(crate) file_count: usize,
}

pub(crate) enum PendingDeleteKind {
    Rows,
    Format,
    TagKey,
}

pub(crate) struct PendingRenameDetails {
    pub(crate) kind: PendingRenameKind,
    pub(crate) item_count: usize,
    pub(crate) file_count: usize,
    pub(crate) current_name: Option<String>,
    pub(crate) bulk: bool,
}

pub(crate) enum PendingRenameKind {
    Rows,
    TagAll,
    TagKey,
}

#[derive(Clone, PartialEq, Eq)]
enum ColumnVisibilityHover {
    Key(String),
    ToggleAll,
}

#[derive(Clone, Debug, PartialEq)]
struct PreviewScrub {
    path: PathBuf,
    ratio: f32,
}

impl PreviewScrub {
    fn new(path: PathBuf, ratio: f32) -> Self {
        Self {
            path,
            ratio: ratio.clamp(0., 1.),
        }
    }

    fn update(&mut self, path: &Path, ratio: f32) -> bool {
        if self.path != path {
            return false;
        }
        self.ratio = ratio.clamp(0., 1.);
        true
    }

    fn take_ratio_for_path(scrub: &mut Option<Self>, path: &Path) -> Option<f32> {
        if scrub.as_ref().is_some_and(|scrub| scrub.path == path) {
            scrub.take().map(|scrub| scrub.ratio)
        } else {
            None
        }
    }
}

pub struct FileTable {
    library: Entity<Library>,
    tag_input: Entity<InputState>,
    tag_key_input: Entity<InputState>,
    rename_input: Entity<InputState>,
    editing: Option<TagEdit>,
    creating_tag_key: bool,
    focus_handle: FocusHandle,
    alt_down: bool,
    cmd_down: bool,
    hovered_row: Option<PathBuf>,
    preview_active_row: Option<PathBuf>,
    preview_scrub: Option<PreviewScrub>,
    preview_playhead_bits: Arc<AtomicU32>,
    hovered_tag_chip: Option<TagChipTarget>,
    hovered_tag_key: Option<String>,
    hovered_format_chip: Option<PathBuf>,
    hovered_delete_row: Option<PathBuf>,
    pending_delete: Option<PendingDelete>,
    pending_context_menu_delete: Option<DeleteTarget>,
    pending_rename: Option<PendingRename>,
    pending_context_menu_rename: Option<RenameTarget>,
    pending_context_menu_tag_rename: Option<TagChipTarget>,
    row_context_menu_open: bool,
    pending_drag: Option<PendingFileDrag>,
    active_file_drag: Option<ActiveFileDrag>,
    native_drag_session: NativeDragSession,
    selected: BTreeSet<PathBuf>,
    selection_anchor: Option<PathBuf>,
    selected_rows_actions_cache: Option<SelectedRowsActionsCache>,
    hidden_tag_keys: BTreeSet<String>,
    column_visibility_menu_position: Option<Point<Pixels>>,
    hovered_column_visibility: Option<ColumnVisibilityHover>,
    row_scroll_handle: VirtualListScrollHandle,
    row_sizes: Rc<Vec<Size<Pixels>>>,
    tag_width_cache: Option<TagWidthCache>,
    folder_prompt_active: bool,
}

impl FileTable {
    fn store_preview_playhead(bits: &AtomicU32, ratio: Option<f32>) {
        bits.store(
            ratio.map_or(u32::MAX, |ratio| ratio.clamp(0., 1.).to_bits()),
            Ordering::Relaxed,
        );
    }

    fn load_preview_playhead(bits: &AtomicU32) -> Option<f32> {
        let bits = bits.load(Ordering::Relaxed);
        (bits != u32::MAX).then(|| f32::from_bits(bits))
    }

    fn editing_cache_key(editing: Option<&TagEdit>) -> Option<TagEditCacheKey> {
        match editing {
            Some(TagEdit::Add { path, key }) => Some(TagEditCacheKey {
                path: path.clone(),
                key: key.clone(),
            }),
            Some(TagEdit::Rename { .. }) | None => None,
        }
    }

    fn editing_is_add(editing: Option<&TagEdit>, path: &Path, key: &str) -> bool {
        matches!(
            editing,
            Some(TagEdit::Add {
                path: editing_path,
                key: editing_key
            }) if editing_path == path && editing_key == key
        )
    }

    fn editing_is_rename_value(
        editing: Option<&TagEdit>,
        path: &Path,
        key: &str,
        value: &str,
    ) -> bool {
        matches!(
            editing,
            Some(TagEdit::Rename {
                path: editing_path,
                key: editing_key,
                old_value
            }) if editing_path == path && editing_key == key && old_value == value
        )
    }

    fn tag_values_width(values: &[String]) -> f32 {
        values
            .iter()
            .map(|value| value.chars().count() as f32 * TAG_TEXT_WIDTH + TAG_CHIP_X_PADDING_WIDTH)
            .sum::<f32>()
            + values.len().saturating_sub(1) as f32 * TAG_GAP_WIDTH
    }

    fn tag_header_width(window: &mut Window, key: &str) -> f32 {
        let text_style = window.text_style();
        let font_size = text_style.font_size.to_pixels(window.rem_size());
        let shaped = window.text_system().shape_line(
            key.into(),
            font_size,
            &[text_style.to_run(key.len())],
            None,
        );

        shaped.width.as_f32() + TAG_CELL_LEFT_PADDING_WIDTH
    }

    fn tag_column_width(
        window: &mut Window,
        state: &crate::model::CategoryState,
        key: &str,
        editing: Option<&TagEdit>,
    ) -> Pixels {
        let header_width = Self::tag_header_width(window, key);
        let mut width = header_width;

        for record in &state.results {
            let value_width = record
                .tags
                .get(key)
                .map_or(0., |values| Self::tag_values_width(values));
            let gap_width = if value_width > 0. { TAG_GAP_WIDTH } else { 0. };
            let action_width = if Self::editing_is_add(editing, &record.path, key) {
                TAG_EDITOR_WIDTH
            } else if record.is_convertible() {
                0.
            } else {
                TAG_ADD_BUTTON_WIDTH
            };
            let row_width = value_width + gap_width + action_width;
            width = width.max(row_width + TAG_CELL_LEFT_PADDING_WIDTH);
        }

        px(width.max(TAG_COLUMN_MIN_WIDTH))
    }

    pub fn new(library: Entity<Library>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        cx.subscribe_in(
            &library,
            window,
            |this, library, event: &LibraryEvent, window, cx| match event {
                LibraryEvent::TagEdited { path } => {
                    this.tag_width_cache = None;
                    this.hovered_tag_chip = None;
                    this.hovered_tag_key = None;
                    if this.hovered_row.as_ref() == Some(path) {
                        this.hovered_delete_row = Some(path.clone());
                    }
                    cx.notify();
                }
                LibraryEvent::PreviewAdvanced => {
                    let ratio = this.preview_active_row.as_deref().and_then(|path| {
                        this.preview_scrub
                            .as_ref()
                            .filter(|scrub| scrub.path == path)
                            .map(|scrub| scrub.ratio)
                            .or_else(|| library.read(cx).preview_playhead_ratio_for_path(path))
                    });
                    Self::store_preview_playhead(&this.preview_playhead_bits, ratio);
                    window.refresh();
                }
            },
        )
        .detach();

        let tag_input = cx.new(|cx| InputState::new(window, cx));
        let tag_key_input = cx.new(|cx| InputState::new(window, cx));
        let rename_input = cx.new(|cx| InputState::new(window, cx));

        cx.subscribe_in(
            &tag_input,
            window,
            |this, state, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let value = state.read(cx).value().to_string();
                    this.commit_tag_edit(value, window, cx);
                }
                InputEvent::Blur => this.cancel_tag(window, cx),
                _ => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &tag_key_input,
            window,
            |this, state, event: &InputEvent, window, cx| match event {
                InputEvent::PressEnter { .. } => {
                    let key = state.read(cx).value().to_string();
                    this.commit_tag_key(key, window, cx);
                }
                InputEvent::Blur => {
                    this.cancel_tag_key(window, cx);
                }
                _ => {}
            },
        )
        .detach();
        cx.subscribe_in(
            &rename_input,
            window,
            |this, state, event: &InputEvent, window, cx| {
                if let InputEvent::PressEnter { .. } = event {
                    let value = state.read(cx).value().to_string();
                    this.confirm_rename_with_value(value, window, cx);
                }
            },
        )
        .detach();

        let hidden_tag_keys = library.read(cx).hidden_tag_column_keys();

        Self {
            library,
            tag_input,
            tag_key_input,
            rename_input,
            editing: None,
            creating_tag_key: false,
            focus_handle: cx.focus_handle(),
            alt_down: false,
            cmd_down: false,
            hovered_row: None,
            preview_active_row: None,
            preview_scrub: None,
            preview_playhead_bits: Arc::new(AtomicU32::new(u32::MAX)),
            hovered_tag_chip: None,
            hovered_tag_key: None,
            hovered_format_chip: None,
            hovered_delete_row: None,
            pending_delete: None,
            pending_context_menu_delete: None,
            pending_rename: None,
            pending_context_menu_rename: None,
            pending_context_menu_tag_rename: None,
            row_context_menu_open: false,
            pending_drag: None,
            active_file_drag: None,
            native_drag_session: NativeDragSession::default(),
            selected: BTreeSet::new(),
            selection_anchor: None,
            selected_rows_actions_cache: None,
            hidden_tag_keys,
            column_visibility_menu_position: None,
            hovered_column_visibility: None,
            row_scroll_handle: VirtualListScrollHandle::new(),
            row_sizes: Rc::new(Vec::new()),
            tag_width_cache: None,
            folder_prompt_active: false,
        }
    }

    fn choose_category_folder(&mut self, category: Category, cx: &mut Context<Self>) {
        if self.folder_prompt_active {
            return;
        }

        self.folder_prompt_active = true;
        cx.notify();

        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some(format!("Select {} folder", category.label()).into()),
        });
        let library = self.library.downgrade();

        cx.spawn(async move |this, cx: &mut AsyncApp| {
            let path = paths
                .await
                .ok()
                .and_then(|paths| paths.ok())
                .flatten()
                .and_then(|paths| paths.into_iter().next());

            this.update(cx, |this, cx| {
                this.folder_prompt_active = false;
                cx.notify();
            })
            .ok()?;

            let Some(path) = path else {
                return Some(());
            };

            library
                .update(cx, |lib, cx| {
                    let _ = lib.set_category_folder(category, path, cx);
                })
                .ok()?;
            Some(())
        })
        .detach();
    }

    fn table_columns(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Vec<String>, Vec<Pixels>, usize) {
        let editing = Self::editing_cache_key(self.editing.as_ref());
        let (revision, keys, row_count, widths) = {
            let library = self.library.read(cx);
            let revision = library.table_revision();
            let state = library.active_state();
            let keys: Vec<String> = state
                .schema
                .keys()
                .filter(|key| !self.hidden_tag_keys.contains(*key))
                .cloned()
                .collect();
            let row_count = state.results.len();

            if let Some(cache) = self.tag_width_cache.as_ref()
                && cache.revision == revision
                && cache.keys == keys
                && cache.editing == editing
                && cache.row_count == row_count
            {
                return (cache.keys.clone(), cache.widths.clone(), cache.row_count);
            }

            let width_start = crate::perf::start();
            let widths: Vec<Pixels> = keys
                .iter()
                .map(|key| Self::tag_column_width(window, state, key, self.editing.as_ref()))
                .collect();
            crate::perf::finish("table.widths", width_start, || {
                format!(
                    "rows={} keys={} cached=false",
                    state.results.len(),
                    keys.len()
                )
            });

            (revision, keys, row_count, widths)
        };

        self.tag_width_cache = Some(TagWidthCache {
            revision,
            keys: keys.clone(),
            editing,
            row_count,
            widths: widths.clone(),
        });

        (keys, widths, row_count)
    }

    fn tag_column_keys(&mut self, cx: &mut Context<Self>) -> Vec<String> {
        self.library
            .read(cx)
            .active_state()
            .schema
            .keys()
            .cloned()
            .collect()
    }

    fn persist_hidden_tag_columns(&self, cx: &mut Context<Self>) {
        let hidden = self.hidden_tag_keys.clone();
        self.library.update(cx, |lib, cx| {
            lib.set_hidden_tag_column_keys(hidden, cx);
        });
    }

    fn toggle_tag_column(&mut self, key: &str, cx: &mut Context<Self>) {
        if !self.hidden_tag_keys.remove(key) {
            self.hidden_tag_keys.insert(key.to_string());
        }
        self.tag_width_cache = None;
        self.persist_hidden_tag_columns(cx);
        cx.notify();
    }

    fn show_all_tag_columns(&mut self, cx: &mut Context<Self>) {
        if !self.hidden_tag_keys.is_empty() {
            self.hidden_tag_keys.clear();
            self.tag_width_cache = None;
            self.persist_hidden_tag_columns(cx);
            cx.notify();
        }
    }

    fn hide_all_tag_columns(&mut self, keys: &[String], cx: &mut Context<Self>) {
        self.hidden_tag_keys = keys.iter().cloned().collect();
        self.tag_width_cache = None;
        self.persist_hidden_tag_columns(cx);
        cx.notify();
    }

    fn open_column_visibility_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.focus_handle.focus(window, cx);
        self.column_visibility_menu_position = Some(position);
        self.hovered_column_visibility = None;
        cx.notify();
    }

    fn close_column_visibility_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.column_visibility_menu_position.take().is_some()
            || self.hovered_column_visibility.take().is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    pub(crate) fn cancel_column_visibility_menu(&mut self, cx: &mut Context<Self>) -> bool {
        self.close_column_visibility_menu(cx)
    }

    fn set_column_visibility_hovered(
        &mut self,
        target: ColumnVisibilityHover,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            if self.hovered_column_visibility.as_ref() != Some(&target) {
                self.hovered_column_visibility = Some(target);
                cx.notify();
            }
        } else if self.hovered_column_visibility.as_ref() == Some(&target) {
            self.hovered_column_visibility = None;
            cx.notify();
        }
    }

    fn column_visibility_row(label: impl Into<SharedString>, checked: bool) -> gpui::Div {
        div()
            .h_flex()
            .h(px(26.))
            .w_full()
            .items_center()
            .gap_2()
            .px_2()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .child(
                div()
                    .size_4()
                    .h_flex()
                    .items_center()
                    .justify_center()
                    .when(checked, |el| el.child(Icon::new(IconName::Check).small())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .child(label.into()),
            )
    }

    fn column_visibility_action_row(label: impl Into<SharedString>) -> gpui::Div {
        div()
            .h_flex()
            .h(px(26.))
            .w_full()
            .items_center()
            .px_2()
            .rounded_md()
            .text_sm()
            .cursor_pointer()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .whitespace_nowrap()
                    .child(label.into()),
            )
    }

    fn render_column_visibility_menu(
        &self,
        keys: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let position = self.column_visibility_menu_position?;
        let window_size = window.bounds().size;
        let all_visible = keys
            .iter()
            .all(|key| !self.hidden_tag_keys.contains(key.as_str()));
        let toggle_label = if all_visible { "Hide All" } else { "Show All" };

        let mut rows = div().v_flex().gap_y_0p5();
        if keys.is_empty() {
            rows = rows.child(
                div()
                    .h_flex()
                    .h(px(26.))
                    .w_full()
                    .items_center()
                    .px_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No tag columns"),
            );
        } else {
            rows = rows.child(
                div()
                    .h_flex()
                    .h(px(26.))
                    .w_full()
                    .items_center()
                    .px_2()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("Tag Columns"),
            );

            for key in &keys {
                let checked = !self.hidden_tag_keys.contains(key.as_str());
                let item_key = key.clone();
                let hover_target = ColumnVisibilityHover::Key(key.clone());
                let row_hovered = self.hovered_column_visibility.as_ref() == Some(&hover_target);
                rows = rows.child(
                    Self::column_visibility_row(SharedString::from(key.clone()), checked)
                        .id(SharedString::from(format!("column-visibility-row:{key}")))
                        .when(row_hovered, |el| {
                            el.bg(cx.theme().accent)
                                .text_color(cx.theme().accent_foreground)
                        })
                        .on_hover(cx.listener({
                            let hover_target = hover_target.clone();
                            move |this, hovered: &bool, _, cx| {
                                this.set_column_visibility_hovered(
                                    hover_target.clone(),
                                    *hovered,
                                    cx,
                                );
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                window.prevent_default();
                                this.toggle_tag_column(&item_key, cx);
                                window.refresh();
                                cx.stop_propagation();
                            }),
                        )
                        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
                );
            }

            let toggle_hovered =
                self.hovered_column_visibility.as_ref() == Some(&ColumnVisibilityHover::ToggleAll);

            rows = rows
                .child(
                    div()
                        .h_auto()
                        .p_0()
                        .my_0p5()
                        .mx_neg_1()
                        .border_b(px(2.))
                        .border_color(cx.theme().border),
                )
                .child(
                    Self::column_visibility_action_row(toggle_label)
                        .id("column-visibility-toggle-all")
                        .when(toggle_hovered, |el| {
                            el.bg(cx.theme().accent)
                                .text_color(cx.theme().accent_foreground)
                        })
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            this.set_column_visibility_hovered(
                                ColumnVisibilityHover::ToggleAll,
                                *hovered,
                                cx,
                            );
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener({
                                let keys = keys.clone();
                                move |this, _, window, cx| {
                                    window.prevent_default();
                                    if all_visible {
                                        this.hide_all_tag_columns(&keys, cx);
                                    } else {
                                        this.show_all_tag_columns(cx);
                                    }
                                    window.refresh();
                                    cx.stop_propagation();
                                }
                            }),
                        )
                        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
                );
        }

        Some(
            deferred(
                anchored().child(
                    div()
                        .w(window_size.width)
                        .h(window_size.height)
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.close_column_visibility_menu(cx);
                            }),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _, _, cx| {
                                this.close_column_visibility_menu(cx);
                            }),
                        )
                        .child(
                            anchored()
                                .position(position)
                                .snap_to_window_with_margin(px(8.))
                                .anchor(Anchor::TopLeft)
                                .child(
                                    div()
                                        .id("column-visibility-menu")
                                        .popover_style(cx)
                                        .min_w(px(140.))
                                        .max_w(px(260.))
                                        .p_1()
                                        .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                            cx.stop_propagation();
                                        })
                                        .on_mouse_down(MouseButton::Right, |_, _, cx| {
                                            cx.stop_propagation();
                                        })
                                        .child(rows),
                                ),
                        ),
                ),
            )
            .with_priority(1)
            .into_any_element(),
        )
    }

    fn row_sizes(&mut self, len: usize) -> Rc<Vec<Size<Pixels>>> {
        if self.row_sizes.len() != len {
            self.row_sizes = Rc::new((0..len).map(|_| size(px(0.), ROW_HEIGHT)).collect());
        }
        self.row_sizes.clone()
    }

    pub(crate) fn set_alt_down(&mut self, alt_down: bool, cx: &mut Context<Self>) {
        if self.alt_down != alt_down {
            self.alt_down = alt_down;
            cx.notify();
        }
    }

    pub(crate) fn set_cmd_down(&mut self, cmd_down: bool, cx: &mut Context<Self>) {
        if self.cmd_down == cmd_down {
            return;
        }
        if self.cmd_down && !cmd_down && self.preview_active_row.is_some() {
            self.library.update(cx, |lib, cx| {
                lib.stop_preview(cx);
            });
        }
        if !cmd_down {
            self.preview_scrub = None;
        }
        self.cmd_down = cmd_down;
        self.update_preview_active_row(cx);
    }

    fn row_edit_active(&self) -> bool {
        self.editing.is_some() || self.pending_rename.is_some()
    }

    fn preview_path_for_state(
        cmd_down: bool,
        hovered_row: Option<&PathBuf>,
        row_edit_active: bool,
    ) -> Option<PathBuf> {
        (cmd_down && !row_edit_active)
            .then(|| hovered_row.cloned())
            .flatten()
    }

    fn update_preview_active_row(&mut self, cx: &mut Context<Self>) {
        let next = Self::preview_path_for_state(
            self.cmd_down,
            self.hovered_row.as_ref(),
            self.row_edit_active(),
        );
        if self.preview_active_row != next {
            if self.preview_scrub.as_ref().map(|scrub| &scrub.path) != next.as_ref() {
                self.preview_scrub = None;
            }
            self.preview_active_row = next;
            if let Some(path) = self.preview_active_row.clone() {
                self.maybe_start_priority_waveform_cache(&path, cx);
            }
            cx.notify();
        }
    }

    pub(crate) fn active_preview_path(&self) -> Option<&Path> {
        self.preview_active_row.as_deref()
    }

    pub(crate) fn play_cmd_hovered_preview_from_start(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(path) = self.active_preview_path().map(Path::to_path_buf) else {
            return false;
        };
        self.library
            .update(cx, |lib, cx| lib.play_preview_from_start(path, cx))
    }

    fn play_preview_from_ratio(
        &mut self,
        path: PathBuf,
        ratio: f32,
        cx: &mut Context<Self>,
    ) -> bool {
        self.library
            .update(cx, |lib, cx| lib.play_preview_from_ratio(path, ratio, cx))
    }

    fn begin_preview_scrub(&mut self, path: PathBuf, ratio: f32, cx: &mut Context<Self>) {
        self.cancel_unstarted_file_drag(cx);
        let scrub = PreviewScrub::new(path, ratio);
        if self.preview_scrub.as_ref() != Some(&scrub) {
            self.preview_scrub = Some(scrub);
            cx.notify();
        }
    }

    fn continue_preview_scrub(&mut self, path: &Path, ratio: f32, cx: &mut Context<Self>) {
        let Some(scrub) = self.preview_scrub.as_mut() else {
            return;
        };
        let previous_ratio = scrub.ratio;
        if scrub.update(path, ratio) && scrub.ratio != previous_ratio {
            cx.notify();
        }
    }

    fn end_preview_scrub(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(ratio) = PreviewScrub::take_ratio_for_path(&mut self.preview_scrub, path) else {
            return;
        };
        cx.notify();
        self.play_preview_from_ratio(path.to_path_buf(), ratio, cx);
    }

    fn maybe_start_priority_waveform_cache(&mut self, path: &Path, cx: &mut Context<Self>) {
        let waveform_missing = {
            let library = self.library.read(cx);
            library
                .active_state()
                .results
                .iter()
                .find(|record| record.path.as_path() == path)
                .is_none_or(|record| record.primary_waveform().is_none())
        };
        if waveform_missing {
            self.library.update(cx, |lib, cx| {
                lib.maybe_start_priority_waveform_cache(path.to_path_buf(), cx);
            });
        }
    }

    pub(crate) fn tag_editor_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.tag_input.read(cx).focus_handle(cx).is_focused(window)
            || self
                .tag_key_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
    }

    pub(crate) fn rename_input_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.rename_input
            .read(cx)
            .focus_handle(cx)
            .is_focused(window)
    }

    pub(crate) fn rename_input(&self) -> Entity<InputState> {
        self.rename_input.clone()
    }

    fn start_editing(
        &mut self,
        path: PathBuf,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing = Some(TagEdit::Add { path, key });
        self.tag_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn start_creating_tag_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.creating_tag_key = true;
        self.tag_key_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn commit_tag_key(&mut self, raw: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.creating_tag_key {
            return;
        }
        self.creating_tag_key = false;
        let key = raw.trim().to_string();
        if !key.is_empty() {
            debug_table_interaction(|| format!("tag key add key={key}"));
            self.library.update(cx, |lib, cx| {
                lib.add_tag_key(&key, cx);
            });
        }
        self.focus_handle.focus(window, cx);
        window.refresh();
        cx.notify();
    }

    fn cancel_tag_key(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.creating_tag_key {
            self.creating_tag_key = false;
            self.focus_handle.focus(window, cx);
            cx.notify();
            return true;
        }
        false
    }

    fn start_renaming_tag(
        &mut self,
        target: TagChipTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.editing = Some(TagEdit::Rename {
            path: target.path,
            key: target.key,
            old_value: target.value,
        });
        self.tag_input.update(cx, |state, cx| {
            state.set_value("", window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    fn commit_tag_edit(&mut self, raw: String, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editing) = self.editing.take() else {
            return;
        };
        let value = raw.trim();
        match editing {
            TagEdit::Add { path, key } => {
                if !value.is_empty() {
                    let paths = {
                        let library = self.library.read(cx);
                        self.selected_tag_paths(&library.active_state().results, &path)
                    };
                    let path_count = paths.len();
                    debug_table_interaction(|| {
                        format!("tag add key={key} value={value} paths={path_count}")
                    });
                    self.library.update(cx, |lib, cx| {
                        for path in paths {
                            lib.add_tag(path, &key, value, cx);
                        }
                    });
                }
            }
            TagEdit::Rename {
                path,
                key,
                old_value,
            } => {
                if !value.is_empty() && value != old_value {
                    debug_table_interaction(|| {
                        format!("tag rename key={key} old={old_value} new={value}")
                    });
                    self.library.update(cx, |lib, cx| {
                        lib.rename_tag(path, &key, &old_value, value, cx);
                    });
                }
            }
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

    pub(crate) fn cancel_tag_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.cancel_tag_key(window, cx) {
            true
        } else if self.editing.is_some() {
            self.cancel_tag(window, cx);
            true
        } else {
            false
        }
    }

    fn start_pending_row_drag(
        &mut self,
        record_name: String,
        record_path: &Path,
        drag: InternalFileDrag,
        cx: &mut Context<Self>,
    ) {
        let start = crate::perf::start();
        self.cancel_unstarted_file_drag(cx);
        if self.native_drag_session.is_active() {
            return;
        }
        let state = self.library.read(cx);
        let paths = self.selected_priority_paths(&state.active_state().results, record_path);
        let path_count = paths.len();
        drag.replace(record_name, paths);
        self.pending_drag = Some(PendingFileDrag);
        crate::perf::finish("table.pending_drag", start, || {
            format!("paths={path_count}")
        });
        debug_table_interaction(|| {
            format!(
                "pending row drag path={} paths={path_count}",
                record_path.display()
            )
        });
    }

    fn start_pending_format_drag(
        &mut self,
        record: &FileRecord,
        extension: String,
        drag: InternalFileDrag,
        cx: &mut Context<Self>,
    ) {
        let start = crate::perf::start();
        self.cancel_unstarted_file_drag(cx);
        if self.native_drag_session.is_active() {
            return;
        }
        let state = self.library.read(cx);
        let paths = self.selected_paths_for_extension(
            &state.active_state().results,
            &record.path,
            &extension,
        );
        let path_count = paths.len();
        drag.replace(format!(".{extension}"), paths);
        self.pending_drag = Some(PendingFileDrag);
        crate::perf::finish("table.pending_drag", start, || {
            format!("paths={path_count}")
        });
        debug_table_interaction(|| {
            format!(
                "pending format drag stem={} extension={extension} paths={path_count}",
                record.stem
            )
        });
    }

    fn clear_pending_drag(&mut self) {
        self.pending_drag = None;
    }

    fn cancel_unstarted_file_drag(&mut self, cx: &mut Context<Self>) {
        let had_pending_drag = self.pending_drag.take().is_some();
        let had_active_drag = if self.native_drag_session.is_active() {
            false
        } else {
            self.active_file_drag.take().is_some()
        };
        if had_pending_drag || had_active_drag {
            self.library
                .update(cx, |library, cx| library.clear_internal_file_drag(cx));
        }
    }

    fn select_row(
        &mut self,
        path: PathBuf,
        extend: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let start = crate::perf::start();
        let (row_count, extended_selection) = {
            let library = self.library.read(cx);
            let results = &library.active_state().results;
            let selection = if extend {
                self.selection_anchor.as_ref().and_then(|anchor| {
                    let anchor_ix = results.iter().position(|record| &record.path == anchor)?;
                    let path_ix = results.iter().position(|record| record.path == path)?;
                    let (start, end) = if anchor_ix <= path_ix {
                        (anchor_ix, path_ix)
                    } else {
                        (path_ix, anchor_ix)
                    };
                    Some(
                        results[start..=end]
                            .iter()
                            .map(|record| record.path.clone())
                            .collect(),
                    )
                })
            } else {
                None
            };
            (results.len(), selection)
        };

        if let Some(selection) = extended_selection {
            self.selected = selection;
        } else {
            self.selected.clear();
            self.selected.insert(path.clone());
            self.selection_anchor = Some(path);
        }
        self.invalidate_selected_rows_actions();

        self.focus_handle.focus(window, cx);
        cx.notify();
        crate::perf::finish("table.select_row", start, || {
            format!("rows={row_count} selected={}", self.selected.len())
        });
    }

    fn selected_priority_paths(&self, records: &[FileRecord], path: &Path) -> Vec<PathBuf> {
        if self.selected.len() > 1 && self.selected.contains(path) {
            records
                .iter()
                .filter(|record| self.selected.contains(record.path.as_path()))
                .map(|record| record.path.clone())
                .collect()
        } else {
            records
                .iter()
                .find(|record| record.path.as_path() == path)
                .map(|record| vec![record.path.clone()])
                .unwrap_or_default()
        }
    }

    fn selected_paths_for_extension(
        &self,
        records: &[FileRecord],
        path: &Path,
        extension: &str,
    ) -> Vec<PathBuf> {
        if self.selected.len() > 1 && self.selected.contains(path) {
            records
                .iter()
                .filter(|record| self.selected.contains(record.path.as_path()))
                .filter_map(|record| record.variant_for_extension(extension))
                .map(|variant| variant.path.clone())
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

    fn selected_tag_paths(&self, records: &[FileRecord], path: &Path) -> Vec<PathBuf> {
        if self.selected.len() > 1 && self.selected.contains(path) {
            let paths: Vec<PathBuf> = records
                .iter()
                .filter(|record| self.selected.contains(record.path.as_path()))
                .map(|record| record.path.clone())
                .collect();
            if !paths.is_empty() {
                return paths;
            }
        }

        vec![path.to_path_buf()]
    }

    fn record_conversion_actions(record: &FileRecord) -> Arc<Vec<ConversionAction>> {
        Arc::new(
            record
                .conversion_targets()
                .into_iter()
                .map(|target| ConversionAction {
                    target,
                    sources: vec![record.path.clone()],
                })
                .collect(),
        )
    }

    fn record_delete_target(record: &FileRecord) -> Arc<DeleteTarget> {
        let mut seen = BTreeSet::new();
        let paths = record
            .variants
            .iter()
            .map(|variant| variant.path.clone())
            .filter(|path| seen.insert(path.clone()))
            .collect();

        Arc::new(DeleteTarget {
            kind: DeleteKind::Rows,
            row_count: 1,
            paths,
        })
    }

    fn record_rename_target(record: &FileRecord) -> Arc<RenameTarget> {
        Arc::new(RenameTarget {
            kind: RenameKind::Rows {
                records: vec![row_rename_target(record)],
            },
        })
    }

    fn selected_rows_actions(records: &[&FileRecord]) -> Arc<SelectedRowsActions> {
        let row_drag_paths = Arc::new(records.iter().map(|record| record.path.clone()).collect());
        let mut seen = BTreeSet::new();
        let delete_paths = records
            .iter()
            .flat_map(|record| record.variants.iter().map(|variant| variant.path.clone()))
            .filter(|path| seen.insert(path.clone()))
            .collect();
        let delete_target = Arc::new(DeleteTarget {
            kind: DeleteKind::Rows,
            row_count: records.len(),
            paths: delete_paths,
        });
        let rename_target = Arc::new(RenameTarget {
            kind: RenameKind::Rows {
                records: records
                    .iter()
                    .map(|record| row_rename_target(record))
                    .collect(),
            },
        });
        let conversion_actions = Arc::new(
            AudioFormat::ALL
                .into_iter()
                .filter_map(|target| {
                    let sources: Vec<_> = records
                        .iter()
                        .filter(|record| !record.has_extension(target.extension()))
                        .map(|record| record.path.clone())
                        .collect();
                    (!sources.is_empty()).then_some(ConversionAction { target, sources })
                })
                .collect(),
        );
        let extensions: BTreeSet<String> = records
            .iter()
            .flat_map(|record| {
                record
                    .variants
                    .iter()
                    .map(|variant| variant.extension.to_ascii_lowercase())
            })
            .collect();
        let format_drag_paths = extensions
            .into_iter()
            .map(|extension| {
                let paths = Arc::new(
                    records
                        .iter()
                        .filter_map(|record| record.variant_for_extension(&extension))
                        .map(|variant| variant.path.clone())
                        .collect(),
                );
                (extension, paths)
            })
            .collect();

        Arc::new(SelectedRowsActions {
            delete_target,
            rename_target,
            conversion_actions,
            row_drag_paths,
            format_drag_paths,
        })
    }

    fn invalidate_selected_rows_actions(&mut self) {
        self.selected_rows_actions_cache = None;
    }

    fn cached_selected_rows_actions(
        &mut self,
        cx: &mut Context<Self>,
    ) -> Option<Arc<SelectedRowsActions>> {
        if self.selected.len() <= 1 {
            self.selected_rows_actions_cache = None;
            return None;
        }

        let table_revision = self.library.read(cx).table_revision();
        if let Some(cache) = self.selected_rows_actions_cache.as_ref()
            && cache.table_revision == table_revision
        {
            return Some(cache.actions.clone());
        }

        let actions = {
            let library = self.library.read(cx);
            let selected_records: Vec<_> = library
                .active_state()
                .results
                .iter()
                .filter(|record| self.selected.contains(record.path.as_path()))
                .collect();
            Self::selected_rows_actions(&selected_records)
        };
        self.selected_rows_actions_cache = Some(SelectedRowsActionsCache {
            table_revision,
            actions: actions.clone(),
        });
        Some(actions)
    }

    fn format_delete_target(&self, record: &FileRecord, extension: &str) -> DeleteTarget {
        let paths = record
            .variant_for_extension(extension)
            .map(|variant| vec![variant.path.clone()])
            .unwrap_or_default();

        DeleteTarget {
            kind: DeleteKind::Format,
            row_count: 1,
            paths,
        }
    }

    fn tag_key_delete_target(key: &str) -> DeleteTarget {
        DeleteTarget {
            kind: DeleteKind::TagKey {
                key: key.to_string(),
            },
            row_count: 0,
            paths: Vec::new(),
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
            kind: DeleteKind::Rows,
            row_count: selected_records.len(),
            paths,
        })
    }

    fn selected_rename_target(&self, cx: &mut Context<Self>) -> Option<RenameTarget> {
        if self.selected.is_empty() {
            return None;
        }

        let state = self.library.read(cx);
        let records: Vec<RowRenameTarget> = state
            .active_state()
            .results
            .iter()
            .filter(|record| self.selected.contains(record.path.as_path()))
            .map(row_rename_target)
            .collect();

        (!records.is_empty()).then_some(RenameTarget {
            kind: RenameKind::Rows { records },
        })
    }

    fn confirm_delete_target(
        &mut self,
        target: DeleteTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if target.paths.is_empty() && !matches!(target.kind, DeleteKind::TagKey { .. }) {
            return;
        }

        self.pending_delete = Some(PendingDelete { target });
        self.focus_handle.focus(window, cx);
        cx.notify();
    }

    fn confirm_tag_key_delete(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) {
        debug_table_interaction(|| format!("tag key delete requested key={key}"));
        self.confirm_delete_target(Self::tag_key_delete_target(key), window, cx);
    }

    fn confirm_selected_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(target) = self.selected_delete_target(cx) else {
            return false;
        };
        self.confirm_delete_target(target, window, cx);
        true
    }

    fn request_context_menu_delete(&mut self, target: DeleteTarget) {
        self.pending_context_menu_delete = Some(target);
    }

    fn confirm_pending_context_menu_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.pending_context_menu_delete.take() else {
            return;
        };

        self.confirm_delete_target(target, window, cx);
    }

    fn start_rename_target(
        &mut self,
        target: RenameTarget,
        initial_value: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.pending_rename = Some(PendingRename { target });
        self.rename_input.update(cx, |state, cx| {
            state.set_value(initial_value, window, cx);
            state.focus(window, cx);
        });
        cx.notify();
    }

    pub(crate) fn start_selected_rename(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(target) = self.selected_rename_target(cx) else {
            return false;
        };
        let initial_value = target.default_value();
        self.start_rename_target(target, initial_value, window, cx);
        true
    }

    fn request_context_menu_rename(&mut self, target: RenameTarget) {
        self.pending_context_menu_rename = Some(target);
    }

    fn request_context_menu_tag_rename(&mut self, target: TagChipTarget) {
        self.pending_context_menu_tag_rename = Some(target);
    }

    fn confirm_pending_context_menu_tag_rename(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(target) = self.pending_context_menu_tag_rename.take() else {
            return;
        };
        self.start_renaming_tag(target, window, cx);
    }

    fn confirm_pending_context_menu_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(target) = self.pending_context_menu_rename.take() else {
            return;
        };
        let initial_value = target.default_value();
        self.start_rename_target(target, initial_value, window, cx);
    }

    pub fn pending_rename_details(&self) -> Option<PendingRenameDetails> {
        self.pending_rename
            .as_ref()
            .map(|pending| pending.target.details())
    }

    pub fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        if self.pending_rename.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
            true
        } else {
            false
        }
    }

    pub fn confirm_pending_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let value = self.rename_input.read(cx).value().to_string();
        self.confirm_rename_with_value(value, window, cx)
    }

    fn confirm_rename_with_value(
        &mut self,
        raw: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(pending) = self.pending_rename.take() else {
            return false;
        };
        let value = raw.trim().to_string();
        if value.is_empty() {
            self.focus_handle.focus(window, cx);
            cx.notify();
            return true;
        }

        match pending.target.kind {
            RenameKind::Rows { records } => {
                let rename_records = records
                    .into_iter()
                    .map(|record| RenameRecord {
                        stem: record.stem,
                        paths: record.paths,
                    })
                    .collect::<Vec<_>>();
                self.library.update(cx, |lib, cx| {
                    lib.rename_records(rename_records, &value, cx);
                });
                self.selected.clear();
                self.selection_anchor = None;
                self.invalidate_selected_rows_actions();
            }
            RenameKind::TagAll { key, old_value } => {
                if value != old_value {
                    self.library.update(cx, |lib, cx| {
                        lib.rename_tag_value(&key, &old_value, &value, cx);
                    });
                }
            }
            RenameKind::TagKey { old_key } => {
                if value != old_key {
                    self.library.update(cx, |lib, cx| {
                        lib.rename_tag_key(&old_key, &value, cx);
                    });
                }
            }
        }
        self.focus_handle.focus(window, cx);
        cx.notify();
        true
    }

    fn remove_tag_from_target(
        &mut self,
        path: &Path,
        key: &str,
        value: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let paths = {
            let library = self.library.read(cx);
            self.selected_tag_paths(&library.active_state().results, path)
        };
        let path_count = paths.len();
        debug_table_interaction(|| {
            format!("tag remove key={key} value={value} paths={path_count}")
        });
        self.library.update(cx, |lib, cx| {
            for path in paths {
                lib.remove_tag(path, key, value, cx);
            }
        });
        window.refresh();
        cx.notify();
    }

    pub fn pending_delete_counts(&self) -> Option<PendingDeleteCounts> {
        let pending = self.pending_delete.as_ref()?;
        let kind = match &pending.target.kind {
            DeleteKind::Rows => PendingDeleteKind::Rows,
            DeleteKind::Format => PendingDeleteKind::Format,
            DeleteKind::TagKey { .. } => PendingDeleteKind::TagKey,
        };
        Some(PendingDeleteCounts {
            kind,
            row_count: pending.target.row_count,
            file_count: pending.target.paths.len(),
        })
    }

    pub fn cancel_delete(&mut self, cx: &mut Context<Self>) -> bool {
        if self.pending_delete.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    pub fn has_visible_selection(&self, cx: &mut Context<Self>) -> bool {
        if self.selected.is_empty() {
            return false;
        }

        self.library
            .read(cx)
            .active_state()
            .results
            .iter()
            .any(|record| self.selected.contains(record.path.as_path()))
    }

    pub fn select_all_visible(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let paths: Vec<PathBuf> = self
            .library
            .read(cx)
            .active_state()
            .results
            .iter()
            .map(|record| record.path.clone())
            .collect();

        if paths.is_empty() {
            return false;
        }

        let selected: BTreeSet<PathBuf> = paths.iter().cloned().collect();
        if self.selected == selected {
            self.focus_handle.focus(window, cx);
            return true;
        }

        self.selection_anchor = paths.first().cloned();
        self.selected = selected;
        self.invalidate_selected_rows_actions();
        self.focus_handle.focus(window, cx);
        cx.notify();
        debug_table_interaction(|| format!("select all visible rows={}", self.selected.len()));
        true
    }

    pub(crate) fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        if self.selected.is_empty() && self.selection_anchor.is_none() {
            return false;
        }

        self.selected.clear();
        self.selection_anchor = None;
        self.invalidate_selected_rows_actions();
        cx.notify();
        true
    }

    pub fn confirm_pending_delete(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(pending) = self.pending_delete.take() else {
            return false;
        };

        let paths = match pending.target.kind {
            DeleteKind::TagKey { key } => {
                self.hovered_tag_key = None;
                self.library.update(cx, |lib, cx| {
                    lib.remove_tag_key(&key, cx);
                });
                cx.notify();
                return true;
            }
            DeleteKind::Rows | DeleteKind::Format => pending.target.paths,
        };
        self.selected.clear();
        self.selection_anchor = None;
        self.invalidate_selected_rows_actions();
        self.hovered_row = None;
        self.hovered_tag_chip = None;
        self.hovered_tag_key = None;
        self.hovered_format_chip = None;
        self.hovered_delete_row = None;
        self.library
            .update(cx, |lib, cx| lib.trash_files(paths, cx));
        cx.notify();
        true
    }

    fn set_row_hovered(&mut self, path: PathBuf, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.hovered_row.as_ref() != Some(&path) {
                self.hovered_row = Some(path.clone());
                self.hovered_delete_row = Some(path.clone());
                if self.cmd_down {
                    if self.preview_scrub.as_ref().map(|scrub| &scrub.path) != Some(&path) {
                        self.preview_scrub = None;
                    }
                    self.preview_active_row = Some(path.clone());
                    self.maybe_start_priority_waveform_cache(&path, cx);
                }
                cx.notify();
            }
        } else if self.hovered_row.as_ref() == Some(&path) {
            self.hovered_row = None;
            if self.hovered_delete_row.as_ref() == Some(&path) {
                self.hovered_delete_row = None;
            }
            if self.preview_active_row.as_ref() == Some(&path) {
                self.preview_active_row = None;
            }
            if self.preview_scrub.as_ref().map(|scrub| &scrub.path) == Some(&path) {
                self.preview_scrub = None;
            }
            cx.notify();
        }
    }

    fn set_tag_chip_hovered(
        &mut self,
        target: TagChipTarget,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            let changed = self.hovered_tag_chip.as_ref().is_none_or(|hovered| {
                hovered.path != target.path
                    || hovered.key != target.key
                    || hovered.value != target.value
            });
            if changed {
                self.hovered_tag_chip = Some(target);
                cx.notify();
            }
        } else if self.hovered_tag_chip.as_ref().is_some_and(|hovered| {
            hovered.path == target.path
                && hovered.key == target.key
                && hovered.value == target.value
        }) {
            self.hovered_tag_chip = None;
            cx.notify();
        }
    }

    fn set_tag_key_hovered(&mut self, key: String, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.hovered_tag_key.as_ref() != Some(&key) {
                self.hovered_tag_key = Some(key);
                cx.notify();
            }
        } else if self.hovered_tag_key.as_ref() == Some(&key) {
            self.hovered_tag_key = None;
            cx.notify();
        }
    }

    fn set_format_chip_hovered(&mut self, path: PathBuf, hovered: bool, cx: &mut Context<Self>) {
        if hovered {
            if self.hovered_format_chip.as_ref() != Some(&path) {
                self.hovered_format_chip = Some(path);
                cx.notify();
            }
        } else if self.hovered_format_chip.as_ref() == Some(&path) {
            self.hovered_format_chip = None;
            cx.notify();
        }
    }

    pub(crate) fn cancel_file_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let had_drag = self.pending_drag.is_some()
            || self.active_file_drag.is_some()
            || self.library.read(cx).internal_file_drag_active();
        if !had_drag {
            return false;
        }

        self.clear_pending_drag();
        self.active_file_drag = None;
        if !self.native_drag_session.is_active() {
            window.on_next_frame(|window, _| native_drag::cancel_gpui_drag(window));
        }
        self.library
            .update(cx, |lib, cx| lib.clear_internal_file_drag(cx));
        true
    }

    fn begin_internal_file_drag(
        &mut self,
        drag: &InternalFileDrag,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.native_drag_session.is_active() {
            return;
        }

        let data = drag.snapshot();
        if data.paths.is_empty() {
            self.cancel_unstarted_file_drag(cx);
            return;
        }
        self.pending_drag.take();
        self.active_file_drag = Some(ActiveFileDrag {
            label: data.label.clone(),
            paths: data.paths.as_ref().clone(),
        });
        self.library.update(cx, |lib, cx| {
            if data.paths.len() == 1 {
                lib.begin_internal_file_drag(data.paths[0].clone(), cx);
            } else {
                lib.begin_internal_file_drag_files(data.paths.as_ref().clone(), cx);
            }
        });
        debug_table_interaction(|| {
            format!(
                "internal drag started drag={} paths={}",
                data.label,
                data.paths.len()
            )
        });
        self.start_native_file_drag(window, cx);
    }

    fn finish_local_file_drag(&mut self, window: &mut Window, _cx: &mut Context<Self>) {
        self.clear_pending_drag();
        if self.native_drag_session.is_active() || self.active_file_drag.take().is_none() {
            return;
        }

        let library = self.library.clone();
        window.on_next_frame(move |_, cx| {
            library.update(cx, |lib, cx| lib.clear_internal_file_drag(cx));
        });
    }

    fn start_native_file_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.native_drag_session.is_active() || self.active_file_drag.is_none() {
            return;
        }

        let active = self
            .active_file_drag
            .take()
            .expect("active drag disappeared after presence check");
        if !self.native_drag_session.try_start() {
            self.active_file_drag = Some(active);
            return;
        }

        let drag_label = active.label;
        let drag_paths = active.paths;
        let drag_path_count = drag_paths.len();
        let native_drag_label = if drag_path_count > 1 {
            format!("{drag_label} · {drag_path_count} files")
        } else {
            drag_label.clone()
        };
        let (drag_finished_tx, mut drag_finished_rx) = mpsc::unbounded::<native_drag::DragEnd>();
        let window_bounds = window.bounds();
        let internal_drop_paths = drag_paths.clone();
        let library = self.library.clone();
        cx.spawn(async move |_, cx| {
            if let Some(end) = drag_finished_rx.next().await {
                library.update(cx, |lib, cx| {
                    if let Some(category) = category_for_native_drag_end(end, window_bounds) {
                        lib.import_files(category, internal_drop_paths, cx);
                    } else {
                        lib.clear_internal_file_drag(cx);
                    }
                });
            }
        })
        .detach();

        let library = self.library.clone();
        let native_drag_session = self.native_drag_session.clone();
        window.on_next_frame(move |window, cx| {
            if !library.read(cx).internal_file_drag_active() {
                native_drag_session.finish();
                native_drag::cancel_gpui_drag(window);
                return;
            }
            cx.stop_active_drag(window);
            let native_start = crate::perf::start();
            let finished_session = native_drag_session.clone();
            let result = native_drag::start_file_drag(
                drag_paths.clone(),
                native_drag_label,
                window,
                move |end| {
                    finished_session.finish();
                    let _ = drag_finished_tx.unbounded_send(end);
                },
            );
            crate::perf::finish("table.native_file_drag", native_start, || {
                format!("paths={} ok={}", drag_paths.len(), result.is_ok())
            });
            if let Err(error) = result {
                native_drag_session.finish();
                native_drag::cancel_gpui_drag(window);
                debug_table_interaction(|| format!("native drag rejected: {error}"));
                library.update(cx, |lib, cx| lib.clear_internal_file_drag(cx));
            } else {
                debug_table_interaction(|| {
                    format!("native drag started drag={drag_label} paths={drag_path_count}")
                });
            }
        });
    }
}
impl Focusable for FileTable {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

fn category_for_native_drag_end(
    end: native_drag::DragEnd,
    window_bounds: Bounds<Pixels>,
) -> Option<Category> {
    if !end.released {
        return None;
    }

    let local_x = end.screen_x as f32 - window_bounds.left().as_f32();
    let local_y = window_bounds.bottom().as_f32() - end.screen_y as f32;
    let left = TITLEBAR_LEFT_OFFSET.as_f32();
    let available_width = window_bounds.size.width.as_f32() - left;
    if local_x < left
        || local_y < 0.0
        || local_y > TITLEBAR_HEIGHT.as_f32()
        || available_width <= 0.0
    {
        return None;
    }

    let category_width = available_width / Category::ALL.len() as f32;
    let index = ((local_x - left) / category_width).floor() as usize;
    Category::ALL.get(index).copied()
}

impl RenameTarget {
    fn default_value(&self) -> String {
        match &self.kind {
            RenameKind::Rows { records } if records.len() == 1 => records[0].name.clone(),
            RenameKind::Rows { .. } => String::new(),
            RenameKind::TagAll { old_value, .. } => old_value.clone(),
            RenameKind::TagKey { old_key } => old_key.clone(),
        }
    }

    fn details(&self) -> PendingRenameDetails {
        match &self.kind {
            RenameKind::Rows { records } => {
                let file_count = records.iter().map(|record| record.paths.len()).sum();
                PendingRenameDetails {
                    kind: PendingRenameKind::Rows,
                    item_count: records.len(),
                    file_count,
                    current_name: (records.len() == 1).then(|| records[0].name.clone()),
                    bulk: records.len() > 1,
                }
            }
            RenameKind::TagAll { old_value, .. } => PendingRenameDetails {
                kind: PendingRenameKind::TagAll,
                item_count: 1,
                file_count: 0,
                current_name: Some(old_value.clone()),
                bulk: true,
            },
            RenameKind::TagKey { old_key } => PendingRenameDetails {
                kind: PendingRenameKind::TagKey,
                item_count: 1,
                file_count: 0,
                current_name: Some(old_key.clone()),
                bulk: false,
            },
        }
    }
}

fn row_rename_target(record: &FileRecord) -> RowRenameTarget {
    RowRenameTarget {
        name: record.name.clone(),
        stem: record.stem.clone(),
        paths: record
            .variants
            .iter()
            .map(|variant| variant.path.clone())
            .collect(),
    }
}

fn open_in_default_app(path: &Path) {
    if let Err(err) = open::that(path) {
        eprintln!("failed to open {}: {err}", path.display());
    }
}

fn unique_format_variants(record: &FileRecord) -> Vec<&crate::model::FileVariant> {
    let mut seen = BTreeSet::new();
    record
        .variants
        .iter()
        .filter(|variant| seen.insert(variant.extension.to_ascii_lowercase()))
        .collect()
}

fn debug_table_interaction(details: impl FnOnce() -> String) {
    crate::diagnostics::debug("table", details);
}
