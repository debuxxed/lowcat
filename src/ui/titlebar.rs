use gpui::{
    AsyncApp, ClickEvent, Context, Entity, InteractiveElement as _, IntoElement, ParentElement,
    PathPromptOptions, Pixels, Render, SharedString, StatefulInteractiveElement as _, Styled,
    Window, div, prelude::FluentBuilder as _, px,
};
use gpui_component::{
    ActiveTheme as _, Disableable as _, IconName, Sizable as _, StyledExt, TitleBar,
    button::{Button, ButtonVariants as _},
};

use crate::library::Library;
use crate::model::Category;

/// Left inset of the titlebar category row, leaving room for the traffic
/// lights. The drag-import overlay reuses this so its columns line up with the
/// titlebar categories.
pub(crate) const TITLEBAR_LEFT_OFFSET: Pixels = px(84.);

pub struct AppTitleBar {
    library: Entity<Library>,
    hovered_category: Option<Category>,
    folder_prompt_active: bool,
}

impl AppTitleBar {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();

        Self {
            library,
            hovered_category: None,
            folder_prompt_active: false,
        }
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
                    .when(can_hover, |this| this.hover(move |this| this.bg(hover_bg)))
                    .when(can_hover && hovered, |this| {
                        this.child(div().absolute().right(px(6.)).child(folder_button))
                    })
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
                    })),
            );
        }

        let titlebar = TitleBar::new()
            .h(px(38.))
            .pl(TITLEBAR_LEFT_OFFSET)
            .bg(cx.theme().background)
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
