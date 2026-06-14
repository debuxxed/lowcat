use gpui::{
    AppContext, ClickEvent, Context, Entity, InteractiveElement, IntoElement, ParentElement, Render,
    SharedString, StatefulInteractiveElement, Styled, Window, div,
};
use gpui_component::{
    ActiveTheme as _, Icon, IconName, Selectable, Sizable, StyledExt,
    button::Button,
    input::{Input, InputEvent, InputState},
};

use crate::library::Library;
use crate::model::Category;

pub struct Toolbar {
    library: Entity<Library>,
    search_input: Entity<InputState>,
}

impl Toolbar {
    pub fn new(library: Entity<Library>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder("Search..."));

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
        }
    }
}

impl Render for Toolbar {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let active = self.library.read(cx).active();

        // Active filter chips: (key, value) pairs flattened from the active selection.
        let chips: Vec<(String, String)> = {
            let state = self.library.read(cx).active_state();
            state
                .selected
                .iter()
                .flat_map(|(key, values)| values.iter().map(move |v| (key.clone(), v.clone())))
                .collect()
        };

        let mut toggle = div().h_flex().gap_1();
        for category in Category::ALL {
            toggle = toggle.child(
                Button::new(SharedString::from(category.label()))
                    .label(category.label())
                    .small()
                    .selected(category == active)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.library
                            .update(cx, |lib, cx| lib.set_category(category, cx));
                    })),
            );
        }

        let mut search = div()
            .h_flex()
            .flex_1()
            .items_center()
            .gap_1()
            .px_2()
            .py_1()
            .rounded_md()
            .bg(cx.theme().secondary)
            .border_1()
            .border_color(cx.theme().border)
            .child(Icon::new(IconName::Search).small());

        for (key, value) in chips {
            let key_owned = key.clone();
            let value_owned = value.clone();
            search = search.child(
                div()
                    .id(SharedString::from(format!("chip:{key}:{value}")))
                    .px_1p5()
                    .rounded_md()
                    .text_xs()
                    .bg(cx.theme().muted)
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(value.clone()))
                    .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                        if event.modifiers().alt {
                            let key = key_owned.clone();
                            let value = value_owned.clone();
                            this.library.update(cx, |lib, cx| {
                                lib.remove_value(&key, &value, cx);
                            });
                        }
                    })),
            );
        }

        search = search.child(Input::new(&self.search_input).appearance(false).flex_1());

        div()
            .h_flex()
            .w_full()
            .items_center()
            .gap_2()
            .p_2()
            .child(toggle)
            .child(search)
    }
}
