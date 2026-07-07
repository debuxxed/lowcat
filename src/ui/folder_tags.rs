use std::collections::BTreeSet;

use gpui::{
    Anchor, AnyElement, Context, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Pixels, Point, SharedString, StatefulInteractiveElement, Styled, Window,
    anchored, deferred, div, prelude::FluentBuilder, px, rgba,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, StyledExt,
    button::{Button, ButtonVariants as _},
    scroll::ScrollableElement,
    table::{Table, TableBody, TableCell, TableHead, TableHeader, TableRow},
};

use crate::{
    model::{Category, FolderTagAssignment},
    ui::UI,
};

const FOLDER_TAG_SCROLLBAR_GUTTER: Pixels = px(14.);
const FOLDER_TAG_HEADER_HEIGHT: Pixels = px(28.);
const FOLDER_TAG_ROW_HEIGHT: Pixels = px(28.);
const FOLDER_TAG_KEY_BUTTON_HEIGHT: Pixels = px(20.);
const FOLDER_TAG_KEY_BUTTON_CHROME: Pixels = px(52.);
const FOLDER_TAG_CHECKBOX_SIZE: Pixels = px(20.);

#[derive(Clone)]
struct FolderTagRow {
    value: String,
    key: String,
    enabled: bool,
}

pub(super) struct FolderTagModalState {
    category: Category,
    keys: Vec<String>,
    rows: Vec<FolderTagRow>,
    selected: BTreeSet<usize>,
    selection_anchor: Option<usize>,
    open_key_menu: Option<usize>,
    key_menu_position: Option<Point<Pixels>>,
}

impl FolderTagModalState {
    fn new(category: Category, values: Vec<String>, keys: Vec<String>) -> Self {
        let default_key = keys.first().cloned().unwrap_or_default();
        Self {
            category,
            keys,
            rows: values
                .into_iter()
                .map(|value| FolderTagRow {
                    value,
                    key: default_key.clone(),
                    enabled: true,
                })
                .collect(),
            selected: BTreeSet::new(),
            selection_anchor: None,
            open_key_menu: None,
            key_menu_position: None,
        }
    }

    fn tag_keys(&self) -> Vec<String> {
        self.keys.clone()
    }

    fn close_key_menu(&mut self) -> bool {
        let was_open = self.open_key_menu.is_some() || self.key_menu_position.is_some();
        self.open_key_menu = None;
        self.key_menu_position = None;
        was_open
    }

    fn select_row(&mut self, index: usize, extend: bool, toggle: bool) {
        if index >= self.rows.len() {
            return;
        }
        if extend && let Some(anchor) = self.selection_anchor {
            let (start, end) = if anchor <= index {
                (anchor, index)
            } else {
                (index, anchor)
            };
            self.selected = (start..=end).collect();
        } else if toggle {
            if !self.selected.remove(&index) {
                self.selected.insert(index);
                self.selection_anchor = Some(index);
            }
        } else {
            self.selected.clear();
            self.selected.insert(index);
            self.selection_anchor = Some(index);
        }
    }

    fn select_all(&mut self) -> bool {
        if self.rows.is_empty() || self.selected.len() == self.rows.len() {
            return false;
        }
        self.selected = (0..self.rows.len()).collect();
        self.selection_anchor = Some(0);
        true
    }

    fn clear_selection(&mut self) -> bool {
        if self.selected.is_empty() {
            return false;
        }
        self.selected.clear();
        self.selection_anchor = None;
        true
    }

    fn target_rows(&self, index: usize) -> Vec<usize> {
        if self.selected.len() > 1 && self.selected.contains(&index) {
            self.selected.iter().copied().collect()
        } else {
            vec![index]
        }
    }

    fn set_key(&mut self, index: usize, key: String) {
        for index in self.target_rows(index) {
            if let Some(row) = self.rows.get_mut(index) {
                row.key = key.clone();
            }
        }
    }

    fn set_enabled(&mut self, index: usize, enabled: bool) {
        for index in self.target_rows(index) {
            if let Some(row) = self.rows.get_mut(index) {
                row.enabled = enabled;
            }
        }
    }

    fn assignments(&self) -> Vec<FolderTagAssignment> {
        self.rows
            .iter()
            .map(|row| FolderTagAssignment {
                value: row.value.clone(),
                key: row.key.clone(),
                enabled: row.enabled,
            })
            .collect()
    }
}

impl UI {
    pub(super) fn open_folder_tag_modal(&mut self, cx: &mut Context<Self>) {
        let category = self.library.read(cx).active();
        let values = self
            .library
            .update(cx, |lib, cx| lib.prepare_folder_tag_values(cx));
        let keys = self
            .library
            .read(cx)
            .active_state()
            .schema
            .keys()
            .cloned()
            .collect();
        self.folder_tag_modal = Some(FolderTagModalState::new(category, values, keys));
        cx.notify();
    }

    pub(super) fn close_folder_tag_modal(&mut self, cx: &mut Context<Self>) {
        if self.folder_tag_modal.take().is_some() {
            cx.notify();
        }
    }

    pub(super) fn select_all_folder_tag_rows(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(modal) = self.folder_tag_modal.as_mut() else {
            return false;
        };
        if modal.select_all() {
            cx.notify();
            return true;
        }
        false
    }

    pub(super) fn clear_folder_tag_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(modal) = self.folder_tag_modal.as_mut() else {
            return false;
        };
        if modal.clear_selection() {
            cx.notify();
            return true;
        }
        false
    }

    pub(super) fn apply_folder_tag_modal(&mut self, cx: &mut Context<Self>) {
        let Some(modal) = self.folder_tag_modal.take() else {
            return;
        };
        let category = modal.category;
        let assignments = modal.assignments();
        self.library.update(cx, |lib, cx| {
            lib.assign_folder_tags(category, assignments, cx)
        });
        cx.notify();
    }

    fn select_folder_tag_row(
        &mut self,
        index: usize,
        event: &MouseDownEvent,
        cx: &mut Context<Self>,
    ) {
        if let Some(modal) = self.folder_tag_modal.as_mut() {
            modal.select_row(index, event.modifiers.shift, event.modifiers.platform);
            modal.close_key_menu();
            cx.notify();
        }
    }

    fn toggle_folder_tag_key_menu(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        width: Pixels,
        cx: &mut Context<Self>,
    ) {
        if let Some(modal) = self.folder_tag_modal.as_mut() {
            if modal.open_key_menu == Some(index) {
                modal.open_key_menu = None;
                modal.key_menu_position = None;
            } else {
                modal.open_key_menu = Some(index);
                modal.key_menu_position = Some(Point {
                    x: position.x - width / 2.,
                    y: position.y + FOLDER_TAG_KEY_BUTTON_HEIGHT / 2.,
                });
            }
            cx.notify();
        }
    }

    pub(super) fn close_folder_tag_key_menu(&mut self, cx: &mut Context<Self>) -> bool {
        if let Some(modal) = self.folder_tag_modal.as_mut()
            && modal.close_key_menu()
        {
            cx.notify();
            return true;
        }
        false
    }

    fn set_folder_tag_key(&mut self, index: usize, key: String, cx: &mut Context<Self>) {
        if let Some(modal) = self.folder_tag_modal.as_mut() {
            modal.set_key(index, key);
            modal.close_key_menu();
            cx.notify();
        }
    }

    fn set_folder_tag_enabled(&mut self, index: usize, enabled: bool, cx: &mut Context<Self>) {
        if let Some(modal) = self.folder_tag_modal.as_mut() {
            modal.set_enabled(index, enabled);
            cx.notify();
        }
    }

    pub(super) fn render_folder_tag_modal(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let modal = self.folder_tag_modal.as_ref()?;
        let category = modal.category;
        let rows = modal.rows.clone();
        let selected = modal.selected.clone();
        let open_key_menu = modal.open_key_menu;
        let key_menu_position = modal.key_menu_position;
        let keys = modal.tag_keys();
        let can_choose_key = keys.len() > 1;
        let can_apply = rows.iter().any(|row| row.enabled);
        let modal_height = (window.bounds().size.height - px(64.)).max(px(220.));
        let tag_key_column_width = folder_tag_key_column_width(window, &keys);
        let apply_column_width = folder_tag_apply_column_width(window);
        let control_column_gap = folder_tag_text_width(window, "  ");
        let window_size = window.bounds().size;
        let key_menu_overlay =
            open_key_menu
                .zip(key_menu_position)
                .and_then(|(menu_index, position)| {
                    let row_key = rows.get(menu_index)?.key.clone();
                    let key_menu =
                        folder_tag_key_menu(menu_index, &row_key, &keys, tag_key_column_width, cx);
                    Some(
                        deferred(
                            anchored().child(
                                div()
                                    .w(window_size.width)
                                    .h(window_size.height)
                                    .occlude()
                                    .on_mouse_down(
                                        MouseButton::Left,
                                        cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                            this.close_folder_tag_key_menu(cx);
                                            cx.stop_propagation();
                                        }),
                                    )
                                    .child(
                                        anchored()
                                            .position(position)
                                            .snap_to_window_with_margin(px(8.))
                                            .anchor(Anchor::TopLeft)
                                            .child(key_menu),
                                    ),
                            ),
                        )
                        .with_priority(1)
                        .into_any_element(),
                    )
                });

        let row_elements = if rows.is_empty() {
            vec![
                div()
                    .w_full()
                    .px_2()
                    .py_3()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child("No folder values found.")
                    .into_any_element(),
            ]
        } else {
            let header = TableHeader::new().child(
                TableRow::new()
                    .child(
                        TableHead::new()
                            .flex_1()
                            .min_w_0()
                            .px(px(0.))
                            .py(px(0.))
                            .child(
                                div()
                                    .h(FOLDER_TAG_HEADER_HEIGHT)
                                    .h_flex()
                                    .items_center()
                                    .px_2()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Folder"),
                            ),
                    )
                    .child(
                        TableHead::new()
                            .flex_none()
                            .w(tag_key_column_width)
                            .min_w(tag_key_column_width)
                            .text_center()
                            .px(px(0.))
                            .py(px(0.))
                            .child(
                                div()
                                    .w_full()
                                    .h(FOLDER_TAG_HEADER_HEIGHT)
                                    .h_flex()
                                    .items_center()
                                    .justify_center()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Tag key"),
                            ),
                    )
                    .child(
                        TableHead::new()
                            .flex_none()
                            .w(control_column_gap)
                            .min_w(control_column_gap)
                            .px(px(0.))
                            .py(px(0.))
                            .child(div().h(FOLDER_TAG_HEADER_HEIGHT)),
                    )
                    .child(
                        TableHead::new()
                            .flex_none()
                            .w(apply_column_width)
                            .min_w(apply_column_width)
                            .text_center()
                            .px(px(0.))
                            .py(px(0.))
                            .child(
                                div()
                                    .w_full()
                                    .h(FOLDER_TAG_HEADER_HEIGHT)
                                    .h_flex()
                                    .items_center()
                                    .justify_center()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Apply"),
                            ),
                    ),
            );
            let mut body = TableBody::new();

            for (index, row) in rows.iter().enumerate() {
                let selected_row = selected.contains(&index);
                let row_value = row.value.clone();
                let row_key = row.key.clone();
                let row_enabled = row.enabled;
                let key_button = Button::new(SharedString::from(format!("folder-tag-key:{index}")))
                    .xsmall()
                    .compact()
                    .label(row_key.clone())
                    .icon(IconName::ChevronDown)
                    .when(!can_choose_key, |button| button.disabled(true))
                    .when(can_choose_key, |button| {
                        button.on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                                this.toggle_folder_tag_key_menu(
                                    index,
                                    event.position,
                                    tag_key_column_width,
                                    cx,
                                );
                                cx.stop_propagation();
                            }),
                        )
                    });

                let checkbox = div()
                    .id(SharedString::from(format!("folder-tag-enabled:{index}")))
                    .size_5()
                    .flex_shrink_0()
                    .h_flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .border_1()
                    .border_color(cx.theme().border)
                    .cursor_pointer()
                    .when(row_enabled, |el| {
                        el.bg(cx.theme().primary)
                            .border_color(cx.theme().primary)
                            .child(
                                Icon::new(IconName::Check)
                                    .small()
                                    .text_color(cx.theme().primary_foreground),
                            )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            this.set_folder_tag_enabled(index, !row_enabled, cx);
                            cx.stop_propagation();
                        }),
                    );

                body = body.child(
                    TableRow::new()
                        .when(selected_row, |row| row.bg(cx.theme().accent))
                        .child(
                            TableCell::new()
                                .flex_1()
                                .min_w_0()
                                .px(px(0.))
                                .py(px(0.))
                                .child(
                                    folder_tag_row_cell(
                                        index,
                                        div().w_full().h_flex().items_center().px_2(),
                                        cx,
                                    )
                                    .child(div().min_w_0().truncate().text_sm().child(row_value)),
                                ),
                        )
                        .child(
                            TableCell::new()
                                .flex_none()
                                .w(tag_key_column_width)
                                .min_w(tag_key_column_width)
                                .px(px(0.))
                                .py(px(0.))
                                .child(
                                    folder_tag_row_cell(
                                        index,
                                        div().h_flex().items_center().justify_center(),
                                        cx,
                                    )
                                    .child(key_button),
                                ),
                        )
                        .child(
                            TableCell::new()
                                .flex_none()
                                .w(control_column_gap)
                                .min_w(control_column_gap)
                                .px(px(0.))
                                .py(px(0.))
                                .child(folder_tag_row_cell(index, div(), cx)),
                        )
                        .child(
                            TableCell::new()
                                .flex_none()
                                .w(apply_column_width)
                                .min_w(apply_column_width)
                                .px(px(0.))
                                .py(px(0.))
                                .child(
                                    folder_tag_row_cell(
                                        index,
                                        div().h_flex().items_center().justify_center(),
                                        cx,
                                    )
                                    .child(checkbox),
                                ),
                        ),
                );
            }

            vec![Table::new().child(header).child(body).into_any_element()]
        };

        Some(
            div()
                .id("folder-tag-assignment-overlay")
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .bg(rgba(0x00000099))
                .occlude()
                .hover(|style| style)
                .on_any_mouse_down(cx.listener(|this, _, _, cx| {
                    this.close_folder_tag_modal(cx);
                }))
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
                .children(key_menu_overlay)
                .child(
                    div()
                        .size_full()
                        .h_flex()
                        .items_center()
                        .justify_center()
                        .child(
                            div()
                                .w(px(520.))
                                .max_w(px(640.))
                                .h(modal_height)
                                .max_h(modal_height)
                                .v_flex()
                                .gap_3()
                                .p_4()
                                .overflow_hidden()
                                .rounded(px(8.))
                                .border_1()
                                .border_color(cx.theme().border)
                                .bg(cx.theme().popover)
                                .shadow_lg()
                                .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
                                .child(
                                    div()
                                        .v_flex()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::BOLD)
                                                .child("Assign Folder Tags"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(SharedString::from(format!(
                                                    "{} folder values",
                                                    category.label()
                                                ))),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_h_0()
                                        .min_h(px(72.))
                                        .overflow_hidden()
                                        .child(
                                            div()
                                                .size_full()
                                                .overflow_y_scrollbar()
                                                .v_flex()
                                                .pr(FOLDER_TAG_SCROLLBAR_GUTTER)
                                                .children(row_elements),
                                        ),
                                )
                                .child(
                                    div()
                                        .h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("folder-tag-cancel")
                                                .small()
                                                .label("Cancel")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.close_folder_tag_modal(cx);
                                                })),
                                        )
                                        .child(
                                            Button::new("folder-tag-apply")
                                                .small()
                                                .primary()
                                                .disabled(!can_apply)
                                                .label("Assign Tags")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.apply_folder_tag_modal(cx);
                                                })),
                                        ),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }
}

fn folder_tag_menu_row() -> gpui::Div {
    div()
        .h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_2()
        .h(px(26.))
        .px_2()
        .text_sm()
        .cursor_pointer()
}

fn folder_tag_row_cell(index: usize, el: gpui::Div, cx: &mut Context<UI>) -> gpui::Div {
    el.w_full()
        .h(FOLDER_TAG_ROW_HEIGHT)
        .cursor_pointer()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.select_folder_tag_row(index, event, cx);
            }),
        )
}

fn folder_tag_key_menu(
    index: usize,
    selected_key: &str,
    keys: &[String],
    width: Pixels,
    cx: &mut Context<UI>,
) -> gpui::Div {
    let mut key_menu = div()
        .popover_style(cx)
        .w(width)
        .min_w(width)
        .p_1()
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            cx.stop_propagation();
        });

    for key in keys {
        let key_label = key.clone();
        let is_selected = selected_key == key_label;
        key_menu = key_menu.child(
            folder_tag_menu_row()
                .id(SharedString::from(format!(
                    "folder-tag-key-option:{index}:{key_label}"
                )))
                .hover(|el| el.bg(cx.theme().accent))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.set_folder_tag_key(index, key_label.clone(), cx);
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(key.clone())),
                )
                .when(is_selected, |el| {
                    el.child(
                        Icon::new(IconName::Check)
                            .small()
                            .text_color(cx.theme().primary),
                    )
                }),
        );
    }

    key_menu
}

fn folder_tag_key_column_width(window: &mut Window, keys: &[String]) -> Pixels {
    let header_width = folder_tag_text_width(window, "Tag key");
    let widest_button_width = keys
        .iter()
        .map(|key| folder_tag_text_width(window, key) + FOLDER_TAG_KEY_BUTTON_CHROME)
        .max_by(|a, b| a.as_f32().total_cmp(&b.as_f32()))
        .unwrap_or(FOLDER_TAG_CHECKBOX_SIZE);

    folder_tag_widest_width([header_width, widest_button_width])
}

fn folder_tag_apply_column_width(window: &mut Window) -> Pixels {
    folder_tag_widest_width([
        folder_tag_text_width(window, "Apply"),
        FOLDER_TAG_CHECKBOX_SIZE,
    ])
}

fn folder_tag_widest_width(widths: impl IntoIterator<Item = Pixels>) -> Pixels {
    widths
        .into_iter()
        .max_by(|a, b| a.as_f32().total_cmp(&b.as_f32()))
        .unwrap_or(px(0.))
}

fn folder_tag_text_width(window: &mut Window, label: &str) -> Pixels {
    let text_style = window.text_style();
    let font_size = text_style.font_size.to_pixels(window.rem_size());
    let shaped = window.text_system().shape_line(
        label.into(),
        font_size,
        &[text_style.to_run(label.len())],
        None,
    );
    shaped.width
}

#[cfg(test)]
mod tests {
    use super::*;

    fn music_keys() -> Vec<String> {
        vec!["genre".to_string(), "mood".to_string()]
    }

    #[test]
    fn folder_tag_modal_bulk_edits_selected_rows() {
        let mut modal = FolderTagModalState::new(
            Category::Music,
            vec![
                "ambient".to_string(),
                "dark".to_string(),
                "drone".to_string(),
            ],
            music_keys(),
        );

        modal.select_row(0, false, false);
        modal.select_row(2, true, false);
        modal.set_key(1, "mood".to_string());
        assert_eq!(modal.rows[0].key, "mood");
        assert_eq!(modal.rows[1].key, "mood");
        assert_eq!(modal.rows[2].key, "mood");

        modal.set_enabled(2, false);
        assert!(!modal.rows[0].enabled);
        assert!(!modal.rows[1].enabled);
        assert!(!modal.rows[2].enabled);
    }

    #[test]
    fn folder_tag_modal_edits_unselected_row_alone() {
        let mut modal = FolderTagModalState::new(
            Category::Music,
            vec![
                "ambient".to_string(),
                "dark".to_string(),
                "drone".to_string(),
            ],
            music_keys(),
        );

        modal.select_row(0, false, false);
        modal.select_row(1, false, true);
        modal.set_key(2, "mood".to_string());

        assert_eq!(modal.rows[0].key, "genre");
        assert_eq!(modal.rows[1].key, "genre");
        assert_eq!(modal.rows[2].key, "mood");
    }

    #[test]
    fn folder_tag_modal_select_all_and_clear_selection() {
        let mut modal = FolderTagModalState::new(
            Category::Music,
            vec![
                "ambient".to_string(),
                "dark".to_string(),
                "drone".to_string(),
            ],
            music_keys(),
        );

        assert!(modal.select_all());
        assert_eq!(modal.selected, BTreeSet::from([0, 1, 2]));
        assert!(!modal.select_all());
        assert!(modal.clear_selection());
        assert!(modal.selected.is_empty());
        assert!(!modal.clear_selection());
    }

    #[test]
    fn folder_tag_modal_closes_key_menu_before_selection() {
        let mut modal = FolderTagModalState::new(
            Category::Music,
            vec!["ambient".to_string(), "dark".to_string()],
            music_keys(),
        );

        modal.select_all();
        modal.open_key_menu = Some(0);
        modal.key_menu_position = Some(Point {
            x: px(10.),
            y: px(20.),
        });

        assert!(modal.close_key_menu());
        assert_eq!(modal.selected, BTreeSet::from([0, 1]));
        assert_eq!(modal.open_key_menu, None);
        assert_eq!(modal.key_menu_position, None);
        assert!(!modal.close_key_menu());
    }

    #[test]
    fn folder_tag_modal_uses_schema_keys() {
        let modal = FolderTagModalState::new(
            Category::Music,
            vec!["ambient".to_string()],
            vec![
                "Energy".to_string(),
                "genre".to_string(),
                "mood".to_string(),
            ],
        );

        assert_eq!(
            modal.tag_keys(),
            vec![
                "Energy".to_string(),
                "genre".to_string(),
                "mood".to_string()
            ]
        );
        assert_eq!(modal.rows[0].key, "Energy");
    }
}
