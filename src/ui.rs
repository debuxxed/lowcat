mod filter_panel;
mod table;
mod titlebar;
mod toolbar;

use gpui::{
    App, AppContext, Context, Entity, ExternalPaths, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ModifiersChangedEvent, MouseMoveEvent, ParentElement, Pixels,
    Render, SharedString, Styled, Window, actions, div, prelude::FluentBuilder, px, rgba,
};
use gpui_component::{ActiveTheme as _, StyledExt};

use crate::model::Category;
use crate::ui::titlebar::TITLEBAR_LEFT_OFFSET;

actions!(library, [NextCategory, PreviousCategory, ToggleFilters]);

/// Horizontal inset shared by all window content (toolbar, filter panel, table
/// cells). Change this in one place to adjust the padding everywhere.
pub(crate) const CONTENT_PX: Pixels = px(12.);

use crate::library::Library;
use crate::ui::{
    filter_panel::FilterPanel, table::FileTable, titlebar::AppTitleBar, toolbar::Toolbar,
};

pub struct UI {
    library: Entity<Library>,
    titlebar: Entity<AppTitleBar>,
    toolbar: Entity<Toolbar>,
    filter_panel: Entity<FilterPanel>,
    table: Entity<FileTable>,
    focus_handle: FocusHandle,
    drop_hover_category: Option<Category>,
}

impl UI {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let library = cx.new(|_| Library::new());
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        // Hold focus at the root so window-level actions (e.g. ToggleFilters)
        // still dispatch when no input is active.
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            titlebar: cx.new(|cx| AppTitleBar::new(library.clone(), cx)),
            toolbar: cx.new(|cx| Toolbar::new(library.clone(), window, cx)),
            filter_panel: cx.new(|cx| FilterPanel::new(library.clone(), cx)),
            table: cx.new(|cx| FileTable::new(library.clone(), window, cx)),
            library,
            focus_handle,
            drop_hover_category: None,
        }
    }
}

impl UI {
    fn category_at_position(window: &Window) -> Option<Category> {
        let x = (window.mouse_position().x - TITLEBAR_LEFT_OFFSET).as_f32();
        let width = (window.viewport_size().width - TITLEBAR_LEFT_OFFSET).as_f32();
        if x < 0. || width <= 0. {
            return None;
        }

        let column_width = width / Category::ALL.len() as f32;
        if column_width <= 0. {
            return None;
        }

        let index = ((x / column_width).floor() as usize).min(Category::ALL.len() - 1);
        Category::ALL.get(index).copied()
    }

    fn update_drop_hover(&mut self, window: &Window, cx: &mut Context<Self>) {
        let category = self
            .library
            .read(cx)
            .internal_file_drag_active()
            .then(|| Self::category_at_position(window))
            .flatten();
        if self.drop_hover_category != category {
            self.drop_hover_category = category;
            cx.notify();
        }
    }

    fn cancel_file_drag(&mut self, cx: &mut Context<Self>) {
        self.drop_hover_category = None;
        self.table
            .update(cx, |table, cx| table.cancel_file_drag(cx));
        cx.notify();
    }

    /// Full-window overlay that fades in while OS files are dragged over the
    /// window, aligned with the titlebar categories.
    fn render_drop_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let internal_drag_active = self.library.read(cx).internal_file_drag_active();
        let overlay_opacity = if internal_drag_active { 1. } else { 0. };
        let base_bg = rgba(0x70707066);
        let highlight_bg = rgba(0x9a9a9aa6);
        let mut columns = div().h_flex().size_full().pl(TITLEBAR_LEFT_OFFSET);
        for category in Category::ALL {
            let bg = if self.drop_hover_category == Some(category) {
                highlight_bg
            } else {
                base_bg
            };
            columns = columns.child(
                div()
                    .id(SharedString::from(format!("drop-overlay:{}", category.label())))
                    .h_flex()
                    .flex_1()
                    .h_full()
                    .items_center()
                    .justify_center()
                    .border_l_1()
                    .border_color(rgba(0xffffff22))
                    .bg(bg)
                    .text_color(cx.theme().foreground)
                    .child(SharedString::from(category.label()))
                    .drag_over::<ExternalPaths>(move |style, paths, _, _| {
                        if !internal_drag_active && !paths.paths().is_empty() {
                            style.bg(rgba(0x9a9a9aa6))
                        } else {
                            style
                        }
                    })
                    .on_drop(cx.listener(move |this, paths: &ExternalPaths, _, cx| {
                        let paths = paths.paths().to_vec();
                        this.drop_hover_category = None;
                        this.library
                            .update(cx, |lib, cx| lib.import_files(category, paths, cx));
                    })),
            );
        }

        div()
            .id("drop-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .opacity(overlay_opacity)
            // `drag_over` alone does not register a hitbox. An empty `hover`
            // keeps the overlay detectable without changing layout.
            .hover(|style| style)
            .on_mouse_move(cx.listener(|this, _: &MouseMoveEvent, window, cx| {
                this.update_drop_hover(window, cx);
            }))
            .drag_over::<ExternalPaths>(move |style, _, _, _| {
                if internal_drag_active {
                    style
                } else {
                    style.opacity(1.)
                }
            })
            .on_drop(cx.listener(|this, _: &ExternalPaths, _, cx| {
                this.drop_hover_category = None;
                this.library
                    .update(cx, |lib, cx| lib.clear_internal_file_drag(cx));
            }))
            .child(columns)
    }
}

impl Focusable for UI {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for UI {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let filters_open = self.library.read(cx).filters_open();
        let drop_overlay = self.render_drop_overlay(cx);

        div()
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .v_flex()
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                if event.dragging() {
                    this.update_drop_hover(window, cx);
                }
            }))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                this.toolbar.update(cx, |toolbar, cx| {
                    toolbar.set_alt_down(event.modifiers.alt, cx)
                });
                this.table
                    .update(cx, |table, cx| table.set_alt_down(event.modifiers.alt, cx));
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "escape" {
                    this.cancel_file_drag(cx);
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleFilters, _, cx| {
                this.library.update(cx, |lib, cx| lib.toggle_filters(cx));
            }))
            .on_action(cx.listener(|this, _: &NextCategory, _, cx| {
                this.library.update(cx, |lib, cx| lib.next_category(cx));
            }))
            .on_action(cx.listener(|this, _: &PreviousCategory, _, cx| {
                this.library.update(cx, |lib, cx| lib.previous_category(cx));
            }))
            .child(self.titlebar.clone())
            .child(self.toolbar.clone())
            .when(filters_open, |el| el.child(self.filter_panel.clone()))
            .child(self.table.clone())
            .child(drop_overlay)
    }
}
