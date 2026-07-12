use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Path, PathBuf},
};

use futures::executor::block_on;
use sqlx::Row;

use crate::model::{
    Category, FolderTagAssignment, normalize_tag_key, normalize_tag_value, split_subtag,
};

use super::{
    Database, canonical_existing_tag_key, category_key, ensure_tag_key, folder_tag_values,
    like_descendant_pattern,
};

impl Database {
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
}
