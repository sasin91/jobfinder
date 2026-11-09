//! A shared explanation of actual task routing and optional local alternatives.
//! Assessment never downloads models, changes providers or overwrites task choices.
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;

use super::task_model::{TASKS, Task};
use super::{account_model, task_model};
use crate::context::Context;
use crate::llama::{
    self, catalog, download,
    hardware::Inventory,
    llama_server::LlamaServer,
    model_choice::{self, Fit},
};
use crate::llm::{Effort, LLAMA_CPP};
use crate::service_error::{ErrorKind, ServiceError};
use crate::setup::progress;

const DOCUMENT: &str = "model-assessment";
pub const EVALUATION_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Check {
    pub task: String,
    pub model: String,
    pub at: i64,
    pub milliseconds: u64,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Assessment {
    pub version: u32,
    pub at: i64,
    pub inventory: Inventory,
    #[serde(default)]
    pub runtime_release: String,
    pub checks: Vec<Check>,
}

pub struct Row {
    pub task: &'static Task,
    pub provider: String,
    pub current: String,
    pub suggested: Option<&'static str>,
    pub why: &'static str,
    pub fit: String,
    pub downloaded: bool,
    pub manual: bool,
    pub check: Option<Check>,
}

pub struct Overview {
    pub rows: Vec<Row>,
    pub assessment: Option<Assessment>,
    pub download_bytes: u64,
}

/// Only adapt an unsaved built-in default. Explicit task selections always win.
pub fn resolved(task: &Task, provider: &str, base: &str, saved: &str, server: Option<&LlamaServer>) -> String {
    let mut selected = task_model::model_for(task, provider, base, saved);
    if provider == LLAMA_CPP
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
        if provider == LLAMA_CPP {
            return server.and_then(|s| s.overview().chosen.ok()).unwrap_or_default();
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
            "IBM Granite 4.1 3B Q4_K_M: compact form-answer candidate with successful user experience. Run the local check; broader quality benchmarking is still pending.",
        ),
        "read_job" if fit(catalog::GEMMA_4_E4B) == Some(Fit::Gpu) => (
            Some(catalog::GEMMA_4_E4B),
            "Gemma 4 E4B Q4_K_M: the repository's job-reading evaluation favors it for Danish posts, and it fits this GPU.",
        ),
        _ => (
            base,
            "Qwen3 4B Q4_K_M: the repository's bake-off found it competitive with larger models for CV reading and matching. Reusing it limits downloads and model switching; writing quality still needs review.",
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
        if provider.is_empty() || provider == LLAMA_CPP {
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
        rows,
        assessment,
        download_bytes,
    })
}

pub async fn assessment(context: &Context) -> Result<Option<Assessment>, ServiceError> {
    progress::read_document(context.db(), DOCUMENT)
        .await?
        .map(|text| {
            serde_json::from_str(&text)
                .map_err(|e| ServiceError::internal("models", format!("The last evaluation could not be read: {e}")))
        })
        .transpose()
}

async fn save(context: &Context, assessment: &Assessment) -> Result<(), ServiceError> {
    progress::save_document(
        context.db(),
        DOCUMENT,
        &serde_json::to_string(assessment).expect("serializable assessment"),
    )
    .await?;
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

/// Prepare the exact current local assignments, or the displayed suggestions on a fresh setup.
/// A separate action is required to replace a saved model with a suggested alternative.
pub async fn prepare(context: Arc<Context>) -> Result<String, ServiceError> {
    let server = context
        .llama
        .as_ref()
        .ok_or_else(|| ServiceError::new(ErrorKind::NotFound, "Local model setup is unavailable."))?;
    let _lock = server.fetching.lock().await;
    let turn = server
        .downloads
        .begin_turn("Preparing task models")
        .map_err(|e| ServiceError::new(ErrorKind::TooMany, e))?;
    let result = prepare_inner(&context, server).await;
    turn.end(result.as_ref().map(|_| ()).map_err(|e| e.message.clone()));
    result
}

async fn prepare_inner(context: &Context, server: &LlamaServer) -> Result<String, ServiceError> {
    let plan = overview(context).await?;
    let fresh = account_model::saved(context.db()).await?.is_empty();
    let mut targets = std::collections::BTreeSet::new();
    for row in &plan.rows {
        if fresh {
            targets.extend(row.suggested);
        } else if row.provider == LLAMA_CPP {
            targets.insert(row.current.as_str());
        }
    }
    if targets.is_empty() {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "No local tasks selected. Pick a local provider in Language models first.",
        ));
    }
    if fresh && plan.rows.iter().any(|r| r.suggested.is_none()) {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "There is not enough known memory for the complete local plan. Choose a cloud provider or configure individual tasks.",
        ));
    }
    for file in &targets {
        let model = server
            .models()
            .into_iter()
            .find(|m| m.name == *file)
            .or_else(|| catalog::find(file).map(|m| m.as_gguf(&server.models_folder())))
            .ok_or_else(|| {
                ServiceError::new(
                    ErrorKind::Invalid,
                    format!(
                        "{file} is missing and is not in the download catalog. Restore it or select another model."
                    ),
                )
            })?;
        if model_choice::fit(&model, &server.inventory(), server.inventory().gpu.is_some()) == Fit::TooBig {
            return Err(ServiceError::new(
                ErrorKind::Invalid,
                format!(
                    "{file} no longer fits the detected memory. Review the local alternatives before preparing models."
                ),
            ));
        }
    }
    std::fs::create_dir_all(server.models_folder())
        .map_err(|e| ServiceError::failed("models/folder", e.to_string()))?;
    download::ensure_space(&server.models_folder(), plan.download_bytes)
        .map_err(|e| ServiceError::failed("models/storage", e))?;
    if server.server_path().is_none() {
        download::download_server(server.folder(), &server.inventory(), &server.downloads)
            .await
            .map_err(|e| ServiceError::failed("models/runtime", e))?;
    }
    for file in targets {
        if server.models().iter().any(|m| m.name == file) {
            continue;
        }
        let model = catalog::find(file).ok_or_else(|| {
            ServiceError::new(
                ErrorKind::Invalid,
                format!("{file} is not in the catalog. Restore it to the models folder or choose another model."),
            )
        })?;
        download::download_model(model, &server.models_folder(), &server.downloads)
            .await
            .map_err(|e| ServiceError::failed("models/download", e))?;
    }
    if fresh {
        if !account_model::saved(context.db()).await?.is_empty() {
            return Ok("Downloads finished. Provider settings changed during preparation, so no model assignments were applied. Review Models before continuing.".into());
        }
        account_model::save(context.db(), &context.key_vault, LLAMA_CPP, catalog::QWEN3_4B, "", "")
            .await
            .map_err(|e| ServiceError::internal("models/save", e.to_string()))?;
        let choices: Vec<_> = plan
            .rows
            .iter()
            .filter_map(|r| r.suggested.map(|f| (r.task, f.to_string())))
            .collect();
        task_model::save(context.db(), &choices).await?;
    }
    Ok("Task models are downloaded and configured. Run local checks before importing your CV. You can review each task below.".to_string())
}

/// Applying one proposal is explicit, local-only, and requires its file to be present.
pub async fn apply(context: &Context, name: &str) -> Result<String, ServiceError> {
    let plan = overview(context).await?;
    let row = plan
        .rows
        .iter()
        .find(|r| r.task.name == name)
        .ok_or_else(|| ServiceError::new(ErrorKind::Invalid, "Unknown model task."))?;
    if row.provider != LLAMA_CPP {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Choose the local provider for this task in Language models first.",
        ));
    }
    let file = row
        .suggested
        .ok_or_else(|| ServiceError::new(ErrorKind::Invalid, "No local suggestion fits."))?;
    let server = context.llama.as_ref().expect("suggestion requires a server");
    if !server.models().iter().any(|m| m.name == file) {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Download the suggested model in the catalog below, then apply it.",
        ));
    }
    task_model::save(context.db(), &[(row.task, file.to_string())]).await?;
    Ok(format!("{} now uses {file}.", row.task.label))
}

/// A small repeatable correctness/compatibility check, not a claim of broad model quality.
pub async fn check(context: &Context, name: &str) -> Result<String, ServiceError> {
    let plan = overview(context).await?;
    let row = plan
        .rows
        .iter()
        .find(|r| r.task.name == name)
        .ok_or_else(|| ServiceError::new(ErrorKind::Invalid, "Unknown model task."))?;
    if row.provider != LLAMA_CPP || !row.downloaded {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Local checks require a downloaded model assigned to this task. No cloud call or download was made.",
        ));
    }
    let key = account_model::KeyFor {
        provider: LLAMA_CPP.to_string(),
        model: row.current.clone(),
        base_url: String::new(),
        api_key: String::new(),
        uses: Default::default(),
    };
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
