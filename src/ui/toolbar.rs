use gpui::{
    AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement,
    Render, SharedString, StatefulInteractiveElement, Styled, Window, div, red, relative,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Selectable, Sizable, StyledExt,
    button::Button,
    input::{Input, InputEvent, InputState},
};

use crate::library::Library;
use crate::ui::CONTENT_PX;

pub struct Toolbar {
    library: Entity<Library>,
    search_input: Entity<InputState>,
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
}

impl Render for Toolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let chips: Vec<(String, String)> = {
            let state = self.library.read(cx).active_state();
            state
                .selected
                .iter()
                .flat_map(|(key, values)| values.iter().map(move |v| (key.clone(), v.clone())))
                .collect()
        };
        let chip_delete_bg = red().opacity(0.18);

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
            .child(Icon::new(IconName::Search).small())
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
                    .flex_shrink_0()
                    .px_1p5()
                    .rounded_md()
                    .text_xs()
                    .bg(chip_bg)
                    .text_color(cx.theme().muted_foreground)
                    .cursor_pointer()
                    .child(SharedString::from(value.clone()))
                    .on_hover(cx.listener({
                        let chip_id = chip_id.clone();
                        move |this, hovered: &bool, _, cx| {
                            if *hovered {
                                this.hovered_chip = Some(chip_id.clone());
                            } else if this.hovered_chip.as_deref() == Some(chip_id.as_str()) {
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

        div()
            .h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .py_2()
            .px(CONTENT_PX)
            .child(filter_button)
            .child(chip_row)
            .child(search)
    }
}
