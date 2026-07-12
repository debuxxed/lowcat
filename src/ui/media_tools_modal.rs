use gpui::{
    AnyElement, ExternalPaths, InteractiveElement, IntoElement, ParentElement, SharedString,
    Styled, prelude::FluentBuilder as _, px, relative, rgba,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, StyledExt as _, button::Button, scroll::ScrollableElement as _,
};

use super::{CONTENT_PX, UI, titlebar::TITLEBAR_HEIGHT};
use crate::media_tools::{MissingTool, SearchLocation};

impl UI {
    pub(super) fn render_media_tools_modal(
        &self,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement + use<> {
        let problems = self
            .media_tool_problems
            .iter()
            .map(|problem| render_media_tool_problem(problem, cx))
            .collect::<Vec<_>>();

        gpui::div()
            .id("media-tools-overlay")
            .absolute()
            .top(TITLEBAR_HEIGHT)
            .bottom_0()
            .left_0()
            .w_full()
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
                            .w_full()
                            .h(relative(0.86))
                            .v_flex()
                            .border_y_1()
                            .border_color(cx.theme().border)
                            .bg(cx.theme().popover)
                            .shadow_lg()
                            .child(
                                gpui::div()
                                    .w_full()
                                    .flex_shrink_0()
                                    .px(CONTENT_PX)
                                    .py_3()
                                    .child(
                                    gpui::div()
                                        .w_full()
                                        .min_w_0()
                                        .v_flex()
                                        .gap_1()
                                        .child(
                                            gpui::div()
                                                .text_base()
                                                .font_weight(gpui::FontWeight::BOLD)
                                                .child("Required tools missing"),
                                        )
                                        .child(
                                            gpui::div()
                                                .w_full()
                                                .min_w_0()
                                                .text_sm()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(
                                                    "Lowcat can't function without these tools.",
                                                ),
                                        ),
                                ),
                            )
                            .child(
                                gpui::div().flex_1().min_h_0().overflow_hidden().child(
                                    gpui::div()
                                        .size_full()
                                        .overflow_y_scrollbar()
                                        .v_flex()
                                        .gap_4()
                                        .px(CONTENT_PX)
                                        .pb_4()
                                        .children(problems),
                                ),
                            )
                            .child(
                                gpui::div()
                                    .w_full()
                                    .h(px(1.))
                                    .flex_shrink_0()
                                    .bg(cx.theme().border),
                            ),
                    ),
            )
    }
}

fn render_media_tool_problem(problem: &MissingTool, cx: &mut gpui::Context<UI>) -> AnyElement {
    let locations = if problem.search_locations.is_empty() {
        "No PATH or standard fallback directories".to_string()
    } else {
        problem
            .search_locations
            .iter()
            .map(|location| match location {
                SearchLocation::Path => "PATH".to_string(),
                SearchLocation::Directory(path) => path.display().to_string(),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    let solution = tool_solution(problem.name);

    gpui::div()
        .w_full()
        .min_w_0()
        .v_flex()
        .gap_2()
        .pt_3()
        .border_t_1()
        .border_color(cx.theme().border)
        .child(
            gpui::div()
                .w_full()
                .min_w_0()
                .text_sm()
                .font_weight(gpui::FontWeight::BOLD)
                .child(problem.name),
        )
        .child(
            gpui::div()
                .w_full()
                .min_w_0()
                .v_flex()
                .gap_0()
                .child(
                    gpui::div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .line_height(relative(1.35))
                        .text_color(cx.theme().muted_foreground)
                        .child("Looked up in"),
                )
                .child(
                    gpui::div()
                        .w_full()
                        .min_w_0()
                        .text_xs()
                        .line_height(relative(1.35))
                        .text_color(cx.theme().muted_foreground)
                        .child(locations),
                ),
        )
        .child(
            gpui::div()
                .w_full()
                .min_w_0()
                .h_flex()
                .items_center()
                .gap_2()
                .child(
                    gpui::div()
                        .min_w_0()
                        .flex_1()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(SharedString::from(format!(
                            "Possible solution: {}",
                            solution.text
                        ))),
                )
                .when_some(solution.url, |el, url| {
                    el.child(
                        Button::new(format!("download-{}", problem.name))
                            .xsmall()
                            .flex_shrink_0()
                            .label("Download")
                            .on_click(move |_, _, _| {
                                if let Err(error) = open::that(url) {
                                    eprintln!("failed to open {url}: {error}");
                                }
                            }),
                    )
                }),
        )
        .into_any_element()
}

struct ToolSolution {
    text: &'static str,
    url: Option<&'static str>,
}

fn tool_solution(tool: &str) -> ToolSolution {
    match tool {
        "ffmpeg" => ToolSolution {
            text: "Install FFmpeg.",
            url: cfg!(target_os = "macos").then_some("https://formulae.brew.sh/formula/ffmpeg"),
        },
        "ffprobe" => ToolSolution {
            text: "Install FFmpeg; ffprobe is included with it.",
            url: cfg!(target_os = "macos").then_some("https://formulae.brew.sh/formula/ffmpeg"),
        },
        "yt-dlp" => ToolSolution {
            text: "Install yt-dlp.",
            url: Some("https://github.com/yt-dlp/yt-dlp/wiki/Installation"),
        },
        _ => ToolSolution {
            text: "Install the missing tool.",
            url: None,
        },
    }
}
