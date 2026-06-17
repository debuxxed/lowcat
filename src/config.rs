use std::{
    env, fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::model::Category;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    category_folders: CategoryFolders,
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
