mod native_drag;

use std::path::{Path, PathBuf};

use futures::{StreamExt as _, channel::mpsc};
use gpui::{
    App, AppContext as _, ClickEvent, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, ParentElement, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, prelude::FluentBuilder as _, px, red,
};
use gpui_component::{
    ActiveTheme as _, Sizable, StyledExt,
    input::{Input, InputEvent, InputState},
    scroll::ScrollableElement,
    table::*,
};

use crate::library::Library;
use crate::ui::CONTENT_PX;

const TAG_CELL_X_PADDING_WIDTH: f32 = 24.;
const TAG_CHIP_X_PADDING_WIDTH: f32 = 12.;
const TAG_ADD_BUTTON_WIDTH: f32 = 19.;
const TAG_COLUMN_MIN_WIDTH: f32 = TAG_CELL_X_PADDING_WIDTH + TAG_ADD_BUTTON_WIDTH;
const TAG_GAP_WIDTH: f32 = 4.;
const TAG_TEXT_WIDTH: f32 = 7.;
const TAG_EDITOR_WIDTH: f32 = 90.;
const FILE_DRAG_THRESHOLD_PX: f32 = 4.;

struct PendingFileDrag {
    path: PathBuf,
    origin: Point<Pixels>,
}

pub struct FileTable {
    library: Entity<Library>,
    tag_input: Entity<InputState>,
    editing: Option<(PathBuf, String)>,
    focus_handle: FocusHandle,
    alt_down: bool,
    pending_drag: Option<PendingFileDrag>,
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
            } else {
                TAG_ADD_BUTTON_WIDTH
            };
            let row_width = value_width + gap_width + action_width;
            width = width.max(row_width + TAG_CELL_X_PADDING_WIDTH);
        }

        px(width.max(TAG_COLUMN_MIN_WIDTH))
    }

    pub fn new(library: Entity<Library>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();

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
        }
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

    fn start_pending_drag(&mut self, path: PathBuf, origin: Point<Pixels>) {
        self.pending_drag = Some(PendingFileDrag { path, origin });
    }

    fn clear_pending_drag(&mut self) {
        self.pending_drag = None;
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

        let dx = event.position.x.as_f32() - pending.origin.x.as_f32();
        let dy = event.position.y.as_f32() - pending.origin.y.as_f32();
        if dx.hypot(dy) < FILE_DRAG_THRESHOLD_PX {
            return;
        }

        let path = pending.path.clone();
        self.clear_pending_drag();
        self.library.update(cx, |lib, cx| {
            lib.begin_internal_file_drag_with_anchor(path.clone(), Some(event.position), cx)
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
            let drag_finished_tx = drag_finished_tx.clone();
            if !native_drag::start_file_drag(vec![path.clone()], window, move || {
                let _ = drag_finished_tx.unbounded_send(());
            }) {
                library.update(cx, |lib, cx| lib.clear_internal_file_drag(cx));
                eprintln!("native file drag unavailable for {}", path.display());
            }
        });
        window.refresh();
    }
}

impl Focusable for FileTable {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for FileTable {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.library.read(cx).active_state();
        let keys: Vec<String> = state.schema.keys().cloned().collect();
        let editing = self
            .editing
            .as_ref()
            .map(|(path, key)| (path, key.as_str()));
        let tag_widths: Vec<Pixels> = keys
            .iter()
            .map(|key| Self::tag_column_width(state, key, editing))
            .collect();
        let chip_delete_bg = red().opacity(0.18);

        let mut body = TableBody::new();
        for record in &state.results {
            let path = record.path.clone();
            let name = div()
                .w_full()
                .min_w_0()
                .truncate()
                .child(record.name.clone());
            let open_path = record.path.clone();
            let drag_path = record.path.clone();
            let mut row = div()
                .id(SharedString::from(format!("row:{}", path.display())))
                .flex()
                .flex_row()
                .w_full()
                .hover(|s| s.bg(cx.theme().table_hover))
                .on_click(cx.listener(move |_, event: &ClickEvent, _, _| {
                    if event.click_count() == 2 {
                        open_in_default_app(&open_path);
                    }
                }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, _| {
                        if event.click_count == 1 {
                            this.start_pending_drag(drag_path.clone(), event.position);
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
                .child(
                    div()
                        .flex()
                        .items_center()
                        .flex_1()
                        .min_w_0()
                        .px(CONTENT_PX)
                        .py(px(4.))
                        .child(name),
                );

            for (key, tag_width) in keys.iter().zip(&tag_widths) {
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
                } else {
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

            body = body
                .child(TableRow::new().child(TableCell::new().w_full().min_w_0().p_0().child(row)));
        }

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

        div()
            .track_focus(&self.focus_handle)
            .flex_1()
            .min_h_0()
            .child(
                div().size_full().overflow_y_scrollbar().child(
                    Table::new()
                        .child(TableHeader::new().child(header_row))
                        .child(body),
                ),
            )
    }
}

fn open_in_default_app(path: &Path) {
    if let Err(err) = opener::open(path) {
        eprintln!("failed to open {}: {err}", path.display());
    }
}
