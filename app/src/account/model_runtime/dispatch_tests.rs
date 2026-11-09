//! Exercise the planner with runtimes that do not use llama.cpp or its context state.

use super::*;
use crate::account::{
    key_vault::KeyVault,
    model_runtime::{Recommendation, RuntimeState},
};
use crate::llm::{Llm, LlmException};
use futures_util::future::BoxFuture;
use std::sync::{LazyLock, Mutex};

struct TestRuntime {
    provider: &'static str,
    fail: bool,
    prepared: Mutex<Vec<BTreeSet<String>>>,
}

impl TestRuntime {
    const fn new(provider: &'static str, fail: bool) -> Self {
        Self {
            provider,
            fail,
            prepared: Mutex::new(Vec::new()),
        }
    }
}

impl ModelRuntime for TestRuntime {
    fn provider(&self) -> &'static str {
        self.provider
    }
    fn resolve(&self, _: &Context, _: &Task, base: &str, _: &str) -> String {
        base.into()
    }
    fn recommendation(&self, _: &Context, _: &Task) -> Recommendation {
        Recommendation {
            model: Some("test-model"),
            why: "Test recommendation",
            fit: "Fits".into(),
        }
    }
    fn downloaded(&self, _: &Context, _: &str) -> bool {
        true
    }
    fn download_bytes(&self, _: &Context, _: &BTreeSet<String>) -> u64 {
        0
    }
    fn validate_choice(&self, _: &Context, _: &str) -> Result<(), ServiceError> {
        Ok(())
    }
    fn ensure_model<'a>(&'a self, _: &'a Context, _: &'a str) -> BoxFuture<'a, Result<(), ServiceError>> {
        Box::pin(async { Ok(()) })
    }
    fn prepare<'a>(&'a self, _: &'a Context, models: &'a BTreeSet<String>) -> BoxFuture<'a, Result<(), ServiceError>> {
        Box::pin(async move {
            self.prepared.lock().unwrap().push(models.clone());
            if self.fail {
                Err(ServiceError::failed("test-runtime", "Preparation failed"))
            } else {
                Ok(())
            }
        })
    }
    fn load<'a>(&'a self, _: &'a Context, _: &'a str) -> BoxFuture<'a, Result<Llm, LlmException>> {
        Box::pin(async { Err(LlmException::new("No inference in preparation tests")) })
    }
    fn state(&self, _: &Context) -> RuntimeState {
        RuntimeState {
            inventory: Inventory::default(),
            release: "test-release".into(),
            available: true,
        }
    }
    fn evaluate<'a>(&'a self, context: &'a Context) -> BoxFuture<'a, Result<RuntimeState, ServiceError>> {
        Box::pin(async move { Ok(self.state(context)) })
    }
}

async fn context() -> Context {
    static TAXONOMY: LazyLock<Arc<crate::taxonomy::Taxonomy>> =
        LazyLock::new(|| Arc::new(crate::taxonomy::Taxonomy::load()));
    Context::new(
        crate::db::open_in_memory().await.unwrap(),
        Arc::clone(&TAXONOMY),
        KeyVault::with_key_in_memory(),
    )
}

fn row(task: &'static Task, runtime: &'static dyn ModelRuntime, model: &str) -> Row {
    Row {
        task,
        provider: runtime.provider().into(),
        runtime: Some(runtime),
        runtime_available: true,
        current: model.into(),
        suggested: None,
        why: "",
        fit: "".into(),
        downloaded: false,
        manual: true,
        check: None,
    }
}

#[tokio::test]
async fn plan_dispatches_trait_objects_and_deduplicates_models_per_runtime() {
    static FIRST: TestRuntime = TestRuntime::new("first", false);
    static SECOND: TestRuntime = TestRuntime::new("second", false);
    let context = context().await;
    assert!(context.llama.is_none());
    let plan = Overview {
        fresh: false,
        assessment: None,
        download_bytes: 0,
        rows: vec![
            row(&task_model::READ_JOB, &FIRST, "shared"),
            row(&task_model::READ_CV, &FIRST, "shared"),
            row(&task_model::JUDGE, &FIRST, "other"),
            row(&task_model::RESUME_TASK, &SECOND, "shared"),
        ],
    };
    plan.prepare_models(&context).await.unwrap();
    assert_eq!(
        *FIRST.prepared.lock().unwrap(),
        vec![BTreeSet::from(["shared".into(), "other".into()])]
    );
    assert_eq!(
        *SECOND.prepared.lock().unwrap(),
        vec![BTreeSet::from(["shared".into()])]
    );
}

#[tokio::test]
async fn runtime_preparation_errors_stop_dispatch_and_reach_the_caller() {
    static FAILED: TestRuntime = TestRuntime::new("a-failed", true);
    static LATER: TestRuntime = TestRuntime::new("z-later", false);
    let context = context().await;
    let plan = Overview {
        fresh: false,
        assessment: None,
        download_bytes: 0,
        rows: vec![
            row(&task_model::READ_JOB, &FAILED, "selected"),
            row(&task_model::READ_CV, &LATER, "untouched"),
        ],
    };
    assert!(
        plan.prepare_models(&context)
            .await
            .unwrap_err()
            .message
            .contains("Preparation failed")
    );
    assert!(LATER.prepared.lock().unwrap().is_empty());
}
