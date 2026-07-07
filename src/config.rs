use std::{
    collections::BTreeSet,
    env, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::model::{AudioFormat, Category};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    category_folders: CategoryFolders,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    download_format: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    hidden_tag_groups: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    hidden_tag_columns: BTreeSet<String>,
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
