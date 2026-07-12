mod migrations;
mod tags;

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

use migrations::{
    migrate_files_first_seen_at, migrate_files_preview_waveform, migrate_tag_keys,
    seed_default_tag_keys, seed_tag_keys_from_values,
};

use crate::model::{
    AudioFormat, Category, ConvertConflictBehavior, FileRecord, FileSupport, FileVariant,
    WaveformBinary256, default_format_priority, normalize_format_priority, normalize_tag_key,
    normalize_tag_value, record_matches_scoped, record_search_sort_key_scoped,
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
            let mut tx = self.pool.begin().await?;
            let existing_rows =
                sqlx::query("SELECT path, stem, size, modified FROM files WHERE category = ?")
                    .bind(category_key)
                    .fetch_all(&mut *tx)
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
                let existing_fingerprint = existing.get(&path);
                let changed = existing_fingerprint.is_none_or(|(size, modified)| {
                    *size != record.size || *modified != record.modified
                });
                if !changed {
                    continue;
                }

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
                if existing_fingerprint.is_some() {
                    summary.updated += 1;
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
                .execute(&mut *tx)
                .await?;
                next_first_seen_at += 1;

                if !existing_stems.contains(&record.stem) {
                    let tag_count: i64 = sqlx::query(
                        "SELECT COUNT(*) AS count FROM tag_values WHERE category = ? AND stem = ?",
                    )
                    .bind(category_key)
                    .bind(&record.stem)
                    .fetch_one(&mut *tx)
                    .await?
                    .get("count");
                    existing_stems.insert(record.stem.clone());
                    if tag_count != 0 {
                        continue;
                    }

                    for (key, values) in record.tags {
                        let Some(key) = normalize_tag_key(&key) else {
                            continue;
                        };
                        let key = sqlx::query(
                            "SELECT key FROM tag_keys
                             WHERE category = ? AND lower(key) = lower(?)
                             ORDER BY key COLLATE NOCASE
                             LIMIT 1",
                        )
                        .bind(category_key)
                        .bind(&key)
                        .fetch_optional(&mut *tx)
                        .await?
                        .map(|row| row.get("key"))
                        .unwrap_or(key);
                        sqlx::query(
                            "INSERT OR IGNORE INTO tag_keys(category, key) VALUES (?, ?)",
                        )
                        .bind(category_key)
                        .bind(&key)
                        .execute(&mut *tx)
                        .await?;
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
                            .execute(&mut *tx)
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
                        .execute(&mut *tx)
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
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;

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
        Ok(collect_tags(rows))
    }

    fn tags_for_stems(
        &self,
        category: Category,
        stems: &[String],
    ) -> io::Result<BTreeMap<String, BTreeMap<String, Vec<String>>>> {
        let mut tag_rows = Vec::new();
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
            tag_rows.extend(rows);
        }
        Ok(collect_tags(tag_rows))
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

fn collect_tags(
    rows: impl IntoIterator<Item = sqlx::sqlite::SqliteRow>,
) -> BTreeMap<String, BTreeMap<String, Vec<String>>> {
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
    tags
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

fn placeholders(len: usize) -> String {
    std::iter::repeat_n("?", len).collect::<Vec<_>>().join(", ")
}

fn like_subsequence_pattern(value: &str) -> String {
    let mut pattern = String::with_capacity(value.len() * 2 + 1);
    pattern.push('%');
    for ch in value.to_lowercase().chars() {
        push_escaped_like_char(&mut pattern, ch);
        pattern.push('%');
    }
    pattern
}

fn like_descendant_pattern(value: &str) -> String {
    let mut pattern = String::with_capacity(value.len() + 2);
    for ch in value.chars() {
        push_escaped_like_char(&mut pattern, ch);
    }
    pattern.push_str("/%");
    pattern
}

fn push_escaped_like_char(pattern: &mut String, ch: char) {
    if matches!(ch, '%' | '_' | '\\') {
        pattern.push('\\');
    }
    pattern.push(ch);
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
mod tests;
