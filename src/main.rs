#![allow(unexpected_cfgs)]

mod backend;
mod db;
mod downloader;
mod library;
#[cfg(target_os = "macos")]
mod macos_url_drop;
mod media_tools;
mod model;
mod opus_source;
mod perf;
mod preview_player;
mod preview_waveform;
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
use crate::ui::{
    AssignFolderTags, ClearFilterTags, ClearFilterTagsAndSearch, NextCategory, PreviousCategory,
    RenameSelection, ToggleDownloader, ToggleFilters, ToggleSettings, UI,
};

actions!(
    app,
    [Quit, HideApp, MinimizeWindow, CloseWindow, ShowWindow]
);

fn main() {
    let app = application().with_assets(Assets);
    let main_library = Rc::new(RefCell::new(None::<Entity<Library>>));
    let media_tool_problems = Rc::new(media_tools::missing_required_tools());

    app.on_reopen({
        let main_library = main_library.clone();
        let media_tool_problems = media_tool_problems.clone();
        move |cx| {
            if let Some(library) = main_library.borrow().clone() {
                show_main_window(library, media_tool_problems.clone(), cx);
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
            let media_tool_problems = media_tool_problems.clone();
            move |_: &ShowWindow, cx| {
                show_main_window(library.clone(), media_tool_problems.clone(), cx);
            }
        });

        let bindings = [
            KeyBinding::new("cmd-q", Quit, None),
            KeyBinding::new("cmd-,", ToggleSettings, None),
            KeyBinding::new("cmd-e", ToggleFilters, None),
            KeyBinding::new("cmd-i", AssignFolderTags, None),
            KeyBinding::new("f2", RenameSelection, None),
            KeyBinding::new("shift-delete", ClearFilterTags, None),
            KeyBinding::new("shift-backspace", ClearFilterTags, None),
            KeyBinding::new("cmd-shift-delete", ClearFilterTagsAndSearch, None),
            KeyBinding::new("cmd-shift-backspace", ClearFilterTagsAndSearch, None),
            KeyBinding::new("shift-e", ToggleDownloader, None),
            KeyBinding::new("ctrl-tab", NextCategory, None),
            KeyBinding::new("ctrl-shift-tab", PreviousCategory, None),
        ];
        cx.bind_keys(bindings);
        bind_macos_window_keys(cx);
        cx.set_menus(app_menus());

        cx.activate(true);

        open_main_window(library, media_tool_problems.clone(), cx);
    })
}

fn show_main_window(
    library: Entity<Library>,
    media_tool_problems: Rc<Vec<media_tools::MissingTool>>,
    cx: &mut App,
) {
    if let Some(window_handle) = cx.windows().first().copied() {
        cx.activate(true);
        let _ = window_handle.update(cx, |_, window, _| window.activate_window());
    } else {
        open_main_window(library, media_tool_problems, cx);
    }
}

fn open_main_window(
    library: Entity<Library>,
    media_tool_problems: Rc<Vec<media_tools::MissingTool>>,
    cx: &mut App,
) {
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
            let view =
                cx.new(|cx| UI::new(library, media_tool_problems.as_ref().clone(), window, cx));
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
            MenuItem::action("Settings...", ToggleSettings),
            MenuItem::separator(),
            MenuItem::action("Hide Lowcat", HideApp),
            MenuItem::separator(),
            MenuItem::action("Quit", Quit),
        ]),
        Menu::new("File").items([MenuItem::action("Show Window", ShowWindow)]),
        Menu::new("Library").items([
            MenuItem::action("Rename", RenameSelection),
            MenuItem::separator(),
            MenuItem::action("Assign Folder Tags", AssignFolderTags),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Filters", ToggleFilters),
            MenuItem::action("Toggle Downloader", ToggleDownloader),
            MenuItem::separator(),
            MenuItem::action("Next Category", NextCategory),
            MenuItem::action("Previous Category", PreviousCategory),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Minimize", MinimizeWindow),
            MenuItem::action("Close Window", CloseWindow),
        ]),
    ]
}

#[cfg(not(target_os = "macos"))]
fn app_menus() -> Vec<Menu> {
    vec![
        Menu::new("Lowcat").items([
            MenuItem::action("Show Window", ShowWindow),
            MenuItem::action("Settings...", ToggleSettings),
            MenuItem::separator(),
            MenuItem::action("Quit", Quit),
        ]),
        Menu::new("Library").items([
            MenuItem::action("Rename", RenameSelection),
            MenuItem::separator(),
            MenuItem::action("Assign Folder Tags", AssignFolderTags),
        ]),
        Menu::new("View").items([
            MenuItem::action("Toggle Filters", ToggleFilters),
            MenuItem::action("Toggle Downloader", ToggleDownloader),
            MenuItem::separator(),
            MenuItem::action("Next Category", NextCategory),
            MenuItem::action("Previous Category", PreviousCategory),
        ]),
    ]
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
