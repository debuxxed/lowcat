mod downloader_panel;
mod drop_overlay;
mod filter_panel;
mod folder_tags;
mod media_tools_modal;
mod modals;
mod settings_menu;
mod table;
mod titlebar;
mod toolbar;

use futures::StreamExt as _;
use gpui::{
    App, AppContext as _, Context, Entity, FocusHandle, Focusable, InteractiveElement, IntoElement,
    KeyDownEvent, ModifiersChangedEvent, ParentElement, Pixels, Render, Styled, Window, actions,
    div, prelude::FluentBuilder, px,
};
use gpui_component::{ActiveTheme as _, StyledExt};

use crate::media_tools::MissingTool;
#[cfg(target_os = "macos")]
use crate::{CloseWindow, MinimizeWindow};

actions!(
    library,
    [
        NextCategory,
        PreviousCategory,
        ToggleSettings,
        ToggleFilters,
        ToggleDownloader,
        ClearFilterTags,
        ClearFilterTagsAndSearch,
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
    downloader_panel::DownloaderPanel, filter_panel::FilterPanel, folder_tags::FolderTagModalState,
    table::FileTable, titlebar::AppTitleBar, toolbar::Toolbar,
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
            } else {
                this.table
                    .update(cx, |table, cx| table.set_cmd_down(false, cx));
            }
        })
        .detach();
        // Hold focus at the root so window-level actions (e.g. ToggleFilters)
        // still dispatch when no input is active.
        let focus_handle = cx.focus_handle();
        focus_handle.focus(window, cx);
        #[cfg(target_os = "macos")]
        {
            let mut url_drops = crate::macos_url_drop::install(window);
            cx.spawn(async move |this, cx| {
                while let Some(url) = url_drops.next().await {
                    this.update(cx, |this, cx| {
                        this.download_link_for_active_category(url, cx);
                    })
                    .ok();
                }
            })
            .detach();
        }
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
                | "space"
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

    fn cancel_file_drag(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        self.table
            .update(cx, |table, cx| table.cancel_file_drag(window, cx))
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

    fn cancel_column_visibility_menu(&mut self, cx: &mut Context<Self>) -> bool {
        self.table
            .update(cx, |table, cx| table.cancel_column_visibility_menu(cx))
    }

    fn cancel_tag_group_menu(&mut self, cx: &mut Context<Self>) -> bool {
        self.filter_panel
            .update(cx, |panel, cx| panel.cancel_tag_group_menu(cx))
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

    fn play_cmd_hovered_preview_from_start(&mut self, cx: &mut Context<Self>) -> bool {
        self.table.update(cx, |table, cx| {
            table.play_cmd_hovered_preview_from_start(cx)
        })
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

    fn download_link_for_active_category(&mut self, link_text: String, cx: &mut Context<Self>) {
        let category = self.library.read(cx).active();
        self.library.update(cx, |lib, cx| {
            lib.download_from_clipboard(category, Some(link_text), cx);
        });
    }

    fn toggle_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toolbar
            .update(cx, |toolbar, cx| toolbar.toggle_settings(window, cx));
    }

    fn clear_filter_tags(&mut self, cx: &mut Context<Self>) -> bool {
        self.toolbar
            .update(cx, |toolbar, cx| toolbar.clear_filter_tags(cx))
    }

    fn clear_filter_tags_and_search(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        self.toolbar.update(cx, |toolbar, cx| {
            toolbar.clear_filter_tags_and_search(window, cx)
        })
    }

    fn handle_key_down(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.has_media_tool_problems() {
            cx.stop_propagation();
            return;
        }
        if self.folder_tag_modal.is_some() {
            if event.keystroke.modifiers.platform && event.keystroke.key == "a" {
                self.select_all_folder_tag_rows(cx);
            } else if event.keystroke.key == "escape"
                && !self.close_folder_tag_key_menu(cx)
                && !self.clear_folder_tag_selection(cx)
            {
                self.close_folder_tag_modal(cx);
            }
            cx.stop_propagation();
            return;
        }
        if event.keystroke.key == "escape" {
            if self.cancel_rename(window, cx)
                || self.cancel_delete(cx)
                || self.cancel_tag_edit(window, cx)
                || self.cancel_column_visibility_menu(cx)
                || self.cancel_tag_group_menu(cx)
                || self.cancel_file_drag(window, cx)
                || self.cancel_search_if_no_selection(window, cx)
                || self.clear_selection(cx)
            {
                cx.stop_propagation();
                return;
            }
            cx.stop_propagation();
        } else if event.keystroke.key == "enter" {
            if self.confirm_rename(window, cx) || self.confirm_delete(cx) {
                cx.stop_propagation();
            }
        } else if event.keystroke.key == "space"
            && event.keystroke.modifiers.platform
            && self.play_cmd_hovered_preview_from_start(cx)
        {
            cx.stop_propagation();
        } else if event.keystroke.modifiers.platform && event.keystroke.key == "f" {
            self.library.update(cx, |lib, cx| lib.close_filters(cx));
            self.toolbar
                .update(cx, |toolbar, cx| toolbar.focus_search(window, cx));
            cx.stop_propagation();
        } else if event.keystroke.modifiers.platform
            && event.keystroke.key == "a"
            && !self.tag_editor_is_focused(window, cx)
            && self
                .table
                .update(cx, |table, cx| table.select_all_visible(window, cx))
        {
            debug_ui_interaction(|| "cmd-a selected visible rows".to_string());
            cx.stop_propagation();
        } else if self.library.read(cx).downloader_open()
            && event.keystroke.modifiers.platform
            && event.keystroke.key == "v"
            && !self.text_input_is_focused(window, cx)
        {
            self.paste_download_for_active_category(cx);
            cx.stop_propagation();
        } else if Self::should_activate_search(event) && !self.text_input_is_focused(window, cx) {
            let keystroke = event.keystroke.clone();
            self.toolbar
                .update(cx, |toolbar, cx| toolbar.focus_search(window, cx));
            cx.stop_propagation();
            window.defer(cx, move |window, cx| {
                window.dispatch_keystroke(keystroke, cx);
            });
        }
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
                this.table.update(cx, |table, cx| {
                    table.set_cmd_down(event.modifiers.platform, cx)
                });
            }))
            .on_key_down(cx.listener(Self::handle_key_down))
            .on_action(cx.listener(|this, _: &ToggleFilters, window, cx| {
                if this.has_media_tool_problems() {
                    return;
                }
                this.library.update(cx, |lib, cx| lib.toggle_filters(cx));
                this.toolbar
                    .update(cx, |toolbar, cx| toolbar.focus_search(window, cx));
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
            .on_action(cx.listener(|this, _: &ClearFilterTags, _, cx| {
                if this.has_media_tool_problems() {
                    return;
                }
                this.clear_filter_tags(cx);
            }))
            .on_action(
                cx.listener(|this, _: &ClearFilterTagsAndSearch, window, cx| {
                    if this.has_media_tool_problems() {
                        return;
                    }
                    this.clear_filter_tags_and_search(window, cx);
                }),
            )
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
                el.children(self.render_import_progress_modal(cx))
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

fn debug_ui_interaction(details: impl FnOnce() -> String) {
    crate::diagnostics::debug("ui", details);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{Keystroke, Modifiers};

    #[test]
    fn space_is_left_for_focused_search_input() {
        let event = KeyDownEvent {
            keystroke: Keystroke {
                modifiers: Modifiers::default(),
                key: "space".to_string(),
                key_char: Some(" ".to_string()),
            },
            is_held: false,
            prefer_character_input: false,
        };

        assert!(!UI::should_activate_search(&event));
    }
}
