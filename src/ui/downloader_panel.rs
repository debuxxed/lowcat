use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::FluentBuilder as _, relative,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, Icon, IconName, Selectable as _, Sizable as _, StyledExt,
    button::{Button, ButtonVariants as _},
    progress::Progress,
};

use crate::downloader::DownloadState;
use crate::library::Library;
use crate::model::Category;
use crate::ui::{CONTENT_PX, ROW_PANEL_HEIGHT};

pub struct DownloaderPanel {
    library: Entity<Library>,
}

impl DownloaderPanel {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self { library }
    }

    fn paste_download(&mut self, category: Category, cx: &mut Context<Self>) {
        let clipboard_text = cx.read_from_clipboard().and_then(|item| item.text());
        self.library.update(cx, |lib, cx| {
            lib.download_from_clipboard(category, clipboard_text, cx);
        });
    }
}

impl Render for DownloaderPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.library.read(cx).download_state();
        let running_category = match &state {
            DownloadState::Running(status) => Some(status.category),
            _ => None,
        };

        div()
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
                    .child(self.render_category_button(Category::Music, running_category, cx))
                    .child(self.render_category_button(Category::Sfx, running_category, cx))
                    .child(render_download_details(state.clone(), cx)),
            )
            .child(render_download_progress(state, cx))
    }
}

impl DownloaderPanel {
    fn render_category_button(
        &self,
        category: Category,
        running_category: Option<Category>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selected = running_category == Some(category);
        Button::new(format!("download-paste-{}", category.label()))
            .xsmall()
            .compact()
            .when(selected, |button| button.primary())
            .icon(IconName::ArrowDown)
            .label(category.label())
            .tooltip(SharedString::from(format!("Paste {}", category.label())))
            .selected(selected)
            .loading(selected)
            .disabled(running_category.is_some() && !selected)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.paste_download(category, cx);
            }))
    }
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
