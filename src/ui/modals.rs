use gpui::{
    AnyElement, ExternalPaths, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, prelude::FluentBuilder as _, px, rgba,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, StyledExt as _,
    button::{Button, ButtonVariants as _},
    input::Input,
    progress::Progress,
};

use super::{
    UI,
    table::{PendingDeleteKind, PendingRenameKind},
};

impl UI {
    pub(super) fn render_import_progress_modal(
        &self,
        cx: &mut gpui::Context<Self>,
    ) -> Option<AnyElement> {
        let progress = self.library.read(cx).import_progress().cloned()?;
        let percent = progress.progress.round() as u32;

        Some(
            gpui::div()
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
                    gpui::div()
                        .size_full()
                        .h_flex()
                        .items_center()
                        .justify_center()
                        .child(
                            gpui::div()
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
                                    gpui::div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .child("Converting media"),
                                )
                                .child(
                                    gpui::div()
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
                                    gpui::div()
                                        .text_xs()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(SharedString::from(format!("{percent}%"))),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_delete_confirmation_modal(
        &self,
        cx: &mut gpui::Context<Self>,
    ) -> Option<AnyElement> {
        let counts = self.table.read(cx).pending_delete_counts()?;
        let title = match counts.kind {
            PendingDeleteKind::Rows if counts.row_count == 1 => "Move row to Trash?",
            PendingDeleteKind::Rows => "Move rows to Trash?",
            PendingDeleteKind::Format => "Move format file to Trash?",
            PendingDeleteKind::TagKey => "Remove tag column?",
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
            PendingDeleteKind::TagKey => "Remove this tag column and all values in it?".to_string(),
        };
        let confirm_label = match counts.kind {
            PendingDeleteKind::Rows | PendingDeleteKind::Format => "Move to Trash",
            PendingDeleteKind::TagKey => "Remove",
        };

        Some(
            modal_overlay("delete-confirmation-overlay", cx)
                .child(
                    modal_card(cx)
                        .child(
                            gpui::div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(title),
                        )
                        .child(
                            gpui::div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        )
                        .child(
                            gpui::div()
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
                                        .label(confirm_label)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.table.update(cx, |table, cx| {
                                                table.confirm_pending_delete(cx);
                                            });
                                        })),
                                ),
                        ),
                )
                .into_any_element(),
        )
    }

    pub(super) fn render_rename_modal(&self, cx: &mut gpui::Context<Self>) -> Option<AnyElement> {
        let table = self.table.read(cx);
        let details = table.pending_rename_details()?;
        let input = table.rename_input();
        let title = match details.kind {
            PendingRenameKind::Rows if details.item_count == 1 => "Rename row",
            PendingRenameKind::Rows => "Rename files",
            PendingRenameKind::TagAll => "Rename tag",
            PendingRenameKind::TagKey => "Rename tag column",
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
            PendingRenameKind::TagKey => {
                let name = details.current_name.unwrap_or_else(|| "tag".to_string());
                format!("Rename {name} column")
            }
        };

        Some(
            modal_overlay("rename-overlay", cx)
                .child(
                    modal_card(cx)
                        .child(
                            gpui::div()
                                .text_sm()
                                .font_weight(gpui::FontWeight::BOLD)
                                .child(title),
                        )
                        .child(
                            gpui::div()
                                .text_sm()
                                .text_color(cx.theme().muted_foreground)
                                .child(description),
                        )
                        .child(Input::new(&input).small())
                        .child(
                            gpui::div()
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
                )
                .into_any_element(),
        )
    }
}

fn modal_overlay(id: &'static str, _cx: &mut gpui::Context<UI>) -> gpui::Stateful<gpui::Div> {
    gpui::div()
        .id(id)
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .h_flex()
        .items_center()
        .justify_center()
        .bg(rgba(0x00000099))
        .occlude()
        .hover(|style| style)
        .on_any_mouse_down(|_, _, cx| cx.stop_propagation())
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
        .on_scroll_wheel(|_, _, cx| cx.stop_propagation())
}

fn modal_card(cx: &mut gpui::Context<UI>) -> gpui::Div {
    gpui::div()
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
}

fn pluralize(count: usize, singular: &'static str, plural: &'static str) -> &'static str {
    if count == 1 { singular } else { plural }
}
