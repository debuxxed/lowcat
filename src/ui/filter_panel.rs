use gpui::{
    Anchor, AnyElement, Context, Entity, InteractiveElement as _, IntoElement, Keystroke,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Window, anchored, deferred, div,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt,
    button::{Button, ButtonVariants as _},
    kbd::Kbd,
};

use crate::library::{
    Library, tag_matches_search, tag_search_group_sort_key, tag_search_match_sort_key,
};
use crate::ui::{CONTENT_PX, ROW_PANEL_HEIGHT};

pub struct FilterPanel {
    library: Entity<Library>,
    tag_group_menu_position: Option<Point<Pixels>>,
    hovered_tag_group_menu_item: Option<TagGroupMenuHover>,
}

#[derive(Clone, PartialEq, Eq)]
enum TagGroupMenuHover {
    Key(String),
    ToggleAll,
}

impl FilterPanel {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self {
            library,
            tag_group_menu_position: None,
            hovered_tag_group_menu_item: None,
        }
    }

    fn open_tag_group_menu(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.prevent_default();
        self.tag_group_menu_position = Some(position);
        self.hovered_tag_group_menu_item = None;
        cx.notify();
    }

    fn close_tag_group_menu(&mut self, cx: &mut Context<Self>) -> bool {
        let closed = self.tag_group_menu_position.take().is_some()
            || self.hovered_tag_group_menu_item.take().is_some();
        if closed {
            cx.notify();
        }
        closed
    }

    pub(crate) fn cancel_tag_group_menu(&mut self, cx: &mut Context<Self>) -> bool {
        self.close_tag_group_menu(cx)
    }

    fn set_tag_group_menu_hovered(
        &mut self,
        target: TagGroupMenuHover,
        hovered: bool,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            if self.hovered_tag_group_menu_item.as_ref() != Some(&target) {
                self.hovered_tag_group_menu_item = Some(target);
                cx.notify();
            }
        } else if self.hovered_tag_group_menu_item.as_ref() == Some(&target) {
            self.hovered_tag_group_menu_item = None;
            cx.notify();
        }
    }

    fn tag_group_menu_row(label: impl Into<SharedString>, checked: bool) -> gpui::Div {
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

    fn tag_group_menu_action_row(label: impl Into<SharedString>) -> gpui::Div {
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

    fn render_tag_group_menu(
        &self,
        keys: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let position = self.tag_group_menu_position?;
        let window_size = window.bounds().size;
        let all_visible = {
            let library = self.library.read(cx);
            keys.iter().all(|key| library.tag_group_is_visible(key))
        };
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
                    .child("No tag groups"),
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
                    .child("Tag Groups"),
            );

            for key in &keys {
                let checked = self.library.read(cx).tag_group_is_visible(key);
                let item_key = key.clone();
                let hover_target = TagGroupMenuHover::Key(key.clone());
                let row_hovered = self.hovered_tag_group_menu_item.as_ref() == Some(&hover_target);
                rows = rows.child(
                    Self::tag_group_menu_row(SharedString::from(key.clone()), checked)
                        .id(SharedString::from(format!("tag-group-menu-row:{key}")))
                        .when(row_hovered, |el| {
                            el.bg(cx.theme().accent)
                                .text_color(cx.theme().accent_foreground)
                        })
                        .on_hover(cx.listener({
                            let hover_target = hover_target.clone();
                            move |this, hovered: &bool, _, cx| {
                                this.set_tag_group_menu_hovered(hover_target.clone(), *hovered, cx);
                            }
                        }))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, window, cx| {
                                window.prevent_default();
                                this.library.update(cx, |lib, cx| {
                                    lib.toggle_tag_group_visibility(&item_key, cx);
                                });
                                window.refresh();
                                cx.stop_propagation();
                            }),
                        )
                        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation()),
                );
            }

            let toggle_hovered =
                self.hovered_tag_group_menu_item.as_ref() == Some(&TagGroupMenuHover::ToggleAll);

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
                    Self::tag_group_menu_action_row(toggle_label)
                        .id("tag-group-menu-toggle-all")
                        .when(toggle_hovered, |el| {
                            el.bg(cx.theme().accent)
                                .text_color(cx.theme().accent_foreground)
                        })
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            this.set_tag_group_menu_hovered(
                                TagGroupMenuHover::ToggleAll,
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
                                    this.library.update(cx, |lib, cx| {
                                        if all_visible {
                                            lib.hide_all_tag_groups(&keys, cx);
                                        } else {
                                            lib.show_all_tag_groups(cx);
                                        }
                                    });
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
                                this.close_tag_group_menu(cx);
                            }),
                        )
                        .child(
                            anchored()
                                .position(position)
                                .snap_to_window_with_margin(px(8.))
                                .anchor(Anchor::TopLeft)
                                .child(
                                    div()
                                        .id("tag-group-menu")
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
}

impl Render for FilterPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render_start = crate::perf::start();
        let (schema, selected, tag_search, single_match) = {
            let library = self.library.read(cx);
            let state = library.active_state();
            (
                state.schema.clone(),
                state.selected.clone(),
                library.tag_search().to_string(),
                library.single_tag_search_match(),
            )
        };
        let keys: Vec<String> = schema.keys().cloned().collect();
        let tag_group_menu = self.render_tag_group_menu(keys.clone(), window, cx);
        let include_hidden_groups = !tag_search.is_empty();

        let mut panel = div()
            .v_flex()
            .w_full()
            .min_h(ROW_PANEL_HEIGHT)
            .px(CONTENT_PX)
            .py_1()
            .gap_2()
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.open_tag_group_menu(event.position, window, cx);
                    cx.stop_propagation();
                }),
            );

        let mut matching_groups = Vec::new();
        for (key, values) in &schema {
            if !include_hidden_groups && !self.library.read(cx).tag_group_is_visible(key) {
                continue;
            }

            let checked: Vec<String> = selected
                .get(key)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();

            let mut matching_values: Vec<&String> = values
                .iter()
                .filter(|value| tag_matches_search(value, &tag_search))
                .collect();
            if matching_values.is_empty() {
                continue;
            }
            matching_values.sort_by_key(|value| tag_search_match_sort_key(value, &tag_search));
            let group_sort_key = tag_search_group_sort_key(
                key,
                matching_values.iter().map(|value| value.as_str()),
                &tag_search,
            );
            matching_groups.push((group_sort_key, key, checked, matching_values));
        }

        matching_groups.sort_by_key(|(sort_key, _, _, _)| sort_key.clone());

        for (_, key, checked, matching_values) in matching_groups {
            let mut group = div()
                .h_flex()
                .flex_wrap()
                .w_full()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .id(SharedString::from(format!("filter-key:{key}")))
                        .h_flex()
                        .flex_shrink_0()
                        .items_center()
                        .gap_1()
                        .mr_1()
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(SharedString::from(format!("{key}:"))),
                        ),
                );

            for value in matching_values {
                let is_active = checked.contains(value);
                let is_single_match =
                    single_match
                        .as_ref()
                        .is_some_and(|(match_key, match_value)| {
                            match_key == key && match_value == value
                        });
                let key_owned = key.clone();
                let value_owned = value.to_string();
                let chip_border = if is_single_match {
                    cx.theme().success
                } else if is_active {
                    cx.theme().primary
                } else {
                    cx.theme().border
                };

                group = group.child(
                    Button::new(format!("filter-{key}:{value}"))
                        .xsmall()
                        .compact()
                        .border_1()
                        .border_color(chip_border)
                        .label(value.to_string())
                        .selected(is_active)
                        .when(is_active, |button| button.primary())
                        .when(is_single_match, |button| {
                            button.child(Kbd::new(
                                Keystroke::parse("enter").expect("valid keystroke"),
                            ))
                        })
                        .on_mouse_down(MouseButton::Right, |_, _, cx| {
                            cx.stop_propagation();
                        })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let key = key_owned.clone();
                            let value = value_owned.clone();
                            this.library.update(cx, |lib, cx| {
                                lib.toggle_value(&key, &value, cx);
                            });
                        })),
                );
            }

            panel = panel.child(group);
        }

        crate::perf::finish("filter_panel.render", render_start, || {
            format!(
                "keys={} values={}",
                schema.len(),
                schema.values().map(Vec::len).sum::<usize>()
            )
        });
        panel.when_some(tag_group_menu, |panel, menu| panel.child(menu))
    }
}
