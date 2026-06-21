use gpui::{
    App, AppContext, ClickEvent, Context, Entity, Focusable, InteractiveElement, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement, Styled, Window, div, red,
    relative,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Selectable, Sizable, StyledExt,
    button::{Button, ButtonVariants as _},
    input::{Input, InputEvent, InputState},
};

use crate::library::Library;
use crate::ui::{CONTENT_PX, settings_menu::SettingsMenu};

pub struct Toolbar {
    library: Entity<Library>,
    search_input: Entity<InputState>,
    settings_menu: Entity<SettingsMenu>,
    hovered_chip: Option<String>,
    alt_down: bool,
}

impl Toolbar {
    pub fn new(library: Entity<Library>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search..."));
        search_input.update(cx, |state, cx| state.focus(window, cx));

        cx.subscribe(&search_input, |this, state, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let value = state.read(cx).value().to_string();
                this.library.update(cx, |lib, cx| lib.set_search(value, cx));
            }
        })
        .detach();

        cx.observe(&library, |_, _, cx| cx.notify()).detach();

        Self {
            settings_menu: cx.new(|cx| SettingsMenu::new(library.clone(), cx)),
            library,
            search_input,
            hovered_chip: None,
            alt_down: false,
        }
    }

    pub fn set_alt_down(&mut self, alt_down: bool, cx: &mut Context<Self>) {
        if self.alt_down != alt_down {
            self.alt_down = alt_down;
            cx.notify();
        }
    }

    pub fn focus_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.search_input
            .update(cx, |state, cx| state.focus(window, cx));
    }

    pub fn search_is_focused(&self, window: &Window, cx: &App) -> bool {
        self.search_input.read(cx).focus_handle(cx).is_focused(window)
    }
}

impl Render for Toolbar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let render_start = crate::perf::start();
        let chips: Vec<(String, String)> = {
            let state = self.library.read(cx).active_state();
            state
                .selected
                .iter()
                .flat_map(|(key, values)| values.iter().map(move |v| (key.clone(), v.clone())))
                .collect()
        };
        let chips_len = chips.len();
        let chip_delete_bg = red().opacity(0.18);
        let has_search = !self.search_input.read(cx).value().is_empty();
        let search_icon = if has_search {
            div().child(
                Button::new("clear-search")
                    .icon(IconName::Close)
                    .ghost()
                    .xsmall()
                    .compact()
                    .tab_stop(false)
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.search_input.update(cx, |state, cx| {
                            state.set_value("", window, cx);
                            state.focus(window, cx);
                        });
                        this.library
                            .update(cx, |lib, cx| lib.set_search(String::new(), cx));
                    })),
            )
        } else {
            div().child(Icon::new(IconName::Search).small())
        };

        let search = div()
            .h_flex()
            .h_6()
            .w(relative(0.25))
            .flex_shrink_0()
            .items_center()
            .gap_1()
            .px_2()
            .rounded_md()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(search_icon)
            .child(Input::new(&self.search_input).appearance(false).flex_1());

        let mut chip_row = div()
            .h_flex()
            .h_6()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_1()
            .overflow_x_hidden();

        for (key, value) in chips {
            let chip_id = format!("chip:{key}:{value}");
            let chip_bg = if self.alt_down && self.hovered_chip.as_deref() == Some(chip_id.as_str())
            {
                chip_delete_bg
            } else {
                cx.theme().muted
            };

            chip_row = chip_row.child(
                div()
                    .id(SharedString::from(chip_id.clone()))
                    .group(SharedString::from(format!("chip-group:{chip_id}")))
                    .flex_shrink_0()
                    .relative()
                    .h_6()
                    .h_flex()
                    .items_center()
                    .child(
                        div()
                            .px_1p5()
                            .rounded_md()
                            .text_xs()
                            .bg(chip_bg)
                            .text_color(cx.theme().muted_foreground)
                            .child(SharedString::from(value.clone())),
                    )
                    .child(
                        div()
                            .id(SharedString::from(format!("chip-hitbox:{chip_id}")))
                            .absolute()
                            .inset_0()
                            .w_full()
                            .h_full()
                            .bg(cx.theme().transparent)
                            .cursor_pointer()
                            .on_hover(cx.listener({
                                let chip_id = chip_id.clone();
                                move |this, hovered: &bool, _, cx| {
                                    if *hovered {
                                        this.hovered_chip = Some(chip_id.clone());
                                    } else if this.hovered_chip.as_deref() == Some(chip_id.as_str())
                                    {
                                        this.hovered_chip = None;
                                    }
                                    cx.notify();
                                }
                            }))
                            .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                                if event.modifiers().alt {
                                    this.library.update(cx, |lib, cx| {
                                        lib.remove_value(&key.clone(), &value.clone(), cx);
                                    });
                                }
                            })),
                    ),
            );
        }

        let filters_open = self.library.read(cx).filters_open();
        let filter_button = Button::new("filter-toggle")
            .icon(IconName::Settings2)
            .label("Filter")
            .small()
            .selected(filters_open)
            .on_click(cx.listener(|this, _, _, cx| {
                this.library.update(cx, |lib, cx| lib.toggle_filters(cx));
            }));

        let toolbar = div()
            .h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .py_2()
            .px(CONTENT_PX)
            .child(self.settings_menu.clone())
            .child(filter_button)
            .child(chip_row)
            .child(search);

        crate::perf::finish("toolbar.render", render_start, || {
            format!("chips={chips_len} alt_down={}", self.alt_down)
        });
        toolbar
    }
}
