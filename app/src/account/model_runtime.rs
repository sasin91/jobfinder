//! Adapters for runtimes whose models JobFinder installs and runs itself.
//! Cloud providers and external servers manage their own models.

use std::collections::BTreeSet;

use super::account_model::KeyFor;
use crate::context::Context;
use crate::llama::{
    catalog, download,
    llama_server::LlamaServer,
    model_choice::{self, Fit},
};
use crate::llm::LLAMA_CPP;
use crate::service_error::{ErrorKind, ServiceError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LocalRuntime {
    LlamaCpp,
}

impl LocalRuntime {
    pub fn for_provider(provider: &str) -> Option<Self> {
        match provider {
            LLAMA_CPP => Some(Self::LlamaCpp),
            _ => None,
        }
    }

    pub fn validate_choice(self, context: &Context, file: &str) -> Result<(), ServiceError> {
        match self {
            Self::LlamaCpp => validate_llama_choice(llama_server(context)?, file),
        }
    }

    pub async fn prepare(self, context: &Context, files: &BTreeSet<String>) -> Result<(), ServiceError> {
        match self {
            Self::LlamaCpp => {
                let server = llama_server(context)?;
                let _lock = server.fetching.lock().await;
                let turn = server
                    .downloads
                    .begin_turn("Preparing task models")
                    .map_err(|e| ServiceError::new(ErrorKind::TooMany, e))?;
                let result = prepare_llama_models(server, files).await;
                turn.end(result.as_ref().map(|_| ()).map_err(|e| e.message.clone()));
                result
            }
        }
    }

    pub fn key(self, model: &str) -> KeyFor {
        match self {
            Self::LlamaCpp => KeyFor {
                provider: LLAMA_CPP.into(),
                model: model.into(),
                base_url: String::new(),
                api_key: String::new(),
                uses: Default::default(),
            },
        }
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
