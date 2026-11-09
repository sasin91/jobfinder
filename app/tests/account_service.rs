//! The API key page's, the llama.cpp wizard's and the setup wizard's work answers the same without
//! a web server, as a desktop window will call it: messages to show and view models back, and
//! errors as a kind and a message.

use std::sync::{Arc, LazyLock};

use jobfinder::account::account_model::UseChoices;
use jobfinder::account::account_service::{self, KeyForm, SaveKey};
use jobfinder::account::key_vault::KeyVault;
use jobfinder::account::llama_setup_service;
use jobfinder::context::Context;
use jobfinder::db;
use jobfinder::service_error::ErrorKind;
use jobfinder::setup::setup_steps;
use jobfinder::taxonomy::Taxonomy;

static TAXONOMY: LazyLock<Arc<Taxonomy>> = LazyLock::new(|| Arc::new(Taxonomy::load()));

/// A key that looks like OpenAI's.
const OPENAI_KEY: &str = "sk-proj-0123456789abcdef";

/// The work's shared state on a new, empty in-memory database: a first run.
async fn context() -> Context {
    let db = db::open_in_memory().await.unwrap();
    Context::new(db, TAXONOMY.clone(), KeyVault::with_key_in_memory())
}

#[tokio::test]
async fn onboarding_stays_unfinished_after_cv_import_and_finishes_with_deliberate_scan_choices() {
    let context = context().await;
    use sqlx::Executor;
    assert!(setup_steps::is_first_run(&context).await.unwrap());
    context.db().execute("INSERT INTO profile (id, name, email, phone, title, location, about, created_at, updated_at) VALUES (1, 'Ada', '', '', 'Developer', 'Aarhus', 'Rust', 0, 0)").await.unwrap();
    assert!(
        setup_steps::is_first_run(&context).await.unwrap(),
        "CV import must not complete setup"
    );
    account_service::save_key(&context, &openai_form(OPENAI_KEY))
        .await
        .unwrap();
    let step = setup_steps::search_step(&context).await.unwrap();
    assert_eq!(
        step.form.tailor_from_score, "101",
        "document generation starts as a deliberate choice"
    );
    let mut form = step.form;
    form.name = "Developer".into();
    form.keywords = "Rust".into();
    jobfinder::job_scan::search_service::save(&context, 0, &form)
        .await
        .ok()
        .unwrap();
    assert!(setup_steps::finish(&context).await.is_err());
    setup_steps::skip_scan(&context).await.unwrap();
    assert!(
        setup_steps::finish(&context).await.is_err(),
        "scan frequency must be chosen"
    );
    setup_steps::choose_manual(&context).await.unwrap();
    setup_steps::finish(&context).await.unwrap();
    assert!(!setup_steps::is_first_run(&context).await.unwrap());
}

#[tokio::test]
async fn model_reassessment_preserves_explicit_task_and_provider_choices() {
    use jobfinder::account::{model_plan, task_model};
    use jobfinder::llama::{hardware::Inventory, llama_server::LlamaServer};
    let mut context = context().await;
    context.llama = Some(LlamaServer::new(
        &std::env::temp_dir().join("jobfinder-reassessment-test"),
        Inventory::default(),
    ));
    account_service::save_key(&context, &openai_form(OPENAI_KEY))
        .await
        .unwrap();
    task_model::save(context.db(), &[(&task_model::APPLICATION_FORM, "custom-model".into())])
        .await
        .unwrap();
    model_plan::evaluate_again(&context).await.unwrap();
    let plan = model_plan::overview(&context).await.unwrap();
    let row = plan.rows.iter().find(|r| r.task.name == "application_form").unwrap();
    assert_eq!(row.provider, "openai");
    assert_eq!(row.current, "custom-model");
    assert!(row.manual);
    assert!(plan.assessment.is_some());
    assert!(
        model_plan::check(&context, "application_form").await.is_err(),
        "local check must never call a cloud provider"
    );
    assert!(
        model_plan::apply(&context, "application_form").await.is_err(),
        "local suggestion must never change cloud task routing"
    );
    assert_eq!(
        task_model::saved_for(context.db(), &task_model::APPLICATION_FORM)
            .await
            .unwrap(),
        "custom-model"
    );
}

fn openai_form(api_key: &str) -> KeyForm {
    KeyForm {
        provider: " openai ".to_string(),
        model: String::new(),
        api_key: api_key.to_string(),
        base_url: String::new(),
    }
}

#[tokio::test]
async fn a_key_that_cant_be_saved_comes_back_with_why_and_what_was_typed() {
    let context = context().await;
    let saved = account_service::save_key(&context, &openai_form("sk-short"))
        .await
        .unwrap();
    let SaveKey::Invalid { form, errors } = saved else {
        panic!("saved a short key: {saved:?}");
    };
    assert_eq!(form.provider, "openai");
    assert_eq!(errors, ["The API key field must be at least 20 characters in length."]);
}

#[tokio::test]
async fn a_saved_key_is_listed_and_can_be_deleted() {
    let context = context().await;
    let saved = account_service::save_key(&context, &openai_form(OPENAI_KEY))
        .await
        .unwrap();
    assert_eq!(
        saved,
        SaveKey::Saved {
            message: "Your OpenAI key is saved, for every use.".to_string(),
            setup_unfinished: true,
        }
    );
    let view = account_service::account_view(&context, None).await.unwrap();
    assert_eq!(view.saved.len(), 1);
    assert_eq!(view.saved[0].key_hint, "cdef");
    assert!(!view.key_uses.is_empty());
    let deleted = account_service::delete_key(&context, "openai").await.unwrap();
    assert_eq!(deleted, "Your OpenAI key is deleted.");
    let view = account_service::account_view(&context, None).await.unwrap();
    assert!(view.saved.is_empty());
    assert!(view.key_uses.is_empty());
}

#[tokio::test]
async fn which_key_each_use_runs_on_is_saved() {
    let context = context().await;
    account_service::save_key(&context, &openai_form(OPENAI_KEY))
        .await
        .unwrap();
    let none = UseChoices {
        resume: None,
        application: None,
        job_evaluation: None,
    };
    let said = account_service::save_uses(&context, &none).await.unwrap();
    assert_eq!(said, "Saved which key each use runs on.");
}

#[tokio::test]
async fn a_check_without_a_key_says_to_save_one() {
    let context = context().await;
    let error = account_service::check_key(&context, "openai").await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::NeedKey);
    assert_eq!(error.message, "There's no key to check: save one first.");
}

#[tokio::test]
async fn the_llama_wizard_needs_its_folder_and_a_known_model() {
    let context = Arc::new(context().await);
    let unknown = llama_setup_service::download_model(&context, "poster.gguf").unwrap_err();
    assert_eq!(unknown.kind, ErrorKind::Invalid);
    assert_eq!(unknown.message, "poster.gguf isn't one of the models offered.");
    let no_folder = llama_setup_service::llama_setup_view(&context).await.err().unwrap();
    assert_eq!(no_folder.message, "The app was started without its llama folder.");
    assert!(
        llama_setup_service::use_model(&context, "Qwen3-8B-Q4_K_M.gguf")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn reading_a_cv_needs_a_whole_cv() {
    let context = context().await;
    let error = setup_steps::read_cv(&context, "Too short.", "cv.pdf", false)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Invalid);
    assert!(
        error.message.starts_with("The CV field must be at least"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn the_scan_is_scheduled_and_unscheduled() {
    let context = context().await;
    let said = setup_steps::schedule_scan(&context, 4).await.unwrap();
    assert_eq!(
        said,
        "The job scan runs every 4 hours while you're logged in to this computer."
    );
    assert_eq!(setup_steps::schedule(&context).await.interval_hours, Some(4));
    let odd = setup_steps::schedule_scan(&context, 5).await.unwrap_err();
    assert_eq!(odd.kind, ErrorKind::Failed);
    assert_eq!(
        odd.message,
        "The job scan couldn't be scheduled: 5 hours isn't one of the intervals offered."
    );
    assert_eq!(
        setup_steps::unschedule_scan(&context).await.unwrap(),
        "The job scan no longer runs by itself."
    );
    assert_eq!(setup_steps::schedule(&context).await.interval_hours, None);
}
