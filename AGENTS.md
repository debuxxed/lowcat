## Notes
- For UI work, use the `gpui` and `gpui-component` skills.
- Whenever you add something to UI, check if the module is imported.
- Do not use `git --diff` unless the user explicitly asks for it.
- Do not set `LOWCAT_PROFILE` for normal `cargo run`; perf logs are only for explicit profiling.

## UI Verification Workflow
- For UI changes, Codex should run `cargo check` first.
- If `cargo check` passes, kill existing `lowcat` processes and start `cargo run` in a live PTY session.
- Keep the app running across turns while the user manually interacts with the UI.
- Add targeted debug output for the affected interaction path, and remove it once user confirms the code works as expected.
- Tell the user exactly what to try and what logs are expected.
- Use the user's observation plus the live logs to iterate before declaring the UI fix verified.
- Do not treat compile success alone as UI verification.
