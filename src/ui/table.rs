use gpui::{Context, Entity, IntoElement, ParentElement, Render, Window};
use gpui_component::{Sizable, table::*};

use crate::library::Library;

pub struct FileTable {
    library: Entity<Library>,
}

impl FileTable {
    pub fn new(library: Entity<Library>, cx: &mut Context<Self>) -> Self {
        cx.observe(&library, |_, _, cx| cx.notify()).detach();
        Self { library }
    }
}

impl Render for FileTable {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let state = self.library.read(cx).active_state();

        let mut body = TableBody::new();
        for record in &state.results {
            let tags: String = record
                .tags
                .values()
                .flat_map(|values| values.iter())
                .cloned()
                .collect::<Vec<_>>()
                .join(", ");
            body = body.child(
                TableRow::new()
                    .child(TableCell::new().child(record.name.clone()))
                    .child(TableCell::new().child(tags)),
            );
        }

        Table::new()
            .xsmall()
            .child(
                TableHeader::new().child(
                    TableRow::new()
                        .child(TableHead::new().child("name"))
                        .child(TableHead::new().child("tags")),
                ),
            )
            .child(body)
    }
}
