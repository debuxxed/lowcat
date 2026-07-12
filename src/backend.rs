use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::db::Database;
use crate::model::{
    AudioFormat, Category, ConvertConflictBehavior, FileRecord, FolderTagAssignment,
    WaveformBinary256,
};

mod conversion;
mod scanner;

use conversion::convert_file_to_format;
pub(crate) use conversion::import_to_folder;
use scanner::{file_stem, scan_category_folder};

#[derive(Clone, Debug)]
pub struct RenameRecord {
    pub stem: String,
    pub paths: Vec<PathBuf>,
}

pub struct Backend {
    db: Database,
    folders: BTreeMap<Category, PathBuf>,
}

impl Backend {
    pub fn new(db_path: PathBuf) -> io::Result<Self> {
        Ok(Self {
            db: Database::open(&db_path)?,
            folders: BTreeMap::new(),
        })
    }

    pub fn set_category_folder(&mut self, category: Category, path: PathBuf) -> io::Result<()> {
        self.folders.insert(category, path);
        self.refresh_category(category)
    }

    pub fn remember_category_folder(&mut self, category: Category, path: PathBuf) {
        self.folders.insert(category, path);
    }

    #[cfg(test)]
    pub fn refresh_all(&mut self) -> io::Result<()> {
        for category in Category::ALL {
            self.refresh_category(category)?;
        }
        Ok(())
    }

    pub fn refresh_category(&mut self, category: Category) -> io::Result<()> {
        let refresh_start = crate::perf::start();
        let Some(folder) = self.folders.get(&category) else {
            let summary = self.db.sync_category(category, Vec::new())?;
            crate::perf::finish("backend.refresh_category", refresh_start, || {
                format!(
                    "category={} records=0 missing_folder=true removed={}",
                    category.label(),
                    summary.removed
                )
            });
            return Ok(());
        };

        if !folder.is_dir() {
            let summary = self.db.sync_category(category, Vec::new())?;
            crate::perf::finish("backend.refresh_category", refresh_start, || {
                format!(
                    "category={} records=0 invalid_folder=true removed={}",
                    category.label(),
                    summary.removed
                )
            });
            return Ok(());
        }

        let fingerprints = self.db.file_fingerprints(category)?;
        let scan = scan_category_folder(category, folder, &fingerprints)?;

        let records_len = scan.records.len();
        let summary = self.db.sync_category(category, scan.records)?;
        crate::perf::finish("backend.refresh_category", refresh_start, || {
            format!(
                "category={} records={records_len} files_seen={} dirs_seen={} skipped={} reused={} tags_read={} added={} updated={} removed={}",
                category.label(),
                scan.files_seen,
                scan.dirs_seen,
                scan.skipped_seen,
                scan.reused_seen,
                scan.tags_read,
                summary.added,
                summary.updated,
                summary.removed
            )
        });
        Ok(())
    }

    pub fn filter(
        &self,
        category: Category,
        search: &str,
        selected: &BTreeMap<String, BTreeSet<String>>,
    ) -> Vec<FileRecord> {
        let category_folder = self.folders.get(&category).map(PathBuf::as_path);
        self.db
            .query_visible_rows(
                category,
                search,
                selected,
                &self
                    .format_priority()
                    .unwrap_or_else(|_| crate::model::default_format_priority()),
                category_folder,
            )
            .unwrap_or_default()
    }

    pub fn schema_for(&self, category: Category) -> BTreeMap<String, Vec<String>> {
        self.db.schema_for(category).unwrap_or_else(|_| {
            category
                .tag_keys()
                .iter()
                .map(|key| ((*key).to_string(), Vec::new()))
                .collect()
        })
    }

    pub fn add_tag_key(&mut self, category: Category, key: &str) -> io::Result<Option<String>> {
        self.db.add_tag_key(category, key)
    }

    pub fn remove_tag_key(&mut self, category: Category, key: &str) -> io::Result<bool> {
        self.db.remove_tag_key(category, key)
    }

    pub fn rename_tag_key(
        &mut self,
        category: Category,
        old_key: &str,
        new_key: &str,
    ) -> io::Result<()> {
        self.db.rename_tag_key(category, old_key, new_key)?;
        self.refresh_category(category)
    }

    pub fn add_tag(
        &mut self,
        category: Category,
        path: &Path,
        key: &str,
        value: &str,
    ) -> io::Result<()> {
        let stem = file_stem(path);
        self.db.add_tag(category, &stem, key, value)
    }

    pub fn remove_tag(
        &mut self,
        category: Category,
        path: &Path,
        key: &str,
        value: &str,
    ) -> io::Result<()> {
        let stem = file_stem(path);
        self.db.remove_tag(category, &stem, key, value)
    }

    pub fn rename_tag(
        &mut self,
        category: Category,
        path: &Path,
        key: &str,
        old_value: &str,
        new_value: &str,
    ) -> io::Result<()> {
        let stem = file_stem(path);
        self.db
            .rename_stem_tag_value(category, &stem, key, old_value, new_value)
    }

    pub fn rename_records(
        &mut self,
        category: Category,
        records: &[RenameRecord],
        new_stem: &str,
    ) -> io::Result<usize> {
        rename_record_files(records, new_stem)?;
        for record in records {
            self.db.rename_stem_tags(category, &record.stem, new_stem)?;
        }
        self.refresh_category(category)?;
        Ok(records.iter().map(|record| record.paths.len()).sum())
    }

    pub fn copy_tags_between_categories(
        &mut self,
        source: Category,
        destination: Category,
        source_path: &Path,
        destination_path: &Path,
    ) -> io::Result<()> {
        self.db.copy_stem_tags_between_categories(
            source,
            destination,
            &file_stem(source_path),
            &file_stem(destination_path),
        )
    }

    pub fn rename_tag_value(
        &mut self,
        category: Category,
        key: &str,
        old_value: &str,
        new_value: &str,
    ) -> io::Result<()> {
        self.db
            .rename_tag_value(category, key, old_value, new_value)?;
        self.refresh_category(category)
    }

    pub fn folder_tag_values(&self, category: Category) -> io::Result<Vec<String>> {
        let Some(folder) = self.folders.get(&category) else {
            return Ok(Vec::new());
        };
        if !folder.is_dir() {
            return Ok(Vec::new());
        }
        self.db.folder_tag_values(category, folder)
    }

    pub fn assign_folder_tags(
        &mut self,
        category: Category,
        assignments: &[FolderTagAssignment],
    ) -> io::Result<usize> {
        let Some(folder) = self.folders.get(&category) else {
            return Ok(0);
        };
        if !folder.is_dir() {
            return Ok(0);
        }
        self.db.assign_folder_tags(category, folder, assignments)
    }

    pub fn missing_waveform_cache_paths(&self, limit: usize) -> io::Result<Vec<PathBuf>> {
        self.db.missing_waveform_cache_paths(limit)
    }

    pub fn set_preview_waveform(&self, path: &Path, waveform: WaveformBinary256) -> io::Result<()> {
        self.db.set_preview_waveform(path, waveform)
    }

    #[cfg(test)]
    pub fn clear_preview_waveform(&self, path: &Path) -> io::Result<()> {
        self.db.clear_preview_waveform(path)
    }

    /// Import `source` into `category`'s folder. Supported formats are copied as-is.
    /// The source is only removed
    /// after the destination has been written and verified, so a failed import
    /// never deletes the source. Returns an error if the category has no folder.
    #[cfg(test)]
    pub fn import(&mut self, category: Category, source: &Path) -> io::Result<()> {
        let folder = self.folders.get(&category).cloned().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "category has no configured folder")
        })?;
        import_to_folder(&folder, source, |_| {})?;
        self.refresh_category(category)
    }

    pub fn format_priority(&self) -> io::Result<Vec<AudioFormat>> {
        self.db.format_priority()
    }

    pub fn set_format_priority(&self, priority: Vec<AudioFormat>) -> io::Result<()> {
        self.db.set_format_priority(priority)
    }

    pub fn convert_conflict_behavior(&self) -> io::Result<ConvertConflictBehavior> {
        self.db.convert_conflict_behavior()
    }

    pub fn set_convert_conflict_behavior(
        &self,
        behavior: ConvertConflictBehavior,
    ) -> io::Result<()> {
        self.db.set_convert_conflict_behavior(behavior)
    }

    pub fn convert_file_to_format(
        &self,
        source: &Path,
        target: AudioFormat,
        behavior: ConvertConflictBehavior,
        on_progress: impl FnMut(f32),
    ) -> io::Result<PathBuf> {
        let folder = source.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "source has no parent folder")
        })?;
        convert_file_to_format(source, folder, target, behavior, on_progress)
    }

    pub fn trash_files(paths: Vec<PathBuf>) -> io::Result<usize> {
        trash_files(paths)
    }
}

fn trash_files(paths: Vec<PathBuf>) -> io::Result<usize> {
    let mut seen = BTreeSet::new();
    let paths: Vec<PathBuf> = paths
        .into_iter()
        .filter(|path| seen.insert(path.clone()))
        .collect();
    let path_count = paths.len();
    if path_count == 0 {
        return Ok(0);
    }

    trash::delete_all(&paths).map_err(io::Error::other)?;
    Ok(path_count)
}

fn rename_record_files(records: &[RenameRecord], new_stem: &str) -> io::Result<usize> {
    let new_stem = valid_file_stem(new_stem)?;
    let mut source_paths = BTreeSet::new();
    let mut planned = Vec::new();

    for record in records {
        for source in &record.paths {
            source_paths.insert(source.clone());
            let extension = source
                .extension()
                .and_then(|extension| extension.to_str())
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidInput, "file has no extension")
                })?;
            let parent = source.parent().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "file has no parent folder")
            })?;
            planned.push((
                source.clone(),
                parent.join(format!("{new_stem}.{extension}")),
            ));
        }
    }

    let mut destination_paths = BTreeSet::new();
    for (source, destination) in &planned {
        if source == destination {
            continue;
        }
        if !destination_paths.insert(destination.clone()) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("rename would create duplicate {}", destination.display()),
            ));
        }
        if destination.exists() && !source_paths.contains(destination) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} already exists", destination.display()),
            ));
        }
    }

    let mut renamed = 0;
    for (source, destination) in planned {
        if source == destination {
            continue;
        }
        fs::rename(&source, &destination)?;
        renamed += 1;
    }
    Ok(renamed)
}

fn valid_file_stem(stem: &str) -> io::Result<&str> {
    let stem = stem.trim();
    if stem.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "new name cannot be empty",
        ));
    }
    if stem.contains('/') || stem.contains('\\') {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "new name cannot contain path separators",
        ));
    }
    Ok(stem)
}

#[cfg(test)]
mod tests;
