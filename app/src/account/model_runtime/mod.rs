//! Runtime discovery and shared contracts. Each implementation lives in its own module.

pub mod llama_cpp;
mod runtime;

pub use runtime::{ModelRuntime, Recommendation, RuntimeState};

use super::account_model::KeyFor;
use crate::context::Context;
use crate::llm::{Llm, LlmException};

static RUNTIMES: &[&dyn ModelRuntime] = &[&llama_cpp::LlamaCpp];

pub fn for_provider(provider: &str) -> Option<&'static dyn ModelRuntime> {
    RUNTIMES.iter().copied().find(|runtime| runtime.provider() == provider)
}

pub fn default_runtime() -> &'static dyn ModelRuntime {
    RUNTIMES[0]
}

/// Start a managed runtime, or connect to the configured cloud/external provider.
pub async fn load_key(context: &Context, key: &KeyFor) -> Result<Llm, LlmException> {
    match for_provider(&key.provider) {
        Some(runtime) => runtime.load(context, &key.model).await,
        None => Llm::use_key(&key.provider, &key.api_key, &key.model, &key.base_url),
    }
}
