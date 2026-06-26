mod downloader_panel;
mod filter_panel;
mod settings_menu;
mod table;
mod titlebar;
mod toolbar;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, ExternalPaths, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ModifiersChangedEvent, ParentElement, Pixels,
    Render, SharedString, Styled, Window, actions, div, prelude::FluentBuilder, px, rgba,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, StyledExt,
    button::{Button, ButtonVariants as _},
    progress::Progress,
};

use crate::model::Category;
use crate::ui::titlebar::TITLEBAR_LEFT_OFFSET;
#[cfg(target_os = "macos")]
use crate::{CloseWindow, MinimizeWindow};

actions!(
    library,
    [
        NextCategory,
        PreviousCategory,
        ToggleFilters,
        ToggleDownloader
    ]
);

/// Horizontal inset shared by all window content (toolbar, filter panel, table
/// cells). Change this in one place to adjust the padding everywhere.
pub(crate) const CONTENT_PX: Pixels = px(12.);
pub(crate) const ROW_PANEL_HEIGHT: Pixels = px(32.);
const ROW_PANEL_SEPARATOR_HEIGHT: Pixels = px(1.);

use crate::library::Library;
use crate::ui::{
    downloader_panel::DownloaderPanel,
    filter_panel::FilterPanel,
    table::{FileTable, PendingDeleteKind},
    titlebar::AppTitleBar,
    toolbar::Toolbar,
};

pub struct UI {
    library: Entity<Library>,
    titlebar: Entity<AppTitleBar>,
    toolbar: Entity<Toolbar>,
    filter_panel: Entity<FilterPanel>,
    downloader_panel: Entity<DownloaderPanel>,
    table: Entity<FileTable>,
    focus_handle: FocusHandle,
}

impl UI {
    pub fn new(library: Entity<Library>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.library
                    .update(cx, |lib, cx| lib.rescan_after_focus(cx));
            }
        })
        .detach();
        // Hold focus at the root so window-level actions (e.g. ToggleFilters)
        // still dispatch when no input is active.
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        Self {
            titlebar: cx.new(|cx| AppTitleBar::new(library.clone(), cx)),
            toolbar: cx.new(|cx| Toolbar::new(library.clone(), window, cx)),
            filter_panel: cx.new(|cx| FilterPanel::new(library.clone(), cx)),
            downloader_panel: cx.new(|cx| DownloaderPanel::new(library.clone(), cx)),
            table: cx.new(|cx| FileTable::new(library.clone(), window, cx)),
            library,
            focus_handle,
        }
    }
}

impl UI {
    fn should_activate_search(event: &KeyDownEvent) -> bool {
        let modifiers = &event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform || modifiers.function {
            return false;
        }
        if modifiers.shift && event.keystroke.key == "e" {
            return false;
        }

        !matches!(
            event.keystroke.key.as_str(),
            "enter"
                | "escape"
                | "shift"
                | "control"
                | "ctrl"
                | "alt"
                | "cmd"
                | "super"
                | "win"
                | "fn"
        )
    }

    fn cancel_file_drag(&mut self, cx: &mut Context<Self>) {
        self.table
            .update(cx, |table, cx| table.cancel_file_drag(cx));
        cx.notify();
    }

    fn cancel_delete(&mut self, cx: &mut Context<Self>) -> bool {
        self.table.update(cx, |table, cx| table.cancel_delete(cx))
    }

    fn confirm_delete(&mut self, cx: &mut Context<Self>) -> bool {
        let confirmed = self
            .table
            .update(cx, |table, cx| table.confirm_pending_delete(cx));
        if confirmed {
            debug_ui_interaction(|| "enter confirmed delete".to_string());
        }
        confirmed
    }

    fn text_input_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.toolbar.read(cx).search_is_focused(window, cx)
            || self.table.read(cx).tag_editor_is_focused(window, cx)
    }

    fn cancel_search_if_no_selection(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if self
            .table
            .update(cx, |table, cx| table.has_visible_selection(cx))
        {
            return false;
        }

        let cleared = self
            .toolbar
            .update(cx, |toolbar, cx| toolbar.clear_search(window, cx));
        if cleared {
            debug_ui_interaction(|| "escape cleared search".to_string());
        }
        cleared
    }

    fn paste_download_for_active_category(&mut self, cx: &mut Context<Self>) {
        let category = self.library.read(cx).active();
        let clipboard_text = cx.read_from_clipboard().and_then(|item| item.text());
        self.library.update(cx, |lib, cx| {
            lib.download_from_clipboard(category, clipboard_text, cx);
        });
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

    fn render_delete_confirmation_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let counts = self.table.read(cx).pending_delete_counts()?;
        let title = match counts.kind {
            PendingDeleteKind::Rows if counts.row_count == 1 => "Move row to Trash?",
            PendingDeleteKind::Rows => "Move rows to Trash?",
            PendingDeleteKind::Format => "Move format file to Trash?",
        };
        let file_label = pluralize(counts.file_count, "file", "files");
        let description = match counts.kind {
            PendingDeleteKind::Rows => {
                let row_label = pluralize(counts.row_count, "row", "rows");
                format!(
                    "Move {} {} ({} {}) to Trash?",
                    counts.row_count, row_label, counts.file_count, file_label
                )
            }
            PendingDeleteKind::Format => {
                format!("Move {} {} to Trash?", counts.file_count, file_label)
            }
        };

        Some(
            div()
                .id("delete-confirmation-overlay")
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
                                        .child(title),
                                )
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(description),
                                )
                                .child(
                                    div()
                                        .h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("delete-cancel")
                                                .small()
                                                .label("Cancel")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.cancel_delete(cx);
                                                })),
                                        )
                                        .child(
                                            Button::new("delete-confirm")
                                                .small()
                                                .danger()
                                                .label("Move to Trash")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.table.update(cx, |table, cx| {
                                                        table.confirm_pending_delete(cx);
                                                    });
                                                })),
                                        ),
                                ),
                        ),
                )
                .into_any_element(),
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
        crate::perf::sample("ui.render.rate");
        let render_start = crate::perf::start();
        let filters_open = self.library.read(cx).filters_open();
        let downloader_open = self.library.read(cx).downloader_open();
        let internal_drag_active = self.library.read(cx).internal_file_drag_active();
        let import_progress_active = self.library.read(cx).import_progress().is_some();
        let drop_overlay = self.render_drop_overlay(cx);
        crate::perf::finish("ui.render", render_start, || {
            format!(
                "filters_open={filters_open} downloader_open={downloader_open} import_progress={import_progress_active}"
            )
        });

        let root = div()
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
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if event.keystroke.key == "escape" {
                    if this.cancel_delete(cx) {
                        cx.stop_propagation();
                        return;
                    }
                    if this.cancel_search_if_no_selection(window, cx) {
                        cx.stop_propagation();
                        return;
                    }
                    this.cancel_file_drag(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "enter" && this.confirm_delete(cx) {
                    cx.stop_propagation();
                } else if this.library.read(cx).downloader_open()
                    && event.keystroke.modifiers.platform
                    && event.keystroke.key == "v"
                    && !this.text_input_is_focused(window, cx)
                {
                    this.paste_download_for_active_category(cx);
                    cx.stop_propagation();
                } else if Self::should_activate_search(event)
                    && !this.text_input_is_focused(window, cx)
                {
                    let keystroke = event.keystroke.clone();
                    this.toolbar
                        .update(cx, |toolbar, cx| toolbar.focus_search(window, cx));
                    cx.stop_propagation();
                    window.defer(cx, move |window, cx| {
                        window.dispatch_keystroke(keystroke, cx);
                    });
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleFilters, _, cx| {
                this.library.update(cx, |lib, cx| lib.toggle_filters(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleDownloader, _, cx| {
                this.library.update(cx, |lib, cx| lib.toggle_downloader(cx));
            }))
            .on_action(cx.listener(|this, _: &NextCategory, _, cx| {
                this.library.update(cx, |lib, cx| lib.next_category(cx));
            }))
            .on_action(cx.listener(|this, _: &PreviousCategory, _, cx| {
                this.library.update(cx, |lib, cx| lib.previous_category(cx));
            }));

        #[cfg(target_os = "macos")]
        let root = root
            .on_action(|_: &MinimizeWindow, window, _| {
                window.minimize_window();
            })
            .on_action(|_: &CloseWindow, window, _| {
                window.remove_window();
            });

        root.child(self.titlebar.clone())
            .child(self.toolbar.clone())
            .when(filters_open || downloader_open, |el| {
                el.child(
                    div()
                        .h(ROW_PANEL_SEPARATOR_HEIGHT)
                        .w_full()
                        .flex_shrink_0()
                        .bg(cx.theme().border),
                )
            })
            .when(filters_open, |el| el.child(self.filter_panel.clone()))
            .when(downloader_open, |el| {
                el.child(self.downloader_panel.clone())
            })
            .child(self.table.clone())
            .when(!internal_drag_active, |el| el.child(drop_overlay))
            .when(import_progress_active, |el| {
                el.child(self.render_import_progress_modal(cx))
            })
            .children(self.render_delete_confirmation_modal(cx))
    }
}

fn pluralize(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}

fn debug_ui_interaction(details: impl FnOnce() -> String) {
    let enabled = std::env::var("LOWCAT_DEBUG")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if enabled {
        eprintln!("[lowcat:ui] {}", details());
    }
}
