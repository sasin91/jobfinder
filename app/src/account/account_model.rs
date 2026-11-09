//! The user's own language model keys (llm_keys), one per provider, and which one each use
//! runs on (use_for_*: each use is on for one key at most). The keys themselves are sealed in the
//! secrets table (KeyVault); this table keeps each one's provider, model, last 4 characters
//! and, for a local model, base_url. A plain key only leaves here through key_for() and
//! key_of(), for the llm module.

use std::fmt;

use sqlx::SqlitePool;

use super::key_vault::KeyVault;
use super::task_model;
use crate::llm::{ANTHROPIC, LLAMA_CPP, LOCAL, OPENAI};
use crate::matches::match_documents::Uses;

/// provider => what the page calls it.
pub const PROVIDERS: [(&str, &str); 4] = [
    (OPENAI, "OpenAI"),
    (ANTHROPIC, "Anthropic (Claude)"),
    (LOCAL, "A local model (Ollama, LM Studio)"),
    (LLAMA_CPP, "This app's own model (llama.cpp)"),
];

/// What a key can be used for, each run on the key the user picks for it, or on none: purpose =>
/// what the page calls it. A new key takes the uses no other key has.
pub const USES: [(&str, &str); 3] = [
    (RESUME, "Tailoring résumés"),
    (APPLICATION, "Writing job applications"),
    (JOB_EVALUATION, "Evaluating job posts (the AI match)"),
];
pub use crate::matches::document_box::{APPLICATION, RESUME};
pub const JOB_EVALUATION: &str = "job_evaluation";

/// How many of the key's last characters the page shows.
const KEY_HINT_CHARACTERS: usize = 4;

/// A saved key's details without the key.
#[derive(Debug, Clone, PartialEq)]
pub struct Saved {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    /// The key's last characters; "" for a local model saved without a key.
    pub key_hint: String,
    pub uses: Uses,
    pub updated_at: i64,
}

/// { provider, model, base_url, api_key, uses } for the llm module.
#[derive(Clone, PartialEq)]
pub struct KeyFor {
    pub provider: String,
    pub model: String,
    pub base_url: String,
    pub api_key: String,
    pub uses: Uses,
}

impl fmt::Debug for KeyFor {
    /// Everything but the key.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyFor")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("base_url", &self.base_url)
            .field("uses", &self.uses)
            .finish_non_exhaustive()
    }
}

/// For each use, the provider whose key runs it, or None to run it on no key.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct UseChoices {
    pub resume: Option<String>,
    pub application: Option<String>,
    pub job_evaluation: Option<String>,
}

impl UseChoices {
    /// The provider chosen for use_ (a name in USES), or None.
    fn of(&self, use_: &str) -> Option<&str> {
        let chosen = match use_ {
            RESUME => &self.resume,
            APPLICATION => &self.application,
            _ => &self.job_evaluation,
        };
        chosen.as_deref()
    }
}

/// Why a key couldn't be read or kept.
#[derive(Debug)]
pub enum Error {
    Database(sqlx::Error),
    Vault(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Database(e) => write!(f, "the database couldn't answer: {e}"),
            Error::Vault(e) => f.write_str(e),
        }
    }
}

impl From<sqlx::Error> for Error {
    fn from(e: sqlx::Error) -> Self {
        Error::Database(e)
    }
}

/// The saved keys' details without the keys, oldest first.
pub async fn saved(db: &SqlitePool) -> sqlx::Result<Vec<Saved>> {
    let rows = sqlx::query_file!("queries/account_model/saved.sql")
        .fetch_all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| Saved {
            provider: row.provider,
            model: row.model,
            base_url: row.base_url,
            key_hint: row.key_hint,
            uses: Uses {
                resume: row.use_for_resume != 0,
                application: row.use_for_application != 0,
                job_evaluation: row.use_for_job_evaluation != 0,
            },
            updated_at: row.updated_at,
        })
        .collect())
}

/// The saved key the user picked for use_ (a name in USES), or None.
pub fn picked_for<'a>(saved: &'a [Saved], use_: &str) -> Option<&'a Saved> {
    saved.iter().find(|saved| saved.uses.of(use_))
}

/// The key to call the model with for use_, or None without a usable one: none picked for it,
/// or picked but its key is no longer kept.
pub async fn key_for(db: &SqlitePool, vault: &KeyVault, use_: &str) -> Result<Option<KeyFor>, Error> {
    let _together = vault.lock().await;
    let saved = saved(db).await?;
    match picked_for(&saved, use_) {
        Some(picked) => with_key(db, vault, picked).await,
        None => Ok(None),
    }
}

/// The provider's saved key, or None when none is saved or its key is no longer kept.
pub async fn key_of(db: &SqlitePool, vault: &KeyVault, provider: &str) -> Result<Option<KeyFor>, Error> {
    let _together = vault.lock().await;
    let saved = saved(db).await?;
    match saved.iter().find(|saved| saved.provider == provider) {
        Some(found) => with_key(db, vault, found).await,
        None => Ok(None),
    }
}

/// A saved key with its key from the vault, or None when it isn't there. A local model saved
/// without a key runs with none. The caller holds the vault's lock.
pub async fn with_key(db: &SqlitePool, vault: &KeyVault, saved: &Saved) -> Result<Option<KeyFor>, Error> {
    let api_key = if saved.key_hint.is_empty() {
        Some(String::new())
    } else {
        kept_key(db, vault, saved).await?
    };
    Ok(api_key.map(|api_key| KeyFor {
        provider: saved.provider.clone(),
        model: saved.model.clone(),
        base_url: saved.base_url.clone(),
        api_key,
        uses: saved.uses,
    }))
}

/// The saved key's key in the vault. A key kept before keys were kept per provider is moved to
/// its provider's secret the first time it is read, when its last characters show it is this
/// row's.
async fn kept_key(db: &SqlitePool, vault: &KeyVault, saved: &Saved) -> Result<Option<String>, Error> {
    if let Some(key) = vault.key_for(db, &saved.provider).await.map_err(Error::Vault)? {
        return Ok(Some(key));
    }
    let before = vault.key_from_before_providers(db).await.map_err(Error::Vault)?;
    let Some(key) = before.filter(|key| key_hint(key) == saved.key_hint) else {
        return Ok(None);
    };
    let mut tx = crate::db::write_transaction(db).await?;
    vault
        .save(&mut *tx, &saved.provider, &key)
        .await
        .map_err(Error::Vault)?;
    vault
        .delete_key_from_before_providers(&mut *tx)
        .await
        .map_err(Error::Vault)?;
    tx.commit().await?;
    Ok(Some(key))
}

/// Runs each use on the key chosen for it, and on no other. A use that moves to another key (or
/// none) forgets its tasks' models (task_model), which were the old key's.
pub async fn save_uses(db: &SqlitePool, chosen: &UseChoices) -> sqlx::Result<()> {
    let before = saved(db).await?;
    let moved = moved_uses(&before, chosen);
    let mut tx = crate::db::write_transaction(db).await?;
    sqlx::query!(
        r#"
        UPDATE llm_keys
        SET use_for_resume = provider IS ?1,
            use_for_application = provider IS ?2,
            use_for_job_evaluation = provider IS ?3
        "#,
        chosen.resume,
        chosen.application,
        chosen.job_evaluation
    )
    .execute(&mut *tx)
    .await?;
    task_model::forget_for_uses(&mut tx, &moved).await?;
    tx.commit().await
}

/// The uses whose chosen provider isn't the one picked for them in before.
fn moved_uses(before: &[Saved], chosen: &UseChoices) -> Vec<&'static str> {
    USES.iter()
        .map(|(use_, _)| *use_)
        .filter(|use_| picked_for(before, use_).map(|saved| saved.provider.as_str()) != chosen.of(use_))
        .collect()
}

/// Saves the provider's key, replacing the one saved for it before (its uses stay as they were);
/// other providers' keys stay. A new provider's key takes the uses no other key has. api_key ""
/// (a local model without one) keeps no key.
///
/// The row and the key are written in one transaction, under the vault's lock, so no one reads
/// one without the other.
pub async fn save(
    db: &SqlitePool,
    vault: &KeyVault,
    provider: &str,
    model: &str,
    base_url: &str,
    api_key: &str,
) -> Result<(), Error> {
    let hint = key_hint(api_key);
    let now = chrono::Utc::now().timestamp();
    let _together = vault.lock().await;
    let mut tx = crate::db::write_transaction(db).await?;
    sqlx::query_file!("queries/account_model/save.sql", provider, model, base_url, hint, now)
        .execute(&mut *tx)
        .await?;
    let kept = if api_key.is_empty() {
        vault.delete(&mut *tx, provider).await
    } else {
        vault.save(&mut *tx, provider, api_key).await
    };
    kept.map_err(Error::Vault)?;
    tx.commit().await?;
    Ok(())
}

/// Save first-run local routing and its explicit task choices atomically.
pub async fn save_local_choices(
    db: &SqlitePool,
    vault: &KeyVault,
    base: &str,
    choices: &[(&'static task_model::Task, String)],
) -> Result<(), Error> {
    let _together = vault.lock().await;
    let mut tx = crate::db::write_transaction(db).await?;
    let provider = LLAMA_CPP;
    let empty = "";
    let now = chrono::Utc::now().timestamp();
    sqlx::query_file!("queries/account_model/save.sql", provider, base, empty, empty, now)
        .execute(&mut *tx)
        .await?;
    vault.delete(&mut *tx, provider).await.map_err(Error::Vault)?;
    task_model::save_on(&mut tx, choices).await?;
    tx.commit().await?;
    Ok(())
}

/// Deletes the provider's saved key, its row and its key together. The uses it was
/// picked for run on no key until another is picked, and forget their tasks' models.
pub async fn delete_key(db: &SqlitePool, vault: &KeyVault, provider: &str) -> Result<(), Error> {
    let _together = vault.lock().await;
    let before = saved(db).await?;
    let picked: Vec<&str> = USES
        .iter()
        .map(|(use_, _)| *use_)
        .filter(|use_| picked_for(&before, use_).is_some_and(|saved| saved.provider == provider))
        .collect();
    let mut tx = crate::db::write_transaction(db).await?;
    sqlx::query!(r#"DELETE FROM llm_keys WHERE provider = ?1"#, provider)
        .execute(&mut *tx)
        .await?;
    task_model::forget_for_uses(&mut tx, &picked).await?;
    vault.delete(&mut *tx, provider).await.map_err(Error::Vault)?;
    tx.commit().await?;
    Ok(())
}

/// What the page calls a provider.
pub fn provider_label(provider: &str) -> &str {
    PROVIDERS
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, label)| *label)
        .unwrap_or(provider)
}

/// The key's last characters, which the page shows to tell keys apart.
fn key_hint(api_key: &str) -> String {
    let count = api_key.chars().count();
    api_key
        .chars()
        .skip(count.saturating_sub(KEY_HINT_CHARACTERS))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::key_vault::Secret;
    use crate::db;

    const OPENAI_KEY: &str = "sk-proj-0123456789abcdWXYZ";
    const ANTHROPIC_KEY: &str = "sk-ant-api03-0123456789-abcd";

    async fn opened() -> SqlitePool {
        db::open_in_memory().await.unwrap()
    }

    fn all_uses() -> Uses {
        Uses {
            resume: true,
            application: true,
            job_evaluation: true,
        }
    }

    fn provider_for(saved: &[Saved], use_: &str) -> Option<String> {
        picked_for(saved, use_).map(|saved| saved.provider.clone())
    }

    #[tokio::test]
    async fn a_saved_key_is_in_the_vault_and_only_its_hint_in_the_table() {
        let db = opened().await;
        let vault = KeyVault::with_key_in_memory();
        save(&db, &vault, OPENAI, "", "", OPENAI_KEY).await.unwrap();
        let found = saved(&db).await.unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key_hint, "WXYZ");
        assert_eq!(found[0].uses, all_uses(), "the first key takes every use");
        let key = key_for(&db, &vault, RESUME).await.unwrap().unwrap();
        assert_eq!(key.api_key, OPENAI_KEY);
        assert!(!format!("{key:?}").contains("WXYZ"), "the key stays out of logs");

        delete_key(&db, &vault, OPENAI).await.unwrap();
        assert_eq!(saved(&db).await.unwrap(), []);
        assert_eq!(vault.key_for(&db, OPENAI).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_second_providers_key_is_kept_beside_the_first() {
        let db = opened().await;
        let vault = KeyVault::with_key_in_memory();
        save(&db, &vault, ANTHROPIC, "", "", ANTHROPIC_KEY).await.unwrap();
        save(&db, &vault, OPENAI, "", "", OPENAI_KEY).await.unwrap();

        let found = saved(&db).await.unwrap();
        let providers: Vec<&str> = found.iter().map(|saved| saved.provider.as_str()).collect();
        assert_eq!(providers, [ANTHROPIC, OPENAI]);
        assert_eq!(found[1].uses, Uses::default(), "the uses stay on the first key");
        let key = key_of(&db, &vault, ANTHROPIC).await.unwrap().unwrap();
        assert_eq!(key.api_key, ANTHROPIC_KEY);
        let key = key_of(&db, &vault, OPENAI).await.unwrap().unwrap();
        assert_eq!(key.api_key, OPENAI_KEY);
    }

    #[tokio::test]
    async fn each_use_runs_on_the_key_picked_for_it() {
        let db = opened().await;
        let vault = KeyVault::with_key_in_memory();
        save(&db, &vault, ANTHROPIC, "claude-opus-5-5", "", ANTHROPIC_KEY)
            .await
            .unwrap();
        save(&db, &vault, OPENAI, "", "", OPENAI_KEY).await.unwrap();
        let chosen = UseChoices {
            resume: Some(OPENAI.to_string()),
            application: Some(ANTHROPIC.to_string()),
            job_evaluation: None,
        };
        save_uses(&db, &chosen).await.unwrap();

        let found = saved(&db).await.unwrap();
        assert_eq!(provider_for(&found, RESUME).as_deref(), Some(OPENAI));
        assert_eq!(provider_for(&found, APPLICATION).as_deref(), Some(ANTHROPIC));
        assert_eq!(provider_for(&found, JOB_EVALUATION), None);
        let key = key_for(&db, &vault, APPLICATION).await.unwrap().unwrap();
        assert_eq!(
            (key.provider.as_str(), key.model.as_str()),
            (ANTHROPIC, "claude-opus-5-5")
        );
        assert!(key_for(&db, &vault, JOB_EVALUATION).await.unwrap().is_none());

        // Saved again, a key keeps its uses.
        save(&db, &vault, ANTHROPIC, "", "", ANTHROPIC_KEY).await.unwrap();
        let found = saved(&db).await.unwrap();
        assert_eq!(provider_for(&found, APPLICATION).as_deref(), Some(ANTHROPIC));
        assert_eq!(found[0].model, "");
    }

    #[tokio::test]
    async fn a_use_moved_to_another_key_forgets_its_tasks_models() {
        let db = opened().await;
        let vault = KeyVault::with_key_in_memory();
        save(&db, &vault, LLAMA_CPP, "", "", "").await.unwrap();
        save(&db, &vault, OPENAI, "", "", OPENAI_KEY).await.unwrap();
        let models = [
            (&task_model::READ_JOB, "gemma-4-E4B-it-Q4_K_M.gguf".to_string()),
            (&task_model::RESUME_TASK, "Qwen3-8B-Q4_K_M.gguf".to_string()),
        ];
        task_model::save(&db, &models).await.unwrap();
        let key = |provider: &str| Some(provider.to_string());
        let chosen = UseChoices {
            resume: key(LLAMA_CPP),
            application: key(LLAMA_CPP),
            job_evaluation: key(OPENAI),
        };
        save_uses(&db, &chosen).await.unwrap();
        assert_eq!(task_model::saved_for(&db, &task_model::READ_JOB).await.unwrap(), "");
        assert_eq!(
            task_model::saved_for(&db, &task_model::RESUME_TASK).await.unwrap(),
            "Qwen3-8B-Q4_K_M.gguf",
            "the résumé stays on its key, with its model"
        );

        delete_key(&db, &vault, LLAMA_CPP).await.unwrap();
        assert_eq!(task_model::saved(&db).await.unwrap(), []);
    }

    #[tokio::test]
    async fn a_key_missing_from_the_vault_is_not_usable() {
        let db = opened().await;
        let vault = KeyVault::with_key_in_memory();
        save(&db, &vault, OPENAI, "", "", OPENAI_KEY).await.unwrap();
        vault.delete(&db, OPENAI).await.unwrap();
        assert_eq!(saved(&db).await.unwrap().len(), 1);
        assert!(key_for(&db, &vault, RESUME).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_key_kept_before_providers_moves_to_its_providers_secret() {
        let db = opened().await;
        let vault = KeyVault::with_key_in_memory();
        save(&db, &vault, OPENAI, "", "", OPENAI_KEY).await.unwrap();
        vault.delete(&db, OPENAI).await.unwrap();
        vault
            .keep(&db, &Secret::KeyFromBeforeProviders, OPENAI_KEY)
            .await
            .unwrap();

        let key = key_for(&db, &vault, RESUME).await.unwrap().unwrap();
        assert_eq!(key.api_key, OPENAI_KEY);
        assert_eq!(vault.key_for(&db, OPENAI).await.unwrap().as_deref(), Some(OPENAI_KEY));
        assert_eq!(vault.key_from_before_providers(&db).await.unwrap(), None);
    }

    #[tokio::test]
    async fn a_key_kept_before_providers_that_isnt_the_rows_stays_put() {
        let db = opened().await;
        let vault = KeyVault::with_key_in_memory();
        save(&db, &vault, ANTHROPIC, "", "", ANTHROPIC_KEY).await.unwrap();
        vault.delete(&db, ANTHROPIC).await.unwrap();
        vault
            .keep(&db, &Secret::KeyFromBeforeProviders, OPENAI_KEY)
            .await
            .unwrap();

        assert!(key_of(&db, &vault, ANTHROPIC).await.unwrap().is_none());
        assert_eq!(
            vault.key_from_before_providers(&db).await.unwrap().as_deref(),
            Some(OPENAI_KEY)
        );
    }

    #[tokio::test]
    async fn a_local_model_runs_without_a_key() {
        let db = opened().await;
        let vault = KeyVault::with_key_in_memory();
        save(&db, &vault, LOCAL, "llama3.1:8b", "http://localhost:11434/v1", "")
            .await
            .unwrap();
        let key = key_for(&db, &vault, JOB_EVALUATION).await.unwrap().unwrap();
        assert_eq!(
            (key.api_key.as_str(), key.base_url.as_str()),
            ("", "http://localhost:11434/v1")
        );
        assert_eq!(saved(&db).await.unwrap()[0].key_hint, "");
    }
}
