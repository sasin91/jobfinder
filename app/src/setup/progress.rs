//! Persist setup choices independently of profile import. Every user completes onboarding once.
use sqlx::SqlitePool;

#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub completed: bool,
    pub manual_scan: bool,
    pub skipped_scan: bool,
    pub current_step: usize,
}

pub async fn read(db: &SqlitePool) -> sqlx::Result<Progress> {
    let row = sqlx::query!(r#"SELECT completed AS "completed!: bool", manual_scan AS "manual_scan!: bool", skipped_scan AS "skipped_scan!: bool", current_step FROM onboarding WHERE id = 1"#)
        .fetch_one(db).await?;
    Ok(Progress {
        completed: row.completed,
        manual_scan: row.manual_scan,
        skipped_scan: row.skipped_scan,
        current_step: row.current_step as usize,
    })
}

pub async fn save(db: &SqlitePool, progress: &Progress) -> sqlx::Result<()> {
    let current_step = i64::try_from(progress.current_step).map_err(|e| sqlx::Error::Encode(Box::new(e)))?;
    sqlx::query!(
        "UPDATE onboarding SET completed = ?1, manual_scan = ?2, skipped_scan = ?3, current_step = ?4 WHERE id = 1",
        progress.completed,
        progress.manual_scan,
        progress.skipped_scan,
        current_step
    )
    .execute(db)
    .await?;
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
    async fn invalid_steps_are_rejected_without_changing_other_choices() {
        let db = crate::db::open_in_memory().await.unwrap();
        let original = read(&db).await.unwrap();
        let mut invalid = original.clone();
        invalid.current_step = 6;
        invalid.completed = true;
        assert!(save(&db, &invalid).await.is_err());
        assert_eq!(read(&db).await.unwrap(), original);
    }

    #[tokio::test]
    async fn migration_requires_onboarding_without_resetting_existing_data() {
        use sqlx::Executor;
        for (has_profile, has_matches) in [(false, false), (true, false), (false, true), (true, true)] {
            let db = sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect("sqlite::memory:")
                .await
                .unwrap();
            db.execute("CREATE TABLE profile (id INTEGER); CREATE TABLE matches (id INTEGER);")
                .await
                .unwrap();
            if has_profile {
                db.execute("INSERT INTO profile VALUES (1)").await.unwrap();
            }
            if has_matches {
                db.execute("INSERT INTO matches VALUES (10)").await.unwrap();
            }
            db.execute(include_str!("../../migrations/0018_setup_state.sql"))
                .await
                .unwrap();
            let mut progress = read(&db).await.unwrap();
            assert!(!progress.completed, "existing data must not bypass onboarding");
            assert_eq!(progress.current_step, 0);
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM profile")
                    .fetch_one(&db)
                    .await
                    .unwrap(),
                i64::from(has_profile)
            );
            assert_eq!(
                sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM matches")
                    .fetch_one(&db)
                    .await
                    .unwrap(),
                i64::from(has_matches)
            );
            progress.completed = true;
            save(&db, &progress).await.unwrap();
            assert!(read(&db).await.unwrap().completed, "explicit completion persists");
        }
    }
}
