//! A shared explanation of actual task routing and optional local alternatives.
//! Assessment never downloads models, changes providers or overwrites task choices.
use serde_json::json;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use super::model_runtime::LocalRuntime;
use super::task_model::{TASKS, Task};
use super::{account_model, task_model};
use crate::context::Context;
use crate::llama::{
    self, catalog, download,
    hardware::Inventory,
    llama_server::LlamaServer,
    model_choice::{self, Fit},
};
use crate::llm::Effort;
use crate::service_error::{ErrorKind, ServiceError};
pub const EVALUATION_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct Check {
    pub task: String,
    pub model: String,
    pub at: i64,
    pub milliseconds: u64,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Assessment {
    pub version: u32,
    pub at: i64,
    pub inventory: Inventory,
    pub runtime_release: String,
    pub checks: Vec<Check>,
}

pub struct Row {
    pub task: &'static Task,
    pub provider: String,
    pub runtime: Option<LocalRuntime>,
    pub current: String,
    pub suggested: Option<&'static str>,
    pub why: &'static str,
    pub fit: String,
    pub downloaded: bool,
    pub manual: bool,
    pub check: Option<Check>,
}

pub struct Overview {
    pub fresh: bool,
    pub rows: Vec<Row>,
    pub assessment: Option<Assessment>,
    pub download_bytes: u64,
}

impl Overview {
    fn local_models(&self) -> BTreeMap<LocalRuntime, BTreeSet<String>> {
        let mut models: BTreeMap<_, BTreeSet<_>> = BTreeMap::new();
        for row in &self.rows {
            if let Some(runtime) = row.runtime {
                models.entry(runtime).or_default().insert(row.current.clone());
            }
        }
        models
    }
}

/// Only adapt an unsaved built-in default. Explicit task selections always win.
pub fn resolved(task: &Task, provider: &str, base: &str, saved: &str, server: Option<&LlamaServer>) -> String {
    let mut selected = task_model::model_for(task, provider, base, saved);
    let runtime = LocalRuntime::for_provider(provider);
    if runtime.is_some()
        && saved.is_empty()
        && let (Some(server), Some(model)) = (server, catalog::find(&selected))
        && model_choice::fit(
            &model.as_gguf(&server.models_folder()),
            &server.inventory(),
            server.card_usable().unwrap_or(false),
        ) == Fit::TooBig
    {
        selected = base.to_string();
    }
    if selected.is_empty() {
        if let Some(LocalRuntime::LlamaCpp) = runtime {
            return server
                .and_then(|s| {
                    s.overview()
                        .chosen
                        .ok()
                        .or_else(|| recommendation(task, s).0.map(str::to_string))
                })
                .unwrap_or_default();
        }
        return crate::llm::default_model(provider).unwrap_or("").to_string();
    }
    selected
}

pub fn recommendation(task: &Task, server: &LlamaServer) -> (Option<&'static str>, &'static str) {
    let inventory = server.inventory();
    let fit = |file| {
        catalog::find(file)
            .map(|m| model_choice::fit(&m.as_gguf(&server.models_folder()), &inventory, inventory.gpu.is_some()))
    };
    let base = fit(catalog::QWEN3_4B)
        .filter(|f| *f != Fit::TooBig)
        .map(|_| catalog::QWEN3_4B);
    match task.name {
        "application_form" if fit(catalog::GRANITE_4_1_3B).is_some_and(|f| f != Fit::TooBig) => (
            Some(catalog::GRANITE_4_1_3B),
            "A compact model for short, structured form answers. It has worked well in previous use; broader quality comparison is still pending.",
        ),
        "read_job" if fit(catalog::GEMMA_4_E4B) == Some(Fit::Gpu) => (
            Some(catalog::GEMMA_4_E4B),
            "Strong results reading Danish job posts in our comparisons, and it fits fully on your graphics card.",
        ),
        _ => (
            base,
            "A compact choice for CV reading and matching: larger models did not improve those tasks in our comparisons. Sharing this model reduces downloads; writing quality still needs review.",
        ),
    }
}

pub async fn overview(context: &Context) -> Result<Overview, ServiceError> {
    let keys = account_model::saved(context.db()).await?;
    let saved = task_model::saved(context.db()).await?;
    let assessment = assessment(context).await?;
    let files = llama::model_files(context.llama.as_ref());
    let mut downloads = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    for task in &TASKS {
        let key = account_model::picked_for(&keys, task.use_);
        let saved = saved
            .iter()
            .find(|(name, _)| name == task.name)
            .map(|(_, value)| value.as_str())
            .unwrap_or("");
        let provider = key.map(|k| k.provider.as_str()).unwrap_or("");
        let runtime = LocalRuntime::for_provider(provider);
        let current = key
            .map(|k| resolved(task, provider, &k.model, saved, context.llama.as_ref()))
            .unwrap_or_default();
        let (suggested, why) = context.llama.as_ref().map(|s| recommendation(task, s)).unwrap_or((
            None,
            "Local setup is unavailable. Choose a provider in Language models.",
        ));
        // Existing cloud users keep their provider. The alternative is explicitly local.
        let fit = suggested
            .and_then(catalog::find)
            .zip(context.llama.as_ref())
            .map(|(m, s)| {
                model_choice::fit(
                    &m.as_gguf(&s.models_folder()),
                    &s.inventory(),
                    s.inventory().gpu.is_some(),
                )
                .label()
            })
            .unwrap_or_else(|| "No suitable local candidate with the known memory.".to_string());
        let downloaded = files.iter().any(|f| f == &current);
        let check = assessment
            .as_ref()
            .filter(|a| a.version == EVALUATION_VERSION && a.runtime_release == runtime_release(context))
            .and_then(|a| {
                a.checks
                    .iter()
                    .find(|c| c.task == task.name && c.model == current)
                    .cloned()
            });
        if provider.is_empty() || runtime.is_some() {
            let target = if provider.is_empty() {
                suggested.unwrap_or("")
            } else {
                &current
            };
            if !files.iter().any(|f| f == target) {
                downloads.insert(target.to_string());
            }
        }
        rows.push(Row {
            task,
            provider: provider.to_string(),
            runtime,
            current,
            suggested,
            why,
            fit,
            downloaded,
            manual: !saved.is_empty(),
            check,
        });
    }
    let download_bytes = downloads
        .iter()
        .filter_map(|f| catalog::find(f))
        .map(|m| {
            context.llama.as_ref().map_or(m.bytes, |server| {
                download::remaining_bytes(&m.url(), &server.models_folder().join(m.file), m.bytes)
            })
        })
        .sum();
    Ok(Overview {
        fresh: keys.is_empty(),
        rows,
        assessment,
        download_bytes,
    })
}

pub async fn assessment(context: &Context) -> Result<Option<Assessment>, ServiceError> {
    Ok(super::model_assessment::read(context.db()).await?)
}

async fn save(context: &Context, assessment: &Assessment) -> Result<(), ServiceError> {
    super::model_assessment::save(context.db(), assessment).await?;
    Ok(())
}

pub async fn evaluate_again(context: &Context) -> Result<String, ServiceError> {
    let server = context
        .llama
        .as_ref()
        .ok_or_else(|| ServiceError::new(ErrorKind::NotFound, "Local model setup is unavailable."))?;
    let inventory = tokio::task::spawn_blocking(Inventory::detect)
        .await
        .map_err(|e| ServiceError::failed("models/hardware", e.to_string()))?;
    server.refresh_inventory(inventory);
    save(
        context,
        &Assessment {
            version: EVALUATION_VERSION,
            at: chrono::Utc::now().timestamp(),
            inventory: server.inventory(),
            runtime_release: runtime_release(context),
            checks: Vec::new(),
        },
    )
    .await?;
    Ok("Hardware and model fit evaluated again. Your choices are unchanged. Run a local check to measure a downloaded model.".to_string())
}

/// Save an explicit set of local choices. Downloads follow this action in the UI; keeping the
/// assignments first makes interrupted downloads recoverable without choosing everything again.
pub async fn save_choices(context: &Context, chosen: &[(String, String)]) -> Result<(), ServiceError> {
    let plan = overview(context).await?;
    let mut choices = Vec::new();
    for (name, file) in chosen {
        let row = plan
            .rows
            .iter()
            .find(|r| r.task.name == name)
            .ok_or_else(|| ServiceError::new(ErrorKind::Invalid, "Unknown model task."))?;
        let runtime = if plan.fresh {
            LocalRuntime::LlamaCpp
        } else {
            row.runtime.ok_or_else(|| {
                ServiceError::new(
                    ErrorKind::Invalid,
                    "This task uses another provider. Change its provider in Language models first.",
                )
            })?
        };
        if choices.iter().any(|(task, _): &(&Task, String)| task.name == name) {
            return Err(ServiceError::new(ErrorKind::Invalid, "Choose one model per task."));
        }
        runtime.validate_choice(context, file)?;
        choices.push((row.task, file.clone()));
    }
    if choices.is_empty() || (plan.fresh && choices.len() != TASKS.len()) {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Choose a model for every local task first.",
        ));
    }
    if plan.fresh {
        let base = &choices
            .iter()
            .find(|(task, _)| task.name == "resume")
            .expect("all tasks chosen")
            .1;
        account_model::save_local_choices(context.db(), &context.key_vault, base, &choices)
            .await
            .map_err(|e| ServiceError::internal("models/save", e.to_string()))?;
    } else {
        task_model::save(context.db(), &choices).await?;
    }
    Ok(())
}

/// Model selections automatically prepare their local files. Cloud-only choices need no download.
pub async fn download_selected(context: Arc<Context>) -> Result<String, ServiceError> {
    if overview(&context).await?.local_models().is_empty() {
        return Ok("Model choices saved.".into());
    }
    let work = Arc::clone(&context.running);
    let result = work
        .run(
            crate::task_manager::running_work::WorkKind::Download,
            "Download selected models",
            prepare(context),
        )
        .await
        .unwrap_or_else(|_| {
            Err(ServiceError::failed(
                "models",
                "Download stopped. Your choices and partial files are kept; retry in Models to resume.",
            ))
        });
    result.map_err(|e| {
        ServiceError::failed(
            "models/download",
            format!(
                "Your model choices are saved. Download needs attention: {}. Retry in Models to resume.",
                e.message
            ),
        )
    })
}

/// Dispatch each set of assigned files to its runtime's preparation adapter.
pub async fn prepare(context: Arc<Context>) -> Result<String, ServiceError> {
    let models = overview(&context).await?.local_models();
    if models.is_empty() {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "No local tasks selected. Pick a local provider in Language models first.",
        ));
    }
    for (runtime, files) in models {
        runtime.prepare(&context, &files).await?;
    }
    Ok("Your selected local models are ready. You can run a local check below or continue setup.".to_string())
}

/// A small repeatable correctness/compatibility check, not a claim of broad model quality.
pub async fn check(context: &Context, name: &str) -> Result<String, ServiceError> {
    let plan = overview(context).await?;
    let row = plan
        .rows
        .iter()
        .find(|r| r.task.name == name)
        .ok_or_else(|| ServiceError::new(ErrorKind::Invalid, "Unknown model task."))?;
    let runtime = row.runtime.filter(|_| row.downloaded).ok_or_else(|| {
        ServiceError::new(
            ErrorKind::Invalid,
            "Local checks require a downloaded model assigned to this task. No cloud call or download was made.",
        )
    })?;
    let key = runtime.key(&row.current);
    let (prompt, expected) = fixture(name);
    let started = std::time::Instant::now();
    let result = tokio::time::timeout(std::time::Duration::from_secs(120), async {
        let llm = llama::use_key(context.llama.as_ref(), &key).await.map_err(|e| e.message)?;
        llm.structured("Use only the supplied facts. Return the requested answer exactly; do not invent missing facts.", prompt,
            &json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}),
            Effort::Low, 256).await.map_err(|e| e.message)
    }).await;
    let (passed, detail) = match result {
        Ok(Ok(value)) if value.get("answer").and_then(|v| v.as_str()) == Some(expected) => (
            true,
            "Passed the synthetic task fixture. This does not establish overall task quality.".to_string(),
        ),
        Ok(Ok(_)) => (
            false,
            "Answered, but did not meet the task fixture's expected facts/format.".to_string(),
        ),
        Ok(Err(why)) => (false, why),
        Err(_) => (
            false,
            "Loading and answering exceeded two minutes. Try a smaller model or inspect the runtime log.".to_string(),
        ),
    };
    let mut assessment = assessment_for_check(
        plan.assessment,
        context.llama.as_ref().unwrap().inventory(),
        runtime_release(context),
        chrono::Utc::now().timestamp(),
    );
    assessment.checks.retain(|c| c.task != name);
    assessment.checks.push(Check {
        task: name.to_string(),
        model: row.current.clone(),
        at: chrono::Utc::now().timestamp(),
        milliseconds: started.elapsed().as_millis() as u64,
        passed,
        detail: detail.clone(),
    });
    save(context, &assessment).await?;
    if !passed {
        return Err(ServiceError::failed(
            "models/check",
            format!("{} needs attention. {detail}", row.task.label),
        ));
    }
    Ok(format!(
        "{}: {} ({:.1} seconds including loading). {detail}",
        row.task.label,
        if passed { "passed" } else { "needs attention" },
        started.elapsed().as_secs_f64()
    ))
}

fn assessment_for_check(
    previous: Option<Assessment>,
    inventory: Inventory,
    runtime_release: String,
    at: i64,
) -> Assessment {
    previous
        .filter(|a| a.version == EVALUATION_VERSION && a.runtime_release == runtime_release)
        .unwrap_or(Assessment {
            version: EVALUATION_VERSION,
            at,
            inventory,
            runtime_release,
            checks: Vec::new(),
        })
}

fn runtime_release(context: &Context) -> String {
    context
        .llama
        .as_ref()
        .and_then(|s| std::fs::read_to_string(s.folder().join("runtime-release.txt")).ok())
        .unwrap_or_else(|| "External or unversioned runtime".to_string())
}

fn fixture(task: &str) -> (&'static str, &'static str) {
    match task {
        "application_form" => (
            "Candidate: Ada. No salary expectation or work authorization is given. Form asks expected salary. Return answer as unknown.",
            "unknown",
        ),
        "read_job" => (
            "Job: Rust developer. Location: Aarhus. Ansøgningsfrist: 30 november 2026. Return only the deadline as YYYY-MM-DD in answer.",
            "2026-11-30",
        ),
        "read_cv" => (
            "CV: Ada worked at Acme from January 2020 through December 2022. Return the employer name only in answer.",
            "Acme",
        ),
        "judge" => (
            "Job requires Python. CV lists Rust only. Return unknown in answer because Python experience is not established.",
            "unknown",
        ),
        "application" => (
            "Candidate: Ada, Rust developer. Write exactly this grounded application sentence in answer: I develop software in Rust.",
            "I develop software in Rust.",
        ),
        _ => (
            "CV: Ada, Rust developer at Acme. Tailor to Rust role without inventing facts. Return exactly Rust developer at Acme in answer.",
            "Rust developer at Acme",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llama::hardware::{GIB, Gpu, GpuVendor};
    use crate::llm::LLAMA_CPP;

    async fn test_context(folder: &std::path::Path) -> Context {
        let db = crate::db::open_in_memory().await.unwrap();
        let mut context = Context::new(
            db,
            Arc::new(crate::taxonomy::Taxonomy::load()),
            crate::account::key_vault::KeyVault::with_key_in_memory(),
        );
        context.llama = Some(LlamaServer::new(
            folder,
            Inventory {
                memory_bytes: 16 * GIB,
                gpu: None,
            },
        ));
        context
    }

    fn selections(file: &str) -> Vec<(String, String)> {
        TASKS.iter().map(|task| (task.name.into(), file.into())).collect()
    }

    #[tokio::test]
    async fn cloud_and_external_servers_never_enter_the_local_runtime_adapter() {
        use crate::llm::{ANTHROPIC, LOCAL, OPENAI};
        let folder = std::env::temp_dir().join("jobfinder-external-runtime-routing");
        let mut context = test_context(&folder).await;
        context.llama = None;
        // Even names resembling catalog files belong to these providers, not our downloader.
        for provider in [OPENAI, ANTHROPIC, LOCAL] {
            account_model::save(
                context.db(),
                &context.key_vault,
                provider,
                catalog::QWEN3_4B,
                "http://localhost:11434/v1",
                "",
            )
            .await
            .unwrap();
        }
        account_model::save_uses(
            context.db(),
            &account_model::UseChoices {
                resume: Some(LOCAL.into()),
                application: Some(ANTHROPIC.into()),
                job_evaluation: Some(OPENAI.into()),
            },
        )
        .await
        .unwrap();
        let plan = overview(&context).await.unwrap();
        assert!(plan.local_models().is_empty());
        assert!(plan.rows.iter().all(|r| r.runtime.is_none()));
        assert_eq!(plan.download_bytes, 0);
        let before = account_model::saved(context.db()).await.unwrap();
        let context = Arc::new(context);
        assert_eq!(
            download_selected(Arc::clone(&context)).await.unwrap(),
            "Model choices saved."
        );
        assert_eq!(account_model::saved(context.db()).await.unwrap(), before);
    }

    #[tokio::test]
    async fn explicit_first_run_choices_are_saved_before_files_are_downloaded() {
        let folder = std::env::temp_dir().join(format!("jobfinder-explicit-models-{}", std::process::id()));
        let context = test_context(&folder).await;
        let mut chosen = selections(catalog::QWEN3_4B);
        chosen
            .iter_mut()
            .find(|(task, _)| task == "application_form")
            .unwrap()
            .1 = catalog::GRANITE_4_1_3B.into();
        // Deliberately override the recommendation for one task.
        chosen.iter_mut().find(|(task, _)| task == "read_cv").unwrap().1 = catalog::GRANITE_4_1_3B.into();
        save_choices(&context, &chosen).await.unwrap();
        let plan = overview(&context).await.unwrap();
        assert!(!plan.fresh);
        for row in &plan.rows {
            assert_eq!(row.provider, LLAMA_CPP);
            assert_eq!(
                &row.current,
                &chosen.iter().find(|(task, _)| task == row.task.name).unwrap().1
            );
            assert!(row.manual);
            assert!(!row.downloaded);
        }
        assert_eq!(
            plan.download_bytes,
            catalog::find(catalog::QWEN3_4B).unwrap().bytes + catalog::find(catalog::GRANITE_4_1_3B).unwrap().bytes
        );
    }

    #[tokio::test]
    async fn incomplete_duplicate_unknown_and_oversized_choices_leave_first_run_untouched() {
        let folder = std::env::temp_dir().join(format!("jobfinder-invalid-choices-{}", std::process::id()));
        let context = test_context(&folder).await;
        let mut incomplete = selections(catalog::QWEN3_4B);
        incomplete.pop();
        let mut duplicate = selections(catalog::QWEN3_4B);
        duplicate[1] = duplicate[0].clone();
        let mut unknown = selections(catalog::QWEN3_4B);
        unknown[1].0 = "unknown".into();
        // 14B needs more than this test computer's available memory budget.
        for chosen in [
            incomplete,
            duplicate,
            unknown,
            selections("unknown.gguf"),
            selections("Qwen3-14B-Q4_K_M.gguf"),
        ] {
            assert!(save_choices(&context, &chosen).await.is_err());
            assert!(account_model::saved(context.db()).await.unwrap().is_empty());
            assert!(task_model::saved(context.db()).await.unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn first_run_routing_rolls_back_when_task_choices_cannot_be_saved() {
        let folder = std::env::temp_dir().join(format!("jobfinder-choices-rollback-{}", std::process::id()));
        let context = test_context(&folder).await;
        sqlx::query("CREATE TRIGGER reject_choices BEFORE INSERT ON llm_task_models BEGIN SELECT RAISE(ABORT, 'test rollback'); END")
            .execute(context.db()).await.unwrap();
        assert!(save_choices(&context, &selections(catalog::QWEN3_4B)).await.is_err());
        assert!(account_model::saved(context.db()).await.unwrap().is_empty());
        assert!(task_model::saved(context.db()).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn preparing_downloaded_choices_never_fetches_recommendations_and_failure_keeps_choices() {
        let folder = std::env::temp_dir().join(format!("jobfinder-selected-files-{}", std::process::id()));
        let context = test_context(&folder).await;
        let server = context.llama.as_ref().unwrap();
        std::fs::create_dir_all(server.models_folder()).unwrap();
        let file = server.models_folder().join("custom.gguf");
        std::fs::write(&file, b"GGUF").unwrap();
        std::fs::write(folder.join(crate::llama::llama_server::SERVER_FILE), b"test runtime").unwrap();
        save_choices(&context, &selections("custom.gguf")).await.unwrap();
        let context = Arc::new(context);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            download_selected(Arc::clone(&context)),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(overview(&context).await.unwrap().download_bytes, 0);
        std::fs::remove_file(file).unwrap();
        let error = download_selected(Arc::clone(&context)).await.unwrap_err();
        assert!(error.message.contains("choices are saved"));
        assert!(
            overview(&context)
                .await
                .unwrap()
                .rows
                .iter()
                .all(|r| r.current == "custom.gguf")
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[tokio::test]
    async fn local_edits_preserve_cloud_routing_and_other_explicit_choices() {
        let folder = std::env::temp_dir().join(format!("jobfinder-mixed-choices-{}", std::process::id()));
        let context = test_context(&folder).await;
        save_choices(&context, &selections(catalog::QWEN3_4B)).await.unwrap();
        account_model::save(context.db(), &context.key_vault, "openai", "cloud-model", "", "sk-test")
            .await
            .unwrap();
        account_model::save_uses(
            context.db(),
            &account_model::UseChoices {
                resume: Some(LLAMA_CPP.into()),
                application: Some("openai".into()),
                job_evaluation: Some(LLAMA_CPP.into()),
            },
        )
        .await
        .unwrap();
        task_model::save(
            context.db(),
            &[(&task_model::APPLICATION_FORM, "custom-cloud-model".into())],
        )
        .await
        .unwrap();
        save_choices(&context, &[("read_cv".into(), catalog::GRANITE_4_1_3B.into())])
            .await
            .unwrap();
        let plan = overview(&context).await.unwrap();
        let cv = plan.rows.iter().find(|r| r.task.name == "read_cv").unwrap();
        assert_eq!(cv.current, catalog::GRANITE_4_1_3B);
        let cloud = plan.rows.iter().find(|r| r.task.name == "application_form").unwrap();
        assert_eq!(cloud.provider, "openai");
        assert_eq!(cloud.current, "custom-cloud-model");
        assert_eq!(
            plan.rows.iter().find(|r| r.task.name == "judge").unwrap().current,
            catalog::QWEN3_4B
        );
        assert!(
            save_choices(&context, &[("application_form".into(), catalog::GRANITE_4_1_3B.into())])
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn overview_credits_partial_files_once_per_shared_model() {
        let folder = std::env::temp_dir().join(format!("jobfinder-plan-resume-{}", std::process::id()));
        let context = test_context(&folder).await;
        let models = context.llama.as_ref().unwrap().models_folder();
        std::fs::create_dir_all(&models).unwrap();
        let before = overview(&context).await.unwrap().download_bytes;
        for file in [catalog::QWEN3_4B, catalog::GRANITE_4_1_3B] {
            let model = catalog::find(file).unwrap();
            let part = models.join(format!("{file}.part"));
            std::fs::write(&part, [0u8; 128]).unwrap();
            std::fs::write(
                part.with_extension("resume.json"),
                serde_json::to_vec(&json!({"url":model.url(),"version":"\"v1\""})).unwrap(),
            )
            .unwrap();
        }
        assert_eq!(overview(&context).await.unwrap().download_bytes, before - 256);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[tokio::test]
    async fn checks_after_runtime_or_evaluation_changes_are_visible_after_reload() {
        let folder = std::env::temp_dir().join(format!("jobfinder-assessment-reset-{}", std::process::id()));
        let context = test_context(&folder).await;
        let server = context.llama.as_ref().unwrap();
        std::fs::create_dir_all(server.models_folder()).unwrap();
        std::fs::write(folder.join("runtime-release.txt"), "new-runtime").unwrap();
        let model = "candidate.gguf";
        std::fs::write(server.models_folder().join(model), b"GGUF").unwrap();
        account_model::save(context.db(), &context.key_vault, LLAMA_CPP, model, "", "")
            .await
            .unwrap();
        let check = Check {
            task: "application".into(),
            model: model.into(),
            at: 10,
            milliseconds: 50,
            passed: true,
            detail: "passed".into(),
        };
        for (version, release) in [
            (EVALUATION_VERSION, "old-runtime"),
            (EVALUATION_VERSION + 1, "new-runtime"),
        ] {
            let prior = Assessment {
                version,
                at: 1,
                inventory: Inventory::default(),
                runtime_release: release.into(),
                checks: vec![check.clone()],
            };
            save(&context, &prior).await.unwrap();
            let loaded = overview(&context).await.unwrap();
            assert!(
                loaded
                    .rows
                    .iter()
                    .find(|r| r.task.name == "application")
                    .unwrap()
                    .check
                    .is_none()
            );
            let mut refreshed =
                assessment_for_check(loaded.assessment, server.inventory(), runtime_release(&context), 10);
            assert!(
                refreshed.checks.is_empty(),
                "old measurements must not be relabelled as current"
            );
            assert_eq!(refreshed.inventory, server.inventory());
            refreshed.checks.push(check.clone());
            save(&context, &refreshed).await.unwrap();
            let reloaded = overview(&context).await.unwrap();
            assert!(
                reloaded
                    .rows
                    .iter()
                    .find(|r| r.task.name == "application")
                    .unwrap()
                    .check
                    .as_ref()
                    .unwrap()
                    .passed
            );
            let current = assessment_for_check(reloaded.assessment, server.inventory(), runtime_release(&context), 20);
            assert_eq!(
                current.checks.len(),
                1,
                "preserve other checks when metadata already matches"
            );
        }
        std::fs::remove_dir_all(folder).unwrap();
    }
    #[test]
    fn recommendations_are_task_specific_and_fit_small_hardware() {
        let inventory = Inventory {
            memory_bytes: 16 * GIB,
            gpu: Some(Gpu {
                name: "GPU".into(),
                vendor: GpuVendor::Amd,
                vram_bytes: 8 * GIB,
            }),
        };
        let server = LlamaServer::new(std::path::Path::new("/tmp/plan-test"), inventory);
        assert_eq!(
            recommendation(&task_model::APPLICATION_FORM, &server).0,
            Some(catalog::GRANITE_4_1_3B)
        );
        assert_eq!(recommendation(&task_model::READ_CV, &server).0, Some(catalog::QWEN3_4B));
        assert_eq!(
            resolved(
                &task_model::READ_JOB,
                LLAMA_CPP,
                catalog::QWEN3_4B,
                "custom.gguf",
                Some(&server)
            ),
            "custom.gguf"
        );
        assert_eq!(
            resolved(&task_model::READ_JOB, "openai", "cloud-model", "", Some(&server)),
            "cloud-model"
        );
    }
    #[test]
    fn too_large_implicit_default_uses_the_selected_base_model() {
        let server = LlamaServer::new(
            std::path::Path::new("/tmp/small-plan-test"),
            Inventory {
                memory_bytes: 10 * GIB,
                gpu: None,
            },
        );
        assert_eq!(
            resolved(&task_model::READ_JOB, LLAMA_CPP, catalog::QWEN3_4B, "", Some(&server)),
            catalog::QWEN3_4B
        );
    }
}
