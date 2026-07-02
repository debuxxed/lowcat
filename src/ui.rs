mod downloader_panel;
mod filter_panel;
mod folder_tags;
mod settings_menu;
mod table;
mod titlebar;
mod toolbar;

use gpui::{
    AnyElement, App, AppContext as _, Context, Entity, ExternalPaths, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ModifiersChangedEvent, ParentElement, Pixels,
    Render, SharedString, Styled, Window, actions, div, prelude::FluentBuilder, px, relative, rgba,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, StyledExt,
    button::{Button, ButtonVariants as _},
    input::Input,
    progress::Progress,
    scroll::ScrollableElement,
};

use crate::ui::titlebar::{TITLEBAR_HEIGHT, TITLEBAR_LEFT_OFFSET};
#[cfg(target_os = "macos")]
use crate::{CloseWindow, MinimizeWindow};
use crate::{
    media_tools::{MissingTool, SearchLocation},
    model::Category,
};

actions!(
    library,
    [
        NextCategory,
        PreviousCategory,
        ToggleSettings,
        ToggleFilters,
        ToggleDownloader,
        AssignFolderTags,
        RenameSelection
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
    folder_tags::FolderTagModalState,
    table::{FileTable, PendingDeleteKind, PendingRenameKind},
    titlebar::AppTitleBar,
    toolbar::Toolbar,
};

pub struct UI {
    library: Entity<Library>,
    media_tool_problems: Vec<MissingTool>,
    titlebar: Entity<AppTitleBar>,
    toolbar: Entity<Toolbar>,
    filter_panel: Entity<FilterPanel>,
    downloader_panel: Entity<DownloaderPanel>,
    table: Entity<FileTable>,
    focus_handle: FocusHandle,
    folder_tag_modal: Option<FolderTagModalState>,
}

impl UI {
    pub fn new(
        library: Entity<Library>,
        media_tool_problems: Vec<MissingTool>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
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
            media_tool_problems,
            titlebar: cx.new(|cx| AppTitleBar::new(library.clone(), cx)),
            toolbar: cx.new(|cx| Toolbar::new(library.clone(), window, cx)),
            filter_panel: cx.new(|cx| FilterPanel::new(library.clone(), cx)),
            downloader_panel: cx.new(|cx| DownloaderPanel::new(library.clone(), cx)),
            table: cx.new(|cx| FileTable::new(library.clone(), window, cx)),
            library,
            focus_handle,
            folder_tag_modal: None,
        }
    }
}

impl UI {
    fn has_media_tool_problems(&self) -> bool {
        !self.media_tool_problems.is_empty()
    }

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

    fn cancel_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.table
            .update(cx, |table, cx| table.cancel_rename(window, cx))
    }

    fn cancel_tag_edit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.table
            .update(cx, |table, cx| table.cancel_tag_edit(window, cx))
    }

    fn clear_selection(&mut self, cx: &mut Context<Self>) -> bool {
        self.table.update(cx, |table, cx| table.clear_selection(cx))
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

    fn confirm_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let confirmed = self
            .table
            .update(cx, |table, cx| table.confirm_pending_rename(window, cx));
        if confirmed {
            debug_ui_interaction(|| "enter confirmed rename".to_string());
        }
        confirmed
    }

    fn text_input_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.toolbar.read(cx).search_is_focused(window, cx)
            || self.table.read(cx).tag_editor_is_focused(window, cx)
            || self.table.read(cx).rename_input_is_focused(window, cx)
    }

    fn tag_editor_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.table.read(cx).tag_editor_is_focused(window, cx)
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

    fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toolbar
            .update(cx, |toolbar, cx| toolbar.toggle_settings(window, cx));
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

    fn render_rename_modal(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let details = self.table.read(cx).pending_rename_details()?;
        let input = self.table.read(cx).rename_input();
        let title = match details.kind {
            PendingRenameKind::Rows if details.item_count == 1 => "Rename row",
            PendingRenameKind::Rows => "Rename files",
            PendingRenameKind::TagAll => "Rename tag",
        };
        let description = match details.kind {
            PendingRenameKind::Rows if details.item_count == 1 => {
                let name = details.current_name.unwrap_or_else(|| "row".to_string());
                format!("Rename {name}")
            }
            PendingRenameKind::Rows => {
                let file_label = pluralize(details.file_count, "file", "files");
                format!("Rename {} {}", details.file_count, file_label)
            }
            PendingRenameKind::TagAll => {
                let name = details.current_name.unwrap_or_else(|| "tag".to_string());
                format!("Rename all {name} tags")
            }
        };

        Some(
            div()
                .id("rename-overlay")
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
                                .child(Input::new(&input).small())
                                .child(
                                    div()
                                        .h_flex()
                                        .justify_end()
                                        .gap_2()
                                        .child(
                                            Button::new("rename-cancel")
                                                .small()
                                                .label("Cancel")
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.cancel_rename(window, cx);
                                                })),
                                        )
                                        .child(
                                            Button::new("rename-confirm")
                                                .small()
                                                .when(details.bulk, |button| button.warning())
                                                .when(!details.bulk, |button| button.primary())
                                                .label("Apply")
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.table.update(cx, |table, cx| {
                                                        table.confirm_pending_rename(window, cx);
                                                    });
                                                })),
                                        ),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_media_tools_modal(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let problems = self
            .media_tool_problems
            .iter()
            .map(|problem| render_media_tool_problem(problem, cx))
            .collect::<Vec<_>>();

        div()
            .id("media-tools-overlay")
            .absolute()
            .top(TITLEBAR_HEIGHT)
            .bottom_0()
            .left_0()
            .w_full()
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
                            .w_full()
                            .h(relative(0.86))
                            .v_flex()
                            .border_y_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().popover)
                            .shadow_lg()
                            .child(
                                div().w_full().flex_shrink_0().px(CONTENT_PX).py_3().child(
                                    div()
                                        .w_full()
                                        .min_w_0()
                                        .v_flex()
                                        .gap_1()
                                        .child(
                                            div()
                                                .text_base()
                                                .font_weight(gpui::FontWeight::BOLD)
                                                .child("Required tools missing"),
                                        )
                                        .child(
                                            div()
                                                .w_full()
                                                .min_w_0()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(
                                                    "Lowcat can't function without these tools.",
                                                ),
                                        ),
                                ),
                            )
                            .child(
                                div().flex_1().min_h_0().overflow_hidden().child(
                                    div()
                                        .size_full()
                                        .overflow_y_scrollbar()
                                        .v_flex()
                                        .gap_4()
                                        .px(CONTENT_PX)
                                        .pb_4()
                                        .children(problems),
                                ),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(1.))
                                    .flex_shrink_0()
                                    .bg(cx.theme().border),
                            ),
                    ),
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
                if this.has_media_tool_problems() {
                    cx.stop_propagation();
                    return;
                }
                if this.folder_tag_modal.is_some() {
                    if event.keystroke.modifiers.platform && event.keystroke.key == "a" {
                        this.select_all_folder_tag_rows(cx);
                    } else if event.keystroke.key == "escape" {
                        if !this.close_folder_tag_key_menu(cx)
                            && !this.clear_folder_tag_selection(cx)
                        {
                            this.close_folder_tag_modal(cx);
                        }
                    }
                    cx.stop_propagation();
                    return;
                }
                if event.keystroke.key == "escape" {
                    if this.cancel_rename(window, cx) {
                        cx.stop_propagation();
                        return;
                    }
                    if this.cancel_delete(cx) {
                        cx.stop_propagation();
                        return;
                    }
                    if this.cancel_tag_edit(window, cx) {
                        cx.stop_propagation();
                        return;
                    }
                    if this.cancel_search_if_no_selection(window, cx) {
                        cx.stop_propagation();
                        return;
                    }
                    if this.clear_selection(cx) {
                        cx.stop_propagation();
                        return;
                    }
                    this.cancel_file_drag(cx);
                    cx.stop_propagation();
                } else if event.keystroke.key == "enter" {
                    if this.confirm_rename(window, cx) || this.confirm_delete(cx) {
                        cx.stop_propagation();
                    }
                } else if event.keystroke.modifiers.platform && event.keystroke.key == "f" {
                    this.toolbar
                        .update(cx, |toolbar, cx| toolbar.focus_search(window, cx));
                    cx.stop_propagation();
                } else if event.keystroke.modifiers.platform
                    && event.keystroke.key == "a"
                    && !this.tag_editor_is_focused(window, cx)
                    && this
                        .table
                        .update(cx, |table, cx| table.select_all_visible(window, cx))
                {
                    debug_ui_interaction(|| "cmd-a selected visible rows".to_string());
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
                if this.has_media_tool_problems() {
                    return;
                }
                this.library.update(cx, |lib, cx| lib.toggle_filters(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleDownloader, _, cx| {
                if this.has_media_tool_problems() {
                    return;
                }
                this.library.update(cx, |lib, cx| lib.toggle_downloader(cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleSettings, window, cx| {
                if this.has_media_tool_problems() {
                    return;
                }
                this.toggle_settings(window, cx);
            }))
            .on_action(cx.listener(|this, _: &AssignFolderTags, _, cx| {
                if this.has_media_tool_problems() {
                    return;
                }
                this.open_folder_tag_modal(cx);
            }))
            .on_action(cx.listener(|this, _: &RenameSelection, window, cx| {
                if this.has_media_tool_problems() {
                    return;
                }
                this.table.update(cx, |table, cx| {
                    table.start_selected_rename(window, cx);
                });
            }))
            .on_action(cx.listener(|this, _: &NextCategory, _, cx| {
                if this.has_media_tool_problems() {
                    return;
                }
                this.library.update(cx, |lib, cx| lib.next_category(cx));
            }))
            .on_action(cx.listener(|this, _: &PreviousCategory, _, cx| {
                if this.has_media_tool_problems() {
                    return;
                }
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

        let has_media_tool_problems = self.has_media_tool_problems();
        let content = div()
            .size_full()
            .v_flex()
            .child(self.titlebar.clone())
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
            .when(has_media_tool_problems, |el| el.opacity(0.35));

        root.child(content)
            .when(!has_media_tool_problems && !internal_drag_active, |el| {
                el.child(drop_overlay)
            })
            .when(!has_media_tool_problems && import_progress_active, |el| {
                el.child(self.render_import_progress_modal(cx))
            })
            .when(!has_media_tool_problems, |el| {
                el.children(self.render_folder_tag_modal(_window, cx))
            })
            .when(!has_media_tool_problems, |el| {
                el.children(self.render_rename_modal(cx))
            })
            .when(!has_media_tool_problems, |el| {
                el.children(self.render_delete_confirmation_modal(cx))
            })
            .when(has_media_tool_problems, |el| {
                el.child(self.render_media_tools_modal(cx))
            })
    }
}

fn pluralize(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}

fn render_media_tool_problem(problem: &MissingTool, cx: &mut Context<UI>) -> AnyElement {
    let locations = if problem.search_locations.is_empty() {
        "No PATH or standard fallback directories".to_string()
    } else {
        problem
            .search_locations
            .iter()
            .map(|location| match location {
                SearchLocation::Path => "PATH".to_string(),
                SearchLocation::Directory(path) => path.display().to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let solution = tool_solution(problem.name);

    div()
        .w_full()
        .min_w_0()
        .v_flex()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            div()
                .w_full()
                .min_w_0()
                .text_sm()
                .font_weight(gpui::FontWeight::BOLD)
                .child(problem.name),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .v_flex()
                .gap_0()
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .line_height(relative(1.35))
                        .text_color(cx.theme().muted_foreground)
                        .child("Looked up in"),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .line_height(relative(1.35))
                        .text_color(cx.theme().muted_foreground)
                        .child(locations),
                ),
        )
        .child(
            div()
                .w_full()
                .min_w_0()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(format!(
                            "Possible solution: {}",
                            solution.text
                        ))),
                )
                .when_some(solution.url, |el, url| {
                    el.child(
                        Button::new(format!("download-{}", problem.name))
                            .xsmall()
                            .flex_shrink_0()
                            .label("Download")
                            .on_click(move |_, _, _| {
                                if let Err(error) = open::that(url) {
                                    eprintln!("failed to open {url}: {error}");
                                }
                            }),
                    )
                }),
        )
        .into_any_element()
}

struct ToolSolution {
    text: &'static str,
    url: Option<&'static str>,
}

fn tool_solution(tool: &str) -> ToolSolution {
    match tool {
        "ffmpeg" => ToolSolution {
            text: "Install FFmpeg.",
            url: cfg!(target_os = "macos").then_some("https://formulae.brew.sh/formula/ffmpeg"),
        },
        "ffprobe" => ToolSolution {
            text: "Install FFmpeg; ffprobe is included with it.",
            url: cfg!(target_os = "macos").then_some("https://formulae.brew.sh/formula/ffmpeg"),
        },
        "yt-dlp" => ToolSolution {
            text: "Install yt-dlp.",
            url: Some("https://github.com/yt-dlp/yt-dlp/wiki/Installation"),
        },
        _ => ToolSolution {
            text: "Install the missing tool.",
            url: None,
        },
    }
}

fn debug_ui_interaction(details: impl FnOnce() -> String) {
    let enabled = std::env::var("LOWCAT_DEBUG")
        .map(|value| matches!(value.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(false);
    if enabled {
        eprintln!("[lowcat:ui] {}", details());
    }
}
