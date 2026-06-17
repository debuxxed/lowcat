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

    const GENERIC_DOCUMENT_ICON: &str =
        "/System/Library/CoreServices/CoreTypes.bundle/Contents/Resources/GenericDocumentIcon.icns";
    const FALLBACK_PREVIEW_PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6,
        0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84, 120, 156, 99, 0, 1, 0, 0, 5, 0, 1,
        13, 10, 45, 180, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];

    pub(super) fn start_file_drag(
        paths: Vec<PathBuf>,
        window: &mut Window,
        on_finish: impl Fn() + Send + 'static,
    ) -> bool {
        let files = absolute_files(paths);
        if files.is_empty() {
            return false;
        }

        drag::start_drag(
            window,
            DragItem::Files(files),
            preview_image(),
            move |_, _| on_finish(),
            Options::default(),
        )
        .is_ok()
    }

    fn absolute_files(paths: Vec<PathBuf>) -> Vec<PathBuf> {
        paths
            .into_iter()
            .filter_map(|path| {
                if path.is_file() {
                    path.canonicalize().ok()
                } else {
                    None
                }
            })
            .collect()
    }

    fn preview_image() -> Image {
        let icon_path = PathBuf::from(GENERIC_DOCUMENT_ICON);
        if icon_path.is_file() {
            Image::File(icon_path)
        } else {
            Image::Raw(FALLBACK_PREVIEW_PNG.to_vec())
        }
    }
}
