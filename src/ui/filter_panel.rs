use gpui::{
    Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    prelude::FluentBuilder as _,
};
use gpui_component::{
    ActiveTheme as _, Selectable as _, Sizable as _, StyledExt,
    button::{Button, ButtonVariants as _},
};

use crate::library::Library;
use crate::ui::{CONTENT_PX, ROW_PANEL_HEIGHT};

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
        let render_start = crate::perf::start();
        let state = self.library.read(cx).active_state();

        let mut panel = div()
            .h_flex()
            .flex_wrap()
            .items_center()
            .w_full()
            .min_h(ROW_PANEL_HEIGHT)
            .px(CONTENT_PX)
            .py_1()
            .gap_x_4()
            .gap_y_2();

        for (key, values) in &state.schema {
            let checked: Vec<String> = state
                .selected
                .get(key)
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();

            let mut group = div().h_flex().flex_wrap().items_center().gap_1().child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(SharedString::from(format!("{}:", key))),
            );

            for value in values {
                let is_active = checked.contains(value);
                let key_owned = key.clone();
                let value_owned = value.clone();

                group = group.child(
                    Button::new(format!("filter-{key}:{value}"))
                        .xsmall()
                        .compact()
                        .label(value.clone())
                        .selected(is_active)
                        .when(is_active, |button| button.primary())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let key = key_owned.clone();
                            let value = value_owned.clone();
                            this.library.update(cx, |lib, cx| {
                                lib.toggle_value(&key, &value, cx);
                            });
                        })),
                );
            }

            panel = panel.child(group);
        }

        crate::perf::finish("filter_panel.render", render_start, || {
            format!(
                "keys={} values={}",
                state.schema.len(),
                state.schema.values().map(Vec::len).sum::<usize>()
            )
        });
        panel
    }
}
