use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::executor::block_on;
use sea_query::{Alias, Expr, Query, SqliteQueryBuilder};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::model::{
    AudioFormat, Category, ConvertConflictBehavior, FileRecord, FileSupport, FileVariant,
    FolderTagAssignment, WaveformBinary256, default_format_priority, normalize_format_priority,
    normalize_tag_key, normalize_tag_value, record_matches_scoped, record_search_sort_key_scoped,
    split_subtag,
};

const FORMAT_PRIORITY_KEY: &str = "format_priority";
const CONVERT_CONFLICT_KEY: &str = "convert_conflict";
const DEFAULT_TAG_KEYS_SEEDED_KEY: &str = "default_tag_keys_seeded";
const PREVIEW_WAVEFORM_VERSION_KEY: &str = "preview_waveform_version";
const PREVIEW_WAVEFORM_VERSION: &str = "2";

pub struct Database {
    pool: SqlitePool,
}

struct FileRow {
    path: PathBuf,
    stem: String,
    extension: String,
    size: u64,
    modified: i64,
    first_seen_at: i64,
    preview_waveform: Option<WaveformBinary256>,
}

#[derive(Debug, Clone)]
pub struct FileScanRecord {
    pub path: PathBuf,
    pub stem: String,
    pub extension: String,
    pub size: u64,
    pub modified: i64,
    pub tags: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyncSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
}

impl Database {
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
            .map_err(io::Error::other)?
            .create_if_missing(true);
        let pool = block_on(
            SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options),
        )
        .map_err(io::Error::other)?;
        let db = Self { pool };
        db.init()?;
        Ok(db)
    }

    pub fn init(&self) -> io::Result<()> {
        block_on(async {
            for sql in [
                "CREATE TABLE IF NOT EXISTS files (
                    category TEXT NOT NULL,
                    path TEXT NOT NULL PRIMARY KEY,
                    stem TEXT NOT NULL,
                    extension TEXT NOT NULL,
                    size INTEGER NOT NULL,
                    modified INTEGER NOT NULL,
                    first_seen_at INTEGER NOT NULL DEFAULT 0,
                    preview_waveform BLOB CHECK(preview_waveform IS NULL OR length(preview_waveform) = 256),
                    search_text TEXT NOT NULL
                )",
                "CREATE INDEX IF NOT EXISTS files_category_stem ON files(category, stem)",
                "CREATE INDEX IF NOT EXISTS files_search ON files(category, search_text)",
                "CREATE TABLE IF NOT EXISTS tag_keys (
                    category TEXT NOT NULL,
                    key TEXT NOT NULL,
                    PRIMARY KEY(category, key)
                )",
                "CREATE TABLE IF NOT EXISTS tag_values (
                    category TEXT NOT NULL,
                    stem TEXT NOT NULL,
                    key TEXT NOT NULL,
                    value TEXT NOT NULL,
                    PRIMARY KEY(category, stem, key, value)
                )",
                "CREATE INDEX IF NOT EXISTS tag_values_lookup ON tag_values(category, key, value, stem)",
                "CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                )",
            ] {
                sqlx::query(sql).execute(&self.pool).await?;
            }
            migrate_tag_keys(&self.pool).await?;
            migrate_files_first_seen_at(&self.pool).await?;
            migrate_files_preview_waveform(&self.pool).await?;
            seed_tag_keys_from_values(&self.pool).await?;
            seed_default_tag_keys(&self.pool).await?;

            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    pub fn sync_category(
        &self,
        category: Category,
        records: Vec<FileScanRecord>,
    ) -> io::Result<SyncSummary> {
        block_on(async {
            let category_key = category_key(category);
            let mut next_first_seen_at = current_unix_millis();
            let existing_rows =
                sqlx::query("SELECT path, stem, size, modified FROM files WHERE category = ?")
                    .bind(category_key)
                    .fetch_all(&self.pool)
                    .await?;
            let mut existing = BTreeMap::new();
            let mut existing_stems = BTreeSet::new();
            for row in existing_rows {
                existing_stems.insert(row.get::<String, _>("stem"));
                existing.insert(
                    row.get::<String, _>("path"),
                    (
                        row.get::<i64, _>("size") as u64,
                        row.get::<i64, _>("modified"),
                    ),
                );
            }

            let mut seen = BTreeSet::new();
            let mut summary = SyncSummary::default();
            for record in records {
                let path = record.path.to_string_lossy().to_string();
                seen.insert(path.clone());
                let search_text = format!(
                    "{} {} {} {}",
                    record.stem,
                    record.extension,
                    category.label(),
                    record
                        .tags
                        .values()
                        .flatten()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ")
                )
                .to_lowercase();
                let changed = existing.get(&path).is_none_or(|(size, modified)| {
                    *size != record.size || *modified != record.modified
                });
                if existing.contains_key(&path) {
                    if changed {
                        summary.updated += 1;
                    }
                } else {
                    summary.added += 1;
                }

                sqlx::query(
                    "INSERT INTO files(category, path, stem, extension, size, modified, first_seen_at, preview_waveform, search_text)
                     VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?)
                     ON CONFLICT(path) DO UPDATE SET
                        category = excluded.category,
                        stem = excluded.stem,
                        extension = excluded.extension,
                        size = excluded.size,
                        modified = excluded.modified,
                        preview_waveform = CASE
                            WHEN files.size != excluded.size OR files.modified != excluded.modified THEN NULL
                            ELSE files.preview_waveform
                        END,
                        search_text = excluded.search_text",
                )
                .bind(category_key)
                .bind(&path)
                .bind(&record.stem)
                .bind(&record.extension)
                .bind(record.size as i64)
                .bind(record.modified)
                .bind(next_first_seen_at)
                .bind(search_text)
                .execute(&self.pool)
                .await?;
                next_first_seen_at += 1;

                let tag_count: i64 = sqlx::query(
                    "SELECT COUNT(*) AS count FROM tag_values WHERE category = ? AND stem = ?",
                )
                .bind(category_key)
                .bind(&record.stem)
                .fetch_one(&self.pool)
                .await?
                .get("count");
                if !existing_stems.contains(&record.stem) && tag_count == 0 {
                    for (key, values) in record.tags {
                        let Some(key) = normalize_tag_key(&key) else {
                            continue;
                        };
                        let key = canonical_existing_tag_key(&self.pool, category, &key)
                            .await?
                            .unwrap_or(key);
                        ensure_tag_key(&self.pool, category, &key).await?;
                        for value in values {
                            let Some(value) = normalize_tag_value(&value) else {
                                continue;
                            };
                            sqlx::query(
                                "INSERT OR IGNORE INTO tag_values(category, stem, key, value)
                                 VALUES (?, ?, ?, ?)",
                            )
                            .bind(category_key)
                            .bind(&record.stem)
                            .bind(&key)
                            .bind(value)
                            .execute(&self.pool)
                            .await?;
                        }
                    }
                }
            }

            for path in existing.keys() {
                if !seen.contains(path) {
                    summary.removed += 1;
                    sqlx::query("DELETE FROM files WHERE path = ?")
                        .bind(path)
                        .execute(&self.pool)
                        .await?;
                }
            }
            sqlx::query(
                "DELETE FROM tag_values
                 WHERE category = ?
                   AND NOT EXISTS (
                       SELECT 1 FROM files
                       WHERE files.category = tag_values.category
                         AND files.stem = tag_values.stem
                   )",
            )
            .bind(category_key)
            .execute(&self.pool)
            .await?;

            Ok::<_, sqlx::Error>(summary)
        })
        .map_err(io::Error::other)
    }

    pub fn file_fingerprints(
        &self,
        category: Category,
    ) -> io::Result<BTreeMap<String, (u64, i64)>> {
        let rows = block_on(async {
            sqlx::query("SELECT path, size, modified FROM files WHERE category = ?")
                .bind(category_key(category))
                .fetch_all(&self.pool)
                .await
        })
        .map_err(io::Error::other)?;

        let mut fingerprints = BTreeMap::new();
        for row in rows {
            fingerprints.insert(
                row.get::<String, _>("path"),
                (
                    row.get::<i64, _>("size") as u64,
                    row.get::<i64, _>("modified"),
                ),
            );
        }
        Ok(fingerprints)
    }

    pub fn query_visible_rows(
        &self,
        category: Category,
        search: &str,
        selected: &BTreeMap<String, BTreeSet<String>>,
        priority: &[AudioFormat],
        category_folder: Option<&Path>,
    ) -> io::Result<Vec<FileRecord>> {
        self.query_visible_rows_scoped(category, search, selected, true, priority, category_folder)
    }

    pub fn query_visible_rows_scoped(
        &self,
        category: Category,
        search: &str,
        selected: &BTreeMap<String, BTreeSet<String>>,
        include_tags: bool,
        priority: &[AudioFormat],
        category_folder: Option<&Path>,
    ) -> io::Result<Vec<FileRecord>> {
        let selected = canonical_selected(selected);
        let has_filter = !search.is_empty() || selected.values().any(|values| !values.is_empty());
        let (rows, tags) = if has_filter && search.is_ascii() {
            let stems = self.matching_stems(category, search, &selected, include_tags)?;
            if stems.is_empty() {
                return Ok(Vec::new());
            }
            (
                self.file_rows_for_stems(category, &stems)?,
                self.tags_for_stems(category, &stems)?,
            )
        } else {
            (self.file_rows(category)?, self.tags_for_category(category)?)
        };
        let rows: Vec<FileRow> = rows.into_iter().map(file_row_from_sql).collect();
        let display_names = display_names_for_rows(&rows, category_folder);
        let mut grouped: BTreeMap<String, Vec<FileVariant>> = BTreeMap::new();
        let mut stems: BTreeMap<String, String> = BTreeMap::new();

        for row in rows {
            let display_name = display_names
                .get(&row.path)
                .cloned()
                .unwrap_or_else(|| row.stem.clone());
            stems
                .entry(display_name.clone())
                .or_insert_with(|| row.stem.clone());
            grouped.entry(display_name).or_default().push(FileVariant {
                path: row.path,
                extension: row.extension,
                size: row.size,
                modified: row.modified,
                first_seen_at: row.first_seen_at,
                waveform: row.preview_waveform,
            });
        }

        let mut records = Vec::new();
        for (name, mut variants) in grouped {
            sort_variants(&mut variants, priority);
            let path = variants
                .first()
                .map(|variant| variant.path.clone())
                .unwrap_or_default();
            let stem = stems.remove(&name).unwrap_or_else(|| name.clone());
            let tags = tags.get(&stem).cloned().unwrap_or_default();
            let record = FileRecord {
                name,
                path,
                support: FileSupport::Native,
                stem,
                variants,
                tags,
            };
            if record_matches_scoped(&record, search, &selected, include_tags) {
                records.push(record);
            }
        }

        records.sort_by_key(|record| record_search_sort_key_scoped(record, search, include_tags));
        Ok(records)
    }

    pub fn schema_for(&self, category: Category) -> io::Result<BTreeMap<String, Vec<String>>> {
        let category_key = category_key(category);
        let rows = block_on(async {
            sqlx::query(
                "SELECT DISTINCT tv.key, tv.value FROM tag_values tv
                 WHERE tv.category = ?
                   AND EXISTS (
                       SELECT 1 FROM files f
                       WHERE f.category = tv.category
                         AND f.stem = tv.stem
                   )
                 ORDER BY tv.key COLLATE NOCASE, tv.value COLLATE NOCASE",
            )
            .bind(category_key)
            .fetch_all(&self.pool)
            .await
        })
        .map_err(io::Error::other)?;

        let mut schema: BTreeMap<String, BTreeSet<String>> = self
            .tag_keys(category)?
            .into_iter()
            .map(|key| (key, BTreeSet::new()))
            .collect();
        for row in rows {
            let key: String = row.get("key");
            let value: String = row.get("value");
            if let Some(key) = normalize_tag_key(&key)
                && let Some(value) = normalize_tag_value(&value)
            {
                schema.entry(key).or_default().insert(value);
            }
        }

        Ok(schema
            .into_iter()
            .map(|(key, values)| (key, values.into_iter().collect()))
            .collect())
    }

    pub fn tag_keys(&self, category: Category) -> io::Result<Vec<String>> {
        let rows = block_on(async {
            sqlx::query(
                "SELECT key FROM tag_keys
                 WHERE category = ?
                 ORDER BY key COLLATE NOCASE",
            )
            .bind(category_key(category))
            .fetch_all(&self.pool)
            .await
        })
        .map_err(io::Error::other)?;

        Ok(rows
            .into_iter()
            .map(|row| row.get::<String, _>("key"))
            .collect())
    }

    pub fn add_tag_key(&self, category: Category, key: &str) -> io::Result<Option<String>> {
        let Some(key) = self.canonical_tag_key(category, key)? else {
            return Ok(None);
        };
        block_on(async {
            ensure_tag_key(&self.pool, category, &key).await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)?;
        Ok(Some(key))
    }

    pub fn remove_tag_key(&self, category: Category, key: &str) -> io::Result<bool> {
        let Some(key) = self.canonical_tag_key(category, key)? else {
            return Ok(false);
        };
        block_on(async {
            let category_key = category_key(category);
            let mut tx = self.pool.begin().await?;
            sqlx::query("DELETE FROM tag_values WHERE category = ? AND key = ?")
                .bind(category_key)
                .bind(&key)
                .execute(&mut *tx)
                .await?;
            let deleted_key = sqlx::query("DELETE FROM tag_keys WHERE category = ? AND key = ?")
                .bind(category_key)
                .bind(&key)
                .execute(&mut *tx)
                .await?
                .rows_affected();
            tx.commit().await?;
            Ok::<_, sqlx::Error>(deleted_key > 0)
        })
        .map_err(io::Error::other)
    }

    pub fn rename_tag_key(
        &self,
        category: Category,
        old_key: &str,
        new_key: &str,
    ) -> io::Result<()> {
        let Some(old_key) = self.canonical_tag_key(category, old_key)? else {
            return Ok(());
        };
        let Some(new_key) = self.canonical_tag_key(category, new_key)? else {
            return Ok(());
        };
        if old_key == new_key {
            return Ok(());
        }
        block_on(async {
            let category_key = category_key(category);
            let mut tx = self.pool.begin().await?;
            sqlx::query("INSERT OR IGNORE INTO tag_keys(category, key) VALUES (?, ?)")
                .bind(category_key)
                .bind(&new_key)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "INSERT OR IGNORE INTO tag_values(category, stem, key, value)
                 SELECT category, stem, ?, value
                 FROM tag_values
                 WHERE category = ? AND key = ?",
            )
            .bind(&new_key)
            .bind(category_key)
            .bind(&old_key)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM tag_values WHERE category = ? AND key = ?")
                .bind(category_key)
                .bind(&old_key)
                .execute(&mut *tx)
                .await?;
            sqlx::query("DELETE FROM tag_keys WHERE category = ? AND key = ?")
                .bind(category_key)
                .bind(&old_key)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    fn canonical_tag_key(&self, category: Category, key: &str) -> io::Result<Option<String>> {
        let Some(key) = normalize_tag_key(key) else {
            return Ok(None);
        };
        block_on(async {
            canonical_existing_tag_key(&self.pool, category, &key)
                .await
                .map(|existing| Some(existing.unwrap_or(key)))
        })
        .map_err(io::Error::other)
    }

    pub fn add_tag(
        &self,
        category: Category,
        stem: &str,
        key: &str,
        value: &str,
    ) -> io::Result<()> {
        let Some(key) = self.canonical_tag_key(category, key)? else {
            return Ok(());
        };
        let Some(value) = normalize_tag_value(value) else {
            return Ok(());
        };
        block_on(async {
            ensure_tag_key(&self.pool, category, &key).await?;
            let category = category_key(category);
            let mut tx = self.pool.begin().await?;
            if let Some((parent, _)) = split_subtag(&value) {
                sqlx::query(
                    "DELETE FROM tag_values
                     WHERE category = ? AND stem = ? AND key = ?
                       AND value LIKE ? ESCAPE '\\'",
                )
                .bind(category)
                .bind(stem)
                .bind(&key)
                .bind(like_descendant_pattern(parent))
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(
                "INSERT OR IGNORE INTO tag_values(category, stem, key, value) VALUES (?, ?, ?, ?)",
            )
            .bind(category)
            .bind(stem)
            .bind(&key)
            .bind(value)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    pub fn remove_tag(
        &self,
        category: Category,
        stem: &str,
        key: &str,
        value: &str,
    ) -> io::Result<()> {
        let Some(key) = self.canonical_tag_key(category, key)? else {
            return Ok(());
        };
        let Some(value) = normalize_tag_value(value) else {
            return Ok(());
        };
        block_on(async {
            sqlx::query(
                "DELETE FROM tag_values WHERE category = ? AND stem = ? AND key = ? AND value = ?",
            )
            .bind(category_key(category))
            .bind(stem)
            .bind(&key)
            .bind(value)
            .execute(&self.pool)
            .await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    pub fn rename_stem_tag_value(
        &self,
        category: Category,
        stem: &str,
        key: &str,
        old_value: &str,
        new_value: &str,
    ) -> io::Result<()> {
        let Some(key) = self.canonical_tag_key(category, key)? else {
            return Ok(());
        };
        if old_value == new_value {
            return Ok(());
        }
        let Some(old_value) = normalize_tag_value(old_value) else {
            return Ok(());
        };
        let Some(new_value) = normalize_tag_value(new_value) else {
            return Ok(());
        };
        block_on(async {
            let category = category_key(category);
            let mut tx = self.pool.begin().await?;
            if let Some((parent, _)) = split_subtag(&new_value) {
                sqlx::query(
                    "DELETE FROM tag_values
                     WHERE category = ? AND stem = ? AND key = ? AND value != ?
                       AND value LIKE ? ESCAPE '\\'",
                )
                .bind(category)
                .bind(stem)
                .bind(&key)
                .bind(&old_value)
                .bind(like_descendant_pattern(parent))
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(
                "INSERT OR IGNORE INTO tag_values(category, stem, key, value)
                 SELECT category, stem, key, ?
                 FROM tag_values
                 WHERE category = ? AND stem = ? AND key = ? AND value = ?",
            )
            .bind(new_value)
            .bind(category)
            .bind(stem)
            .bind(&key)
            .bind(&old_value)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM tag_values
                 WHERE category = ? AND stem = ? AND key = ? AND value = ?",
            )
            .bind(category)
            .bind(stem)
            .bind(&key)
            .bind(&old_value)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    pub fn rename_stem_tags(
        &self,
        category: Category,
        old_stem: &str,
        new_stem: &str,
    ) -> io::Result<()> {
        if old_stem == new_stem {
            return Ok(());
        }
        block_on(async {
            let category = category_key(category);
            let mut tx = self.pool.begin().await?;
            sqlx::query(
                "INSERT OR IGNORE INTO tag_values(category, stem, key, value)
                 SELECT category, ?, key, value
                 FROM tag_values
                 WHERE category = ? AND stem = ?",
            )
            .bind(new_stem)
            .bind(category)
            .bind(old_stem)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM tag_values WHERE category = ? AND stem = ?")
                .bind(category)
                .bind(old_stem)
                .execute(&mut *tx)
                .await?;
            tx.commit().await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    pub fn copy_stem_tags_between_categories(
        &self,
        source: Category,
        destination: Category,
        source_stem: &str,
        destination_stem: &str,
    ) -> io::Result<()> {
        if source == destination {
            return self.rename_stem_tags(source, source_stem, destination_stem);
        }
        block_on(async {
            let source_category = category_key(source);
            let destination_category = category_key(destination);
            let rows = sqlx::query(
                "SELECT key, value FROM tag_values
                 WHERE category = ? AND stem = ?",
            )
            .bind(source_category)
            .bind(source_stem)
            .fetch_all(&self.pool)
            .await?;

            let mut tx = self.pool.begin().await?;
            for row in rows {
                let source_key: String = row.get("key");
                let value: String = row.get("value");
                let destination_key = sqlx::query(
                    "SELECT key FROM tag_keys
                     WHERE category = ? AND key = ? COLLATE NOCASE",
                )
                .bind(destination_category)
                .bind(&source_key)
                .fetch_optional(&mut *tx)
                .await?
                .map(|row| row.get::<String, _>("key"))
                .unwrap_or(source_key);
                sqlx::query("INSERT OR IGNORE INTO tag_keys(category, key) VALUES (?, ?)")
                    .bind(destination_category)
                    .bind(&destination_key)
                    .execute(&mut *tx)
                    .await?;
                sqlx::query(
                    "INSERT OR IGNORE INTO tag_values(category, stem, key, value)
                     VALUES (?, ?, ?, ?)",
                )
                .bind(destination_category)
                .bind(destination_stem)
                .bind(destination_key)
                .bind(value)
                .execute(&mut *tx)
                .await?;
            }
            tx.commit().await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    pub fn rename_tag_value(
        &self,
        category: Category,
        key: &str,
        old_value: &str,
        new_value: &str,
    ) -> io::Result<()> {
        let Some(key) = self.canonical_tag_key(category, key)? else {
            return Ok(());
        };
        if old_value == new_value {
            return Ok(());
        }
        let Some(old_value) = normalize_tag_value(old_value) else {
            return Ok(());
        };
        let Some(new_value) = normalize_tag_value(new_value) else {
            return Ok(());
        };
        block_on(async {
            let category = category_key(category);
            let mut tx = self.pool.begin().await?;
            if let Some((parent, _)) = split_subtag(&new_value) {
                sqlx::query(
                    "DELETE FROM tag_values
                     WHERE category = ? AND key = ? AND value != ?
                       AND value LIKE ? ESCAPE '\\'
                       AND stem IN (
                         SELECT stem FROM tag_values
                         WHERE category = ? AND key = ? AND value = ?
                       )",
                )
                .bind(category)
                .bind(&key)
                .bind(&old_value)
                .bind(like_descendant_pattern(parent))
                .bind(category)
                .bind(&key)
                .bind(&old_value)
                .execute(&mut *tx)
                .await?;
            }
            sqlx::query(
                "INSERT OR IGNORE INTO tag_values(category, stem, key, value)
                 SELECT category, stem, key, ?
                 FROM tag_values
                 WHERE category = ? AND key = ? AND value = ?",
            )
            .bind(new_value)
            .bind(category)
            .bind(&key)
            .bind(&old_value)
            .execute(&mut *tx)
            .await?;
            sqlx::query(
                "DELETE FROM tag_values
                 WHERE category = ? AND key = ? AND value = ?",
            )
            .bind(category)
            .bind(&key)
            .bind(&old_value)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    pub fn folder_tag_values(
        &self,
        category: Category,
        category_folder: &Path,
    ) -> io::Result<Vec<String>> {
        let rows = block_on(async {
            sqlx::query(
                "SELECT path FROM files
                 WHERE category = ?
                 ORDER BY path COLLATE NOCASE",
            )
            .bind(category_key(category))
            .fetch_all(&self.pool)
            .await
        })
        .map_err(io::Error::other)?;

        let mut values = BTreeSet::new();
        for row in rows {
            let path = PathBuf::from(row.get::<String, _>("path"));
            values.extend(folder_tag_values(category_folder, &path));
        }

        Ok(values.into_iter().collect())
    }

    pub fn assign_folder_tags(
        &self,
        category: Category,
        category_folder: &Path,
        assignments: &[FolderTagAssignment],
    ) -> io::Result<usize> {
        let assignments: BTreeMap<String, String> = assignments
            .iter()
            .filter(|assignment| assignment.enabled)
            .filter_map(|assignment| {
                let key = self
                    .canonical_tag_key(category, &assignment.key)
                    .ok()
                    .flatten()?;
                Some((assignment.value.clone(), key))
            })
            .collect();
        if assignments.is_empty() {
            return Ok(0);
        }

        block_on(async {
            let category_key = category_key(category);
            for key in assignments.values() {
                ensure_tag_key(&self.pool, category, key).await?;
            }
            let rows = sqlx::query(
                "SELECT path, stem FROM files
                 WHERE category = ?
                 ORDER BY path COLLATE NOCASE",
            )
            .bind(category_key)
            .fetch_all(&self.pool)
            .await?;
            let mut tx = self.pool.begin().await?;
            let mut inserted = 0;

            for row in rows {
                let path = PathBuf::from(row.get::<String, _>("path"));
                let stem: String = row.get("stem");
                for value in folder_tag_values(category_folder, &path) {
                    let Some(key) = assignments.get(&value) else {
                        continue;
                    };
                    let result = sqlx::query(
                        "INSERT OR IGNORE INTO tag_values(category, stem, key, value)
                         VALUES (?, ?, ?, ?)",
                    )
                    .bind(category_key)
                    .bind(&stem)
                    .bind(key)
                    .bind(value)
                    .execute(&mut *tx)
                    .await?;
                    inserted += result.rows_affected() as usize;
                }
            }

            tx.commit().await?;
            Ok::<_, sqlx::Error>(inserted)
        })
        .map_err(io::Error::other)
    }

    pub fn missing_waveform_cache_paths(&self, limit: usize) -> io::Result<Vec<PathBuf>> {
        let rows = block_on(async {
            sqlx::query(
                "SELECT path FROM files
                 WHERE preview_waveform IS NULL
                 ORDER BY first_seen_at, path
                 LIMIT ?",
            )
            .bind(limit as i64)
            .fetch_all(&self.pool)
            .await
        })
        .map_err(io::Error::other)?;

        Ok(rows
            .into_iter()
            .map(|row| PathBuf::from(row.get::<String, _>("path")))
            .collect())
    }

    pub fn set_preview_waveform(&self, path: &Path, waveform: WaveformBinary256) -> io::Result<()> {
        block_on(async {
            sqlx::query("UPDATE files SET preview_waveform = ? WHERE path = ?")
                .bind(waveform.as_slice())
                .bind(path.to_string_lossy().as_ref())
                .execute(&self.pool)
                .await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    #[cfg(test)]
    pub fn clear_preview_waveform(&self, path: &Path) -> io::Result<()> {
        block_on(async {
            sqlx::query("UPDATE files SET preview_waveform = NULL WHERE path = ?")
                .bind(path.to_string_lossy().as_ref())
                .execute(&self.pool)
                .await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }

    pub fn format_priority(&self) -> io::Result<Vec<AudioFormat>> {
        let value = self.setting(FORMAT_PRIORITY_KEY)?;
        let priority = value
            .as_deref()
            .map(|value| {
                value
                    .split(',')
                    .filter_map(|format| AudioFormat::from_str(format).ok())
                    .collect()
            })
            .unwrap_or_else(default_format_priority);
        Ok(normalize_format_priority(priority))
    }

    pub fn set_format_priority(&self, priority: Vec<AudioFormat>) -> io::Result<()> {
        let priority = normalize_format_priority(priority)
            .into_iter()
            .map(|format| format.extension())
            .collect::<Vec<_>>()
            .join(",");
        self.set_setting(FORMAT_PRIORITY_KEY, &priority)
    }

    pub fn convert_conflict_behavior(&self) -> io::Result<ConvertConflictBehavior> {
        Ok(self
            .setting(CONVERT_CONFLICT_KEY)?
            .as_deref()
            .and_then(|value| ConvertConflictBehavior::from_str(value).ok())
            .unwrap_or(ConvertConflictBehavior::AddCopy))
    }

    pub fn set_convert_conflict_behavior(
        &self,
        behavior: ConvertConflictBehavior,
    ) -> io::Result<()> {
        self.set_setting(CONVERT_CONFLICT_KEY, behavior.key())
    }

    fn file_rows(&self, category: Category) -> io::Result<Vec<sqlx::sqlite::SqliteRow>> {
        let files = Alias::new("files");
        let mut query = Query::select();
        query
            .columns([
                Alias::new("path"),
                Alias::new("stem"),
                Alias::new("extension"),
                Alias::new("size"),
                Alias::new("modified"),
                Alias::new("first_seen_at"),
                Alias::new("preview_waveform"),
            ])
            .from(files)
            .and_where(Expr::col(Alias::new("category")).eq(category_key(category)));
        query
            .order_by(Alias::new("stem"), sea_query::Order::Asc)
            .order_by(Alias::new("extension"), sea_query::Order::Asc);

        let sql = query.to_string(SqliteQueryBuilder);
        block_on(async { sqlx::query(&sql).fetch_all(&self.pool).await }).map_err(io::Error::other)
    }

    fn matching_stems(
        &self,
        category: Category,
        search: &str,
        selected: &BTreeMap<String, BTreeSet<String>>,
        include_tags: bool,
    ) -> io::Result<Vec<String>> {
        let mut sql = String::from(
            "SELECT DISTINCT f.stem FROM files f
             WHERE f.category = ?",
        );
        if !search.is_empty() {
            sql.push_str(
                " AND (lower(f.stem) LIKE ? ESCAPE '\\'
                    OR lower(f.extension) LIKE ? ESCAPE '\\'",
            );
            if include_tags {
                sql.push_str(
                    " OR EXISTS (
                        SELECT 1 FROM tag_values tv
                        WHERE tv.category = f.category
                          AND tv.stem = f.stem
                          AND (
                            lower(tv.key) LIKE ? ESCAPE '\\'
                            OR lower(tv.value) LIKE ? ESCAPE '\\'
                          )
                    )",
                );
            }
            sql.push(')');
        }
        for values in selected.values() {
            for _ in values {
                sql.push_str(
                    " AND EXISTS (
                        SELECT 1 FROM tag_values selected_tv
                        WHERE selected_tv.category = f.category
                          AND selected_tv.stem = f.stem
                          AND lower(selected_tv.key) = lower(?)
                          AND (
                            selected_tv.value = ?
                            OR selected_tv.value LIKE ? ESCAPE '\\'
                          )
                    )",
                );
            }
        }
        sql.push_str(" ORDER BY lower(f.stem), f.stem");

        let search_pattern = like_subsequence_pattern(search);
        let rows = block_on(async {
            let mut query = sqlx::query(&sql).bind(category_key(category));
            if !search.is_empty() {
                query = query.bind(&search_pattern).bind(&search_pattern);
                if include_tags {
                    query = query.bind(&search_pattern).bind(&search_pattern);
                }
            }
            for (key, values) in selected {
                for value in values {
                    query = query
                        .bind(key)
                        .bind(value)
                        .bind(like_descendant_pattern(value));
                }
            }
            query.fetch_all(&self.pool).await
        })
        .map_err(io::Error::other)?;

        Ok(rows
            .into_iter()
            .map(|row| row.get::<String, _>("stem"))
            .collect())
    }

    fn file_rows_for_stems(
        &self,
        category: Category,
        stems: &[String],
    ) -> io::Result<Vec<sqlx::sqlite::SqliteRow>> {
        let mut rows = Vec::new();
        for chunk in stems.chunks(500) {
            let placeholders = placeholders(chunk.len());
            let sql = format!(
                "SELECT path, stem, extension, size, modified, first_seen_at, preview_waveform FROM files
                 WHERE category = ? AND stem IN ({placeholders})
                 ORDER BY stem, extension"
            );
            let chunk_rows = block_on(async {
                let mut query = sqlx::query(&sql).bind(category_key(category));
                for stem in chunk {
                    query = query.bind(stem);
                }
                query.fetch_all(&self.pool).await
            })
            .map_err(io::Error::other)?;
            rows.extend(chunk_rows);
        }
        Ok(rows)
    }

    fn tags_for_category(
        &self,
        category: Category,
    ) -> io::Result<BTreeMap<String, BTreeMap<String, Vec<String>>>> {
        let rows = block_on(async {
            sqlx::query(
                "SELECT stem, key, value FROM tag_values
                 WHERE category = ?
                 ORDER BY key COLLATE NOCASE, value COLLATE NOCASE",
            )
            .bind(category_key(category))
            .fetch_all(&self.pool)
            .await
        })
        .map_err(io::Error::other)?;
        let mut tags: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
        for row in rows {
            let stem: String = row.get("stem");
            let key: String = row.get("key");
            let value: String = row.get("value");
            tags.entry(stem)
                .or_default()
                .entry(key)
                .or_default()
                .push(value);
        }
        Ok(tags)
    }

    fn tags_for_stems(
        &self,
        category: Category,
        stems: &[String],
    ) -> io::Result<BTreeMap<String, BTreeMap<String, Vec<String>>>> {
        let mut tags: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
        for chunk in stems.chunks(500) {
            let placeholders = placeholders(chunk.len());
            let sql = format!(
                "SELECT stem, key, value FROM tag_values
                 WHERE category = ? AND stem IN ({placeholders})
                 ORDER BY key COLLATE NOCASE, value COLLATE NOCASE"
            );
            let rows = block_on(async {
                let mut query = sqlx::query(&sql).bind(category_key(category));
                for stem in chunk {
                    query = query.bind(stem);
                }
                query.fetch_all(&self.pool).await
            })
            .map_err(io::Error::other)?;
            for row in rows {
                let stem: String = row.get("stem");
                let key: String = row.get("key");
                let value: String = row.get("value");
                tags.entry(stem)
                    .or_default()
                    .entry(key)
                    .or_default()
                    .push(value);
            }
        }
        Ok(tags)
    }

    fn setting(&self, key: &str) -> io::Result<Option<String>> {
        block_on(async {
            sqlx::query("SELECT value FROM settings WHERE key = ?")
                .bind(key)
                .fetch_optional(&self.pool)
                .await
                .map(|row| row.map(|row| row.get("value")))
        })
        .map_err(io::Error::other)
    }

    fn set_setting(&self, key: &str, value: &str) -> io::Result<()> {
        block_on(async {
            sqlx::query(
                "INSERT INTO settings(key, value) VALUES (?, ?)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
            Ok::<_, sqlx::Error>(())
        })
        .map_err(io::Error::other)
    }
}

async fn migrate_files_first_seen_at(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let columns = sqlx::query("PRAGMA table_info(files)")
        .fetch_all(pool)
        .await?;
    let has_first_seen_at = columns
        .iter()
        .any(|row| row.get::<String, _>("name") == "first_seen_at");
    if !has_first_seen_at {
        sqlx::query("ALTER TABLE files ADD COLUMN first_seen_at INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await?;
    }
    sqlx::query("UPDATE files SET first_seen_at = rowid WHERE first_seen_at = 0")
        .execute(pool)
        .await?;
    Ok(())
}

async fn migrate_files_preview_waveform(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let columns = sqlx::query("PRAGMA table_info(files)")
        .fetch_all(pool)
        .await?;
    let has_preview_waveform = columns
        .iter()
        .any(|row| row.get::<String, _>("name") == "preview_waveform");
    if !has_preview_waveform {
        sqlx::query(
            "ALTER TABLE files
             ADD COLUMN preview_waveform BLOB CHECK(preview_waveform IS NULL OR length(preview_waveform) = 256)",
        )
        .execute(pool)
        .await?;
    }
    let current_version = sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(PREVIEW_WAVEFORM_VERSION_KEY)
        .fetch_optional(pool)
        .await?
        .map(|row| row.get::<String, _>("value"));
    if current_version.as_deref() != Some(PREVIEW_WAVEFORM_VERSION) {
        sqlx::query("UPDATE files SET preview_waveform = NULL")
            .execute(pool)
            .await?;
        sqlx::query("INSERT OR REPLACE INTO settings(key, value) VALUES (?, ?)")
            .bind(PREVIEW_WAVEFORM_VERSION_KEY)
            .bind(PREVIEW_WAVEFORM_VERSION)
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn migrate_tag_keys(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let columns = sqlx::query("PRAGMA table_info(tag_keys)")
        .fetch_all(pool)
        .await?;
    let has_category = columns
        .iter()
        .any(|row| row.get::<String, _>("name") == "category");
    if has_category {
        return Ok(());
    }

    let legacy_keys = sqlx::query("SELECT key FROM tag_keys")
        .fetch_all(pool)
        .await?;
    sqlx::query("ALTER TABLE tag_keys RENAME TO tag_keys_legacy")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE TABLE tag_keys (
            category TEXT NOT NULL,
            key TEXT NOT NULL,
            PRIMARY KEY(category, key)
        )",
    )
    .execute(pool)
    .await?;

    for row in legacy_keys {
        let key: String = row.get("key");
        let Some(key) = normalize_tag_key(&key) else {
            continue;
        };
        for category in legacy_tag_key_categories(&key) {
            ensure_tag_key(pool, category, &key).await?;
        }
    }

    sqlx::query("DROP TABLE tag_keys_legacy")
        .execute(pool)
        .await?;
    Ok(())
}

async fn seed_tag_keys_from_values(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query("SELECT DISTINCT category, key FROM tag_values")
        .fetch_all(pool)
        .await?;
    for row in rows {
        let category: String = row.get("category");
        let key: String = row.get("key");
        let Some(category) = category_from_key(&category) else {
            continue;
        };
        let Some(key) = normalize_tag_key(&key) else {
            continue;
        };
        ensure_tag_key(pool, category, &key).await?;
    }
    Ok(())
}

async fn seed_default_tag_keys(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let seeded = sqlx::query("SELECT value FROM settings WHERE key = ?")
        .bind(DEFAULT_TAG_KEYS_SEEDED_KEY)
        .fetch_optional(pool)
        .await?;
    if seeded.is_some() {
        return Ok(());
    }

    for category in Category::ALL {
        for key in category.tag_keys() {
            ensure_tag_key(pool, category, key).await?;
        }
    }
    sqlx::query("INSERT OR REPLACE INTO settings(key, value) VALUES (?, '1')")
        .bind(DEFAULT_TAG_KEYS_SEEDED_KEY)
        .execute(pool)
        .await?;
    Ok(())
}

async fn canonical_existing_tag_key(
    pool: &SqlitePool,
    category: Category,
    key: &str,
) -> Result<Option<String>, sqlx::Error> {
    sqlx::query(
        "SELECT key FROM tag_keys
         WHERE category = ? AND lower(key) = lower(?)
         ORDER BY key COLLATE NOCASE
         LIMIT 1",
    )
    .bind(category_key(category))
    .bind(key)
    .fetch_optional(pool)
    .await
    .map(|row| row.map(|row| row.get("key")))
}

async fn ensure_tag_key(
    pool: &SqlitePool,
    category: Category,
    key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT OR IGNORE INTO tag_keys(category, key) VALUES (?, ?)")
        .bind(category_key(category))
        .bind(key)
        .execute(pool)
        .await?;
    Ok(())
}

fn current_unix_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or_default()
}

fn sort_variants(variants: &mut [FileVariant], priority: &[AudioFormat]) {
    variants.sort_by(|a, b| {
        priority_index(&a.extension, priority)
            .cmp(&priority_index(&b.extension, priority))
            .then_with(|| a.extension.cmp(&b.extension))
            .then_with(|| a.path.cmp(&b.path))
    });
}

fn file_row_from_sql(row: sqlx::sqlite::SqliteRow) -> FileRow {
    let preview_waveform = row
        .try_get::<Option<Vec<u8>>, _>("preview_waveform")
        .ok()
        .flatten()
        .and_then(|bytes| bytes.try_into().ok());
    FileRow {
        path: PathBuf::from(row.get::<String, _>("path")),
        stem: row.get("stem"),
        extension: row.get("extension"),
        size: row.get::<i64, _>("size") as u64,
        modified: row.get("modified"),
        first_seen_at: row.get("first_seen_at"),
        preview_waveform,
    }
}

fn display_names_for_rows(
    rows: &[FileRow],
    category_folder: Option<&Path>,
) -> BTreeMap<PathBuf, String> {
    let mut duplicate_keys: BTreeMap<(String, String), Vec<&FileRow>> = BTreeMap::new();
    for row in rows {
        duplicate_keys
            .entry((row.stem.clone(), row.extension.to_ascii_lowercase()))
            .or_default()
            .push(row);
    }

    let mut display_names = BTreeMap::new();
    for duplicates in duplicate_keys.values_mut().filter(|rows| rows.len() > 1) {
        duplicates.sort_by(|a, b| {
            a.first_seen_at
                .cmp(&b.first_seen_at)
                .then_with(|| a.path.cmp(&b.path))
        });
        for row in duplicates.iter().skip(1) {
            display_names.insert(
                row.path.clone(),
                prefixed_duplicate_name(row, category_folder),
            );
        }
    }
    display_names
}

fn prefixed_duplicate_name(row: &FileRow, category_folder: Option<&Path>) -> String {
    let relative_parent = category_folder
        .and_then(|folder| row.path.strip_prefix(folder).ok())
        .and_then(Path::parent)
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.to_string_lossy().replace('\\', "/"));

    relative_parent
        .map(|parent| format!("{parent}/{}", row.stem))
        .unwrap_or_else(|| row.stem.clone())
}

fn priority_index(extension: &str, priority: &[AudioFormat]) -> usize {
    priority
        .iter()
        .position(|format| format.extension().eq_ignore_ascii_case(extension))
        .unwrap_or(priority.len())
}

fn category_key(category: Category) -> &'static str {
    match category {
        Category::Music => "music",
        Category::Sfx => "sfx",
    }
}

fn category_from_key(key: &str) -> Option<Category> {
    match key {
        "music" => Some(Category::Music),
        "sfx" => Some(Category::Sfx),
        _ => None,
    }
}

fn legacy_tag_key_categories(key: &str) -> Vec<Category> {
    let categories: Vec<Category> = Category::ALL
        .into_iter()
        .filter(|category| category.tag_keys().contains(&key))
        .collect();
    if categories.is_empty() {
        Category::ALL.to_vec()
    } else {
        categories
    }
}

fn placeholders(len: usize) -> String {
    std::iter::repeat_n("?", len).collect::<Vec<_>>().join(", ")
}

fn like_subsequence_pattern(value: &str) -> String {
    let mut pattern = String::with_capacity(value.len() * 2 + 1);
    pattern.push('%');
    for ch in value.to_lowercase().chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
        pattern.push('%');
    }
    pattern
}

fn like_descendant_pattern(value: &str) -> String {
    let mut pattern = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        if matches!(ch, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(ch);
    }
    pattern.push_str("/%");
    pattern
}

fn folder_tag_values(category_folder: &Path, path: &Path) -> Vec<String> {
    let Some(parent) = path
        .strip_prefix(category_folder)
        .ok()
        .and_then(Path::parent)
        .filter(|parent| !parent.as_os_str().is_empty())
    else {
        return Vec::new();
    };

    let mut values = Vec::new();
    for component in parent.components() {
        if let Component::Normal(name) = component {
            let value = name.to_string_lossy().trim().to_string();
            if !value.is_empty() && !values.contains(&value) {
                values.push(value);
            }
        }
    }
    values
}

fn canonical_selected(
    selected: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for (key, values) in selected {
        if let Some(key) = normalize_tag_key(key) {
            out.entry(key.to_string())
                .or_insert_with(BTreeSet::new)
                .extend(values.iter().filter_map(|value| normalize_tag_value(value)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn db_path(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir()
            .join(format!("lowcat-db-{name}-{}-{nanos}", std::process::id()))
            .join("library.sqlite")
    }

    fn scan(name: &str, tags: &[(&str, &[&str])]) -> FileScanRecord {
        let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap();
        let extension = name.rsplit_once('.').map(|(_, ext)| ext).unwrap();
        FileScanRecord {
            path: PathBuf::from(format!("/tmp/{name}")),
            stem: stem.to_string(),
            extension: extension.to_string(),
            size: 1,
            modified: 1,
            tags: tags
                .iter()
                .map(|(key, values)| {
                    (
                        key.to_string(),
                        values.iter().map(|value| value.to_string()).collect(),
                    )
                })
                .collect(),
        }
    }

    fn scan_path(path: &str) -> FileScanRecord {
        let file_name = Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap();
        let stem = file_name.rsplit_once('.').map(|(stem, _)| stem).unwrap();
        let extension = file_name.rsplit_once('.').map(|(_, ext)| ext).unwrap();
        FileScanRecord {
            path: PathBuf::from(path),
            stem: stem.to_string(),
            extension: extension.to_string(),
            size: 1,
            modified: 1,
            tags: BTreeMap::new(),
        }
    }

    fn waveform(value: u8) -> WaveformBinary256 {
        [value; 256]
    }

    #[test]
    fn sync_groups_variants_and_seeds_tags() {
        let db = Database::open(&db_path("group")).unwrap();

        let summary = db
            .sync_category(
                Category::Music,
                vec![
                    scan("track.flac", &[("GENRE", &["Ambient"])]),
                    scan("track.mp3", &[]),
                ],
            )
            .unwrap();
        assert_eq!(summary.added, 2);

        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "track");
        assert_eq!(
            rows[0]
                .variants
                .iter()
                .map(|variant| variant.extension.as_str())
                .collect::<Vec<_>>(),
            vec!["mp3", "flac"]
        );
        assert_eq!(rows[0].tags["GENRE"], vec!["Ambient"]);
    }

    #[test]
    fn duplicate_same_format_prefixes_newer_row_with_relative_parent() {
        let db = Database::open(&db_path("duplicate-format-prefix")).unwrap();
        db.sync_category(
            Category::Music,
            vec![scan_path("/tmp/music/ambient/song.wav")],
        )
        .unwrap();
        db.sync_category(
            Category::Music,
            vec![
                scan_path("/tmp/music/ambient/song.wav"),
                scan_path("/tmp/music/ambient/alt/song.wav"),
            ],
        )
        .unwrap();

        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                Some(Path::new("/tmp/music")),
            )
            .unwrap();
        let names = rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>();

        assert_eq!(names, vec!["ambient/alt/song", "song"]);
        assert_eq!(rows[0].variants.len(), 1);
        assert_eq!(rows[1].variants.len(), 1);
    }

    #[test]
    fn assign_folder_tags_uses_relative_parent_components_for_stems() {
        let db = Database::open(&db_path("folder-tags")).unwrap();
        db.sync_category(
            Category::Music,
            vec![
                scan_path("/tmp/music/ambient/dark/song.wav"),
                scan_path("/tmp/music/bright/song.wav"),
                scan_path("/tmp/music/root.wav"),
            ],
        )
        .unwrap();

        let values = db
            .folder_tag_values(Category::Music, Path::new("/tmp/music"))
            .unwrap();
        assert_eq!(values, vec!["ambient", "bright", "dark"]);

        let inserted = db
            .assign_folder_tags(
                Category::Music,
                Path::new("/tmp/music"),
                &[
                    FolderTagAssignment {
                        value: "ambient".to_string(),
                        key: "GENRE".to_string(),
                        enabled: true,
                    },
                    FolderTagAssignment {
                        value: "dark".to_string(),
                        key: "MOOD".to_string(),
                        enabled: true,
                    },
                    FolderTagAssignment {
                        value: "bright".to_string(),
                        key: "GENRE".to_string(),
                        enabled: false,
                    },
                ],
            )
            .unwrap();
        assert_eq!(inserted, 2);

        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                Some(Path::new("/tmp/music")),
            )
            .unwrap();
        let song_rows = rows
            .iter()
            .filter(|row| row.stem == "song")
            .collect::<Vec<_>>();
        assert_eq!(song_rows.len(), 2);
        for row in song_rows {
            assert_eq!(row.tags["GENRE"], vec!["ambient"]);
            assert_eq!(row.tags["MOOD"], vec!["dark"]);
        }
        let root = rows.iter().find(|row| row.stem == "root").unwrap();
        assert!(!root.tags.contains_key("GENRE"));

        let inserted = db
            .assign_folder_tags(
                Category::Music,
                Path::new("/tmp/music"),
                &[
                    FolderTagAssignment {
                        value: "ambient".to_string(),
                        key: "GENRE".to_string(),
                        enabled: true,
                    },
                    FolderTagAssignment {
                        value: "dark".to_string(),
                        key: "MOOD".to_string(),
                        enabled: true,
                    },
                ],
            )
            .unwrap();
        assert_eq!(inserted, 0);
    }

    #[test]
    fn open_migrates_legacy_files_table_with_first_seen_at() {
        let path = db_path("legacy-first-seen");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        block_on(async {
            let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE files (
                    category TEXT NOT NULL,
                    path TEXT NOT NULL PRIMARY KEY,
                    stem TEXT NOT NULL,
                    extension TEXT NOT NULL,
                    size INTEGER NOT NULL,
                    modified INTEGER NOT NULL,
                    search_text TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO files(category, path, stem, extension, size, modified, search_text)
                 VALUES ('music', '/tmp/song.wav', 'song', 'wav', 1, 1, 'song wav music')",
            )
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        });

        let db = Database::open(&path).unwrap();
        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();

        assert_eq!(rows[0].variants[0].first_seen_at, 1);
    }

    #[test]
    fn open_migrates_legacy_files_table_with_preview_waveform() {
        let path = db_path("legacy-preview-waveform");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        block_on(async {
            let options = SqliteConnectOptions::from_str(&format!("sqlite://{}", path.display()))
                .unwrap()
                .create_if_missing(true);
            let pool = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
                .unwrap();
            sqlx::query(
                "CREATE TABLE files (
                    category TEXT NOT NULL,
                    path TEXT NOT NULL PRIMARY KEY,
                    stem TEXT NOT NULL,
                    extension TEXT NOT NULL,
                    size INTEGER NOT NULL,
                    modified INTEGER NOT NULL,
                    first_seen_at INTEGER NOT NULL DEFAULT 1,
                    search_text TEXT NOT NULL
                )",
            )
            .execute(&pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO files(category, path, stem, extension, size, modified, search_text)
                 VALUES ('music', '/tmp/song.wav', 'song', 'wav', 1, 1, 'song wav music')",
            )
            .execute(&pool)
            .await
            .unwrap();
            pool.close().await;
        });

        let db = Database::open(&path).unwrap();
        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert!(rows[0].variants[0].waveform.is_none());
        assert_eq!(
            db.missing_waveform_cache_paths(10).unwrap(),
            vec![PathBuf::from("/tmp/song.wav")]
        );
    }

    #[test]
    fn set_preview_waveform_round_trips_binary256() {
        let db = Database::open(&db_path("waveform-round-trip")).unwrap();
        let path = PathBuf::from("/tmp/song.wav");
        db.sync_category(Category::Music, vec![scan_path("/tmp/song.wav")])
            .unwrap();

        db.set_preview_waveform(&path, waveform(42)).unwrap();
        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();
        assert_eq!(rows[0].variants[0].waveform, Some(waveform(42)));

        db.clear_preview_waveform(&path).unwrap();
        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();
        assert!(rows[0].variants[0].waveform.is_none());
    }

    #[test]
    fn opening_new_waveform_version_invalidates_cached_shapes() {
        let db_path = db_path("waveform-version");
        let db = Database::open(&db_path).unwrap();
        let path = PathBuf::from("/tmp/song.wav");
        db.sync_category(Category::Music, vec![scan_path("/tmp/song.wav")])
            .unwrap();
        db.set_preview_waveform(&path, waveform(42)).unwrap();
        db.set_setting(PREVIEW_WAVEFORM_VERSION_KEY, "1").unwrap();
        drop(db);

        let db = Database::open(&db_path).unwrap();
        assert_eq!(db.missing_waveform_cache_paths(10).unwrap(), vec![path]);
        assert_eq!(
            db.setting(PREVIEW_WAVEFORM_VERSION_KEY).unwrap().as_deref(),
            Some(PREVIEW_WAVEFORM_VERSION)
        );
    }

    #[test]
    fn sync_category_preserves_preview_waveform_when_file_unchanged() {
        let db = Database::open(&db_path("waveform-preserve")).unwrap();
        let path = PathBuf::from("/tmp/song.wav");
        let record = scan_path("/tmp/song.wav");
        db.sync_category(Category::Music, vec![record.clone()])
            .unwrap();
        db.set_preview_waveform(&path, waveform(99)).unwrap();

        db.sync_category(Category::Music, vec![record]).unwrap();

        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();
        assert_eq!(rows[0].variants[0].waveform, Some(waveform(99)));
    }

    #[test]
    fn sync_category_clears_preview_waveform_when_file_changes() {
        let db = Database::open(&db_path("waveform-invalidate")).unwrap();
        let path = PathBuf::from("/tmp/song.wav");
        let mut record = scan_path("/tmp/song.wav");
        db.sync_category(Category::Music, vec![record.clone()])
            .unwrap();
        db.set_preview_waveform(&path, waveform(99)).unwrap();

        record.size += 1;
        db.sync_category(Category::Music, vec![record]).unwrap();

        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();
        assert!(rows[0].variants[0].waveform.is_none());
    }

    #[test]
    fn missing_waveform_cache_paths_returns_uncached_files() {
        let db = Database::open(&db_path("waveform-missing")).unwrap();
        db.sync_category(
            Category::Music,
            vec![scan_path("/tmp/a.wav"), scan_path("/tmp/b.wav")],
        )
        .unwrap();
        db.set_preview_waveform(Path::new("/tmp/a.wav"), waveform(1))
            .unwrap();

        assert_eq!(
            db.missing_waveform_cache_paths(10).unwrap(),
            vec![PathBuf::from("/tmp/b.wav")]
        );
        assert_eq!(
            db.missing_waveform_cache_paths(0).unwrap(),
            Vec::<PathBuf>::new()
        );
    }

    #[test]
    fn sync_category_preserves_removed_last_tag() {
        let db = Database::open(&db_path("sync-category-removed-last-tag")).unwrap();

        db.sync_category(
            Category::Music,
            vec![scan("song.flac", &[("GENRE", &["sgdsfg"])])],
        )
        .unwrap();
        db.remove_tag(Category::Music, "song", "GENRE", "sgdsfg")
            .unwrap();
        db.sync_category(
            Category::Music,
            vec![scan("song.flac", &[("GENRE", &["sgdsfg"])])],
        )
        .unwrap();

        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();
        assert!(!rows[0].tags.contains_key("GENRE"));
    }

    #[test]
    fn schema_ignores_tags_without_current_file_rows() {
        let db = Database::open(&db_path("schema-orphan-tags")).unwrap();

        db.sync_category(
            Category::Music,
            vec![scan("removed.flac", &[("GENRE", &["aaaaa"])])],
        )
        .unwrap();
        db.sync_category(Category::Music, Vec::new()).unwrap();

        let schema = db.schema_for(Category::Music).unwrap();

        assert_eq!(schema["GENRE"], Vec::<String>::new());
        assert_eq!(schema["MOOD"], Vec::<String>::new());
    }

    #[test]
    fn sync_category_deletes_tags_for_removed_stems() {
        let db = Database::open(&db_path("sync-removes-orphan-tags")).unwrap();

        db.sync_category(
            Category::Music,
            vec![
                scan("removed.flac", &[("GENRE", &["Stale"])]),
                scan("kept.flac", &[("GENRE", &["Current"])]),
            ],
        )
        .unwrap();
        db.sync_category(
            Category::Music,
            vec![scan("kept.flac", &[("GENRE", &["Current"])])],
        )
        .unwrap();

        let schema = db.schema_for(Category::Music).unwrap();
        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();

        assert_eq!(schema["GENRE"], vec!["Current"]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "kept");
        assert_eq!(rows[0].tags["GENRE"], vec!["Current"]);
    }

    #[test]
    fn query_visible_rows_filters_search_and_selected_in_sql() {
        let db = Database::open(&db_path("sql-filter")).unwrap();

        db.sync_category(
            Category::Music,
            vec![
                scan("dark.flac", &[("MOOD", &["Dark"])]),
                scan("bright.flac", &[("MOOD", &["Bright"])]),
                scan("dark.mp3", &[]),
            ],
        )
        .unwrap();
        db.add_tag(Category::Music, "dark", "GENRE", "Electronic")
            .unwrap();

        let selected = BTreeMap::from([(
            "Genre".to_string(),
            BTreeSet::from(["Electronic".to_string()]),
        )]);
        let rows = db
            .query_visible_rows(
                Category::Music,
                "dark",
                &selected,
                &default_format_priority(),
                None,
            )
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "dark");
        assert_eq!(
            rows[0]
                .variants
                .iter()
                .map(|variant| variant.extension.as_str())
                .collect::<Vec<_>>(),
            vec!["mp3", "flac"]
        );
        assert_eq!(rows[0].tags["GENRE"], vec!["Electronic"]);
    }

    #[test]
    fn adding_subtag_replaces_existing_sibling_for_stem() {
        let db = Database::open(&db_path("replace-subtag")).unwrap();
        db.sync_category(Category::Music, vec![scan("post.flac", &[])])
            .unwrap();

        db.add_tag(Category::Music, "post", "Use", "shitpost/comedy")
            .unwrap();
        db.add_tag(Category::Music, "post", "Use", "shitpost/meme")
            .unwrap();

        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();
        assert_eq!(rows[0].tags["use"], vec!["shitpost/meme"]);
    }

    #[test]
    fn parent_filter_matches_stored_subtag_path() {
        let db = Database::open(&db_path("filter-subtag-parent")).unwrap();
        db.sync_category(Category::Music, vec![scan("post.flac", &[])])
            .unwrap();
        db.add_tag(Category::Music, "post", "Use", "shitpost/comedy")
            .unwrap();

        let selected =
            BTreeMap::from([("use".to_string(), BTreeSet::from(["shitpost".to_string()]))]);
        let rows = db
            .query_visible_rows(
                Category::Music,
                "",
                &selected,
                &default_format_priority(),
                None,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].tags["use"], vec!["shitpost/comedy"]);
    }

    #[test]
    fn query_visible_rows_fuzzy_search_matches_characters_in_order() {
        let db = Database::open(&db_path("sql-filter-fuzzy")).unwrap();

        db.sync_category(
            Category::Music,
            vec![scan("it's me.flac", &[]), scan("mist.flac", &[])],
        )
        .unwrap();

        let rows = db
            .query_visible_rows(
                Category::Music,
                "its m",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "it's me");
    }

    #[test]
    fn query_visible_rows_sorts_search_by_best_name_match() {
        let db = Database::open(&db_path("sql-filter-search-rank")).unwrap();

        db.sync_category(
            Category::Music,
            vec![
                scan("brass loop.flac", &[]),
                scan("bassy.flac", &[]),
                scan("sub bass loop.flac", &[]),
                scan("bass.flac", &[]),
            ],
        )
        .unwrap();

        let rows = db
            .query_visible_rows(
                Category::Music,
                "bass",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();

        assert_eq!(
            rows.iter().map(|row| row.name.as_str()).collect::<Vec<_>>(),
            vec!["bass", "sub bass loop", "bassy", "brass loop"]
        );
    }

    #[test]
    fn query_visible_rows_escapes_sql_like_wildcards() {
        let db = Database::open(&db_path("sql-filter-like")).unwrap();

        db.sync_category(
            Category::Music,
            vec![scan("100_percent.flac", &[]), scan("100Xpercent.flac", &[])],
        )
        .unwrap();

        let rows = db
            .query_visible_rows(
                Category::Music,
                "100_",
                &BTreeMap::new(),
                &default_format_priority(),
                None,
            )
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].name, "100_percent");
    }

    #[test]
    fn settings_round_trip() {
        let db = Database::open(&db_path("settings")).unwrap();
        assert_eq!(db.format_priority().unwrap(), default_format_priority());

        db.set_format_priority(vec![AudioFormat::Flac, AudioFormat::Mp3])
            .unwrap();
        assert_eq!(
            db.format_priority().unwrap(),
            vec![
                AudioFormat::Flac,
                AudioFormat::Mp3,
                AudioFormat::Wav,
                AudioFormat::Opus
            ]
        );

        db.set_convert_conflict_behavior(ConvertConflictBehavior::Overwrite)
            .unwrap();
        assert_eq!(
            db.convert_conflict_behavior().unwrap(),
            ConvertConflictBehavior::Overwrite
        );
    }
}
