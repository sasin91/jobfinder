//! The model-management contract shared by bundled runtimes.

use std::collections::BTreeSet;

use crate::account::task_model::Task;
use crate::context::Context;
use crate::llama::hardware::Inventory;
use crate::llm::{Llm, LlmException};
use crate::service_error::ServiceError;
use futures_util::future::BoxFuture;

pub struct Recommendation {
    pub model: Option<&'static str>,
    pub why: &'static str,
    pub fit: String,
}

pub struct RuntimeState {
    pub inventory: Inventory,
    pub release: String,
    pub available: bool,
}

/// Implementations own their model catalog, installation, and loading behavior.
/// Futures are Send so model work can run on the app's background runtime.
pub trait ModelRuntime: Send + Sync {
    fn provider(&self) -> &'static str;
    fn resolve(&self, context: &Context, task: &Task, base: &str, saved: &str) -> String;
    fn recommendation(&self, context: &Context, task: &Task) -> Recommendation;
    fn downloaded(&self, context: &Context, model: &str) -> bool;
    fn download_bytes(&self, context: &Context, models: &BTreeSet<String>) -> u64;
    fn validate_choice(&self, context: &Context, model: &str) -> Result<(), ServiceError>;
    fn ensure_model<'a>(&'a self, context: &'a Context, model: &'a str) -> BoxFuture<'a, Result<(), ServiceError>>;
    fn prepare<'a>(
        &'a self,
        context: &'a Context,
        models: &'a BTreeSet<String>,
    ) -> BoxFuture<'a, Result<(), ServiceError>>;
    fn load<'a>(&'a self, context: &'a Context, model: &'a str) -> BoxFuture<'a, Result<Llm, LlmException>>;
    fn state(&self, context: &Context) -> RuntimeState;
    fn evaluate<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, Result<RuntimeState, ServiceError>>;
}
