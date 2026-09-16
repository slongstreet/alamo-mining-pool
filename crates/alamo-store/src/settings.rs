//! Operator settings that override the config file, and database backups.

use crate::{Store, StoreError};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

impl Store {
    /// Every stored override, keyed like the config file (`pool.name`,
    /// `coins.ltc.fallback_address`).
    pub async fn load_settings(&self) -> Result<BTreeMap<String, String>, StoreError> {
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT key, value FROM settings ORDER BY key")
                .fetch_all(&self.pool)
                .await?;
        Ok(rows.into_iter().collect())
    }

    /// Store or replace one override.
    pub async fn set_setting(&self, key: &str, value: &str, now: i64) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)
             ON CONFLICT (key) DO UPDATE SET value = excluded.value,
                                            updated_at = excluded.updated_at",
        )
        .bind(key)
        .bind(value)
        .bind(now)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Drop one override so the config file's value applies again.
    pub async fn clear_setting(&self, key: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM settings WHERE key = ?")
            .bind(key)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Write a consistent copy of the database to `path` with `VACUUM INTO`. The copy is
    /// a plain single-file database without the WAL, ready to be restored by copying it
    /// over `alamo.db`. `path` must not exist.
    pub async fn backup_to(&self, path: &Path) -> Result<(), StoreError> {
        let target = path
            .to_str()
            .ok_or_else(|| StoreError::Io(std::io::Error::other("backup path is not UTF-8")))?
            .replace('\'', "''");
        sqlx::query(&format!("VACUUM INTO '{target}'"))
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Where the database file lives.
    pub fn path(&self) -> &PathBuf {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temp_path;

    #[tokio::test]
    async fn settings_round_trip() {
        let path = temp_path("settings");
        let store = Store::open(&path).await.unwrap();
        assert!(store.load_settings().await.unwrap().is_empty());
        store.set_setting("pool.name", "Test", 1).await.unwrap();
        store.set_setting("pool.name", "Test 2", 2).await.unwrap();
        store
            .set_setting("coins.ltc.fallback_address", "ltc1q", 3)
            .await
            .unwrap();
        let all = store.load_settings().await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all["pool.name"], "Test 2");
        store.clear_setting("pool.name").await.unwrap();
        store.clear_setting("never.set").await.unwrap();
        let all = store.load_settings().await.unwrap();
        assert_eq!(all.len(), 1);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[tokio::test]
    async fn backup_is_a_readable_database() {
        let path = temp_path("backup");
        let store = Store::open(&path).await.unwrap();
        store
            .set_setting("pool.name", "Backed up", 1)
            .await
            .unwrap();
        let copy = path.with_file_name("copy.db");
        store.backup_to(&copy).await.unwrap();
        let restored = Store::open(&copy).await.unwrap();
        assert_eq!(
            restored.load_settings().await.unwrap()["pool.name"],
            "Backed up"
        );
        assert!(
            store.backup_to(&copy).await.is_err(),
            "refuses to overwrite"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
