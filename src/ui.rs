mod filter_panel;
mod table;
mod titlebar;
mod toolbar;

use gpui::{
    App, AppContext, Context, Entity, ExternalPaths, FocusHandle, Focusable, InteractiveElement,
    IntoElement, KeyDownEvent, ModifiersChangedEvent, ParentElement, Pixels, Render, SharedString,
    Styled, Window, actions, div, hsla, prelude::FluentBuilder, px, rgba,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Sizable as _, StyledExt,
    button::{Button, ButtonVariants as _},
    progress::Progress,
};

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
        }
    }
}

impl UI {
    fn cancel_file_drag(&mut self, cx: &mut Context<Self>) {
        self.table
            .update(cx, |table, cx| table.cancel_file_drag(cx));
        cx.notify();
    }

    /// Full-window overlay that fades in while OS files are dragged over the
    /// window, aligned with the titlebar categories.
    fn render_drop_overlay(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let base_bg = rgba(0x70707066);
        let highlight_bg = rgba(0x9a9a9aa6);
        let mut columns = div().h_flex().size_full().pl(TITLEBAR_LEFT_OFFSET);
        for category in Category::ALL {
            let column = div()
                .id(SharedString::from(format!(
                    "drop-overlay:{}",
                    category.label()
                )))
                .h_flex()
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .border_l_1()
                .border_color(rgba(0xffffff22))
                .bg(base_bg)
                .text_color(cx.theme().foreground)
                .child(SharedString::from(category.label()))
                .drag_over::<ExternalPaths>(move |style, paths, _, _| {
                    if paths.paths().is_empty() {
                        style
                    } else {
                        style.bg(highlight_bg)
                    }
                });

            columns = columns.child(column.on_drop(cx.listener(
                move |this, paths: &ExternalPaths, _, cx| {
                    let paths = paths.paths().to_vec();
                    this.library
                        .update(cx, |lib, cx| lib.import_files(category, paths, cx));
                },
            )));
        }

        let overlay = div()
            .id("drop-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .opacity(0.)
            // `drag_over` alone does not register a hitbox. An empty `hover`
            // keeps the overlay detectable without changing layout.
            .hover(|style| style);

        overlay
            .drag_over::<ExternalPaths>(|style, _, _, _| style.opacity(1.))
            .on_drop(cx.listener(|_, _: &ExternalPaths, _, _| {}))
            .child(columns)
    }

    fn render_import_progress_modal(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let progress = self
            .library
            .read(cx)
            .import_progress()
            .cloned()
            .expect("progress modal only renders while import progress exists");
        let percent = progress.progress.round() as u32;

        div()
            .id("import-progress-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .bg(rgba(0x00000099))
            .occlude()
            .hover(|style| style)
            .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
            .on_mouse_move(|_, _, cx| cx.stop_propagation())
            .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
            .on_drop(cx.listener(|_, _: &ExternalPaths, _, cx| {
                cx.stop_propagation();
            }))
            .child(
                div()
                    .size_full()
                    .h_flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .w(px(360.))
                            .max_w(px(520.))
                            .v_flex()
                            .gap_3()
                            .p_4()
                            .rounded(px(8.))
                            .border_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().popover)
                            .shadow_lg()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui::FontWeight::BOLD)
                                    .child("Converting media"),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(progress.file_name),
                            )
                            .child(
                                Progress::new("import-progress")
                                    .small()
                                    .value(progress.progress),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(SharedString::from(format!("{percent}%"))),
                            ),
                    ),
            )
    }

    fn render_suggestion_bar(
        &self,
        unsupported_count: usize,
        busy: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let problem = if unsupported_count == 1 {
            "There is 1 file in unsupported format.".to_string()
        } else {
            format!("There are {unsupported_count} files in unsupported format.")
        };

        div()
            .id("suggestion-bar")
            .flex_shrink_0()
            .h(px(48.))
            .w_full()
            .h_flex()
            .items_center()
            .justify_between()
            .gap_3()
            .px(CONTENT_PX)
            .border_t_1()
            .border_color(cx.theme().border)
            .bg(hsla(0.095, 0.68, 0.22, 0.18))
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .text_color(cx.theme().foreground)
                    .child(SharedString::from(problem)),
            )
            .child(
                Button::new("convert-all-unsupported")
                    .small()
                    .warning()
                    .label("Convert all")
                    .loading(busy)
                    .disabled(busy)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.library
                            .update(cx, |lib, cx| lib.convert_active_unsupported(cx));
                    })),
            )
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
        let internal_drag_active = self.library.read(cx).internal_file_drag_active();
        let import_progress_active = self.library.read(cx).import_progress().is_some();
        let unsupported_count = self.library.read(cx).active_unsupported_count();
        let busy = self.library.read(cx).is_busy();
        let drop_overlay = self.render_drop_overlay(cx);

        div()
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .v_flex()
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
            .when(unsupported_count > 0, |el| {
                el.child(self.render_suggestion_bar(unsupported_count, busy, cx))
            })
            .when(!internal_drag_active, |el| el.child(drop_overlay))
            .when(import_progress_active, |el| {
                el.child(self.render_import_progress_modal(cx))
            })
    }
}
