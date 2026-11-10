//! The match work as the window calls it: view models and data back, and errors as a kind and a
//! message to show.

use std::sync::{Arc, LazyLock};

use jobfinder::account::key_vault::KeyVault;
use jobfinder::context::Context;
use jobfinder::db;
use jobfinder::laya::laya_client::{self, LayaClient};
use jobfinder::matches::match_service::{self, MatchAsk, RejectOutcome};
use jobfinder::matches::{Job, Profile, Requirement};
use jobfinder::service_error::ErrorKind;
use jobfinder::taxonomy::Taxonomy;
use sqlx::Executor;

static TAXONOMY: LazyLock<Arc<Taxonomy>> = LazyLock::new(|| Arc::new(Taxonomy::load()));

/// One saved match, with a résumé written as fields.
const SEED: &str = "
INSERT INTO matches (id, job_title, company, job_url, job_text, score, points, max_points, tag, created_at)
VALUES (21, 'PHP-udvikler', 'Firma', 'https://www.jobindex.dk/job/1', 'Vi søger en PHP-udvikler.', 0.86, 9, 10, 'top', 1760000000);
INSERT INTO documents (id, kind, name, text, written_at) VALUES (1, 'cv', 'cv', 'PHP', 1760000000);
INSERT INTO match_documents (match_id, document_id) VALUES (21, 1);
INSERT INTO match_items (match_id, criterion, text, verdict, decided_by, reason, sort_order)
VALUES (21, 'requirements', 'PHP', 'met', 'exact', 'Named in the CV.', 0);
";

/// The work's shared state on an in-memory database with SEED in it.
async fn context() -> Context {
    let db = db::open_in_memory().await.unwrap();
    db.execute(sqlx::raw_sql(SEED)).await.unwrap();
    Context::new(db, TAXONOMY.clone(), KeyVault::with_key_in_memory())
}

#[tokio::test]
async fn the_free_match_answers_a_result_to_print() {
    let context = context().await;
    let result = match_service::quick_match(
        &context,
        "PHP-udvikler\nDin profil\n- Erfaring med PHP\n- Kendskab til Docker",
        "https://www.jobindex.dk/job/1",
        "Udvikler\nPHP, Laravel, MySQL",
    )
    .await
    .unwrap();
    assert_eq!(result.saved_id, None);
    assert_eq!(result.job_url, "https://www.jobindex.dk/job/1");
    assert!(result.sections.iter().any(|section| !section.rows.is_empty()));
    assert!(result.cv_found.is_some());
}

#[tokio::test]
async fn the_free_match_says_what_is_missing_from_its_input() {
    let context = context().await;
    let error = match_service::quick_match(&context, "", "", "PHP").await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Invalid);
    assert_eq!(error.message, "Paste or upload both the CV and the job post.");
}

#[tokio::test]
async fn a_saved_match_is_found_by_id_and_others_are_not() {
    let context = context().await;
    let result = match_service::saved_result(&context, 21).await.unwrap();
    assert_eq!(result.saved_id, Some(21));
    for id in [0, -1, 99] {
        let error = match_service::saved_match(&context, id).await.unwrap_err();
        assert_eq!(error.kind, ErrorKind::NotFound, "id {id}");
    }
}

#[tokio::test]
async fn a_reject_needs_a_reason_and_can_be_undone() {
    let context = context().await;
    let unfinished = match_service::reject(&context, 21, &[], &[], false).await.unwrap();
    assert!(matches!(unfinished, RejectOutcome::Unfinished(problem) if !problem.is_empty()));
    let reasons = ["commute".to_string()];
    let notes = [("commute".to_string(), "1h each way".to_string())];
    let rejected = match_service::reject(&context, 21, &reasons, &notes, false)
        .await
        .unwrap();
    assert_eq!(
        rejected,
        RejectOutcome::Done(Some("Rejected. It is in the Rejected list if you change your mind."))
    );
    let back = match_service::reject(&context, 21, &[], &[], true).await.unwrap();
    assert_eq!(back, RejectOutcome::Done(Some("The match is back in your lists.")));
}

#[tokio::test]
async fn a_pdf_is_named_for_the_job_and_needs_a_known_kind() {
    let context = context().await;
    let made = match_service::pdf(&context, 21, "job", "", None).await.unwrap();
    assert!(made.bytes.starts_with(b"%PDF"));
    assert!(made.name.ends_with(".pdf"), "{}", made.name);
    let error = match_service::pdf(&context, 21, "poster", "", None).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::Invalid);
}

#[tokio::test]
async fn the_apps_own_pdf_reads_back_as_its_text() {
    let context = context().await;
    let made = match_service::pdf(&context, 21, "job", "", None).await.unwrap();
    let text = jobfinder::pdf_text::text_of(made.bytes).unwrap();
    assert!(text.contains("Vi søger en PHP-udvikler."), "{text}");
}

#[tokio::test]
async fn a_joblog_status_must_be_one_of_jobnets() {
    let context = context().await;
    let error = match_service::save_joblog_status(&context, 21, "maybe", true)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::Malformed);
    assert!(error.message.starts_with("Pick one of Jobnet's statuses:"));
}

#[tokio::test]
async fn marking_applied_sets_soegt_but_keeps_samtale_and_the_joblog() {
    let context = context().await;
    let applied = match_service::mark_applied(&context, 21).await.unwrap();
    assert_eq!(applied.status, "Søgt");
    assert_eq!(applied.applied_on.len(), "dd-mm-yyyy".len());

    match_service::save_joblog_status(&context, 21, "Samtale", true)
        .await
        .unwrap();
    let again = match_service::mark_applied(&context, 21).await.unwrap();
    assert_eq!(again.status, "Samtale");
    let saved = match_service::saved_match(&context, 21).await.unwrap();
    assert!(saved.jobnet_joblog.unwrap().logged_at.is_some());

    let error = match_service::mark_applied(&context, 99).await.unwrap_err();
    assert_eq!(error.kind, ErrorKind::NotFound);
}

#[tokio::test]
async fn link_fetches_stop_after_the_limit() {
    let context = context().await;
    let now = chrono::Utc::now().timestamp();
    for _ in 0..match_service::MAX_FETCHES {
        context.count_fetch(now).unwrap();
    }
    let error = match_service::fetch_job(&context, "https://example.com/job")
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::TooMany);
}

#[tokio::test]
async fn writing_without_a_key_points_to_the_key_page() {
    let context = Arc::new(context().await);
    let error = match_service::write(
        context,
        21,
        "",
        "application",
        None,
        match_service::WhenAbandoned::KeepWriting,
    )
    .await
    .unwrap_err();
    assert_eq!(error.kind, ErrorKind::NeedKey);
}

/// A MatchAsk of these texts, with no CV read before.
fn ask<'a>(job_text: &'a str, job_url: &'a str, cv_text: &'a str) -> MatchAsk<'a> {
    MatchAsk {
        job_text,
        job_url,
        cv_text,
        read_cv: None,
    }
}

#[tokio::test]
async fn matching_without_a_model_is_the_free_match_and_says_its_step() {
    let context = context().await;
    let said = std::sync::Mutex::new(Vec::new());
    let say = |step: &str| said.lock().unwrap().push(step.to_string());
    let answer = match_service::match_job(
        &context,
        ask(
            "PHP-udvikler\nDin profil\n- Erfaring med PHP\n- Kendskab til Docker",
            "",
            "Udvikler\nPHP, Laravel, MySQL",
        ),
        &say,
    )
    .await
    .unwrap();
    assert!(answer.fetched.is_none());
    assert!(answer.read_cv.is_none());
    assert!(!answer.matched_before);
    assert!(answer.result.cv_found.is_some());
    assert_eq!(*said.lock().unwrap(), ["Matching…"]);
}

#[tokio::test]
async fn matching_says_what_is_missing_before_any_work() {
    let context = context().await;
    let no_job = match_service::match_job(&context, ask("  ", "", "PHP"), &|_| {})
        .await
        .unwrap_err();
    assert_eq!(no_job.kind, ErrorKind::Invalid);
    let no_cv = match_service::match_job(&context, ask("Vi søger en PHP-udvikler.", "", " "), &|_| {})
        .await
        .unwrap_err();
    assert_eq!(no_cv.message, "Paste or choose a CV first.");
    let not_a_link = match_service::match_job(&context, ask("", "jobindex.dk/job/1", "PHP"), &|_| {})
        .await
        .unwrap_err();
    assert_eq!(not_a_link.message, "Paste a link starting with http:// or https://.");
}

#[tokio::test]
async fn a_link_in_the_job_box_is_fetched_first() {
    let context = context().await;
    let now = chrono::Utc::now().timestamp();
    for _ in 0..match_service::MAX_FETCHES {
        context.count_fetch(now).unwrap();
    }
    // The fetch is counted, so it stops at the limit: the link was fetched, not matched as text.
    let error = match_service::match_job(&context, ask(" https://example.com/job ", "", "PHP"), &|_| {})
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::TooMany);
}

#[test]
fn a_link_is_one_address_and_nothing_else() {
    assert!(match_service::is_link("https://www.jobindex.dk/job/1"));
    assert!(match_service::is_link(" HTTP://example.com "));
    assert!(!match_service::is_link("https://"));
    assert!(!match_service::is_link("See https://example.com"));
    assert!(!match_service::is_link("Vi søger en udvikler"));
}

#[tokio::test]
async fn writing_a_document_without_a_key_points_to_the_key_page_and_says_its_step() {
    let context = Arc::new(context().await);
    let said = std::sync::Mutex::new(Vec::new());
    let say = |step: &str| said.lock().unwrap().push(step.to_string());
    let error = match_service::write_document(context, 21, "", "resume", None, &say)
        .await
        .unwrap_err();
    assert_eq!(error.kind, ErrorKind::NeedKey);
    assert_eq!(*said.lock().unwrap(), ["Tailoring the résumé…"]);
}

/// A match request of a post asking for PHP and a CV naming it, without texts.
fn php_request() -> match_service::MatchRequest {
    match_service::MatchRequest {
        job: Job {
            job_title: "PHP-udvikler".to_string(),
            requirements: vec![Requirement {
                value: "PHP".to_string(),
                kind: "skill".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        },
        profile: Profile {
            skill: vec!["PHP".to_string()],
            ..Default::default()
        },
        ..Default::default()
    }
}

#[tokio::test]
async fn rejection_feedback_lowers_future_scores_and_bring_back_removes_it() {
    let context = context().await;
    let mut request = php_request();
    request.job.job_title = "PHP-UDVIKLER".to_string();
    request.job.company = "A different company".to_string();
    request.profile.title = vec![request.job.job_title.clone()];
    let baseline = match_service::ai_match(&context, &request).await.unwrap().result;

    match_service::reject(&context, 21, &["applied".to_string()], &[], false)
        .await
        .unwrap();
    let applied = match_service::ai_match(&context, &request).await.unwrap();
    assert_eq!(applied.result, baseline);

    match_service::reject(
        &context,
        21,
        &["irrelevant".to_string(), "company".to_string()],
        &[],
        false,
    )
    .await
    .unwrap();
    // A late dismissal from a search card cannot erase explicit feedback.
    jobfinder::matches::match_model::reject(context.db(), 21, &[], 1760000001)
        .await
        .unwrap();
    request.job_text = "Another job posting seeking a PHP developer.".to_string();
    request.cv_text = "PHP-udvikler with experience in PHP.".to_string();
    let adjusted = match_service::ai_match(&context, &request).await.unwrap();
    assert_eq!(adjusted.result.index, baseline.index - 0.25);
    assert_eq!(adjusted.result.tag, "medium");
    assert_eq!(adjusted.result.parts, baseline.parts);
    let saved_id = adjusted.saved_id.unwrap();
    let saved = match_service::saved_result(&context, saved_id).await.unwrap();
    assert!(
        saved
            .parts
            .contains("marked a match with this role title as irrelevant")
    );
    assert_eq!(saved.score.percent, 75);

    let post = "PHP-udvikler\nDin profil\n- Erfaring med PHP";
    let quick = match_service::quick_match(&context, post, "", "PHP").await.unwrap();
    assert!(
        quick
            .parts
            .contains("marked a match with this role title as irrelevant")
    );

    request.job.company = " FIRMA ".to_string();
    let both = match_service::ai_match(&context, &request).await.unwrap();
    assert_eq!(both.result, adjusted.result);

    // A rejection for one role does not penalize a different role, even at the same company,
    // beyond the smaller adjustment for explicitly rejecting the company.
    request.job.job_title = "Laravel developer".to_string();
    request.profile.title = vec![request.job.job_title.clone()];
    let company = match_service::ai_match(&context, &request).await.unwrap();
    assert_eq!(company.result.index, 0.90);
    assert!(company.result.feedback_note.unwrap().contains("this company"));

    match_service::reject(&context, 21, &[], &[], true).await.unwrap();
    request.job.job_title = "PHP-UDVIKLER".to_string();
    request.profile.title = vec![request.job.job_title.clone()];
    let restored = match_service::ai_match(&context, &request).await.unwrap();
    assert_eq!(restored.result, baseline);
    let saved = match_service::saved_result(&context, saved_id).await.unwrap();
    assert_eq!(saved.score.percent, 75);
}

#[tokio::test]
async fn a_laya_model_that_cant_load_is_said_under_the_score() {
    let folder = std::env::temp_dir().join(format!("jobfinder-laya-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join(laya_client::ONNX_FILE), "not a model").unwrap();
    let mut context = context().await;
    context.laya = LayaClient::from_folder(&folder);
    assert!(context.laya.is_some());

    let result = match_service::quick_match(
        &context,
        "PHP-udvikler\nDin profil\n- Erfaring med PHP",
        "",
        "Udvikler\nPHP, Laravel",
    )
    .await
    .unwrap();
    assert!(result.laya.failed);
    assert!(
        result
            .laya
            .text
            .starts_with("No Laya reading this time. Laya couldn't load its model from"),
        "{}",
        result.laya.text
    );
    assert!(result.laya.text.contains("The score above doesn't depend on it."));

    let found = match_service::ai_match(&context, &php_request()).await.unwrap();
    assert!(found.laya_reading.laya.is_none());
    assert!(
        found
            .laya_reading
            .laya_error
            .as_deref()
            .unwrap()
            .starts_with("Laya couldn't load its model from"),
        "{:?}",
        found.laya_reading.laya_error
    );
    std::fs::remove_dir_all(folder).unwrap();
}

#[tokio::test]
async fn without_a_laya_model_a_match_has_no_laya_line() {
    let context = context().await;
    let found = match_service::ai_match(&context, &php_request()).await.unwrap();
    assert!(found.laya_reading.laya.is_none());
    assert!(found.laya_reading.laya_error.is_none());
}

/// Made up: a CV whose dates run from 2010 (an own project) but whose years line, the number
/// the candidate confirmed, says 9. The free reader, which the free match reads the CV with,
/// takes the 9.
#[test]
fn the_free_reader_takes_the_confirmed_years_over_the_dates() {
    use jobfinder::matches::free_reader::{FreeReader, YearMonth};
    let now = YearMonth { year: 2026, month: 11 };
    let dates = "Kim\n\nOwn site\n2010 - nu\n\nDeveloper\n2017 - nu";
    assert_eq!(FreeReader::new(&TAXONOMY, now).cv(dates).experience_years, 16);
    let confirmed = "Kim\nYears of professional work: 9\n\nOwn site\n2010 - nu\n\nDeveloper\n2017 - nu";
    assert_eq!(FreeReader::new(&TAXONOMY, now).cv(confirmed).experience_years, 9);
}
