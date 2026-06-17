use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

pub const TAG_GENRE: &str = "GENRE";
pub const TAG_MOOD: &str = "MOOD";
pub const TAG_TYPE: &str = "TYPE";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Category {
    Music,
    Sfx,
}

impl Category {
    pub const ALL: [Category; 2] = [Category::Music, Category::Sfx];

    pub fn label(&self) -> &'static str {
        match self {
            Category::Music => "Music",
            Category::Sfx => "SFX",
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|category| *category == self)
            .unwrap();
        Self::ALL[(index + 1) % Self::ALL.len()]
    }

    pub fn previous(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|category| *category == self)
            .unwrap();
        Self::ALL[(index + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    pub fn tag_keys(self) -> &'static [&'static str] {
        match self {
            Category::Music => &[TAG_GENRE, TAG_MOOD],
            Category::Sfx => &[TAG_TYPE],
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub name: String,
    pub path: PathBuf,
    pub tags: BTreeMap<String, Vec<String>>,
}

#[derive(Default)]
pub struct CategoryState {
    pub schema: BTreeMap<String, Vec<String>>,
    pub selected: BTreeMap<String, BTreeSet<String>>,
    pub search: String,
    pub results: Vec<FileRecord>,
}

pub fn canonical_tag_key(key: &str) -> Option<&'static str> {
    if key.eq_ignore_ascii_case(TAG_GENRE) {
        Some(TAG_GENRE)
    } else if key.eq_ignore_ascii_case(TAG_MOOD) {
        Some(TAG_MOOD)
    } else if key.eq_ignore_ascii_case(TAG_TYPE) {
        Some(TAG_TYPE)
    } else {
        None
    }
}

pub fn tag_label(key: &str) -> &str {
    match canonical_tag_key(key) {
        Some(TAG_GENRE) => "Genre",
        Some(TAG_MOOD) => "Mood",
        Some(TAG_TYPE) => "Type",
        _ => key,
    }
}

/// A record matches iff its name contains `search` (case-insensitive) AND, for every
/// key with checked values, the record's values for that key contain all of them.
pub fn record_matches(
    record: &FileRecord,
    search: &str,
    selected: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    if !record.name.to_lowercase().contains(&search.to_lowercase()) {
        return false;
    }
    for (key, wanted) in selected {
        if wanted.is_empty() {
            continue;
        }
        match record.tags.get(key) {
            Some(values) => {
                if !wanted.iter().all(|w| values.contains(w)) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn sel(pairs: &[(&str, &[&str])]) -> BTreeMap<String, BTreeSet<String>> {
        pairs
            .iter()
            .map(|(k, vs)| (k.to_string(), vs.iter().map(|v| v.to_string()).collect()))
            .collect()
    }

    #[test]
    fn empty_filters_match_everything() {
        let r = rec("song.ogg", &[("Genre", &["Electronic"])]);
        assert!(record_matches(&r, "", &BTreeMap::new()));
    }

    #[test]
    fn search_is_case_insensitive_substring() {
        let r = rec("Pulse_Drive.ogg", &[]);
        assert!(record_matches(&r, "drive", &BTreeMap::new()));
        assert!(record_matches(&r, "PULSE", &BTreeMap::new()));
        assert!(!record_matches(&r, "ambient", &BTreeMap::new()));
    }

    #[test]
    fn all_checked_values_within_a_key_must_be_present() {
        let r = rec("a.ogg", &[("Mood", &["Dark", "Tense"])]);
        assert!(record_matches(&r, "", &sel(&[("Mood", &["Dark"])])));
        assert!(record_matches(
            &r,
            "",
            &sel(&[("Mood", &["Dark", "Tense"])])
        ));
        assert!(!record_matches(
            &r,
            "",
            &sel(&[("Mood", &["Dark", "Uplifting"])])
        ));
    }

    #[test]
    fn checked_values_across_keys_are_anded() {
        let r = rec("a.ogg", &[("Genre", &["Electronic"]), ("Mood", &["Dark"])]);
        assert!(record_matches(
            &r,
            "",
            &sel(&[("Genre", &["Electronic"]), ("Mood", &["Dark"])])
        ));
        assert!(!record_matches(
            &r,
            "",
            &sel(&[("Genre", &["Electronic"]), ("Mood", &["Tense"])])
        ));
    }

    #[test]
    fn missing_key_fails_when_that_key_is_filtered() {
        let r = rec("a.ogg", &[("Genre", &["Electronic"])]);
        assert!(!record_matches(&r, "", &sel(&[("Mood", &["Dark"])])));
    }
}
