use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;

pub const TAG_GENRE: &str = "GENRE";
pub const TAG_MOOD: &str = "MOOD";
pub const TAG_TYPE: &str = "TYPE";
pub const WAVEFORM_BAR_COUNT: usize = 256;
pub type WaveformBinary256 = [u8; WAVEFORM_BAR_COUNT];

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FolderTagAssignment {
    pub value: String,
    pub key: String,
    pub enabled: bool,
}

#[derive(Debug, Clone)]
pub struct FileRecord {
    pub name: String,
    pub path: PathBuf,
    pub support: FileSupport,
    pub stem: String,
    pub variants: Vec<FileVariant>,
    pub tags: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSupport {
    Native,
    Convertible,
}

impl FileRecord {
    pub fn is_convertible(&self) -> bool {
        self.support == FileSupport::Convertible
    }

    pub fn variant_for_extension(&self, extension: &str) -> Option<&FileVariant> {
        self.variants
            .iter()
            .find(|variant| variant.extension.eq_ignore_ascii_case(extension))
    }

    pub fn has_extension(&self, extension: &str) -> bool {
        self.variant_for_extension(extension).is_some()
    }

    pub fn primary_variant(&self) -> Option<&FileVariant> {
        self.variants.first()
    }

    pub fn primary_waveform(&self) -> Option<&WaveformBinary256> {
        self.primary_variant()
            .and_then(|variant| variant.waveform.as_ref())
    }

    pub fn conversion_targets(&self) -> Vec<AudioFormat> {
        AudioFormat::ALL
            .into_iter()
            .filter(|format| !self.has_extension(format.extension()))
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileVariant {
    pub path: PathBuf,
    pub extension: String,
    pub size: u64,
    pub modified: i64,
    pub first_seen_at: i64,
    pub waveform: Option<WaveformBinary256>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AudioFormat {
    Mp3,
    Wav,
    Opus,
    Flac,
}

impl AudioFormat {
    pub const ALL: [AudioFormat; 4] = [
        AudioFormat::Mp3,
        AudioFormat::Wav,
        AudioFormat::Opus,
        AudioFormat::Flac,
    ];

    pub fn extension(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Wav => "wav",
            AudioFormat::Opus => "opus",
            AudioFormat::Flac => "flac",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "MP3",
            AudioFormat::Wav => "WAV",
            AudioFormat::Opus => "OPUS",
            AudioFormat::Flac => "FLAC",
        }
    }
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.extension())
    }
}

impl FromStr for AudioFormat {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "mp3" => Ok(AudioFormat::Mp3),
            "wav" => Ok(AudioFormat::Wav),
            "opus" => Ok(AudioFormat::Opus),
            "flac" => Ok(AudioFormat::Flac),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConvertConflictBehavior {
    AddCopy,
    Overwrite,
}

impl ConvertConflictBehavior {
    pub fn key(self) -> &'static str {
        match self {
            ConvertConflictBehavior::AddCopy => "add_copy",
            ConvertConflictBehavior::Overwrite => "overwrite",
        }
    }
}

impl fmt::Display for ConvertConflictBehavior {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.key())
    }
}

impl FromStr for ConvertConflictBehavior {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "add_copy" => Ok(ConvertConflictBehavior::AddCopy),
            "overwrite" => Ok(ConvertConflictBehavior::Overwrite),
            _ => Err(()),
        }
    }
}

pub fn default_format_priority() -> Vec<AudioFormat> {
    vec![
        AudioFormat::Mp3,
        AudioFormat::Wav,
        AudioFormat::Opus,
        AudioFormat::Flac,
    ]
}

pub fn normalize_format_priority(priority: Vec<AudioFormat>) -> Vec<AudioFormat> {
    let mut out = Vec::new();
    for format in priority {
        if !out.contains(&format) {
            out.push(format);
        }
    }
    for format in AudioFormat::ALL {
        if !out.contains(&format) {
            out.push(format);
        }
    }
    out
}

pub fn supported_audio_extension(extension: &str) -> Option<AudioFormat> {
    AudioFormat::from_str(extension).ok()
}

#[derive(Default)]
pub struct CategoryState {
    pub schema: BTreeMap<String, Vec<String>>,
    pub selected: BTreeMap<String, BTreeSet<String>>,
    pub search: String,
    pub all_records: Arc<Vec<FileRecord>>,
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

pub fn normalize_tag_key(key: &str) -> Option<String> {
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    canonical_tag_key(key)
        .map(str::to_string)
        .or_else(|| Some(key.to_lowercase()))
}

pub fn normalize_tag_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let mut parts = value.split('/').map(str::trim);
    let parent = parts.next()?;
    if parent.is_empty() {
        return None;
    }
    let child = parts.next();
    if parts.next().is_some() || child.is_some_and(str::is_empty) {
        return None;
    }

    Some(match child {
        Some(child) => format!("{parent}/{child}"),
        None => parent.to_string(),
    })
}

pub(crate) fn split_subtag(value: &str) -> Option<(&str, &str)> {
    let (parent, child) = value.split_once('/')?;
    (!parent.is_empty() && !child.is_empty() && !child.contains('/')).then_some((parent, child))
}

pub(crate) fn tag_value_matches_filter(value: &str, filter: &str) -> bool {
    value == filter
        || value
            .strip_prefix(filter)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

pub fn tag_label(key: &str) -> &str {
    match canonical_tag_key(key) {
        Some(TAG_GENRE) => "genre",
        Some(TAG_MOOD) => "mood",
        Some(TAG_TYPE) => "type",
        _ => key,
    }
}

/// A record matches iff its name fuzzy-matches `search` (case-insensitive
/// ordered characters) AND, for every key with checked values, the record's
/// values for that key contain all of them.
#[cfg(test)]
pub fn record_matches(
    record: &FileRecord,
    search: &str,
    selected: &BTreeMap<String, BTreeSet<String>>,
) -> bool {
    record_matches_scoped(record, search, selected, true)
}

pub(crate) fn record_matches_scoped(
    record: &FileRecord,
    search: &str,
    selected: &BTreeMap<String, BTreeSet<String>>,
    include_tags: bool,
) -> bool {
    let filename_match = fuzzy_search_match(&record.name, search)
        || record
            .variants
            .iter()
            .any(|variant| fuzzy_search_match(&variant.extension, search))
        || (include_tags
            && record.tags.iter().any(|(key, values)| {
                fuzzy_search_match(key, search)
                    || values.iter().any(|value| fuzzy_search_match(value, search))
            }));
    if !filename_match {
        return false;
    }
    for (key, wanted) in selected {
        if wanted.is_empty() {
            continue;
        }
        match record.tags.get(key).or_else(|| {
            record
                .tags
                .iter()
                .find_map(|(tag_key, values)| tag_key.eq_ignore_ascii_case(key).then_some(values))
        }) {
            Some(values) => {
                if !wanted.iter().all(|wanted| {
                    values
                        .iter()
                        .any(|value| tag_value_matches_filter(value, wanted))
                }) {
                    return false;
                }
            }
            None => return false,
        }
    }
    true
}

pub(crate) fn fuzzy_search_match(text: &str, search: &str) -> bool {
    let text = text.to_lowercase();
    let mut text_chars = text.chars();
    search
        .to_lowercase()
        .chars()
        .all(|query_ch| text_chars.any(|text_ch| text_ch == query_ch))
}

#[cfg(test)]
pub(crate) fn record_search_sort_key(
    record: &FileRecord,
    search: &str,
) -> (u8, usize, usize, String, String) {
    record_search_sort_key_scoped(record, search, true)
}

pub(crate) fn record_search_sort_key_scoped(
    record: &FileRecord,
    search: &str,
    include_tags: bool,
) -> (u8, usize, usize, String, String) {
    let lower_name = record.name.to_lowercase();
    if search.is_empty() {
        return (0, 0, 0, lower_name, record.name.clone());
    }

    let mut best = search_sort_key_for_text(&record.name, search);
    for variant in &record.variants {
        merge_search_sort_key(
            &mut best,
            search_sort_key_for_text(&variant.extension, search),
            4,
        );
    }
    if include_tags {
        for (key, values) in &record.tags {
            merge_search_sort_key(&mut best, search_sort_key_for_text(key, search), 4);
            for value in values {
                merge_search_sort_key(&mut best, search_sort_key_for_text(value, search), 4);
            }
        }
    }

    let (class, span, start) = best.unwrap_or((u8::MAX, usize::MAX, usize::MAX));
    (class, span, start, lower_name, record.name.clone())
}

fn merge_search_sort_key(
    best: &mut Option<(u8, usize, usize)>,
    candidate: Option<(u8, usize, usize)>,
    class_offset: u8,
) {
    let Some((class, span, start)) = candidate else {
        return;
    };
    let candidate = (class.saturating_add(class_offset), span, start);
    if best.is_none_or(|current| candidate < current) {
        *best = Some(candidate);
    }
}

fn search_sort_key_for_text(text: &str, search: &str) -> Option<(u8, usize, usize)> {
    let (span, start) = fuzzy_match_span(text, search)?;
    let text_lower = text.to_lowercase();
    let search_lower = search.to_lowercase();
    let trimmed_search = search_lower.trim();
    let exact_word = !trimmed_search.is_empty()
        && text_lower
            .split(|ch: char| !ch.is_alphanumeric())
            .any(|word| word == trimmed_search);

    let class = if exact_word || text_lower.trim() == trimmed_search {
        0
    } else if text_lower.starts_with(trimmed_search) {
        1
    } else if text_lower.contains(trimmed_search) {
        2
    } else {
        3
    };

    Some((class, span, start))
}

fn fuzzy_match_span(text: &str, search: &str) -> Option<(usize, usize)> {
    let text = text.to_lowercase().chars().collect::<Vec<_>>();
    let search = search.to_lowercase().chars().collect::<Vec<_>>();
    if search.is_empty() {
        return Some((0, 0));
    }

    let mut best = None;
    for (start, text_ch) in text.iter().enumerate() {
        if *text_ch != search[0] {
            continue;
        }

        let mut query_index = 1;
        let mut end = start;
        while query_index < search.len() {
            end += 1;
            if end >= text.len() {
                break;
            }
            if text[end] == search[query_index] {
                query_index += 1;
            }
        }
        if query_index == search.len() {
            let candidate = (end - start + 1, start);
            if best.is_none_or(|current| candidate < current) {
                best = Some(candidate);
            }
        }
    }

    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(name: &str, tags: &[(&str, &[&str])]) -> FileRecord {
        FileRecord {
            name: name.to_string(),
            path: PathBuf::from(name),
            support: FileSupport::Native,
            stem: name
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(name)
                .to_string(),
            variants: vec![FileVariant {
                path: PathBuf::from(name),
                extension: name
                    .rsplit_once('.')
                    .map(|(_, extension)| extension.to_string())
                    .unwrap_or_default(),
                size: 0,
                modified: 0,
                first_seen_at: 0,
                waveform: None,
            }],
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
    fn search_is_case_insensitive_fuzzy_match() {
        let r = rec("Pulse_Drive.ogg", &[]);
        assert!(record_matches(&r, "drive", &BTreeMap::new()));
        assert!(record_matches(&r, "PULSE", &BTreeMap::new()));
        assert!(record_matches(&r, "psdv", &BTreeMap::new()));
        assert!(!record_matches(&r, "ambient", &BTreeMap::new()));
        assert!(!record_matches(&r, "drive pulse", &BTreeMap::new()));
    }

    #[test]
    fn fuzzy_search_skips_unqueried_punctuation() {
        let r = rec("it's me.wav", &[]);
        assert!(record_matches(&r, "its m", &BTreeMap::new()));
    }

    #[test]
    fn search_sort_prioritizes_exact_name_words() {
        let mut records = vec![
            rec("brass loop", &[]),
            rec("bassy", &[]),
            rec("sub bass loop", &[]),
            rec("bass", &[]),
        ];

        records.sort_by_key(|record| record_search_sort_key(record, "bass"));

        assert_eq!(
            records
                .iter()
                .map(|record| record.name.as_str())
                .collect::<Vec<_>>(),
            vec!["bass", "sub bass loop", "bassy", "brass loop"]
        );
    }

    #[test]
    fn search_matches_tag_keys_and_values() {
        let r = rec("pulse.flac", &[("Genre", &["Ambient"])]);
        assert!(record_matches(&r, "genre", &BTreeMap::new()));
        assert!(record_matches(&r, "ambient", &BTreeMap::new()));
    }

    #[test]
    fn normalizes_one_level_subtags() {
        assert_eq!(
            normalize_tag_value(" shitpost / comedy "),
            Some("shitpost/comedy".into())
        );
        assert_eq!(normalize_tag_value("shitpost"), Some("shitpost".into()));
        assert_eq!(normalize_tag_value("shitpost/comedy/meme"), None);
        assert_eq!(normalize_tag_value("shitpost/"), None);
        assert_eq!(normalize_tag_value("/comedy"), None);
    }

    #[test]
    fn parent_filter_matches_its_subtags() {
        let r = rec("a.ogg", &[("Use", &["shitpost/comedy"])]);
        assert!(record_matches(&r, "", &sel(&[("Use", &["shitpost"])])));
        assert!(record_matches(
            &r,
            "",
            &sel(&[("Use", &["shitpost/comedy"])])
        ));
        assert!(!record_matches(&r, "", &sel(&[("Use", &["shit"])])));
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
