mod native_drag;

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
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
    paths: Vec<PathBuf>,
}

impl InternalFileDrag {
    fn new(label: String, paths: Vec<PathBuf>) -> Self {
        Self {
            data: Arc::new(Mutex::new(InternalFileDragData { label, paths })),
        }
    }

    fn replace(&self, label: String, paths: Vec<PathBuf>) {
        *self
            .data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) =
            InternalFileDragData { label, paths };
    }

    fn snapshot(&self) -> InternalFileDragData {
        self.data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn same_gesture(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.data, &other.data)
    }

    pub(super) fn paths(&self) -> Vec<PathBuf> {
        self.snapshot().paths
    }

    pub(super) fn is_empty(&self) -> bool {
        self.data
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .paths
            .is_empty()
    }
}

struct PendingFileDrag {
    drag: InternalFileDrag,
}

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
    keys: Vec<String>,
    editing: Option<TagEditCacheKey>,
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
enum TagEditCacheKey {
    Add { path: PathBuf, key: String },
    Rename { path: PathBuf, key: String },
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
    hidden_tag_keys: BTreeSet<String>,
    column_visibility_menu_position: Option<Point<Pixels>>,
    hovered_column_visibility: Option<ColumnVisibilityHover>,
    row_scroll_handle: VirtualListScrollHandle,
    row_sizes: Rc<Vec<Size<Pixels>>>,
    row_sizes_len: usize,
    tag_width_cache: Option<TagWidthCache>,
    folder_prompt_active: bool,
}

impl FileTable {
    fn editing_cache_key(editing: Option<&TagEdit>) -> Option<TagEditCacheKey> {
        match editing {
            Some(TagEdit::Add { path, key }) => Some(TagEditCacheKey::Add {
                path: path.clone(),
                key: key.clone(),
            }),
            Some(TagEdit::Rename { path, key, .. }) => Some(TagEditCacheKey::Rename {
                path: path.clone(),
                key: key.clone(),
            }),
            None => None,
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
        cx.observe(&library, |this, _, cx| {
            this.tag_width_cache = None;
            cx.notify();
        })
        .detach();
        cx.subscribe(&library, |this, _, event: &LibraryEvent, cx| match event {
            LibraryEvent::TagEdited { path } => {
                this.tag_width_cache = None;
                this.hovered_tag_chip = None;
                this.hovered_tag_key = None;
                if this.hovered_row.as_ref() == Some(path) {
                    this.hovered_delete_row = Some(path.clone());
                }
                cx.notify();
            }
        })
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
            hidden_tag_keys,
            column_visibility_menu_position: None,
            hovered_column_visibility: None,
            row_scroll_handle: VirtualListScrollHandle::new(),
            row_sizes: Rc::new(Vec::new()),
            row_sizes_len: 0,
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
        let (keys, row_count, widths) = {
            let state = self.library.read(cx).active_state();
            let keys: Vec<String> = state
                .schema
                .keys()
                .filter(|key| !self.hidden_tag_keys.contains(*key))
                .cloned()
                .into_iter()
                .collect();
            let row_count = state.results.len();

            if let Some(cache) = self.tag_width_cache.as_ref()
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

    fn tag_column_keys(&mut self, cx: &mut Context<Self>) -> Vec<String> {
        let keys: BTreeSet<String> = self
            .library
            .read(cx)
            .active_state()
            .schema
            .keys()
            .cloned()
            .collect();
        keys.into_iter().collect()
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
        record: &FileRecord,
        drag: InternalFileDrag,
        cx: &mut Context<Self>,
    ) {
        let start = crate::perf::start();
        self.cancel_unstarted_file_drag(cx);
        if self.native_drag_session.is_active() {
            return;
        }
        let state = self.library.read(cx);
        let paths = self.selected_priority_paths(&state.active_state().results, &record.path);
        let path_count = paths.len();
        drag.replace(record.name.clone(), paths);
        self.pending_drag = Some(PendingFileDrag { drag });
        crate::perf::finish("table.pending_drag", start, || {
            format!("paths={path_count}")
        });
        debug_table_interaction(|| {
            format!("pending row drag stem={} paths={path_count}", record.stem)
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
        self.pending_drag = Some(PendingFileDrag { drag });
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
            kind: DeleteKind::Rows,
            row_count: records.len(),
            paths,
        }
    }

    fn rename_target(&self, record: &FileRecord, selected_records: &[FileRecord]) -> RenameTarget {
        let records: Vec<&FileRecord> =
            if self.selected.len() > 1 && self.selected.contains(record.path.as_path()) {
                selected_records.iter().collect()
            } else {
                vec![record]
            };

        RenameTarget {
            kind: RenameKind::Rows {
                records: records.into_iter().map(row_rename_target).collect(),
            },
        }
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
        _source: &'static str,
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
        self.confirm_delete_target(Self::tag_key_delete_target(key), "tag-key", window, cx);
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
        let _matches_pending = self
            .pending_drag
            .take()
            .is_some_and(|pending| pending.drag.same_gesture(drag));
        self.active_file_drag = Some(ActiveFileDrag {
            label: data.label.clone(),
            paths: data.paths.clone(),
        });
        self.library.update(cx, |lib, cx| {
            if data.paths.len() == 1 {
                lib.begin_internal_file_drag(data.paths[0].clone(), cx);
            } else {
                lib.begin_internal_file_drag_files(data.paths.clone(), cx);
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

    fn render_rows(
        &mut self,
        range: Range<usize>,
        keys: Vec<String>,
        tag_widths: Vec<Pixels>,
        tag_key_action_width: Pixels,
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
                    tag_key_action_width,
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
        tag_key_action_width: Pixels,
        selected_records: &[FileRecord],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = record.path.clone();
        let convertible = record.is_convertible();
        let selected = self.selected.contains(path.as_path());
        let preview_active = self.preview_active_row.as_ref() == Some(&path);
        let waveform = record.primary_waveform().copied();
        let playhead_ratio = preview_active.then(|| {
            self.preview_scrub
                .as_ref()
                .filter(|scrub| scrub.path == path)
                .map(|scrub| scrub.ratio)
                .or_else(|| self.library.read(cx).preview_playhead_ratio_for_path(&path))
        });
        let playhead_ratio = playhead_ratio.flatten();
        let delete_target = self.delete_target(record, selected_records);
        let rename_target = self.rename_target(record, selected_records);
        let row_hover_bg = if convertible {
            hsla(0.095, 1., 0.55, 0.2)
        } else {
            cx.theme().table_hover
        };
        let convertible_bg = hsla(0.095, 1., 0.55, 0.12);
        let chip_delete_bg = red().opacity(0.18);
        let row_delete_bg = red().opacity(0.18);
        let format_chip_hovered = self.hovered_format_chip.as_ref().is_some_and(|hovered| {
            record
                .variants
                .iter()
                .any(|variant| &variant.path == hovered)
        });
        let row_hovered = self.hovered_row.as_ref() == Some(&path);
        let row_delete_hover_enabled = self.hovered_tag_chip.is_none() && !format_chip_hovered;
        let delete_hovered = row_delete_hover_enabled
            && self.alt_down
            && self.hovered_delete_row.as_ref().is_some_and(|hovered| {
                if self.selected.len() > 1 && self.selected.contains(hovered.as_path()) {
                    selected
                } else {
                    hovered == &path
                }
            });
        let open_path = record.path.clone();
        let open_preview_active = preview_active;
        let select_path = record.path.clone();
        let hover_path = record.path.clone();
        let delete_click_target = delete_target.clone();
        let conversion_actions = self.conversion_actions(record, selected_records);
        let table = cx.entity();
        let row_drag_paths = if self.selected.len() > 1 && selected {
            selected_records
                .iter()
                .map(|record| record.path.clone())
                .collect()
        } else {
            vec![record.path.clone()]
        };
        let row_drag = InternalFileDrag::new(record.name.clone(), row_drag_paths);
        let row_drag_for_mouse_down = row_drag.clone();
        let table_for_drag = table.clone();
        let table_for_context_menu = table.clone();
        let menu_action_context = self.focus_handle.clone();
        let menu_record = record.clone();
        let row_group = SharedString::from(format!("row-group:{}", path.display()));
        let mut row = div()
            .id(SharedString::from(format!("row:{}", path.display())))
            .group(row_group.clone())
            .h(ROW_HEIGHT)
            .flex()
            .flex_row()
            .w_full()
            .relative()
            .overflow_hidden()
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
                if open_preview_active {
                    return;
                }
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
                        if event.click_count == 1 {
                            let records = this.library.read(cx).active_state().results.clone();
                            if let Some(record) =
                                records.iter().find(|record| record.path == select_path)
                            {
                                this.start_pending_row_drag(
                                    record,
                                    row_drag_for_mouse_down.clone(),
                                    cx,
                                );
                            }
                        }
                    }
                }),
            )
            .when(!self.alt_down, |row| {
                row.on_drag(row_drag, move |drag, cursor_offset, window, cx| {
                    let _ = table_for_drag.update(cx, |this, cx| {
                        this.begin_internal_file_drag(drag, window, cx);
                    });
                    cx.new(|_| FileDragPreview::new(drag, cursor_offset))
                })
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    this.finish_local_file_drag(window, cx);
                }),
            )
            .context_menu(move |menu, window, menu_cx| {
                let table = table_for_context_menu.clone();
                let actions = conversion_actions.clone();
                let target = delete_target.clone();
                let row_rename_target = rename_target.clone();
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
                                this.confirm_pending_context_menu_tag_rename(window, cx);
                                this.confirm_pending_context_menu_rename(window, cx);
                                this.confirm_pending_context_menu_delete(window, cx);
                                cx.notify();
                            });
                        }
                    })
                    .detach();
                let mut menu = menu
                    .max_w(px(CONVERT_MENU_PANE_WIDTH))
                    .action_context(menu_action_context.clone());
                let (format_context_target, tag_context_target) =
                    menu_cx.update_entity(&table, |this, _| {
                        let format_context_target =
                            this.hovered_format_chip.as_ref().and_then(|hovered| {
                                unique_format_variants(&menu_record)
                                    .into_iter()
                                    .find(|variant| variant.path == *hovered)
                                    .map(|variant| {
                                        (
                                            variant.extension.clone(),
                                            this.format_delete_target(
                                                &menu_record,
                                                &variant.extension,
                                            ),
                                        )
                                    })
                            });
                        let tag_context_target =
                            this.hovered_tag_chip.as_ref().and_then(|target| {
                                if target.path == menu_record.path
                                    && menu_record
                                        .tags
                                        .get(&target.key)
                                        .is_some_and(|values| values.contains(&target.value))
                                {
                                    Some(target.clone())
                                } else {
                                    None
                                }
                            });
                        (format_context_target, tag_context_target)
                    });
                if let Some((extension, target)) = format_context_target {
                    let table = table.clone();
                    let label =
                        SharedString::from(format!("Delete {}", extension.to_ascii_uppercase()));
                    return menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                        cx.update_entity(&table, |this, _| {
                            this.request_context_menu_delete(target.clone());
                        });
                    }));
                }
                if let Some(target) = tag_context_target {
                    let remove_table = table.clone();
                    let rename_table = table.clone();
                    let rename_all_table = table.clone();
                    let label = if target.value.is_empty() {
                        SharedString::from("Remove Tag")
                    } else {
                        SharedString::from(format!("Remove {}", target.value))
                    };
                    let rename_target = target.clone();
                    let rename_all_target = RenameTarget {
                        kind: RenameKind::TagAll {
                            key: target.key.clone(),
                            old_value: target.value.clone(),
                        },
                    };
                    return menu
                        .item(PopupMenuItem::new("Rename").on_click(move |_, _, cx| {
                            cx.update_entity(&rename_table, |this, _| {
                                this.request_context_menu_tag_rename(rename_target.clone());
                            });
                        }))
                        .item(PopupMenuItem::new("Rename all").on_click(move |_, _, cx| {
                            cx.update_entity(&rename_all_table, |this, _| {
                                this.request_context_menu_rename(rename_all_target.clone());
                            });
                        }))
                        .separator()
                        .item(PopupMenuItem::new(label).on_click(move |_, window, cx| {
                            cx.update_entity(&remove_table, |this, cx| {
                                this.remove_tag_from_target(
                                    &target.path,
                                    &target.key,
                                    &target.value,
                                    window,
                                    cx,
                                );
                            });
                        }));
                }
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
                let rename_table = table.clone();
                menu = menu.item(PopupMenuItem::new("Rename").on_click(move |_, _, cx| {
                    cx.update_entity(&rename_table, |this, _| {
                        this.request_context_menu_rename(row_rename_target.clone());
                    });
                }));
                let delete_table = table.clone();
                let label = if row_count == 1 {
                    SharedString::from("Delete")
                } else {
                    SharedString::from(format!("Delete {row_count} Rows"))
                };
                menu = menu.item(PopupMenuItem::new(label).on_click(move |_, _, cx| {
                    cx.update_entity(&delete_table, |this, _| {
                        this.request_context_menu_delete(target.clone());
                    });
                }));
                menu
            })
            .when(preview_active, |row| {
                row.child(div().absolute().inset_0().child(PreviewWaveformElement {
                    id: SharedString::from(format!("preview-waveform:{}", path.display())).into(),
                    table: cx.entity(),
                    path: path.clone(),
                    waveform,
                    playhead_ratio,
                }))
            })
            .when(!preview_active, |row| {
                row.child(
                    div()
                        .h_full()
                        .h_flex()
                        .items_center()
                        .flex_1()
                        .min_w_0()
                        .px(CONTENT_PX)
                        .py(px(4.))
                        .gap_1()
                        .overflow_hidden()
                        .child(
                            div()
                                .flex_shrink(1.)
                                .min_w_0()
                                .truncate()
                                .child(record.name.clone()),
                        )
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .gap_1()
                                .flex_shrink_0()
                                .children(unique_format_variants(record).into_iter().map(
                                    |variant| {
                                        let record = record.clone();
                                        let extension = variant.extension.clone();
                                        let label = extension.clone();
                                        let variant_path = variant.path.clone();
                                        let format_drag_paths =
                                            if self.selected.len() > 1 && selected {
                                                selected_records
                                                    .iter()
                                                    .filter_map(|record| {
                                                        record.variant_for_extension(&extension)
                                                    })
                                                    .map(|variant| variant.path.clone())
                                                    .collect()
                                            } else {
                                                vec![variant_path.clone()]
                                            };
                                        let format_drag = InternalFileDrag::new(
                                            format!(".{extension}"),
                                            format_drag_paths,
                                        );
                                        let format_drag_for_mouse_down = format_drag.clone();
                                        let table_for_format_drag = table.clone();
                                        let format_delete_target =
                                            self.format_delete_target(&record, &extension);
                                        let chip_bg = if self.alt_down
                                            && self.hovered_format_chip.as_ref()
                                                == Some(&variant_path)
                                        {
                                            chip_delete_bg
                                        } else {
                                            cx.theme().muted
                                        };
                                        div()
                                    .id(SharedString::from(format!(
                                        "extension-chip:{}:{extension}",
                                        record.path.display()
                                    )))
                                    .h(px(18.))
                                    .min_w(px(26.))
                                    .px_1()
                                    .flex_shrink_0()
                                    .h_flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .text_size(px(10.))
                                    .bg(chip_bg)
                                    .text_color(cx.theme().muted_foreground)
                                    .cursor_pointer()
                                    .child(SharedString::from(label))
                                    .on_hover(cx.listener({
                                        let variant_path = variant_path.clone();
                                        move |this, hovered: &bool, _, cx| {
                                            this.set_format_chip_hovered(
                                                variant_path.clone(),
                                                *hovered,
                                                cx,
                                            );
                                        }
                                    }))
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener({
                                            let variant_path = variant_path.clone();
                                            move |this, event: &MouseDownEvent, window, cx| {
                                                if event.click_count == 1 {
                                                    if event.modifiers.alt {
                                                        this.clear_pending_drag();
                                                        this.confirm_delete_target(
                                                            format_delete_target.clone(),
                                                            "format-opt-click",
                                                            window,
                                                            cx,
                                                        );
                                                        debug_table_interaction(|| {
                                                            format!(
                                                                "format delete requested path={}",
                                                                variant_path.display()
                                                            )
                                                        });
                                                        cx.stop_propagation();
                                                        return;
                                                    }
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
                                                    this.start_pending_format_drag(
                                                        &record,
                                                        extension.clone(),
                                                        format_drag_for_mouse_down.clone(),
                                                        cx,
                                                    );
                                                }
                                                cx.stop_propagation();
                                            }
                                        }),
                                    )
                                    .on_mouse_down(
                                        MouseButton::Right,
                                        cx.listener({
                                            let variant_path = variant_path.clone();
                                            move |this, _: &MouseDownEvent, _, cx| {
                                                this.set_format_chip_hovered(
                                                    variant_path.clone(),
                                                    true,
                                                    cx,
                                                );
                                            }
                                        }),
                                    )
                                    .when(!self.alt_down, |chip| {
                                        chip.on_drag(
                                            format_drag,
                                            move |drag, cursor_offset, window, cx| {
                                                let _ = table_for_format_drag.update(
                                                    cx,
                                                    |this, cx| {
                                                        this.begin_internal_file_drag(
                                                            drag, window, cx,
                                                        );
                                                    },
                                                );
                                                cx.new(|_| {
                                                    FileDragPreview::new(drag, cursor_offset)
                                                })
                                            },
                                        )
                                    })
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseUpEvent, window, cx| {
                                            this.finish_local_file_drag(window, cx);
                                            cx.stop_propagation();
                                        }),
                                    )
                                    },
                                )),
                        ),
                )
            });

        for (key, tag_width) in keys.iter().zip(tag_widths) {
            let group = SharedString::from(format!("cell:{}:{key}", path.display()));

            let mut cell = div()
                .id(group.clone())
                .group(group.clone())
                .absolute()
                .left(CONTENT_PX)
                .right(px(0.))
                .top_0()
                .bottom_0()
                .h_flex()
                .flex_nowrap()
                .items_center()
                .gap_1();

            if !preview_active && let Some(values) = record.tags.get(key) {
                for value in values {
                    let (key, value, path) = (key.clone(), value.clone(), path.clone());
                    let tag_target = TagChipTarget {
                        path: path.clone(),
                        key: key.clone(),
                        value: value.clone(),
                    };
                    let chip_group =
                        SharedString::from(format!("chip-group:{}:{key}:{value}", path.display()));
                    let is_renaming =
                        Self::editing_is_rename_value(self.editing.as_ref(), &path, &key, &value);
                    let chip_width =
                        px(value.chars().count() as f32 * TAG_TEXT_WIDTH
                            + TAG_CHIP_X_PADDING_WIDTH);

                    let mut chip = div()
                        .id(SharedString::from(format!(
                            "chip:{}:{key}:{value}",
                            path.display()
                        )))
                        .group(chip_group.clone())
                        .relative()
                        .h(ROW_HEIGHT)
                        .h_flex()
                        .items_center();

                    if is_renaming {
                        chip = chip.child(
                            div()
                                .w(chip_width)
                                .min_w(chip_width)
                                .h_flex()
                                .items_center()
                                .flex_shrink_0()
                                .px_1p5()
                                .rounded_md()
                                .text_xs()
                                .bg(cx.theme().muted)
                                .overflow_hidden()
                                .on_key_down(cx.listener(
                                    |this, event: &KeyDownEvent, window, cx| {
                                        if event.keystroke.key == "escape" {
                                            this.cancel_tag(window, cx);
                                            cx.stop_propagation();
                                        }
                                    },
                                ))
                                .child(
                                    Input::new(&self.tag_input)
                                        .appearance(false)
                                        .xsmall()
                                        .px_0()
                                        .flex_1()
                                        .mr(px(-10.))
                                        .min_w_0(),
                                ),
                        );
                    } else {
                        let rename_target = tag_target.clone();
                        chip = chip
                            .child(
                                div()
                                    .px_1p5()
                                    .rounded_md()
                                    .text_xs()
                                    .h(px(18.))
                                    .h_flex()
                                    .items_center()
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
                                    .on_hover(cx.listener({
                                        let tag_target = tag_target.clone();
                                        move |this, hovered: &bool, _, cx| {
                                            this.set_tag_chip_hovered(
                                                tag_target.clone(),
                                                *hovered,
                                                cx,
                                            );
                                        }
                                    }))
                                    .on_mouse_down(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_mouse_up(MouseButton::Left, |_, _, cx| {
                                        cx.stop_propagation()
                                    })
                                    .on_mouse_move(|_, _, cx| cx.stop_propagation())
                                    .on_click(cx.listener(
                                        move |this, event: &ClickEvent, window, cx| {
                                            let modifiers = event.modifiers();
                                            if modifiers.alt {
                                                this.remove_tag_from_target(
                                                    &path, &key, &value, window, cx,
                                                );
                                            } else if event.click_count() == 2
                                                && !modifiers.control
                                                && !modifiers.platform
                                                && !modifiers.shift
                                                && !modifiers.function
                                            {
                                                this.start_renaming_tag(
                                                    rename_target.clone(),
                                                    window,
                                                    cx,
                                                );
                                            }
                                            cx.stop_propagation();
                                        },
                                    )),
                            );
                    }

                    cell = cell.child(chip);
                }
            }

            if preview_active {
                cell = cell.child(div());
            } else if Self::editing_is_add(self.editing.as_ref(), &path, key) {
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
                        .overflow_hidden()
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if event.keystroke.key == "escape" {
                                this.cancel_tag(window, cx);
                                cx.stop_propagation();
                            }
                        }))
                        .child(
                            Input::new(&self.tag_input)
                                .appearance(false)
                                .xsmall()
                                .px_0()
                                .flex_1()
                                .mr(px(-10.))
                                .min_w_0(),
                        ),
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

        row = row.child(
            div()
                .h_full()
                .w(tag_key_action_width)
                .min_w(tag_key_action_width)
                .flex_shrink_0(),
        );

        row.into_any_element()
    }
}

impl Focusable for FileTable {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileTable {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::perf::sample("table.render.rate");
        let render_start = crate::perf::start();
        let missing_folder_category = {
            let library = self.library.read(cx);
            let active = library.active();
            library.category_needs_folder(active).then_some(active)
        };
        let (keys, tag_widths, row_count) = if missing_folder_category.is_some() {
            (Vec::new(), Vec::new(), 0)
        } else {
            self.table_columns(window, cx)
        };
        let tag_key_action_width = if self.creating_tag_key {
            px(TAG_KEY_EDITOR_WIDTH)
        } else {
            px(TAG_KEY_ACTION_WIDTH)
        };
        let tag_column_keys = self.tag_column_keys(cx);

        let mut header_row = TableRow::new().child(
            TableHead::new().flex_1().min_w_0().px(CONTENT_PX).child(
                div()
                    .h_full()
                    .w_full()
                    .min_w_0()
                    .h_flex()
                    .items_center()
                    .child(div().flex_none().max_w_full().truncate().child("name"))
                    .child(div().h_full().flex_1().min_w_0().on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, event: &MouseDownEvent, window, cx| {
                            this.open_column_visibility_menu(event.position, window, cx);
                            cx.stop_propagation();
                        }),
                    )),
            ),
        );
        for (key, tag_width) in keys.iter().zip(&tag_widths) {
            let key_for_click = key.clone();
            let key_for_menu = key.clone();
            let key_for_hover = key.clone();
            let key_is_delete_hovered = self.alt_down && self.hovered_tag_key.as_ref() == Some(key);
            let table = cx.entity();
            let table_for_label_menu = table.clone();
            let header_label = div()
                .id(SharedString::from(format!("tag-key-header:{key}")))
                .flex_none()
                .max_w_full()
                .truncate()
                .cursor_pointer()
                .text_color(cx.theme().foreground)
                .when(key_is_delete_hovered, |el| el.text_color(red()))
                .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                    this.set_tag_key_hovered(key_for_hover.clone(), *hovered, cx);
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                        if event.modifiers.alt {
                            this.confirm_tag_key_delete(&key_for_click, window, cx);
                            cx.stop_propagation();
                        }
                    }),
                )
                .context_menu(move |menu, _, _| {
                    let rename_table = table_for_label_menu.clone();
                    let remove_table = table_for_label_menu.clone();
                    let rename_key = key_for_menu.clone();
                    let remove_key = key_for_menu.clone();
                    let remove_label = SharedString::from(format!("Remove {remove_key}"));
                    menu.item(PopupMenuItem::new("Rename").on_click(move |_, window, cx| {
                        let rename_key = rename_key.clone();
                        cx.update_entity(&rename_table, |this, cx| {
                            let target = RenameTarget {
                                kind: RenameKind::TagKey {
                                    old_key: rename_key,
                                },
                            };
                            let initial_value = target.default_value();
                            this.start_rename_target(target, initial_value, window, cx);
                        });
                    }))
                    .separator()
                    .item(
                        PopupMenuItem::new(remove_label).on_click(move |_, window, cx| {
                            cx.update_entity(&remove_table, |this, cx| {
                                this.confirm_tag_key_delete(&remove_key, window, cx);
                            });
                        }),
                    )
                })
                .child(SharedString::from(key.clone()))
                .into_any_element();
            header_row = header_row.child(
                TableHead::new()
                    .w(*tag_width)
                    .min_w(*tag_width)
                    .flex_shrink_0()
                    .pl(CONTENT_PX)
                    .pr(px(0.))
                    .child(
                        div()
                            .h_full()
                            .w_full()
                            .min_w_0()
                            .h_flex()
                            .items_center()
                            .child(header_label)
                            .child(div().h_full().flex_1().min_w_0().on_mouse_down(
                                MouseButton::Right,
                                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                    this.open_column_visibility_menu(event.position, window, cx);
                                    cx.stop_propagation();
                                }),
                            )),
                    ),
            );
        }
        let creating_tag_key = self.creating_tag_key;
        let tag_key_action = if creating_tag_key {
            div()
                .w_full()
                .h_flex()
                .items_center()
                .rounded_md()
                .bg(cx.theme().muted)
                .text_color(cx.theme().foreground)
                .overflow_hidden()
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if event.keystroke.key == "escape" {
                        this.cancel_tag_key(window, cx);
                        cx.stop_propagation();
                    }
                }))
                .child(
                    Input::new(&self.tag_key_input)
                        .appearance(false)
                        .xsmall()
                        .text_color(cx.theme().foreground)
                        .px_0()
                        .flex_1()
                        .mr(px(-10.))
                        .min_w_0(),
                )
                .into_any_element()
        } else {
            Button::new("add-tag-key")
                .xsmall()
                .compact()
                .ghost()
                .icon(IconName::Plus)
                .on_click(cx.listener(|this, _, window, cx| {
                    this.start_creating_tag_key(window, cx);
                    cx.stop_propagation();
                }))
                .into_any_element()
        };
        header_row = header_row.child(
            TableHead::new()
                .w(tag_key_action_width)
                .min_w(tag_key_action_width)
                .flex_shrink_0()
                .px(px(4.))
                .child(
                    div()
                        .h_full()
                        .w_full()
                        .min_w_0()
                        .h_flex()
                        .items_center()
                        .child(tag_key_action)
                        .child(div().h_full().flex_1().min_w_0().on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, event: &MouseDownEvent, window, cx| {
                                this.open_column_visibility_menu(event.position, window, cx);
                                cx.stop_propagation();
                            }),
                        )),
                ),
        );

        let virtual_keys = keys.clone();
        let virtual_tag_widths = tag_widths.clone();
        let virtual_tag_key_action_width = tag_key_action_width;
        let row_sizes = self.row_sizes(row_count);
        let row_scroll_handle = self.row_scroll_handle.clone();
        let rows: AnyElement = if let Some(category) = missing_folder_category {
            div()
                .id("missing-category-folder")
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_2()
                .px(CONTENT_PX)
                .text_sm()
                .text_color(cx.theme().muted_foreground)
                .cursor_pointer()
                .child(SharedString::from(format!(
                    "Click this area to choose the {} folder.",
                    category.label()
                )))
                .child("You can always change it via the category buttons above.")
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.choose_category_folder(category, cx);
                    cx.stop_propagation();
                }))
                .into_any_element()
        } else {
            div()
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
                                virtual_tag_key_action_width,
                                cx,
                            )
                        },
                    )
                    .track_scroll(&row_scroll_handle),
                )
                .scrollbar(&row_scroll_handle, ScrollbarAxis::Vertical)
                .into_any_element()
        };
        let column_visibility_menu =
            self.render_column_visibility_menu(tag_column_keys, window, cx);

        let table = div()
            .track_focus(&self.focus_handle)
            .capture_any_mouse_up(cx.listener(|this, event: &MouseUpEvent, window, cx| {
                if event.button == MouseButton::Left {
                    this.finish_local_file_drag(window, cx);
                }
            }))
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, window, cx| {
                    this.finish_local_file_drag(window, cx);
                }),
            )
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                match event.keystroke.key.as_str() {
                    "escape" => {
                        if this.close_column_visibility_menu(cx)
                            || this.cancel_delete(cx)
                            || this.clear_selection(cx)
                        {
                            cx.stop_propagation();
                        }
                    }
                    "backspace" | "delete" => {
                        if event.keystroke.modifiers.shift {
                            return;
                        }
                        if this.row_context_menu_open {
                            window.dispatch_keystroke(Keystroke::parse("escape").unwrap(), cx);
                        }
                        if this.confirm_selected_delete("keyboard", window, cx) {
                            cx.stop_propagation();
                        }
                    }
                    "f2" => {
                        if this.start_selected_rename(window, cx) {
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
            )
            .when_some(column_visibility_menu, |table, menu| table.child(menu));

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

struct PreviewWaveformElement {
    id: ElementId,
    table: Entity<FileTable>,
    path: PathBuf,
    waveform: Option<WaveformBinary256>,
    playhead_ratio: Option<f32>,
}

struct PreviewWaveformPrepaintState {
    hitbox: Hitbox,
}

#[derive(Clone, Copy)]
enum PreviewScrubAction {
    Begin,
    Continue,
    End,
}

impl IntoElement for PreviewWaveformElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for PreviewWaveformElement {
    type RequestLayoutState = ();
    type PrepaintState = PreviewWaveformPrepaintState;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (
            window.request_layout(
                Style {
                    size: size(relative(1.).into(), relative(1.).into()),
                    ..Style::default()
                },
                [],
                cx,
            ),
            (),
        )
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        _cx: &mut App,
    ) -> Self::PrepaintState {
        PreviewWaveformPrepaintState {
            hitbox: window.insert_hitbox(bounds, HitboxBehavior::Normal),
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        paint_preview_waveform(bounds, self.waveform, self.playhead_ratio, window);
        let hitbox = prepaint.hitbox.clone();
        window.set_cursor_style(CursorStyle::PointingHand, &hitbox);

        window.on_mouse_event({
            let table = self.table.clone();
            let path = self.path.clone();
            let hitbox = hitbox.clone();
            move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble
                    || event.button != MouseButton::Left
                    || !event.modifiers.platform
                    || !hitbox.is_hovered(window)
                {
                    return;
                }
                scrub_preview_from_position(
                    &table,
                    &path,
                    event.position,
                    bounds,
                    PreviewScrubAction::Begin,
                    cx,
                );
                cx.stop_propagation();
                window.prevent_default();
            }
        });

        window.on_mouse_event({
            let table = self.table.clone();
            let path = self.path.clone();
            move |event: &MouseMoveEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble || !event.dragging() || !event.modifiers.platform
                {
                    return;
                }
                scrub_preview_from_position(
                    &table,
                    &path,
                    event.position,
                    bounds,
                    PreviewScrubAction::Continue,
                    cx,
                );
                cx.stop_propagation();
            }
        });

        window.on_mouse_event({
            let table = self.table.clone();
            let path = self.path.clone();
            move |event: &MouseUpEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                scrub_preview_from_position(
                    &table,
                    &path,
                    event.position,
                    bounds,
                    PreviewScrubAction::End,
                    cx,
                );
            }
        });
    }
}

fn paint_preview_waveform(
    bounds: Bounds<Pixels>,
    waveform: Option<WaveformBinary256>,
    playhead_ratio: Option<f32>,
    window: &mut Window,
) {
    let row_width = bounds.size.width.as_f32().max(1.);
    let color = white();

    if let Some(waveform) = waveform {
        let row_height = bounds.size.height.as_f32().max(1.);
        let bar_gap = 1.;
        let bar_width = ((row_width - bar_gap * (WAVEFORM_BAR_COUNT - 1) as f32)
            / WAVEFORM_BAR_COUNT as f32)
            .max(1.);

        for (ix, value) in waveform.into_iter().enumerate() {
            let height = if value == 0 {
                1.
            } else {
                ((value as f32 / 255.) * row_height).max(1.)
            };
            let x = bounds.left().as_f32() + ix as f32 * (bar_width + bar_gap);
            let y = bounds.bottom().as_f32() - height;
            window.paint_quad(fill(
                Bounds::new(point(px(x), px(y)), size(px(bar_width), px(height))),
                color,
            ));
        }
    }

    if let Some(ratio) = playhead_ratio {
        let x = bounds.left().as_f32() + row_width * ratio.clamp(0., 1.);
        window.paint_quad(fill(
            Bounds::new(point(px(x), bounds.top()), size(px(2.), bounds.size.height)),
            color,
        ));
    }
}

fn scrub_preview_from_position(
    table: &Entity<FileTable>,
    path: &Path,
    position: Point<Pixels>,
    bounds: Bounds<Pixels>,
    action: PreviewScrubAction,
    cx: &mut App,
) {
    let ratio = ((position.x.as_f32() - bounds.left().as_f32())
        / bounds.size.width.as_f32().max(1.))
    .clamp(0., 1.);
    cx.update_entity(table, |table, cx| match action {
        PreviewScrubAction::Begin => {
            table.begin_preview_scrub(path.to_path_buf(), ratio, cx);
        }
        PreviewScrubAction::Continue => {
            table.continue_preview_scrub(path, ratio, cx);
        }
        PreviewScrubAction::End => {
            table.end_preview_scrub(path, cx);
        }
    });
}

fn debug_table_interaction(details: impl FnOnce() -> String) {
    let enabled = std::env::var("LOWCAT_DEBUG")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if enabled {
        eprintln!("[lowcat:table] {}", details());
    }
}

#[cfg(test)]
mod tests {
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
    fn internal_drag_payload_updates_for_mouse_down_selection() {
        let drag =
            InternalFileDrag::new("first".to_string(), vec![PathBuf::from("/tmp/first.wav")]);
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
        assert!(drag.same_gesture(&drag_value));
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
}
