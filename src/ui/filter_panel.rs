use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
};
use gpui_component::{ActiveTheme as _, StyledExt, checkbox::Checkbox};

use crate::library::Library;

pub struct FilterPanel {
    library: Entity<Library>,
}

impl FilterPanel {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self { library }
    }
}

impl Render for FilterPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.library.read(cx).active_state();

        let mut row = div()
            .h_flex()
            .w_full()
            .gap_4()
            .p_2()
            .border_b_1()
            .border_color(cx.theme().border);

        for (key, values) in &state.schema {
            let checked: Vec<String> = state
                .selected
                .get(key)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();

            let mut column = div().v_flex().gap_1().child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(key.clone())),
            );

            for value in values {
                let is_checked = checked.contains(value);
                let key_owned = key.clone();
                let value_owned = value.clone();
                column = column.child(
                    Checkbox::new(SharedString::from(format!("{key}:{value}")))
                        .label(SharedString::from(value.clone()))
                        .checked(is_checked)
                        .on_click(cx.listener(move |this, _checked: &bool, _, cx| {
                            let key = key_owned.clone();
                            let value = value_owned.clone();
                            this.library.update(cx, |lib, cx| {
                                lib.toggle_value(&key, &value, cx);
                            });
                        })),
                );
            }

            row = row.child(column);
        }

        row
    }
}
