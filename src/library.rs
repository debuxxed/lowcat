use std::collections::BTreeMap;

use gpui::{Context, Task};

use crate::backend::{LibraryBackend, MockBackend};
use crate::model::{Category, CategoryState};

pub struct Library {
    backend: MockBackend,
    active: Category,
    states: BTreeMap<Category, CategoryState>,
    _task: Option<Task<()>>,
}

impl Library {
    pub fn new(backend: MockBackend, cx: &mut Context<Self>) -> Self {
        let states = Category::ALL
            .into_iter()
            .map(|c| (c, CategoryState::default()))
            .collect();
        let mut this = Self {
            backend,
            active: Category::Music,
            states,
            _task: None,
        };
        this.init(cx);
        this
    }

    pub fn active(&self) -> Category {
        self.active
    }

    pub fn active_state(&self) -> &CategoryState {
        &self.states[&self.active]
    }

    pub fn set_category(&mut self, category: Category, cx: &mut Context<Self>) {
        if self.active != category {
            self.active = category;
            cx.notify(); // results are cached per category; no backend call
        }
    }

    pub fn set_search(&mut self, search: String, cx: &mut Context<Self>) {
        let active = self.active;
        if let Some(state) = self.states.get_mut(&active) {
            state.search = search;
        }
        self.refresh(cx);
    }

    pub fn toggle_value(&mut self, key: &str, value: &str, cx: &mut Context<Self>) {
        let active = self.active;
        if let Some(state) = self.states.get_mut(&active) {
            let set = state.selected.entry(key.to_string()).or_default();
            if !set.remove(value) {
                set.insert(value.to_string());
            }
        }
        self.refresh(cx);
    }

    pub fn remove_value(&mut self, key: &str, value: &str, cx: &mut Context<Self>) {
        let active = self.active;
        if let Some(state) = self.states.get_mut(&active) {
            if let Some(set) = state.selected.get_mut(key) {
                set.remove(value);
            }
        }
        self.refresh(cx);
    }

    /// Load tag schema + initial results for every category once at construction.
    fn init(&mut self, cx: &mut Context<Self>) {
        let mut loaders = Vec::new();
        for category in Category::ALL {
            let schema_task = self.backend.tag_keys(category, cx);
            let list_task = self.backend.list(category, String::new(), BTreeMap::new(), cx);
            loaders.push((category, schema_task, list_task));
        }
        // A single task loads every category; storing each in a separate field would drop
        // (cancel) all but the last.
        self._task = Some(cx.spawn(async move |this, cx| {
            for (category, schema_task, list_task) in loaders {
                let schema = schema_task.await;
                let results = list_task.await;
                this.update(cx, |this, cx| {
                    if let Some(state) = this.states.get_mut(&category) {
                        if let Ok(schema) = schema {
                            state.schema = schema;
                        }
                        if let Ok(results) = results {
                            state.results = results;
                        }
                    }
                    cx.notify();
                })
                .ok();
            }
        }));
    }

    /// Re-run the backend query for the active category and store the results.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let active = self.active;
        let (search, selected) = {
            let state = &self.states[&active];
            (state.search.clone(), state.selected.clone())
        };
        let task = self.backend.list(active, search, selected, cx);
        self._task = Some(cx.spawn(async move |this, cx| {
            if let Ok(results) = task.await {
                this.update(cx, |this, cx| {
                    if let Some(state) = this.states.get_mut(&active) {
                        state.results = results;
                    }
                    cx.notify();
                })
                .ok();
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::AppContext as _;

    #[gpui::test]
    fn toggling_a_value_filters_active_results(cx: &mut gpui::TestAppContext) {
        let library = cx.new(|cx| Library::new(MockBackend::seeded(), cx));
        cx.run_until_parked(); // let init() tasks complete

        // All 5 music records loaded initially.
        let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
        assert_eq!(count, 5);

        // Check Genre=Electronic -> 3 records.
        library.update(cx, |lib, cx| lib.toggle_value("Genre", "Electronic", cx));
        cx.run_until_parked();
        let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
        assert_eq!(count, 3);

        // Switching category swaps to the cached SFX results (3) without losing music state.
        library.update(cx, |lib, cx| lib.set_category(Category::Sfx, cx));
        cx.run_until_parked();
        let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
        assert_eq!(count, 3);

        // Music's Electronic filter is still cached.
        library.update(cx, |lib, cx| lib.set_category(Category::Music, cx));
        let count = library.read_with(cx, |lib, _| lib.active_state().results.len());
        assert_eq!(count, 3);
    }
}
