use gpui::{
    Anchor, Context, Entity, FocusHandle, InteractiveElement, IntoElement, KeyDownEvent,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, anchored, deferred, div, prelude::FluentBuilder,
    px, relative,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Selectable, Sizable, StyledExt, button::Button,
};

use crate::library::Library;
use crate::model::{AudioFormat, ConvertConflictBehavior};

const MENU_ROW_HEIGHT_PX: f32 = 28.;
const MENU_ROW_PADDING_PX: f32 = 16.;
const MENU_PANEL_PADDING_PX: f32 = 8.;
const MENU_ROW_GAP_PX: f32 = 12.;
const MENU_AFFORDANCE_PX: f32 = 18.;
const PRIORITY_ACTIONS_PX: f32 = 56.;

pub struct SettingsMenu {
    library: Entity<Library>,
    open: bool,
    position: Option<Point<Pixels>>,
    focus: FocusHandle,
    active_submenu: Option<SettingsSubmenu>,
    hovered_priority: Option<AudioFormat>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SettingsSubmenu {
    Priority,
}

impl SettingsMenu {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();

        Self {
            library,
            open: false,
            position: None,
            focus: cx.focus_handle(),
            active_submenu: None,
            hovered_priority: None,
        }
    }

    fn close(&mut self, reason: &'static str) {
        let _ = reason;
        self.open = false;
        self.active_submenu = None;
        self.hovered_priority = None;
    }

    fn activate_submenu(
        &mut self,
        submenu: SettingsSubmenu,
        source: &'static str,
        cx: &mut Context<Self>,
    ) {
        if self.active_submenu == Some(submenu) {
            return;
        }

        let _ = source;
        self.active_submenu = Some(submenu);
        cx.notify();
    }

    fn set_priority_hovered(
        &mut self,
        format: AudioFormat,
        hovered: bool,
        _source: &'static str,
        cx: &mut Context<Self>,
    ) {
        if hovered {
            self.hovered_priority = Some(format);
            cx.notify();
        } else if self.hovered_priority == Some(format) {
            self.hovered_priority = None;
            cx.notify();
        }
    }
}

impl Render for SettingsMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let priority = self.library.read(cx).format_priority().to_vec();
        let metrics = menu_metrics(window, &priority);
        let settings_button = Button::new("settings-button")
            .icon(IconName::Settings)
            .small()
            .selected(self.open)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.open = !this.open;
                    this.position = Some(event.position);
                    if this.open {
                        this.focus.focus(window, cx);
                        this.active_submenu = None;
                    } else {
                        this.close("settings-button");
                    }
                    cx.stop_propagation();
                    cx.notify();
                }),
            );

        let show_priority_menu = self.active_submenu == Some(SettingsSubmenu::Priority);
        let settings_overlay = self.position.map(|position| {
            let settings_library = self.library.clone();
            let settings_focus = self.focus.clone();
            let window_size = window.bounds().size;
            let menu_width = metrics.main_menu_width;
            let priority_menu_width = metrics.priority_menu_width;
            let priority_position = Point {
                x: position.x + menu_width,
                y: position.y,
            };

            let behavior = settings_library.read(cx).convert_conflict_behavior();
            let overwrite_enabled = behavior == ConvertConflictBehavior::Overwrite;
            let overwrite_library = settings_library.clone();
            let main_menu = div()
                .id("library-settings-panel")
                .track_focus(&settings_focus)
                .popover_style(cx)
                .w(menu_width)
                .min_w(menu_width)
                .p_1()
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                    if event.keystroke.key == "escape" {
                        this.close("escape");
                        cx.notify();
                    }
                }))
                .child(
                    menu_row()
                        .id("settings-format-priority")
                        .hover(|style| style.bg(cx.theme().accent))
                        .when(show_priority_menu, |style| style.bg(cx.theme().accent))
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.activate_submenu(SettingsSubmenu::Priority, "row", cx);
                            }
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .child("Format priority"),
                        )
                        .child(menu_affordance(Some(
                            Icon::new(IconName::ChevronRight).small().into_any_element(),
                        ))),
                )
                .child(
                    menu_row()
                        .id("convert-overwrite-button")
                        .hover(|style| style.bg(cx.theme().accent))
                        .on_click(move |_, _, cx| {
                            let behavior = if overwrite_enabled {
                                ConvertConflictBehavior::AddCopy
                            } else {
                                ConvertConflictBehavior::Overwrite
                            };
                            overwrite_library.update(cx, |lib, cx| {
                                lib.set_convert_conflict_behavior(behavior, cx);
                            });
                        })
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .child("Overwrite existing target"),
                        )
                        .child(menu_affordance(overwrite_enabled.then(|| {
                            Icon::new(IconName::Check)
                                .small()
                                .text_color(cx.theme().primary)
                                .into_any_element()
                        }))),
                );

            let first_priority = priority.first().copied();
            let last_priority = priority.last().copied();
            let mut priority_list = div().v_flex().w_full().items_stretch().gap_1();
            for format in priority {
                let is_top = first_priority == Some(format);
                let is_bottom = last_priority == Some(format);
                let top_library = settings_library.clone();
                let bottom_library = settings_library.clone();
                let row_hovered = self.hovered_priority == Some(format);
                priority_list = priority_list.child(
                    menu_row()
                        .id(SharedString::from(format!(
                            "priority-row:{}",
                            format.extension()
                        )))
                        .hover(|style| style.bg(cx.theme().accent))
                        .when(row_hovered, |style| style.bg(cx.theme().accent))
                        .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                            this.set_priority_hovered(format, *hovered, "row", cx);
                        }))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .whitespace_nowrap()
                                .child(SharedString::from(format.label())),
                        )
                        .child(
                            div()
                                .h_flex()
                                .items_center()
                                .flex_shrink_0()
                                .gap_0()
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "priority-top:{}",
                                            format.extension()
                                        )))
                                        .size_6()
                                        .h_flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_sm()
                                        .when(!is_top, |style| style.cursor_pointer())
                                        .when(is_top, |style| style.opacity(0.35))
                                        .on_hover(cx.listener(
                                            move |this, hovered: &bool, _, cx| {
                                                this.set_priority_hovered(
                                                    format, *hovered, "up", cx,
                                                );
                                            },
                                        ))
                                        .when(!is_top, |style| {
                                            style.on_click(move |_, _, cx| {
                                                top_library.update(cx, |lib, cx| {
                                                    lib.move_format_priority_up(format, cx);
                                                });
                                            })
                                        })
                                        .child(Icon::new(IconName::ArrowUp).small()),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!(
                                            "priority-bottom:{}",
                                            format.extension()
                                        )))
                                        .size_6()
                                        .h_flex()
                                        .items_center()
                                        .justify_center()
                                        .rounded_sm()
                                        .when(!is_bottom, |style| style.cursor_pointer())
                                        .when(is_bottom, |style| style.opacity(0.35))
                                        .on_hover(cx.listener(
                                            move |this, hovered: &bool, _, cx| {
                                                this.set_priority_hovered(
                                                    format, *hovered, "down", cx,
                                                );
                                            },
                                        ))
                                        .when(!is_bottom, |style| {
                                            style.on_click(move |_, _, cx| {
                                                bottom_library.update(cx, |lib, cx| {
                                                    lib.move_format_priority_down(format, cx);
                                                });
                                            })
                                        })
                                        .child(Icon::new(IconName::ArrowDown).small()),
                                ),
                        ),
                );
            }
            let priority_menu = div()
                .id("library-settings-priority-submenu")
                .popover_style(cx)
                .w(priority_menu_width)
                .min_w(priority_menu_width)
                .p_1()
                .on_mouse_down(MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .child(priority_list);

            let submenu_overlay = anchored()
                .position(priority_position)
                .snap_to_window_with_margin(px(8.))
                .anchor(Anchor::TopLeft)
                .child(priority_menu);

            let priority_hover_bridge = anchored()
                .position(Point {
                    x: position.x,
                    y: position.y + px(4.),
                })
                .anchor(Anchor::TopLeft)
                .child(
                    div()
                        .id("settings-format-priority-hover-bridge")
                        .w(menu_width)
                        .h(px(MENU_PANEL_PADDING_PX + MENU_ROW_HEIGHT_PX))
                        .cursor_pointer()
                        .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.activate_submenu(SettingsSubmenu::Priority, "bridge", cx);
                            }
                        })),
                );

            deferred(
                anchored().child(
                    div()
                        .w(window_size.width)
                        .h(window_size.height)
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.close("outside-click");
                                cx.notify();
                            }),
                        )
                        .child(
                            anchored()
                                .position(position)
                                .snap_to_window_with_margin(px(8.))
                                .anchor(Anchor::TopLeft)
                                .child(main_menu),
                        )
                        .child(priority_hover_bridge)
                        .when(show_priority_menu, |overlay| overlay.child(submenu_overlay)),
                ),
            )
            .with_priority(1)
        });

        div().child(settings_button).when(self.open, |menu| {
            menu.when_some(settings_overlay, |menu, overlay| menu.child(overlay))
        })
    }
}

fn menu_row() -> gpui::Div {
    div()
        .h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_3()
        .h(px(MENU_ROW_HEIGHT_PX))
        .px_2()
        .rounded_md()
        .line_height(relative(1.))
        .whitespace_nowrap()
        .cursor_pointer()
}

fn menu_affordance(child: Option<impl IntoElement>) -> gpui::Div {
    let row = div()
        .w(px(MENU_AFFORDANCE_PX))
        .flex_shrink_0()
        .h_full()
        .h_flex()
        .items_center()
        .justify_end();

    match child {
        Some(child) => row.child(child),
        None => row,
    }
}

#[derive(Debug, Clone)]
struct MenuMetrics {
    main_menu_width: Pixels,
    priority_menu_width: Pixels,
}

fn menu_metrics(window: &mut Window, priority: &[AudioFormat]) -> MenuMetrics {
    let format_priority_row_width =
        text_row_width(window, "Format priority") + menu_row_chrome(px(MENU_AFFORDANCE_PX));
    let overwrite_row_width = text_row_width(window, "Overwrite existing target")
        + menu_row_chrome(px(MENU_AFFORDANCE_PX));
    let priority_rows = priority
        .iter()
        .map(|format| {
            let width = (text_row_width(window, format.label())
                + menu_row_chrome(px(PRIORITY_ACTIONS_PX)))
            .as_f32();
            (format.label().to_string(), width)
        })
        .collect::<Vec<_>>();
    let priority_menu_width = widest_width(
        priority_rows
            .iter()
            .map(|(_, width)| px(*width))
            .collect::<Vec<_>>()
            .into_iter(),
    );

    MenuMetrics {
        main_menu_width: widest_width([format_priority_row_width, overwrite_row_width]),
        priority_menu_width,
    }
}

fn widest_width(widths: impl IntoIterator<Item = Pixels>) -> Pixels {
    widths
        .into_iter()
        .max_by(|a, b| a.as_f32().total_cmp(&b.as_f32()))
        .unwrap_or(px(0.))
}

fn menu_row_chrome(trailing_width: Pixels) -> Pixels {
    px(MENU_PANEL_PADDING_PX + MENU_ROW_PADDING_PX + MENU_ROW_GAP_PX)
        + trailing_width
        + px(MENU_PANEL_PADDING_PX + MENU_ROW_PADDING_PX)
}

fn text_row_width(window: &mut Window, label: &str) -> Pixels {
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

    #[test]
    fn widest_width_returns_largest_value() {
        assert_eq!(widest_width([px(12.), px(48.), px(20.)]), px(48.));
    }

    #[test]
    fn row_chrome_grows_with_larger_trailing_affordance() {
        assert!(menu_row_chrome(px(PRIORITY_ACTIONS_PX)) > menu_row_chrome(px(MENU_AFFORDANCE_PX)));
    }

    #[test]
    fn widest_main_menu_row_drives_menu_width() {
        let format_priority_row_width = px(120.) + menu_row_chrome(px(MENU_AFFORDANCE_PX));
        let overwrite_row_width = px(196.) + menu_row_chrome(px(MENU_AFFORDANCE_PX));

        assert_eq!(
            widest_width([format_priority_row_width, overwrite_row_width]),
            overwrite_row_width
        );
    }
}
