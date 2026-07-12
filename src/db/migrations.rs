use sqlx::{Row, SqlitePool};

use crate::model::{Category, normalize_tag_key};

use super::{
    DEFAULT_TAG_KEYS_SEEDED_KEY, PREVIEW_WAVEFORM_VERSION, PREVIEW_WAVEFORM_VERSION_KEY,
    ensure_tag_key,
};

pub(super) async fn migrate_files_first_seen_at(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    if !has_column(pool, "files", "first_seen_at").await? {
        sqlx::query("ALTER TABLE files ADD COLUMN first_seen_at INTEGER NOT NULL DEFAULT 0")
            .execute(pool)
            .await?;
    }
    sqlx::query("UPDATE files SET first_seen_at = rowid WHERE first_seen_at = 0")
        .execute(pool)
        .await?;
    Ok(())
}

pub(super) async fn migrate_files_preview_waveform(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    if !has_column(pool, "files", "preview_waveform").await? {
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

pub(super) async fn migrate_tag_keys(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    if has_column(pool, "tag_keys", "category").await? {
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

async fn has_column(pool: &SqlitePool, table: &str, column: &str) -> Result<bool, sqlx::Error> {
    let rows = sqlx::query(&format!("PRAGMA table_info({table})"))
        .fetch_all(pool)
        .await?;
    Ok(rows
        .iter()
        .any(|row| row.get::<String, _>("name") == column))
}

pub(super) async fn seed_tag_keys_from_values(pool: &SqlitePool) -> Result<(), sqlx::Error> {
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

pub(super) async fn seed_default_tag_keys(pool: &SqlitePool) -> Result<(), sqlx::Error> {
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
