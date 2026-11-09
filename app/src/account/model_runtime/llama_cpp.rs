//! Model setup and execution through the bundled llama.cpp server.

use std::collections::BTreeSet;

use super::{ModelRuntime, Recommendation, RuntimeState};
use crate::account::{
    llama_setup_service,
    task_model::{self, Task},
};
use crate::context::Context;
use crate::llama::{
    catalog, download,
    hardware::Inventory,
    llama_server::LlamaServer,
    model_choice::{self, Fit},
};
use crate::llm::{LLAMA_CPP, LOCAL, Llm, LlmException};
use crate::service_error::{ErrorKind, ServiceError};
use futures_util::future::BoxFuture;
use std::sync::Arc;

pub struct LlamaCpp;

impl ModelRuntime for LlamaCpp {
    fn provider(&self) -> &'static str {
        LLAMA_CPP
    }

    fn resolve(&self, context: &Context, task: &Task, base: &str, saved: &str) -> String {
        let mut selected = task_model::model_for(task, self.provider(), base, saved);
        let server = context.llama.as_ref();
        if saved.is_empty()
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
            return server
                .and_then(|s| {
                    s.overview()
                        .chosen
                        .ok()
                        .or_else(|| self.recommendation(context, task).model.map(str::to_string))
                })
                .unwrap_or_default();
        }
        selected
    }

    fn recommendation(&self, context: &Context, task: &Task) -> Recommendation {
        let Some(server) = context.llama.as_ref() else {
            return Recommendation {
                model: None,
                why: "Local setup is unavailable. Choose a provider in Language models.",
                fit: "No suitable local candidate with the known memory.".into(),
            };
        };
        let inventory = server.inventory();
        let fit = |file| {
            catalog::find(file)
                .map(|m| model_choice::fit(&m.as_gguf(&server.models_folder()), &inventory, inventory.gpu.is_some()))
        };
        let base = fit(catalog::QWEN3_4B)
            .filter(|f| *f != Fit::TooBig)
            .map(|_| catalog::QWEN3_4B);
        let (model, why) = match task.name {
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
        };
        Recommendation {
            model,
            why,
            fit: model
                .and_then(fit)
                .map(|f| f.label())
                .unwrap_or_else(|| "No suitable local candidate with the known memory.".into()),
        }
    }

    fn downloaded(&self, context: &Context, model: &str) -> bool {
        context
            .llama
            .as_ref()
            .is_some_and(|s| s.models().iter().any(|m| m.name == model))
    }

    fn download_bytes(&self, context: &Context, models: &BTreeSet<String>) -> u64 {
        models
            .iter()
            .filter(|file| !self.downloaded(context, file))
            .filter_map(|f| catalog::find(f))
            .map(|m| {
                context.llama.as_ref().map_or(m.bytes, |s| {
                    download::remaining_bytes(&m.url(), &s.models_folder().join(m.file), m.bytes)
                })
            })
            .sum()
    }

    fn validate_choice(&self, context: &Context, file: &str) -> Result<(), ServiceError> {
        validate_llama_choice(llama_server(context)?, file)
    }

    fn ensure_model<'a>(&'a self, context: &'a Context, model: &'a str) -> BoxFuture<'a, Result<(), ServiceError>> {
        Box::pin(llama_setup_service::download_if_missing(context, model))
    }

    fn prepare<'a>(
        &'a self,
        context: &'a Context,
        files: &'a BTreeSet<String>,
    ) -> BoxFuture<'a, Result<(), ServiceError>> {
        Box::pin(async move {
            let server = llama_server(context)?;
            let _lock = server.fetching.lock().await;
            let turn = server
                .downloads
                .begin_turn("Preparing task models")
                .map_err(|e| ServiceError::new(ErrorKind::TooMany, e))?;
            let result = prepare_llama_models(server, files).await;
            turn.end(result.as_ref().map(|_| ()).map_err(|e| e.message.clone()));
            result
        })
    }

    fn load<'a>(&'a self, context: &'a Context, model: &'a str) -> BoxFuture<'a, Result<Llm, LlmException>> {
        Box::pin(async move {
            let server = context
                .llama
                .as_ref()
                .ok_or_else(|| LlmException::new("The app was started without its llama folder."))?;
            let in_use = server.start(model).await.map_err(LlmException::new)?;
            let mut llm = Llm::use_key(LOCAL, "", &in_use.endpoint.model, &in_use.endpoint.base_url)?;
            llm.server_in_use = Some(Arc::new(in_use));
            Ok(llm)
        })
    }

    fn state(&self, context: &Context) -> RuntimeState {
        RuntimeState {
            inventory: context.llama.as_ref().map(|s| s.inventory()).unwrap_or_default(),
            release: context
                .llama
                .as_ref()
                .and_then(|s| std::fs::read_to_string(s.folder().join("runtime-release.txt")).ok())
                .unwrap_or_else(|| "External or unversioned runtime".into()),
            available: context.llama.as_ref().is_some_and(|s| s.server_path().is_some()),
        }
    }

    fn evaluate<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, Result<RuntimeState, ServiceError>> {
        Box::pin(async move {
            let server = llama_server(context)?;
            let inventory = tokio::task::spawn_blocking(Inventory::detect)
                .await
                .map_err(|e| ServiceError::failed("models/hardware", e.to_string()))?;
            server.refresh_inventory(inventory);
            Ok(self.state(context))
        })
    }
}

fn llama_server(context: &Context) -> Result<&LlamaServer, ServiceError> {
    context
        .llama
        .as_ref()
        .ok_or_else(|| ServiceError::new(ErrorKind::NotFound, "Local model setup is unavailable."))
}

fn validate_llama_choice(server: &LlamaServer, file: &str) -> Result<(), ServiceError> {
    let model = server
        .models()
        .into_iter()
        .find(|m| m.name == file)
        .or_else(|| catalog::find(file).map(|m| m.as_gguf(&server.models_folder())))
        .ok_or_else(|| {
            ServiceError::new(
                ErrorKind::Invalid,
                format!("{file} is missing and is not in the download catalog. Restore it or select another model."),
            )
        })?;
    if model_choice::fit(&model, &server.inventory(), server.inventory().gpu.is_some()) == Fit::TooBig {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            format!("{file} does not fit the detected memory. Review the local alternatives."),
        ));
    }
    Ok(())
}

async fn prepare_llama_models(server: &LlamaServer, files: &BTreeSet<String>) -> Result<(), ServiceError> {
    for file in files {
        validate_llama_choice(server, file)?;
    }
    std::fs::create_dir_all(server.models_folder())
        .map_err(|e| ServiceError::failed("models/folder", e.to_string()))?;
    let downloaded = server.models();
    let missing: Vec<_> = files
        .iter()
        .filter(|file| !downloaded.iter().any(|m| m.name == **file))
        .map(|file| {
            catalog::find(file).ok_or_else(|| {
                ServiceError::new(
                    ErrorKind::Invalid,
                    format!("{file} is not in the catalog. Restore it to the models folder or choose another model."),
                )
            })
        })
        .collect::<Result<_, _>>()?;
    let remaining = missing
        .iter()
        .map(|m| download::remaining_bytes(&m.url(), &server.models_folder().join(m.file), m.bytes))
        .sum();
    download::ensure_space(&server.models_folder(), remaining)
        .map_err(|e| ServiceError::failed("models/storage", e))?;
    if server.server_path().is_none() {
        download::download_server(server.folder(), &server.inventory(), &server.downloads)
            .await
            .map_err(|e| ServiceError::failed("models/runtime", e))?;
    }
    for model in missing {
        download::download_model(model, &server.models_folder(), &server.downloads)
            .await
            .map_err(|e| ServiceError::failed("models/download", e))?;
    }
    Ok(())
}
