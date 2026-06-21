use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use futures::executor::block_on;
use sea_query::{Alias, Expr, Query, SqliteQueryBuilder};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::{Row, SqlitePool};

use crate::model::{
    AudioFormat, Category, ConvertConflictBehavior, FileRecord, FileSupport, FileVariant,
    canonical_tag_key, default_format_priority, normalize_format_priority, record_matches,
};

const FORMAT_PRIORITY_KEY: &str = "format_priority";
const CONVERT_CONFLICT_KEY: &str = "convert_conflict";

pub struct Database {
    pool: SqlitePool,
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
                    search_text TEXT NOT NULL
                )",
                "CREATE INDEX IF NOT EXISTS files_category_stem ON files(category, stem)",
                "CREATE INDEX IF NOT EXISTS files_search ON files(category, search_text)",
                "CREATE TABLE IF NOT EXISTS tag_keys (
                    key TEXT PRIMARY KEY
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

            for category in Category::ALL {
                for key in category.tag_keys() {
                    sqlx::query("INSERT OR IGNORE INTO tag_keys(key) VALUES (?)")
                        .bind(*key)
                        .execute(&self.pool)
                        .await?;
                }
            }

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
                    "INSERT INTO files(category, path, stem, extension, size, modified, search_text)
                     VALUES (?, ?, ?, ?, ?, ?, ?)
                     ON CONFLICT(path) DO UPDATE SET
                        category = excluded.category,
                        stem = excluded.stem,
                        extension = excluded.extension,
                        size = excluded.size,
                        modified = excluded.modified,
                        search_text = excluded.search_text",
                )
                .bind(category_key)
                .bind(&path)
                .bind(&record.stem)
                .bind(&record.extension)
                .bind(record.size as i64)
                .bind(record.modified)
                .bind(search_text)
                .execute(&self.pool)
                .await?;

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
                        let Some(key) = canonical_tag_key(&key) else {
                            continue;
                        };
                        if !category.tag_keys().contains(&key) {
                            continue;
                        }
                        for value in values {
                            sqlx::query(
                                "INSERT OR IGNORE INTO tag_values(category, stem, key, value)
                                 VALUES (?, ?, ?, ?)",
                            )
                            .bind(category_key)
                            .bind(&record.stem)
                            .bind(key)
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
    ) -> io::Result<Vec<FileRecord>> {
        let rows = self.file_rows(category)?;
        let tags = self.tags_for_category(category)?;
        let selected = canonical_selected(selected);
        let mut grouped: BTreeMap<String, Vec<FileVariant>> = BTreeMap::new();

        for row in rows {
            let stem: String = row.get("stem");
            grouped.entry(stem).or_default().push(FileVariant {
                path: PathBuf::from(row.get::<String, _>("path")),
                extension: row.get("extension"),
                size: row.get::<i64, _>("size") as u64,
                modified: row.get("modified"),
            });
        }

        let mut records = Vec::new();
        for (stem, mut variants) in grouped {
            sort_variants(&mut variants, priority);
            let path = variants
                .first()
                .map(|variant| variant.path.clone())
                .unwrap_or_default();
            let tags = tags.get(&stem).cloned().unwrap_or_default();
            let record = FileRecord {
                name: stem.clone(),
                path,
                support: FileSupport::Native,
                stem,
                variants,
                tags,
            };
            if record_matches(&record, search, &selected) {
                records.push(record);
            }
        }

        records.sort_by(|a, b| {
            a.name
                .to_lowercase()
                .cmp(&b.name.to_lowercase())
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(records)
    }

    pub fn schema_for(&self, category: Category) -> io::Result<BTreeMap<String, Vec<String>>> {
        let category_key = category_key(category);
        let rows = block_on(async {
            sqlx::query(
                "SELECT key, value FROM tag_values
                 WHERE category = ?
                 ORDER BY key COLLATE NOCASE, value COLLATE NOCASE",
            )
            .bind(category_key)
            .fetch_all(&self.pool)
            .await
        })
        .map_err(io::Error::other)?;

        let mut schema: BTreeMap<String, BTreeSet<String>> = category
            .tag_keys()
            .iter()
            .map(|key| ((*key).to_string(), BTreeSet::new()))
            .collect();
        for row in rows {
            let key: String = row.get("key");
            let value: String = row.get("value");
            if let Some(key) = canonical_tag_key(&key)
                && category.tag_keys().contains(&key)
            {
                schema.entry(key.to_string()).or_default().insert(value);
            }
        }

        Ok(schema
            .into_iter()
            .map(|(key, values)| (key, values.into_iter().collect()))
            .collect())
    }

    pub fn add_tag(
        &self,
        category: Category,
        stem: &str,
        key: &str,
        value: &str,
    ) -> io::Result<()> {
        let Some(key) = canonical_tag_key(key) else {
            return Ok(());
        };
        if !category.tag_keys().contains(&key) {
            return Ok(());
        }
        block_on(async {
            sqlx::query(
                "INSERT OR IGNORE INTO tag_values(category, stem, key, value) VALUES (?, ?, ?, ?)",
            )
            .bind(category_key(category))
            .bind(stem)
            .bind(key)
            .bind(value)
            .execute(&self.pool)
            .await?;
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
        let Some(key) = canonical_tag_key(key) else {
            return Ok(());
        };
        block_on(async {
            sqlx::query(
                "DELETE FROM tag_values WHERE category = ? AND stem = ? AND key = ? AND value = ?",
            )
            .bind(category_key(category))
            .bind(stem)
            .bind(key)
            .bind(value)
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
            ])
            .from(files)
            .and_where(Expr::col(Alias::new("category")).eq(category_key(category)));
        query
            .order_by(Alias::new("stem"), sea_query::Order::Asc)
            .order_by(Alias::new("extension"), sea_query::Order::Asc);

        let sql = query.to_string(SqliteQueryBuilder);
        block_on(async { sqlx::query(&sql).fetch_all(&self.pool).await }).map_err(io::Error::other)
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

fn sort_variants(variants: &mut [FileVariant], priority: &[AudioFormat]) {
    variants.sort_by(|a, b| {
        priority_index(&a.extension, priority)
            .cmp(&priority_index(&b.extension, priority))
            .then_with(|| a.extension.cmp(&b.extension))
            .then_with(|| a.path.cmp(&b.path))
    });
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

fn canonical_selected(
    selected: &BTreeMap<String, BTreeSet<String>>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut out = BTreeMap::new();
    for (key, values) in selected {
        if let Some(key) = canonical_tag_key(key) {
            out.entry(key.to_string())
                .or_insert_with(BTreeSet::new)
                .extend(values.iter().cloned());
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
            )
            .unwrap();
        assert!(!rows[0].tags.contains_key("GENRE"));
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
