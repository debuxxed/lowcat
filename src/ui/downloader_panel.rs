use gpui::{
    Anchor, Context, Entity, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Pixels, Point, Render, SharedString, StatefulInteractiveElement, Styled, Window,
    anchored, deferred, div, prelude::FluentBuilder as _, px, relative,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt,
    button::{Button, ButtonVariants as _},
    progress::Progress,
};

use crate::downloader::DownloadState;
use crate::library::Library;
use crate::model::{AudioFormat, Category};
use crate::ui::{CONTENT_PX, ROW_PANEL_HEIGHT};

pub struct DownloaderPanel {
    library: Entity<Library>,
    format_menu_open: bool,
    format_menu_position: Option<Point<Pixels>>,
}

impl DownloaderPanel {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self {
            library,
            format_menu_open: false,
            format_menu_position: None,
        }
    }
}

impl Render for DownloaderPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.library.read(cx).download_state();
        let download_format = self.library.read(cx).download_format();
        let running_category = match &state {
            DownloadState::Running(status) => Some(status.category),
            _ => None,
        };
        let panel = div()
            .h_flex()
            .items_center()
            .w_full()
            .min_w_0()
            .min_h(ROW_PANEL_HEIGHT)
            .px(CONTENT_PX)
            .py_1()
            .gap_2()
            .child(
                div()
                    .h_flex()
                    .items_center()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(self.render_format_button(download_format, running_category, cx))
                    .child(render_download_details(state.clone(), cx)),
            )
            .child(render_download_progress(state, cx));

        let format_menu = self.render_format_menu(window, download_format, running_category, cx);
        panel.when_some(format_menu, |el, format_menu| el.child(format_menu))
    }
}

impl DownloaderPanel {
    fn render_format_button(
        &self,
        format: AudioFormat,
        running_category: Option<Category>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        Button::new("download-format-select")
            .xsmall()
            .compact()
            .icon(IconName::ChevronDown)
            .label(format.label())
            .tooltip("Download format")
            .selected(self.format_menu_open)
            .disabled(running_category.is_some())
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.format_menu_open = !this.format_menu_open;
                    this.format_menu_position = Some(event.position);
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }

    fn render_format_menu(
        &self,
        window: &mut Window,
        selected_format: AudioFormat,
        running_category: Option<Category>,
        cx: &mut Context<Self>,
    ) -> Option<impl IntoElement + use<>> {
        if !self.format_menu_open || running_category.is_some() {
            return None;
        }

        let position = self.format_menu_position?;
        let window_size = window.bounds().size;
        let mut menu = div()
            .popover_style(cx)
            .w(px(92.))
            .min_w(px(92.))
            .p_1()
            .on_mouse_down(MouseButton::Left, |_, _, cx| {
                cx.stop_propagation();
            });
        for format in [
            AudioFormat::Wav,
            AudioFormat::Mp3,
            AudioFormat::Opus,
            AudioFormat::Flac,
        ] {
            let library = self.library.clone();
            menu = menu.child(
                format_menu_row()
                    .id(SharedString::from(format!(
                        "download-format:{}",
                        format.extension()
                    )))
                    .hover(|style| style.bg(cx.theme().accent))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        library.update(cx, |lib, cx| lib.set_download_format(format, cx));
                        this.format_menu_open = false;
                        this.format_menu_position = None;
                        cx.notify();
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
                            .w(px(18.))
                            .h_full()
                            .h_flex()
                            .items_center()
                            .justify_end()
                            .when(selected_format == format, |el| {
                                el.child(
                                    Icon::new(IconName::Check)
                                        .small()
                                        .text_color(cx.theme().primary),
                                )
                            }),
                    ),
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
                            cx.listener(|this, _: &MouseDownEvent, _, cx| {
                                this.format_menu_open = false;
                                this.format_menu_position = None;
                                cx.notify();
                            }),
                        )
                        .child(
                            anchored()
                                .position(position)
                                .snap_to_window_with_margin(px(8.))
                                .anchor(Anchor::TopLeft)
                                .child(menu),
                        ),
                ),
            )
            .with_priority(1),
        )
    }
}

fn format_menu_row() -> gpui::Div {
    div()
        .h_flex()
        .w_full()
        .items_center()
        .justify_between()
        .gap_2()
        .h(px(26.))
        .px_2()
        .rounded_md()
        .line_height(relative(1.))
        .whitespace_nowrap()
        .cursor_pointer()
}

fn render_download_details(
    state: DownloadState,
    cx: &mut Context<DownloaderPanel>,
) -> impl IntoElement {
    match state {
        DownloadState::Idle => div().flex_1().min_w_0().into_any_element(),
        DownloadState::Running(status) => div()
            .h_flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .child(
                div()
                    .text_xs()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().foreground)
                    .child(SharedString::from(status.label)),
            )
            .into_any_element(),
        DownloadState::Error(error) => div()
            .h_flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(
                Icon::new(IconName::TriangleAlert)
                    .small()
                    .text_color(cx.theme().danger),
            )
            .child(
                div()
                    .text_xs()
                    .min_w_0()
                    .truncate()
                    .text_color(cx.theme().danger)
                    .child(SharedString::from(error.message)),
            )
            .child(
                Button::new("download-error-dismiss")
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .compact()
                    .tooltip("Dismiss download error")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.library
                            .update(cx, |lib, cx| lib.dismiss_download_error(cx));
                    })),
            )
            .into_any_element(),
    }
}

fn render_download_progress(
    state: DownloadState,
    cx: &mut Context<DownloaderPanel>,
) -> impl IntoElement {
    let progress = match state {
        DownloadState::Running(status) => Some(status.progress),
        _ => None,
    };

    div()
        .h_flex()
        .w(relative(0.25))
        .flex_shrink_0()
        .items_center()
        .when_some(progress, |el, progress| {
            el.child(
                div()
                    .h_flex()
                    .w_full()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(Progress::new("download-progress").small().value(progress)),
                    )
                    .child(
                        Button::new("download-cancel")
                            .icon(IconName::CircleX)
                            .ghost()
                            .xsmall()
                            .compact()
                            .tooltip("Cancel download")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.library.update(cx, |lib, cx| lib.cancel_download(cx));
                            })),
                    ),
            )
        })
}
