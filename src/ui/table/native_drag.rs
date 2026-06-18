use std::path::PathBuf;

use gpui::Window;

#[cfg(target_os = "macos")]
pub(super) fn start_file_drag(
    paths: Vec<PathBuf>,
    window: &mut Window,
    on_finish: impl Fn() + Send + 'static,
) -> bool {
    macos::start_file_drag(paths, window, on_finish)
}

#[cfg(target_os = "macos")]
mod macos {
    use std::path::PathBuf;

    use drag::{DragItem, Image, Options};
    use gpui::Window;

    const DRAG_PREVIEW_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 32, 0, 0, 0, 32, 8,
        6, 0, 0, 0, 115, 122, 122, 244, 0, 0, 0, 117, 73, 68, 65, 84, 120, 218, 99, 96, 160, 51,
        56, 118, 237, 221, 87, 100, 60, 160, 150, 211, 213, 1, 216, 44, 39, 232, 128, 216, 226,
        190, 255, 212, 192, 184, 44, 39, 202, 1, 159, 190, 253, 249, 74, 12, 198, 165, 118, 64, 29,
        0, 178, 96, 192, 28, 0, 179, 96, 64, 28, 128, 108, 1, 221, 29, 128, 110, 1, 93, 29, 48,
        160, 185, 0, 159, 195, 200, 46, 60, 70, 29, 48, 234, 0, 116, 7, 248, 166, 54, 189, 38, 6,
        143, 70, 193, 104, 26, 24, 141, 130, 209, 52, 48, 26, 5, 163, 14, 160, 170, 3, 168, 129,
        25, 70, 193, 96, 6, 0, 136, 232, 36, 138, 248, 176, 39, 250, 0, 0, 0, 0, 73, 69, 78, 68,
        174, 66, 96, 130,
    ];

    pub(super) fn start_file_drag(
        paths: Vec<PathBuf>,
        window: &mut Window,
        on_finish: impl Fn() + Send + 'static,
    ) -> bool {
        let start = crate::perf::start();
        let files = absolute_files(paths);
        if files.is_empty() {
            crate::perf::finish("native_drag.prepare", start, || "files=0".to_string());
            return false;
        }
        let file_count = files.len();
        crate::perf::finish("native_drag.prepare", start, || {
            format!("files={file_count}")
        });

        let start = crate::perf::start();
        let result = drag::start_drag(
            window,
            DragItem::Files(files),
            preview_image(),
            move |_, _| on_finish(),
            Options::default(),
        )
        .is_ok();
        crate::perf::finish("native_drag.start", start, || format!("ok={result}"));
        result
    }

    fn absolute_files(paths: Vec<PathBuf>) -> Vec<PathBuf> {
        paths
            .into_iter()
            .filter_map(|path| {
                if !path.is_file() {
                    return None;
                }
                if path.is_absolute() {
                    Some(path)
                } else {
                    path.canonicalize().ok()
                }
            })
            .collect()
    }

    fn preview_image() -> Image {
        Image::Raw(DRAG_PREVIEW_PNG.to_vec())
    }
}
