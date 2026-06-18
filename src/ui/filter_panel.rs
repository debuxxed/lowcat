use gpui::{
    Context, Entity, InteractiveElement as _, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div,
};
use gpui_component::{ActiveTheme as _, StyledExt};

use crate::library::Library;
use crate::ui::CONTENT_PX;

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
            .px(CONTENT_PX)
            .py_1()
            .mb_2()
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

                let (border, bg, fg) = if is_active {
                    (
                        cx.theme().primary,
                        cx.theme().primary.opacity(0.15),
                        cx.theme().primary,
                    )
                } else {
                    (
                        cx.theme().border,
                        cx.theme().transparent,
                        cx.theme().foreground,
                    )
                };

                group = group.child(
                    div()
                        .id(SharedString::from(format!("{key}:{value}")))
                        .px_2()
                        .py_0p5()
                        .rounded_md()
                        .border_1()
                        .border_color(border)
                        .bg(bg)
                        .text_xs()
                        .text_color(fg)
                        .cursor_pointer()
                        .child(SharedString::from(value.clone()))
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
