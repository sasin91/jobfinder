//! The model each task runs on (llm_task_models). A task runs on the key its use picks
//! (account_model::USES), on the model saved for the task, else on the task's default for this
//! app's own model (downloaded when the task first needs it, matches::llm_key), else on the key's
//! own model.
//!
//! Which use each task belongs to:
//! - read_job: reading a job post (match_service::extract_job), and the job scan's web search;
//!   job_evaluation.
//! - read_cv: reading a CV for the match (match_service::extract_cv); job_evaluation.
//! - judge: judging the requirements the taxonomy can't decide (ai_match); job_evaluation.
//! - application: writing an application (document_writer); application.
//! - application_form: answering questions on an employer's application form; application.
//! - resume: tailoring a résumé (document_writer), and reading a CV into the profile
//!   (profile_fill), which runs on the résumé key; resume.
//!
//! A saved model belongs to the key its use picked when it was saved: when a use moves to another
//! key, or its key is deleted, its tasks' models are forgotten, so a GGUF file name never goes to
//! OpenAI or a model name of one provider to another.

use sqlx::{SqliteConnection, SqlitePool};

use super::account_model::{APPLICATION, JOB_EVALUATION, RESUME};
use crate::llama::catalog::{GEMMA_4_E4B, QWEN3_4B};
use crate::llm::LLAMA_CPP;

/// What a task saves to run on its key's own model even when it has a default: not a model name
/// (names are letters, digits and . _ : / -, account_service), so never mistaken for one, and not
/// "" (nothing saved, which runs the default).
pub const KEYS_OWN_MODEL: &str = "=key";

/// Something the AI does, on its own model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Task {
    /// Its name in llm_task_models and in the forms (model_for_{name}).
    pub name: &'static str,
    /// The use (account_model::USES) whose key it runs on.
    pub use_: &'static str,
    /// What the pages call it.
    pub label: &'static str,
    /// The model file it runs on with this app's own model when the user saved none; downloaded
    /// when the task first needs it.
    pub llama_default: Option<&'static str>,
}

pub const READ_JOB: Task = Task {
    name: "read_job",
    use_: JOB_EVALUATION,
    label: "Reading job posts",
    llama_default: Some(GEMMA_4_E4B),
};

pub const READ_CV: Task = Task {
    name: "read_cv",
    use_: JOB_EVALUATION,
    label: "Reading CVs for the match",
    llama_default: Some(QWEN3_4B),
};

pub const JUDGE: Task = Task {
    name: "judge",
    use_: JOB_EVALUATION,
    label: "Judging the match",
    llama_default: Some(QWEN3_4B),
};

pub const APPLICATION_TASK: Task = Task {
    name: "application",
    use_: APPLICATION,
    label: "Writing job applications",
    llama_default: None,
};

pub const APPLICATION_FORM: Task = Task {
    name: "application_form",
    use_: APPLICATION,
    label: "Answering application form questions",
    llama_default: Some(QWEN3_4B),
};

pub const RESUME_TASK: Task = Task {
    name: "resume",
    use_: RESUME,
    label: "Tailoring résumés and reading your CV into the profile",
    llama_default: None,
};

/// Every task, in the order the pages list them.
pub const TASKS: [Task; 6] = [
    READ_JOB,
    READ_CV,
    JUDGE,
    APPLICATION_TASK,
    APPLICATION_FORM,
    RESUME_TASK,
];

/// The task that writes kind (document_box::APPLICATION or RESUME).
pub fn writing(kind: &str) -> &'static Task {
    if kind == RESUME {
        &RESUME_TASK
    } else {
        &APPLICATION_TASK
    }
}

/// The model the task runs on, on a key of provider whose own model is key_model: saved (""
/// when none is; KEYS_OWN_MODEL is the key's model), else the task's default for this app's own
/// model, else the key's model.
pub fn model_for(task: &Task, provider: &str, key_model: &str, saved: &str) -> String {
    if saved == KEYS_OWN_MODEL {
        return key_model.to_string();
    }
    if !saved.is_empty() {
        return saved.to_string();
    }
    match task.llama_default {
        Some(default) if provider == LLAMA_CPP => default.to_string(),
        _ => key_model.to_string(),
    }
}

/// The model saved for each task, as (task, model); a task without one isn't listed.
pub async fn saved(db: &SqlitePool) -> sqlx::Result<Vec<(String, String)>> {
    let rows = sqlx::query!(r#"SELECT task AS "task!", model FROM llm_task_models WHERE model != ''"#)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(|row| (row.task, row.model)).collect())
}

/// The model saved for task, "" when none is.
pub async fn saved_for(db: &SqlitePool, task: &Task) -> sqlx::Result<String> {
    let model = sqlx::query_scalar!(r#"SELECT model FROM llm_task_models WHERE task = ?1"#, task.name)
        .fetch_optional(db)
        .await?;
    Ok(model.unwrap_or_default())
}

/// Saves each (task, model); "" runs the task on its default again.
pub async fn save(db: &SqlitePool, chosen: &[(&'static Task, String)]) -> sqlx::Result<()> {
    let mut tx = crate::db::write_transaction(db).await?;
    save_on(&mut tx, chosen).await?;
    tx.commit().await
}

pub(super) async fn save_on(connection: &mut SqliteConnection, chosen: &[(&'static Task, String)]) -> sqlx::Result<()> {
    for (task, model) in chosen {
        sqlx::query!(
            r#"
            INSERT INTO llm_task_models (task, model) VALUES (?1, ?2)
            ON CONFLICT (task) DO UPDATE SET model = excluded.model
            "#,
            task.name,
            model
        )
        .execute(&mut *connection)
        .await?;
    }
    Ok(())
}

/// Forgets the models saved for the tasks of each of uses, which now run on another key or on
/// none, so they run on their defaults. Runs on connection, so it can be in the caller's
/// transaction.
pub async fn forget_for_uses(connection: &mut SqliteConnection, uses: &[&str]) -> sqlx::Result<()> {
    for task in TASKS.iter().filter(|task| uses.contains(&task.use_)) {
        sqlx::query!(r#"DELETE FROM llm_task_models WHERE task = ?1"#, task.name)
            .execute(&mut *connection)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::llm::{LOCAL, OPENAI};

    async fn opened() -> SqlitePool {
        db::open_in_memory().await.unwrap()
    }

    #[test]
    fn a_saved_model_runs_before_any_default() {
        assert_eq!(
            model_for(&READ_JOB, LLAMA_CPP, "", "Qwen3-8B-Q4_K_M.gguf"),
            "Qwen3-8B-Q4_K_M.gguf"
        );
        assert_eq!(model_for(&READ_JOB, OPENAI, "gpt-5.4-mini", "gpt-5.4"), "gpt-5.4");
    }

    #[test]
    fn this_apps_own_model_reads_job_posts_on_gemma_and_judges_on_qwen3_4b() {
        let key_model = "Qwen3-8B-Q4_K_M.gguf";
        assert_eq!(model_for(&READ_JOB, LLAMA_CPP, key_model, ""), GEMMA_4_E4B);
        assert_eq!(model_for(&READ_CV, LLAMA_CPP, key_model, ""), QWEN3_4B);
        assert_eq!(model_for(&JUDGE, LLAMA_CPP, key_model, ""), QWEN3_4B);
        assert_eq!(model_for(&APPLICATION_TASK, LLAMA_CPP, key_model, ""), key_model);
        assert_eq!(model_for(&APPLICATION_FORM, LLAMA_CPP, key_model, ""), QWEN3_4B);
        assert_eq!(model_for(&RESUME_TASK, LLAMA_CPP, "", ""), "");
    }

    #[test]
    fn the_keys_own_model_turns_a_default_off() {
        let key_model = "Qwen3-8B-Q4_K_M.gguf";
        assert_eq!(model_for(&READ_JOB, LLAMA_CPP, key_model, KEYS_OWN_MODEL), key_model);
        assert_eq!(model_for(&JUDGE, LLAMA_CPP, "", KEYS_OWN_MODEL), "");
    }

    #[test]
    fn on_another_provider_the_keys_model_runs() {
        assert_eq!(model_for(&JUDGE, LOCAL, "llama3.1:8b", ""), "llama3.1:8b");
    }

    #[test]
    fn writing_picks_its_kind_and_munin_is_no_tasks_default() {
        assert_eq!(writing(RESUME), &RESUME_TASK);
        assert_eq!(writing(APPLICATION), &APPLICATION_TASK);
        assert!(
            TASKS
                .iter()
                .all(|task| task.llama_default != Some("munin-qwen3.5-9B.Q4_K_M.gguf"))
        );
    }

    #[tokio::test]
    async fn the_table_takes_every_task() {
        let db = opened().await;
        let every: Vec<(&'static Task, String)> = TASKS.iter().map(|task| (task, "m".to_string())).collect();
        save(&db, &every).await.unwrap();
        assert_eq!(saved(&db).await.unwrap().len(), TASKS.len());
    }

    #[tokio::test]
    async fn a_saved_model_is_kept_per_task_until_its_use_moves() {
        let db = opened().await;
        save(
            &db,
            &[
                (&READ_JOB, GEMMA_4_E4B.to_string()),
                (&JUDGE, QWEN3_4B.to_string()),
                (&RESUME_TASK, String::new()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(saved_for(&db, &READ_JOB).await.unwrap(), GEMMA_4_E4B);
        assert_eq!(saved_for(&db, &READ_CV).await.unwrap(), "");
        let mut listed = saved(&db).await.unwrap();
        listed.sort();
        assert_eq!(
            listed,
            [
                ("judge".to_string(), QWEN3_4B.to_string()),
                ("read_job".to_string(), GEMMA_4_E4B.to_string())
            ],
            "a task saved with \"\" isn't listed"
        );

        save(&db, &[(&READ_JOB, "Qwen3-8B-Q4_K_M.gguf".to_string())])
            .await
            .unwrap();
        assert_eq!(saved_for(&db, &READ_JOB).await.unwrap(), "Qwen3-8B-Q4_K_M.gguf");

        save(&db, &[(&APPLICATION_TASK, "gpt-5.4".to_string())]).await.unwrap();
        let mut connection = db.acquire().await.unwrap();
        forget_for_uses(&mut connection, &[JOB_EVALUATION]).await.unwrap();
        drop(connection);
        assert_eq!(
            saved(&db).await.unwrap(),
            [("application".to_string(), "gpt-5.4".to_string())]
        );
    }
}
