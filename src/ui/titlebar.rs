use std::time::Duration;

use gpui::{
    AsyncApp, Bounds, ClickEvent, Context, DragMoveEvent, Entity, ExternalPaths,
    InteractiveElement as _, IntoElement, MouseButton, ParentElement, PathPromptOptions, Pixels,
    Render, SharedString, StatefulInteractiveElement as _, Styled, Window, div, point,
    prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt,
    button::{Button, ButtonVariants as _},
};

use crate::library::Library;
use crate::model::Category;

/// Left inset of the titlebar category row, leaving room for the traffic
/// lights. The drag-import overlay reuses this so its columns line up with the
/// titlebar categories.
pub(crate) const TITLEBAR_LEFT_OFFSET: Pixels = px(84.);
pub(crate) const TITLEBAR_HEIGHT: Pixels = px(38.);

pub struct AppTitleBar {
    library: Entity<Library>,
    hovered_category: Option<Category>,
    drag_hovered_category: Option<Category>,
    drag_hover_bounds: Option<Bounds<Pixels>>,
    drag_hover_watch_running: bool,
    folder_prompt_active: bool,
    should_move_window: bool,
}

impl AppTitleBar {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();

        Self {
            library,
            hovered_category: None,
            drag_hovered_category: None,
            drag_hover_bounds: None,
            drag_hover_watch_running: false,
            folder_prompt_active: false,
            should_move_window: false,
        }
    }

    fn start_drag_hover_watch(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.drag_hover_watch_running {
            return;
        }

        self.drag_hover_watch_running = true;
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let should_continue = this
                    .update_in(cx, |this, window, cx| {
                        if this.drag_hovered_category.is_none()
                            || !this.library.read(cx).internal_file_drag_active()
                        {
                            this.drag_hovered_category = None;
                            this.drag_hover_bounds = None;
                            this.drag_hover_watch_running = false;
                            cx.notify();
                            return false;
                        }

                        let Some(bounds) = this.drag_hover_bounds else {
                            this.drag_hover_watch_running = false;
                            return false;
                        };

                        let mouse_position = live_window_mouse_position(window);
                        if !bounds.contains(&mouse_position) {
                            this.drag_hovered_category = None;
                            this.drag_hover_bounds = None;
                            this.drag_hover_watch_running = false;
                            debug_titlebar_interaction(|| {
                                format!(
                                    "clear drag hover: watched leave mouse_x={:.1} mouse_y={:.1}",
                                    mouse_position.x.as_f32(),
                                    mouse_position.y.as_f32()
                                )
                            });
                            cx.notify();
                            return false;
                        }

                        true
                    })
                    .ok()
                    .unwrap_or(false);

                if !should_continue {
                    break;
                }
            }
        })
        .detach();
    }

    fn choose_category_folder(
        &mut self,
        category: Category,
        _event: &ClickEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.stop_propagation();
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
}

impl Render for AppTitleBar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render_start = crate::perf::start();
        let active = self.library.read(cx).active();
        let internal_drag_active = self.library.read(cx).internal_file_drag_active();
        if !internal_drag_active && self.drag_hovered_category.take().is_some() {
            self.drag_hover_bounds = None;
            debug_titlebar_interaction(|| "clear drag hover: internal drag inactive".to_string());
            self.drag_hovered_category = None;
        }
        let outline = cx.theme().title_bar_border;
        let selected_bg = outline.opacity(0.16);

        let mut categories = div()
            .h_flex()
            .h(px(37.))
            .w_full()
            .flex_1()
            .min_w_0()
            .mt(px(1.))
            .items_center()
            .overflow_hidden()
            .border_color(outline);
        for category in Category::ALL {
            let selected = category == active;
            let hovered = self.hovered_category == Some(category);
            let missing_folder = self.library.read(cx).category_needs_folder(category);
            let drag_hovered = self.drag_hovered_category == Some(category);
            let bg = if selected {
                selected_bg
            } else {
                cx.theme().background
            };
            let hover_bg = if selected {
                selected_bg
            } else {
                cx.theme().secondary
            };
            let fg = if selected {
                cx.theme().foreground
            } else {
                cx.theme().muted_foreground
            };
            let border = if selected {
                outline
            } else {
                cx.theme().transparent
            };
            let can_hover = !internal_drag_active;
            let drag_bg = cx.theme().secondary;
            let folder_button = Button::new(SharedString::from(format!(
                "category-folder:{}",
                category.label()
            )))
            .icon(IconName::Folder)
            .small()
            .compact()
            .ghost()
            .disabled(self.folder_prompt_active)
            .tooltip(if self.folder_prompt_active {
                SharedString::from("Folder picker is already open")
            } else {
                SharedString::from(format!("Choose {} folder", category.label()))
            })
            .on_click(cx.listener(move |this, event, window, cx| {
                this.choose_category_folder(category, event, window, cx);
            }));

            let show_folder_button = can_hover && (hovered || missing_folder);

            categories = categories.child(
                div()
                    .id(SharedString::from(category.label()))
                    .relative()
                    .h_flex()
                    .h_full()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .justify_center()
                    .bg(bg)
                    .border_l_1()
                    .border_r_1()
                    .border_color(border)
                    .text_sm()
                    .text_color(fg)
                    .cursor_pointer()
                    .child(SharedString::from(category.label()))
                    .when(internal_drag_active && drag_hovered, |this| {
                        this.bg(drag_bg)
                    })
                    .when(can_hover, |this| this.hover(move |this| this.bg(hover_bg)))
                    .when(show_folder_button, |this| {
                        this.child(div().absolute().right(px(6.)).child(folder_button))
                    })
                    .on_drag_move::<ExternalPaths>(cx.listener(
                        move |this, event: &DragMoveEvent<ExternalPaths>, window, cx| {
                            let is_current = this.drag_hovered_category == Some(category);
                            if !this.library.read(cx).internal_file_drag_active() {
                                if is_current {
                                    this.drag_hovered_category = None;
                                    this.drag_hover_bounds = None;
                                    debug_titlebar_interaction(|| {
                                        format!(
                                            "clear drag hover: inactive category={}",
                                            category.label()
                                        )
                                    });
                                    cx.notify();
                                }
                                return;
                            }

                            if event.drag(cx).paths().is_empty() {
                                if is_current {
                                    this.drag_hovered_category = None;
                                    this.drag_hover_bounds = None;
                                    debug_titlebar_interaction(|| {
                                        format!(
                                            "clear drag hover: empty paths category={}",
                                            category.label()
                                        )
                                    });
                                    cx.notify();
                                }
                                return;
                            }

                            if event.bounds.contains(&event.event.position) {
                                this.drag_hover_bounds = Some(event.bounds);
                                this.start_drag_hover_watch(window, cx);
                                if !is_current {
                                    this.drag_hovered_category = Some(category);
                                    debug_titlebar_interaction(|| {
                                        format!(
                                            "set drag hover: category={} x={:.1} y={:.1}",
                                            category.label(),
                                            event.event.position.x.as_f32(),
                                            event.event.position.y.as_f32()
                                        )
                                    });
                                    cx.notify();
                                }
                            } else if is_current {
                                let mouse_position = live_window_mouse_position(window);
                                if !event.bounds.contains(&mouse_position) {
                                    this.drag_hovered_category = None;
                                    this.drag_hover_bounds = None;
                                    debug_titlebar_interaction(|| {
                                        format!(
                                            "clear drag hover: actual leave category={} event_x={:.1} event_y={:.1} mouse_x={:.1} mouse_y={:.1}",
                                            category.label(),
                                            event.event.position.x.as_f32(),
                                            event.event.position.y.as_f32(),
                                            mouse_position.x.as_f32(),
                                            mouse_position.y.as_f32()
                                        )
                                    });
                                    cx.notify();
                                }
                            }
                        },
                    ))
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if internal_drag_active {
                            if this.hovered_category.is_some() {
                                this.hovered_category = None;
                                cx.notify();
                            }
                            return;
                        }

                        if *hovered {
                            this.hovered_category = Some(category);
                        } else if this.hovered_category == Some(category) {
                            this.hovered_category = None;
                        }
                        cx.notify();
                    }))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.click_count() == 1 {
                            this.library
                                .update(cx, |lib, cx| lib.set_category(category, cx));
                        }
                    }))
                    .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                        let paths = paths.paths().to_vec();
                        this.drag_hovered_category = None;
                        this.drag_hover_bounds = None;
                        debug_titlebar_interaction(|| {
                            format!("drop category={} paths={}", category.label(), paths.len())
                        });
                        this.library
                            .update(cx, |lib, cx| lib.import_files(category, paths, cx));
                    })),
            );
        }

        let titlebar = div()
            .h_flex()
            .flex_shrink_0()
            .h(TITLEBAR_HEIGHT)
            .pl(TITLEBAR_LEFT_OFFSET)
            .bg(cx.theme().background)
            .border_b_1()
            .border_color(cx.theme().title_bar_border)
            .on_mouse_down_out(cx.listener(|this, _, _, _| {
                this.should_move_window = false;
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.should_move_window = true;
                }),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.should_move_window = false;
                }),
            )
            .on_mouse_move(cx.listener(|this, _, window, _| {
                if this.should_move_window {
                    this.should_move_window = false;
                    window.start_window_move();
                }
            }))
            .child(categories);

        crate::perf::finish("titlebar.render", render_start, || {
            format!(
                "active={} internal_drag={internal_drag_active}",
                active.label()
            )
        });
        titlebar
    }
}

#[cfg(target_os = "macos")]
fn live_window_mouse_position(window: &Window) -> gpui::Point<Pixels> {
    use cocoa::{appkit::NSEvent as _, base::nil};

    let screen_position = unsafe { cocoa::base::id::mouseLocation(nil) };
    let window_bounds = window.bounds();
    point(
        px(screen_position.x as f32) - window_bounds.left(),
        window_bounds.bottom() - px(screen_position.y as f32),
    )
}

#[cfg(not(target_os = "macos"))]
fn live_window_mouse_position(window: &Window) -> gpui::Point<Pixels> {
    window.mouse_position()
}

fn debug_titlebar_interaction(details: impl FnOnce() -> String) {
    let enabled = std::env::var("LOWCAT_DEBUG")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if enabled {
        eprintln!("[lowcat:titlebar] {}", details());
    }
}
