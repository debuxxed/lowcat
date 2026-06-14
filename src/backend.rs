use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Result;
use gpui::{App, Task};

use crate::model::{record_matches, Category, FileRecord};

pub trait LibraryBackend: Send + Sync {
    fn list(
        &self,
        category: Category,
        search: String,
        selected: BTreeMap<String, BTreeSet<String>>,
        cx: &App,
    ) -> Task<Result<Vec<FileRecord>>>;

    fn tag_keys(&self, category: Category, cx: &App) -> Task<Result<BTreeMap<String, Vec<String>>>>;
}

pub struct MockBackend {
    files: BTreeMap<Category, Vec<FileRecord>>,
}

impl MockBackend {
    /// Seeded with the sample data from the UI mockup, plus a couple of SFX entries that use
    /// different tag keys to exercise the dynamic-per-category schema.
    pub fn seeded() -> Self {
        let mut files = BTreeMap::new();
        files.insert(Category::Music, music_seed());
        files.insert(Category::Sfx, sfx_seed());
        Self { files }
    }

    /// Synchronous filtering used by `list`; kept separate so it is unit-testable without
    /// a GPUI executor.
    pub fn filter(
        &self,
        category: Category,
        search: &str,
        selected: &BTreeMap<String, BTreeSet<String>>,
    ) -> Vec<FileRecord> {
        self.files
            .get(&category)
            .map(|recs| {
                recs.iter()
                    .filter(|r| record_matches(r, search, selected))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Available tag keys and their values for a category, derived from the seed files.
    pub fn schema_for(&self, category: Category) -> BTreeMap<String, Vec<String>> {
        let mut schema: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        if let Some(recs) = self.files.get(&category) {
            for rec in recs {
                for (key, values) in &rec.tags {
                    let entry = schema.entry(key.clone()).or_default();
                    for v in values {
                        entry.insert(v.clone());
                    }
                }
            }
        }
        schema
            .into_iter()
            .map(|(k, set)| (k, set.into_iter().collect()))
            .collect()
    }
}

impl LibraryBackend for MockBackend {
    fn list(
        &self,
        category: Category,
        search: String,
        selected: BTreeMap<String, BTreeSet<String>>,
        _cx: &App,
    ) -> Task<Result<Vec<FileRecord>>> {
        Task::ready(Ok(self.filter(category, &search, &selected)))
    }

    fn tag_keys(&self, category: Category, _cx: &App) -> Task<Result<BTreeMap<String, Vec<String>>>> {
        Task::ready(Ok(self.schema_for(category)))
    }
}

fn rec(name: &str, tags: &[(&str, &[&str])]) -> FileRecord {
    FileRecord {
        name: name.to_string(),
        path: PathBuf::from(name),
        tags: tags
            .iter()
            .map(|(k, vs)| (k.to_string(), vs.iter().map(|v| v.to_string()).collect()))
            .collect(),
    }
}

fn music_seed() -> Vec<FileRecord> {
    vec![
        rec("pulse_drive.ogg", &[("Genre", &["Electronic"]), ("Mood", &["Tense", "Dark"])]),
        rec("dark_circuit.flac", &[("Genre", &["Electronic"]), ("Mood", &["Dark"])]),
        rec("ghost_signal.ogg", &[("Genre", &["Ambient"]), ("Mood", &["Melancholic"])]),
        rec("neon_rush.flac", &[("Genre", &["Electronic"]), ("Mood", &["Tense"])]),
        rec("void_drift.ogg", &[("Genre", &["Cinematic"]), ("Mood", &["Dark", "Neutral"])]),
    ]
}

fn sfx_seed() -> Vec<FileRecord> {
    vec![
        rec("door_slam.wav", &[("Type", &["Impact"]), ("Material", &["Wood"])]),
        rec("glass_break.wav", &[("Type", &["Impact"]), ("Material", &["Glass"])]),
        rec("footstep_metal.wav", &[("Type", &["Foley"]), ("Material", &["Metal"])]),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sel(pairs: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        pairs
            .iter()
            .map(|(k, vs)| (k.to_string(), vs.iter().map(|v| v.to_string()).collect()))
            .collect()
    }

    #[test]
    fn filter_no_criteria_returns_all_music() {
        let b = MockBackend::seeded();
        assert_eq!(b.filter(Category::Music, "", &BTreeMap::new()).len(), 5);
    }

    #[test]
    fn filter_applies_search_and_tags() {
        let b = MockBackend::seeded();
        let out = b.filter(Category::Music, "", &sel(&[("Genre", &["Electronic"]), ("Mood", &["Dark"])]));
        let names: Vec<_> = out.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["pulse_drive.ogg", "dark_circuit.flac"]);
    }

    #[test]
    fn schema_keys_differ_per_category() {
        let b = MockBackend::seeded();
        let music_keys: Vec<_> = b.schema_for(Category::Music).into_keys().collect();
        let sfx_keys: Vec<_> = b.schema_for(Category::Sfx).into_keys().collect();
        assert_eq!(music_keys, vec!["Genre".to_string(), "Mood".to_string()]);
        assert_eq!(sfx_keys, vec!["Material".to_string(), "Type".to_string()]);
    }

    #[test]
    fn schema_values_are_sorted_and_deduped() {
        let b = MockBackend::seeded();
        let schema = b.schema_for(Category::Music);
        assert_eq!(schema["Genre"], vec!["Ambient", "Cinematic", "Electronic"]);
    }
}
