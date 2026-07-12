use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::model::{AudioFormat, Category};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    category_folders: CategoryFolders,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    download_format: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    hidden_tag_groups: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    hidden_tag_columns: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "IntersectionTags::is_empty")]
    intersection_tags: IntersectionTags,
    #[serde(default = "default_preview_volume")]
    preview_volume: f32,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            category_folders: CategoryFolders::default(),
            download_format: None,
            hidden_tag_groups: BTreeSet::new(),
            hidden_tag_columns: BTreeSet::new(),
            intersection_tags: IntersectionTags::default(),
            preview_volume: default_preview_volume(),
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        let Ok(contents) = fs::read_to_string(path) else {
            return Self::default();
        };
        toml::from_str(&contents).unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        let contents = toml::to_string_pretty(self).map_err(io::Error::other)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, contents)
    }

    pub fn category_folder(&self, category: Category) -> Option<&Path> {
        match category {
            Category::Music => self.category_folders.music.as_deref(),
            Category::Sfx => self.category_folders.sfx.as_deref(),
        }
    }

    pub fn set_category_folder(&mut self, category: Category, path: PathBuf) {
        match category {
            Category::Music => self.category_folders.music = Some(path),
            Category::Sfx => self.category_folders.sfx = Some(path),
        }
    }

    pub fn download_format(&self) -> AudioFormat {
        self.download_format
            .as_deref()
            .and_then(|format| AudioFormat::from_str(format).ok())
            .unwrap_or(AudioFormat::Opus)
    }

    pub fn set_download_format(&mut self, format: AudioFormat) {
        self.download_format = Some(format.extension().to_string());
    }

    pub fn preview_volume(&self) -> f32 {
        self.preview_volume.clamp(0., 1.)
    }

    pub fn set_preview_volume(&mut self, volume: f32) {
        self.preview_volume = volume.clamp(0., 1.);
    }

    pub fn hidden_tag_groups(&self) -> BTreeSet<String> {
        self.hidden_tag_groups.clone()
    }

    pub fn set_hidden_tag_groups(&mut self, keys: BTreeSet<String>) {
        self.hidden_tag_groups = keys;
    }

    pub fn hidden_tag_columns(&self) -> BTreeSet<String> {
        self.hidden_tag_columns.clone()
    }

    pub fn set_hidden_tag_columns(&mut self, keys: BTreeSet<String>) {
        self.hidden_tag_columns = keys;
    }

    pub fn intersection_tags(&self, category: Category) -> BTreeMap<String, BTreeSet<String>> {
        self.intersection_tags.for_category(category).clone()
    }

    pub fn set_intersection_tags(
        &mut self,
        category: Category,
        tags: BTreeMap<String, BTreeSet<String>>,
    ) {
        *self.intersection_tags.for_category_mut(category) = tags;
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct IntersectionTags {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    music: BTreeMap<String, BTreeSet<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    sfx: BTreeMap<String, BTreeSet<String>>,
}

impl IntersectionTags {
    fn is_empty(&self) -> bool {
        self.music.is_empty() && self.sfx.is_empty()
    }

    fn for_category(&self, category: Category) -> &BTreeMap<String, BTreeSet<String>> {
        match category {
            Category::Music => &self.music,
            Category::Sfx => &self.sfx,
        }
    }

    fn for_category_mut(&mut self, category: Category) -> &mut BTreeMap<String, BTreeSet<String>> {
        match category {
            Category::Music => &mut self.music,
            Category::Sfx => &mut self.sfx,
        }
    }
}

fn default_preview_volume() -> f32 {
    1.
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct CategoryFolders {
    music: Option<PathBuf>,
    sfx: Option<PathBuf>,
}

pub fn settings_path() -> PathBuf {
    if let Some(config_home) = non_empty_env_path("XDG_CONFIG_HOME") {
        return config_home.join("lowcat").join("settings.toml");
    }

    if let Some(home) = non_empty_env_path("HOME") {
        return home.join(".config").join("lowcat").join("settings.toml");
    }

    PathBuf::from(".config")
        .join("lowcat")
        .join("settings.toml")
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    let value = env::var_os(key)?;
    if value.is_empty() {
        None
    } else {
        Some(PathBuf::from(value))
    }
}
