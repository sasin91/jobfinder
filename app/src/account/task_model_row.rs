//! The model per task as the API key page and the Language models screen show it: per task whose
//! use runs on a key, the key, and either a list of the models folder's files (this app's own
//! model) or a field to type a model name in (every other provider). A list offers the key's own
//! model and the task's default apart, so a default can be turned off.

use super::account_model::{self, Saved};
use super::saved_key::KeyChoice;
use super::task_model::{KEYS_OWN_MODEL, TASKS, Task};
use crate::llm::{self, LLAMA_CPP};

/// A task's row: what the screen calls it, and its model.
pub struct TaskModelRow {
    /// The task's name, as the desktop screen sends it back.
    pub task: &'static str,
    /// The use (account_model::USES) whose key it runs on.
    pub use_: &'static str,
    pub label: &'static str,
    /// Picked from the models folder's files (this app's own model), not typed.
    pub picks_a_file: bool,
    /// For a picked one: the key's own model, the task's default when it has one, then each
    /// file (file_choices).
    pub choices: Vec<KeyChoice>,
    /// For a typed one: the model saved for the task, "" for none.
    pub model: String,
    /// The key's own model, as the list's first entry and the typed field's grey hint: "Same as
    /// the key (gpt-5.4-mini)".
    pub keys_own: String,
}

/// A row per task whose use runs on one of saved, with the models saved per task (as
/// task_model::saved lists them) and the models folder's file names.
pub fn rows(saved: &[Saved], saved_models: &[(String, String)], downloaded: &[String]) -> Vec<TaskModelRow> {
    TASKS
        .iter()
        .filter_map(|task| {
            let key = account_model::picked_for(saved, task.use_)?;
            let model = saved_models
                .iter()
                .find(|(name, _)| name == task.name)
                .map(|(_, model)| model.clone())
                .unwrap_or_default();
            Some(row(task, key, model, downloaded))
        })
        .collect()
}

fn row(task: &'static Task, key: &Saved, model: String, downloaded: &[String]) -> TaskModelRow {
    let picks_a_file = key.provider == LLAMA_CPP;
    let keys_own = keys_own_label(key);
    TaskModelRow {
        task: task.name,
        use_: task.use_,
        label: task.label,
        picks_a_file,
        choices: if picks_a_file {
            file_choices(task, &keys_own, &model, downloaded)
        } else {
            Vec::new()
        },
        // Only a list picks the key's own model by name; a typed field's blank is the same.
        model: if model == KEYS_OWN_MODEL { String::new() } else { model },
        keys_own,
    }
}

/// "Same as the key (gpt-5.4-mini)", or without the name when it can't be said.
fn keys_own_label(key: &Saved) -> String {
    match runs_without_a_name(&key.provider, &key.model) {
        Some(model) => format!("Same as the key ({model})"),
        None => "Same as the key".to_string(),
    }
}

/// The model a key runs: its own, else its provider's default, else for this app's own model the
/// one that fits best; None when it can't be said.
fn runs_without_a_name<'a>(provider: &str, model: &'a str) -> Option<&'a str> {
    if !model.is_empty() {
        return Some(model);
    }
    if provider == LLAMA_CPP {
        return Some("the one that fits best");
    }
    llm::USER_DEFAULT_MODELS
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, model)| *model)
}

/// "Default (gemma-4-E4B-it-Q4_K_M.gguf)": short, so it fits a list. A default that isn't
/// downloaded yet comes down when the task first needs it (matches::llm_key).
fn default_label(default: &str) -> String {
    format!("Default ({default})")
}

/// What the list offers, with the saved one selected: the key's own model (KEYS_OWN_MODEL); for a
/// task with a default, the default (""); then each file. Without a default, nothing saved is the
/// key's own model. A saved file that is no longer in the models folder is offered too, said so.
fn file_choices(task: &Task, keys_own: &str, saved: &str, downloaded: &[String]) -> Vec<KeyChoice> {
    let own = KeyChoice {
        value: KEYS_OWN_MODEL.to_string(),
        label: keys_own.to_string(),
        selected: saved == KEYS_OWN_MODEL || (saved.is_empty() && task.llama_default.is_none()),
    };
    let default = task.llama_default.map(|default| KeyChoice {
        value: String::new(),
        label: default_label(default),
        selected: saved.is_empty(),
    });
    let files = downloaded.iter().map(|file| KeyChoice {
        value: file.clone(),
        label: file.clone(),
        selected: file.eq_ignore_ascii_case(saved),
    });
    let names_a_file = !saved.is_empty() && saved != KEYS_OWN_MODEL;
    let gone = names_a_file && !downloaded.iter().any(|file| file.eq_ignore_ascii_case(saved));
    let gone = gone.then(|| KeyChoice {
        value: saved.to_string(),
        label: format!("{saved} (not in the models folder)"),
        selected: true,
    });
    std::iter::once(own).chain(default).chain(files).chain(gone).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llama::catalog::{GEMMA_4_E4B, QWEN3_4B};
    use crate::llm::OPENAI;
    use crate::matches::match_documents::Uses;

    fn key(provider: &str, model: &str, uses: Uses) -> Saved {
        Saved {
            provider: provider.to_string(),
            model: model.to_string(),
            base_url: String::new(),
            key_hint: String::new(),
            uses,
            updated_at: 0,
        }
    }

    fn selected(row: &TaskModelRow) -> Vec<&str> {
        row.choices
            .iter()
            .filter(|choice| choice.selected)
            .map(|choice| choice.value.as_str())
            .collect()
    }

    #[test]
    fn this_apps_own_model_picks_a_file_per_task_and_another_key_types_a_name() {
        let llama = key(
            LLAMA_CPP,
            "Qwen3-8B-Q4_K_M.gguf",
            Uses {
                job_evaluation: true,
                resume: true,
                ..Uses::default()
            },
        );
        let openai = key(
            OPENAI,
            "",
            Uses {
                application: true,
                ..Uses::default()
            },
        );
        let downloaded = vec![GEMMA_4_E4B.to_string(), QWEN3_4B.to_string()];
        let saved_models = vec![
            ("read_cv".to_string(), KEYS_OWN_MODEL.to_string()),
            ("judge".to_string(), "Qwen3-14B-Q4_K_M.gguf".to_string()),
        ];
        let rows = rows(&[llama, openai], &saved_models, &downloaded);

        let tasks: Vec<&str> = rows.iter().map(|row| row.task).collect();
        assert_eq!(
            tasks,
            [
                "read_job",
                "read_cv",
                "judge",
                "application",
                "application_form",
                "resume"
            ]
        );
        let read_job = &rows[0];
        assert!(read_job.picks_a_file);
        let labels: Vec<&str> = read_job.choices.iter().map(|choice| choice.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "Same as the key (Qwen3-8B-Q4_K_M.gguf)",
                "Default (gemma-4-E4B-it-Q4_K_M.gguf)",
                GEMMA_4_E4B,
                QWEN3_4B
            ]
        );
        assert_eq!(selected(read_job), [""], "nothing saved is the default");
        assert_eq!(selected(&rows[1]), [KEYS_OWN_MODEL], "the default turned off");
        let judge = &rows[2];
        assert_eq!(selected(judge), ["Qwen3-14B-Q4_K_M.gguf"]);
        assert_eq!(
            judge.choices.last().unwrap().label,
            "Qwen3-14B-Q4_K_M.gguf (not in the models folder)"
        );
        let application = &rows[3];
        assert!(!application.picks_a_file && application.choices.is_empty());
        assert_eq!(application.keys_own, "Same as the key (gpt-5.4-mini)");
        let resume = &rows[5];
        assert_eq!(resume.choices.len(), 3, "no default to offer");
        assert_eq!(
            selected(resume),
            [KEYS_OWN_MODEL],
            "without a default, nothing saved is the key's own"
        );
    }
}
