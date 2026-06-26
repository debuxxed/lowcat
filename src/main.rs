mod backend;
mod db;
mod downloader;
mod library;
mod media_tools;
mod model;
mod perf;
mod ui;

use std::borrow::Cow;
use std::cell::RefCell;
use std::rc::Rc;

use gpui::{
    App, AppContext, Bounds, Entity, KeyBinding, Menu, MenuItem, WindowBounds, WindowOptions,
    actions, px, size,
};
use gpui_component::{Root, Theme, ThemeMode, TitleBar};
use gpui_component_assets::Assets;
use gpui_platform::application;

use crate::library::Library;
use crate::ui::{NextCategory, PreviousCategory, ToggleDownloader, ToggleFilters, UI};

actions!(
    app,
    [Quit, HideApp, MinimizeWindow, CloseWindow, ShowWindow]
);

fn main() {
    if let Err(error) = check_media_tools() {
        eprintln!("{error}");
        std::process::exit(1);
    }

    let app = application().with_assets(Assets);
    let main_library = Rc::new(RefCell::new(None::<Entity<Library>>));

    app.on_reopen({
        let main_library = main_library.clone();
        move |cx| {
            if let Some(library) = main_library.borrow().clone() {
                show_main_window(library, cx);
            }
        }
    });

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);

        load_fonts(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        Theme::global_mut(cx).font_family = ".ZedMono".into();

        cx.on_action(|_: &Quit, cx| cx.quit());
        #[cfg(target_os = "macos")]
        cx.on_action(|_: &HideApp, cx| {
            cx.hide();
        });

        let library = cx.new(|cx| Library::new_for_app(cx));
        *main_library.borrow_mut() = Some(library.clone());
        cx.on_action({
            let library = library.clone();
            move |_: &ShowWindow, cx| {
                show_main_window(library.clone(), cx);
            }
        });

        let bindings = [
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-e", ToggleFilters, None),
            KeyBinding::new("shift-e", ToggleDownloader, None),
            KeyBinding::new("ctrl-tab", NextCategory, None),
            KeyBinding::new("ctrl-shift-tab", PreviousCategory, None),
        ];
        cx.bind_keys(bindings);
        bind_macos_window_keys(cx);
        cx.set_menus(app_menus());

        cx.activate(true);

        open_main_window(library, cx);
    })
}

fn show_main_window(library: Entity<Library>, cx: &mut App) {
    if let Some(window_handle) = cx.windows().first().copied() {
        cx.activate(true);
        let _ = window_handle.update(cx, |_, window, _| window.activate_window());
    } else {
        open_main_window(library, cx);
    }
}

fn open_main_window(library: Entity<Library>, cx: &mut App) {
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
            let view = cx.new(|cx| UI::new(library, window, cx));
            cx.new(|cx| Root::new(view, window, cx))
        },
    )
    .expect("Failed to open window");
}

#[cfg(target_os = "macos")]
fn bind_macos_window_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-h", HideApp, None),
        KeyBinding::new("cmd-m", MinimizeWindow, None),
        KeyBinding::new("cmd-w", CloseWindow, None),
    ]);
}

#[cfg(not(target_os = "macos"))]
fn bind_macos_window_keys(_cx: &mut App) {}

#[cfg(target_os = "macos")]
fn app_menus() -> Vec<Menu> {
    vec![
        Menu::new("Lowcat").items([
            MenuItem::action("Hide Lowcat", HideApp),
            MenuItem::separator(),
            MenuItem::action("Quit", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Show Window", ShowWindow)]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Close Window", CloseWindow),
        ]),
    ]
}

#[cfg(not(target_os = "macos"))]
fn app_menus() -> Vec<Menu> {
    vec![Menu::new("Lowcat").items([
        MenuItem::action("Show Window", ShowWindow),
        MenuItem::separator(),
        MenuItem::action("Quit", Quit),
    ])]
}

fn check_media_tools() -> Result<(), String> {
    let missing: Vec<&str> = ["ffmpeg", "ffprobe"]
        .into_iter()
        .filter(|tool| !media_tools::available(tool))
        .collect();

    if missing.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Lowcat requires ffmpeg and ffprobe in PATH or a standard Homebrew location. Missing: {}",
            missing.join(", ")
        ))
    }
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
