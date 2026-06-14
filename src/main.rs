mod backend;
mod library;
mod model;
mod ui;

use std::borrow::Cow;

use gpui::{
    actions, size, App, AppContext, Bounds, KeyBinding, Menu, MenuItem, WindowBounds,
    WindowOptions, px,
};
use gpui_component::{Root, Theme, ThemeMode};
use gpui_component_assets::Assets;
use gpui_platform::application;

use crate::ui::UI;

actions!(app, [Quit]);

fn main() {
    let app = application().with_assets(Assets);

    app.run(move |cx: &mut App| {
        gpui_component::init(cx);

        load_fonts(cx);
        Theme::change(ThemeMode::Dark, None, cx);
        Theme::global_mut(cx).font_family = "IBM Plex Sans".into();

        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.set_menus(vec![Menu::new("Lowcat").items([MenuItem::action("Quit", Quit)])]);

        cx.activate(true);

        cx.open_window(
            WindowOptions {
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

fn load_fonts(cx: &App) {
    let fonts: Vec<Cow<'static, [u8]>> = vec![
        Cow::Borrowed(include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Regular.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBold.ttf")),
        Cow::Borrowed(include_bytes!("../assets/fonts/ibm-plex-sans/IBMPlexSans-Italic.ttf")),
        Cow::Borrowed(include_bytes!(
            "../assets/fonts/ibm-plex-sans/IBMPlexSans-SemiBoldItalic.ttf"
        )),
    ];
    cx.text_system().add_fonts(fonts).expect("failed to load fonts");
}
