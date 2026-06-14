mod filter_panel;
mod table;
mod toolbar;

use gpui::{
    AppContext, Context, Entity, IntoElement, ParentElement, Render, Styled, Window, div,
};
use gpui_component::StyledExt;

use crate::backend::MockBackend;
use crate::library::Library;
use crate::ui::{filter_panel::FilterPanel, table::FileTable, toolbar::Toolbar};

pub struct UI {
    toolbar: Entity<Toolbar>,
    filter_panel: Entity<FilterPanel>,
    table: Entity<FileTable>,
}

impl UI {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let library = cx.new(|cx| Library::new(MockBackend::seeded(), cx));
        Self {
            toolbar: cx.new(|cx| Toolbar::new(library.clone(), window, cx)),
            filter_panel: cx.new(|cx| FilterPanel::new(library.clone(), cx)),
            table: cx.new(|cx| FileTable::new(library.clone(), cx)),
        }
    }
}

impl Render for UI {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .v_flex()
            .child(self.toolbar.clone())
            .child(self.filter_panel.clone())
            .child(self.table.clone())
    }
}
