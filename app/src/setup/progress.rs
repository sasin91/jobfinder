//! Persist setup choices independently of profile import, without changing existing users.
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Progress {
    pub version: u32,
    pub completed: bool,
    pub manual_scan: bool,
    pub skipped_scan: bool,
    #[serde(default)]
    pub current_step: usize,
}

pub async fn read(db: &SqlitePool) -> sqlx::Result<Progress> {
    let value = read_document(db, "onboarding").await?.unwrap_or_default();
    serde_json::from_str(&value).map_err(|e| sqlx::Error::Decode(Box::new(e)))
}

pub async fn save(db: &SqlitePool, progress: &Progress) -> sqlx::Result<()> {
    save_document(
        db,
        "onboarding",
        &serde_json::to_string(progress).expect("serializable progress"),
    )
    .await
}

pub async fn read_document(db: &SqlitePool, name: &str) -> sqlx::Result<Option<String>> {
    sqlx::query_scalar!(
        r#"SELECT value AS "value!: String" FROM app_state WHERE name = ?1"#,
        name
    )
    .fetch_optional(db)
    .await
}

pub async fn save_document(db: &SqlitePool, name: &str, value: &str) -> sqlx::Result<()> {
    sqlx::query!(r#"INSERT INTO app_state (name, value) VALUES (?1, ?2) ON CONFLICT (name) DO UPDATE SET value = excluded.value"#, name, value)
        .execute(db).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn choices_survive_reads_and_cv_presence_is_not_completion() {
        let db = crate::db::open_in_memory().await.unwrap();
        let mut progress = read(&db).await.unwrap();
        assert!(!progress.completed);
        progress.manual_scan = true;
        progress.skipped_scan = true;
        save(&db, &progress).await.unwrap();
        let read = read(&db).await.unwrap();
        assert!(!read.completed);
        assert!(read.manual_scan && read.skipped_scan);
    }

    #[tokio::test]
    async fn migration_preserves_existing_users_without_forcing_setup() {
        use sqlx::Executor;
        let db = sqlx::SqlitePool::connect("sqlite::memory:").await.unwrap();
        db.execute(
            "CREATE TABLE profile (id INTEGER); CREATE TABLE matches (id INTEGER); INSERT INTO profile VALUES (1);",
        )
        .await
        .unwrap();
        db.execute(include_str!("../../migrations/0018_app_state.sql"))
            .await
            .unwrap();
        assert!(read(&db).await.unwrap().completed);
    }
}
