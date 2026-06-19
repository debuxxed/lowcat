mod backend;
mod db;
mod library;
mod model;
mod perf;
mod ui;

use std::borrow::Cow;
use std::process::{Command, Stdio};

use gpui::{
    App, AppContext, Bounds, KeyBinding, Menu, MenuItem, WindowBounds, WindowOptions, actions, px,
    size,
};
use gpui_component::{Root, Theme, ThemeMode, TitleBar};
use gpui_component_assets::Assets;
use gpui_platform::application;

use crate::ui::{NextCategory, PreviousCategory, ToggleFilters, UI};

actions!(app, [Quit]);

fn main() {
    if let Err(error) = check_media_tools() {
        eprintln!("{error}");
        std::process::exit(1);
    }

    let app = application().with_assets(Assets);

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);

        load_fonts(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        Theme::global_mut(cx).font_family = ".ZedMono".into();

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-e", ToggleFilters, None),
            KeyBinding::new("ctrl-tab", NextCategory, None),
            KeyBinding::new("ctrl-shift-tab", PreviousCategory, None),
        ]);
        cx.set_menus(vec![
            Menu::new("Lowcat").items([MenuItem::action("Quit", Quit)]),
        ]);

        cx.on_window_closed(|app, _window_id| {
            app.quit();
        })
        .detach();

        cx.activate(true);

        cx.open_window(
            WindowOptions {
                titlebar: Some(title_bar_options()),
                window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                    None,
                    size(px(800.), px(600.)),
                    cx,
                ))),
                ..Default::default()
            },
            |window, cx| {
                let view = cx.new(|cx| UI::new(window, cx));
                cx.new(|cx| Root::new(view, window, cx))
            },
        )
        .expect("Failed to open window");
    })
}

fn check_media_tools() -> Result<(), String> {
    let missing: Vec<&str> = ["ffmpeg", "ffprobe"]
        .into_iter()
        .filter(|tool| !tool_available(tool))
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Lowcat requires ffmpeg and ffprobe in PATH. Missing: {}",
            missing.join(", ")
        ))
    }
}

fn tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn title_bar_options() -> gpui::TitlebarOptions {
    let mut options = TitleBar::title_bar_options();
    options.traffic_light_position = Some(gpui::point(px(12.), px(12.)));
    options
}

fn load_fonts(cx: &App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/lilex/Lilex-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/lilex/Lilex-Bold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/lilex/Lilex-Italic.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/lilex/Lilex-BoldItalic.ttf")),
    ];
    cx.text_system()
        .add_fonts(fonts)
        .expect("failed to load fonts");
}
