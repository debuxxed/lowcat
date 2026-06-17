## Project Description
Lowcat is a GPUI desktop audio library app for organizing `.opus` and `.flac` files into category folders, with canonical audio tags for genre, mood, and type. It imports dropped files, converts unsupported incoming audio to `.opus`, and relies on `ffmpeg`/`ffprobe` plus `lofty` for media handling.

## Instructions
- For UI work, use the `gpui` and `gpui-component` skills.
- If changing UI, after `cargo check` passes, kill all running instances of current project processes non-blockingly and run `cargo run` again; don't wait for the build to finish.
- Whenever you add something to UI, check if the module is imported.
- Do not use `git --diff` unless the user explicitly asks for it.
