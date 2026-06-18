mod native_drag;

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, SharedString, Size,
    StatefulInteractiveElement as _, Styled, Window, div, hsla, prelude::FluentBuilder as _, px,
    red, size,
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
use crate::{library::Library, model::FileRecord};

const TAG_CELL_X_PADDING_WIDTH: f32 = 24.;
const TAG_CHIP_X_PADDING_WIDTH: f32 = 12.;
const TAG_ADD_BUTTON_WIDTH: f32 = 19.;
const TAG_COLUMN_MIN_WIDTH: f32 = TAG_CELL_X_PADDING_WIDTH + TAG_ADD_BUTTON_WIDTH;
const TAG_GAP_WIDTH: f32 = 4.;
const TAG_TEXT_WIDTH: f32 = 7.;
const TAG_EDITOR_WIDTH: f32 = 90.;
const FILE_DRAG_THRESHOLD_PX: f32 = 4.;
const ROW_HEIGHT: Pixels = px(32.);

struct PendingFileDrag {
    path: PathBuf,
    paths: Vec<PathBuf>,
    origin: Point<Pixels>,
}

struct TagWidthCache {
    keys: Vec<String>,
    editing: Option<(PathBuf, String)>,
    row_count: usize,
    widths: Vec<Pixels>,
}

pub struct FileTable {
    library: Entity<Library>,
    tag_input: Entity<InputState>,
    editing: Option<(PathBuf, String)>,
    focus_handle: FocusHandle,
    alt_down: bool,
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
            let editing_ref = editing
                .as_ref()
                .map(|(path, key)| (path, key.as_str()));
            let widths: Vec<Pixels> = keys
                .iter()
                .map(|key| Self::tag_column_width(state, key, editing_ref))
                .collect();
            crate::perf::finish("table.widths", width_start, || {
                format!("rows={} keys={} cached=false", state.results.len(), keys.len())
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
        cx.notify();
    }

    fn cancel_tag(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing.take().is_some() {
            self.focus_handle.focus(window, cx);
            cx.notify();
        }
    }

    fn start_pending_drag(&mut self, path: PathBuf, origin: Point<Pixels>, cx: &mut Context<Self>) {
        let start = crate::perf::start();
        let paths = self.selected_paths_for(&self.library.read(cx).active_state().results, &path);
        let path_count = paths.len();
        self.pending_drag = Some(PendingFileDrag {
            path,
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

    fn select_row(&mut self, path: PathBuf, extend: bool, cx: &mut Context<Self>) {
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

        cx.notify();
        crate::perf::finish("table.select_row", start, || {
            format!("rows={} selected={}", paths.len(), self.selected.len())
        });
    }

    fn selected_paths_for(&self, records: &[FileRecord], path: &Path) -> Vec<PathBuf> {
        if self.selected.len() > 1 && self.selected.contains(path) {
            records
                .iter()
                .filter(|record| self.selected.contains(record.path.as_path()))
                .map(|record| record.path.clone())
                .collect()
        } else {
            vec![path.to_path_buf()]
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
        let path = pending.path.clone();
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
                eprintln!("native file drag unavailable for {}", path.display());
            }
        });
        window.refresh();
        crate::perf::finish("table.drag_start", drag_start, || {
            format!("paths={drag_path_count}")
        });
        crate::perf::finish("table.drag_move", move_start, || {
            format!("started paths={drag_path_count}")
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
        let (records, selected_convertible_paths, total_rows, visible_start, visible_end) = {
            let state = self.library.read(cx).active_state();
            let total_rows = state.results.len();
            let visible_start = range.start.min(total_rows);
            let visible_end = range.end.min(total_rows).max(visible_start);
            let selected_convertible_paths = if self.selected.is_empty() {
                Vec::new()
            } else {
                state
                    .results
                    .iter()
                    .filter(|record| {
                        self.selected.contains(record.path.as_path()) && record.is_convertible()
                    })
                    .map(|record| record.path.clone())
                    .collect()
            };

            (
                state.results[visible_start..visible_end].to_vec(),
                selected_convertible_paths,
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
                    &selected_convertible_paths,
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
        selected_convertible_paths: &[PathBuf],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let path = record.path.clone();
        let convertible = record.is_convertible();
        let selected = self.selected.contains(path.as_path());
        let row_hover_bg = if convertible {
            hsla(0.095, 1., 0.55, 0.2)
        } else {
            cx.theme().table_hover
        };
        let convertible_bg = hsla(0.095, 1., 0.55, 0.12);
        let chip_delete_bg = red().opacity(0.18);
        let name = div()
            .w_full()
            .min_w_0()
            .truncate()
            .child(record.name.clone());
        let open_path = record.path.clone();
        let drag_path = record.path.clone();
        let select_path = record.path.clone();
        let convert_paths = if selected && !selected_convertible_paths.is_empty() {
            selected_convertible_paths.to_vec()
        } else if record.is_convertible() {
            vec![record.path.clone()]
        } else {
            Vec::new()
        };
        let convert_target = conversion_target_label(&convert_paths);
        let table = cx.entity();
        let mut row = div()
            .id(SharedString::from(format!("row:{}", path.display())))
            .h(ROW_HEIGHT)
            .flex()
            .flex_row()
            .w_full()
            .text_sm()
            .when(row_ix > 0, |s| {
                s.border_t_1().border_color(cx.theme().table_row_border)
            })
            .when(convertible, |s| s.bg(convertible_bg))
            .when(selected, |s| s.bg(row_hover_bg))
            .hover(move |s| s.bg(row_hover_bg))
            .on_click(cx.listener(move |_, event: &ClickEvent, _, _| {
                if event.click_count() == 2 {
                    open_in_default_app(&open_path);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    if event.click_count == 1 {
                        if event.modifiers.shift || !this.selected.contains(&select_path) {
                            this.select_row(select_path.clone(), event.modifiers.shift, cx);
                        }
                        this.start_pending_drag(drag_path.clone(), event.position, cx);
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
            .context_menu(move |menu, _, _| {
                if convert_paths.is_empty() {
                    return menu;
                }

                let paths = convert_paths.clone();
                let table = table.clone();
                menu.item(
                    PopupMenuItem::new(SharedString::from(format!("Convert to {convert_target}")))
                        .on_click(move |_, _, cx| {
                            cx.update_entity(&table, |this, cx| {
                                this.library.update(cx, |lib, cx| {
                                    if paths.len() == 1 {
                                        lib.convert_active_unsupported_file(paths[0].clone(), cx);
                                    } else {
                                        lib.convert_active_unsupported_files(paths.clone(), cx);
                                    }
                                });
                            });
                        }),
                )
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
                    .child(name),
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
                .h_flex()
                .flex_nowrap()
                .items_center()
                .gap_1();

            if let Some(values) = record.tags.get(key) {
                for value in values {
                    let (key, value, path) = (key.clone(), value.clone(), path.clone());

                    cell = cell.child(
                        div()
                            .id(SharedString::from(format!(
                                "chip:{}:{key}:{value}",
                                path.display()
                            )))
                            .px_1p5()
                            .rounded_md()
                            .text_xs()
                            .whitespace_nowrap()
                            .bg(cx.theme().muted)
                            .text_color(cx.theme().muted_foreground)
                            .cursor_pointer()
                            .child(SharedString::from(value.clone()))
                            .when(self.alt_down, |this| {
                                this.hover(move |this| this.bg(chip_delete_bg))
                            })
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                if event.modifiers().alt {
                                    let path = path.clone();
                                    this.library.update(cx, |lib, cx| {
                                        lib.remove_tag(path, &key, &value, cx)
                                    });
                                }
                            })),
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
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.start_editing(path.clone(), key.clone(), window, cx);
                        })),
                );
            }

            row = row.child(
                div()
                    .h_full()
                    .flex()
                    .items_center()
                    .w(*tag_width)
                    .min_w(*tag_width)
                    .flex_shrink_0()
                    .px(CONTENT_PX)
                    .py(px(4.))
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

fn conversion_target_format(path: &Path) -> &'static str {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("wav"))
    {
        "flac"
    } else {
        "opus"
    }
}

fn conversion_target_label(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| conversion_target_format(path))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>()
        .join("/")
}
