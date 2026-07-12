use std::{
    fs,
    path::{Path, PathBuf},
};

use gpui::{
    ExternalPaths, InteractiveElement, IntoElement, ParentElement, SharedString, Styled, rgba,
};
use gpui_component::{ActiveTheme as _, StyledExt as _};

use super::{UI, titlebar::TITLEBAR_LEFT_OFFSET};
use crate::model::Category;

const DROPPED_LINK_MAX_BYTES: u64 = 64 * 1024;

impl UI {
    fn download_dropped_link(
        &mut self,
        category: Category,
        paths: &[PathBuf],
        cx: &mut gpui::Context<Self>,
    ) -> bool {
        let Some(link_text) = dropped_download_link_text(paths) else {
            return false;
        };

        self.library.update(cx, |lib, cx| {
            lib.download_from_clipboard(category, Some(link_text), cx);
        });
        true
    }

    /// Full-window overlay that fades in while OS files are dragged over the
    /// window, aligned with the titlebar categories.
    pub(super) fn render_drop_overlay(
        &self,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement + use<> {
        let base_bg = rgba(0x70707066);
        let highlight_bg = rgba(0x9a9a9aa6);
        let mut columns = gpui::div().h_flex().size_full().pl(TITLEBAR_LEFT_OFFSET);
        for category in Category::ALL {
            let column = gpui::div()
                .id(SharedString::from(format!(
                    "drop-overlay:{}",
                    category.label()
                )))
                .h_flex()
                .flex_1()
                .h_full()
                .items_center()
                .justify_center()
                .border_l_1()
                .border_color(rgba(0xffffff22))
                .bg(base_bg)
                .text_color(cx.theme().foreground)
                .child(SharedString::from(category.label()))
                .drag_over::<ExternalPaths>(move |style, paths, _, _| {
                    if paths.paths().is_empty() {
                        style
                    } else {
                        style.bg(highlight_bg)
                    }
                });

            columns = columns.child(column.on_drop(cx.listener(
                move |this, paths: &ExternalPaths, _, cx| {
                    let paths = paths.paths().to_vec();
                    if !this.download_dropped_link(category, &paths, cx) {
                        this.library
                            .update(cx, |lib, cx| lib.import_files(category, paths, cx));
                    }
                },
            )));
        }

        gpui::div()
            .id("drop-overlay")
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .opacity(0.)
            // `drag_over` alone does not register a hitbox. An empty `hover`
            // keeps the overlay detectable without changing layout.
            .hover(|style| style)
            .drag_over::<ExternalPaths>(|style, _, _, _| style.opacity(1.))
            .on_drop(cx.listener(|_, _: &ExternalPaths, _, _| {}))
            .child(columns)
    }
}

fn dropped_download_link_text(paths: &[PathBuf]) -> Option<String> {
    let [path] = paths else {
        return None;
    };
    let path_text = path.to_string_lossy();
    if let Some(url) = extract_dropped_youtube_url(&path_text) {
        return Some(url);
    }
    if !is_dropped_link_file(path) {
        return None;
    }
    let metadata = fs::metadata(path).ok()?;
    if !metadata.is_file() || metadata.len() > DROPPED_LINK_MAX_BYTES {
        return None;
    }

    let bytes = fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    extract_dropped_youtube_url(&text)
}

fn extract_dropped_youtube_url(text: &str) -> Option<String> {
    if let Ok(url) = crate::downloader::extract_youtube_url(text) {
        return Some(url);
    }

    let lower = text.to_ascii_lowercase();
    let markers = [
        "https://",
        "http://",
        "youtube.com/",
        "www.youtube.com/",
        "m.youtube.com/",
        "music.youtube.com/",
        "youtu.be/",
    ];
    markers.iter().find_map(|marker| {
        lower.match_indices(marker).find_map(|(start, _)| {
            let rest = &text[start..];
            let end = rest
                .find(|c: char| {
                    c.is_whitespace()
                        || matches!(
                            c,
                            '<' | '>' | '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | ','
                        )
                })
                .unwrap_or(rest.len());
            crate::downloader::extract_youtube_url(&rest[..end]).ok()
        })
    })
}

fn is_dropped_link_file(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("webloc" | "url" | "website" | "inetloc" | "txt")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_link_path(extension: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "lowcat-dropped-link-{}-{nanos}.{extension}",
            std::process::id()
        ))
    }

    #[test]
    fn extracts_dropped_youtube_link_file() {
        let path = temp_link_path("webloc");
        fs::write(
            &path,
            r#"<?xml version="1.0"?><plist><dict><key>URL</key><string>https://www.youtube.com/watch?v=abc123</string></dict></plist>"#,
        )
        .unwrap();

        let text = dropped_download_link_text(std::slice::from_ref(&path)).unwrap();

        assert_eq!(text, "https://www.youtube.com/watch?v=abc123");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_non_link_file_drop() {
        let path = temp_link_path("wav");
        fs::write(&path, "not a link").unwrap();

        assert!(dropped_download_link_text(std::slice::from_ref(&path)).is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn ignores_multiple_dropped_paths_for_downloader() {
        let first = temp_link_path("url");
        let second = temp_link_path("url");
        fs::write(&first, "URL=https://youtu.be/abc123").unwrap();
        fs::write(&second, "URL=https://youtu.be/def456").unwrap();

        assert!(dropped_download_link_text(&[first.clone(), second.clone()]).is_none());
        let _ = fs::remove_file(first);
        let _ = fs::remove_file(second);
    }
}
