//! The local server answers the Jobnet extension as it expects, and only the extension and this
//! app: the saved matches, a match as a joblog entry and its PDFs, the joblog status, and a post
//! the browser shows sent to a job search.

use std::sync::{Arc, LazyLock};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use jobfinder::account::key_vault::KeyVault;
use jobfinder::context::Context;
use jobfinder::db;
use jobfinder::job_scan::search_service::{self, SearchForm};
use jobfinder::matches::match_model::{SHORTLIST, SortOrder};
use jobfinder::matches::match_service;
use jobfinder::server::{self, AppState};
use jobfinder::taxonomy::Taxonomy;
use serde_json::{Value, json};
use sqlx::{Executor, SqlitePool};
use tower::ServiceExt;

const PORT: u16 = server::DEFAULT_PORT;
const HOST: &str = "127.0.0.1:8765";
const ORIGIN: &str = "http://127.0.0.1:8765";

static TAXONOMY: LazyLock<Arc<Taxonomy>> = LazyLock::new(|| Arc::new(Taxonomy::load()));

/// A saved match with a missed location, an email instruction and a résumé written as fields, and
/// a rejected one.
const SEED: &str = "
INSERT INTO matches (id, job_title, company, job_url, job_text, score, points, max_points, tag, created_at)
VALUES (21, 'PHP-udvikler', 'Firma <A/S>', 'https://www.jobindex.dk/job/1', 'Vi søger en PHP-udvikler.\nSend din ansøgning til job@firma.dk. Skriv \"Backend\" i emnefeltet.', 0.86, 9, 10, 'top', 1760000000),
       (22, 'Mekaniker', '', NULL, 'Mekaniker søges', 0.2, 1, 10, 'poor', 1760000100);
INSERT INTO documents (id, kind, name, text, written_at) VALUES (1, 'cv', 'cv', 'PHP', 1760000000);
INSERT INTO match_documents (match_id, document_id) VALUES (21, 1), (22, 1);
INSERT INTO match_items (match_id, criterion, text, verdict, decided_by, reason, sort_order)
VALUES (21, 'requirements', 'PHP', 'met', 'exact', 'Named in the CV.', 0),
       (21, 'location', 'Bor i København', 'missing', 'llm', 'Lives in Slagelse.', 1);
INSERT INTO match_rejections (match_id, reason, note, created_at) VALUES (22, 'commute', '1h each way', 0);
UPDATE matches SET rejected_at = 1 WHERE id = 22;
INSERT INTO documents (id, kind, text, written_at) VALUES (5, 'resume', 'Mette Sørensen', 1760000200);
INSERT INTO match_documents (match_id, document_id) VALUES (21, 5);
INSERT INTO match_resumes (document_id, name, intro) VALUES (5, 'Mette Sørensen', 'Backend-udvikler.');
INSERT INTO match_resume_entries (id, document_id, section, title, sort_order) VALUES (1, 5, 'experience', 'Udvikler', 0);
INSERT INTO match_resume_lines (document_id, entry_id, kind, text, sort_order)
VALUES (5, 1, 'bullet', 'Byggede en API', 0), (5, NULL, 'skill', 'PHP', 1);
";

/// An in-memory database with SEED in it.
async fn seeded() -> SqlitePool {
    let db = db::open_in_memory().await.unwrap();
    db.execute(sqlx::raw_sql(SEED)).await.unwrap();
    db
}

async fn app() -> axum::Router {
    let db = seeded().await;
    let context = Context::new(db, TAXONOMY.clone(), KeyVault::with_key_in_memory());
    server::router(Arc::new(AppState::new(Arc::new(context), PORT)))
}

async fn send(request: Request<Body>) -> (StatusCode, String) {
    send_to(&app().await, request).await
}

/// Sends to one app, so a later request sees what an earlier one saved.
async fn send_to(app: &axum::Router, request: Request<Body>) -> (StatusCode, String) {
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
}

fn get(path: &str) -> Request<Body> {
    Request::get(path)
        .header(header::HOST, HOST)
        .body(Body::empty())
        .unwrap()
}

fn post_json(path: &str, json: &str) -> Request<Body> {
    Request::post(path)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, ORIGIN)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json.to_string()))
        .unwrap()
}

#[tokio::test]
async fn other_sites_cant_ask_and_nothing_else_is_here() {
    let request = Request::post("/match/joblog_status")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, "https://evil.example")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"id": 21, "status": "Soegt"}"#))
        .unwrap();
    let (status, _) = send(request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        Request::get("/match/history")
            .header(header::HOST, "evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The web pages are gone: the window is the app.
    for path in [
        "/",
        "/match",
        "/setup",
        "/match/result/21",
        "/match_module/css/match.css",
    ] {
        let (status, answer) = send(get(path)).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            answer,
            r#"{"error":"The JobFinder app has no web pages: open it from its window."}"#
        );
    }
}

/// A saved match's PDF: its status, Content-Type and Content-Disposition, and its bytes.
async fn pdf(app: &axum::Router, json: &str) -> (StatusCode, String, String, Vec<u8>) {
    let response = app.clone().oneshot(post_json("/match/pdf", json)).await.unwrap();
    let status = response.status();
    let header_of = |name| {
        response
            .headers()
            .get(name)
            .map(|value: &header::HeaderValue| value.to_str().unwrap().to_string())
            .unwrap_or_default()
    };
    let (mime, disposition) = (header_of(header::CONTENT_TYPE), header_of(header::CONTENT_DISPOSITION));
    let body = response.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, mime, disposition, body)
}

#[tokio::test]
async fn a_saved_match_downloads_as_a_pdf_named_for_the_job() {
    let app = app().await;
    let (status, mime, disposition, body) = pdf(
        &app,
        r#"{"id": "21", "kind": "application", "text": "Kære Firma\n\nJeg søger."}"#,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert_eq!(mime, "application/pdf");
    assert_eq!(
        disposition,
        "attachment; filename=\"Application - PHP-udvikler - Firma A S.pdf\"; filename*=UTF-8''Application%20-%20PHP-udvikler%20-%20Firma%20A%20S.pdf"
    );
    assert!(body.starts_with(b"%PDF-"));

    // The job post and the CV are always the saved text; the job post's name is in Danish too.
    let (status, _, disposition, _) = pdf(&app, r#"{"id": 22, "kind": "job"}"#).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        disposition.contains("filename*=UTF-8''Job%20ad%20-%20Mekaniker.pdf"),
        "{disposition}"
    );
    let (status, _, _, _) = pdf(&app, r#"{"id": 21, "kind": "cv", "text": "ignored"}"#).await;
    assert_eq!(status, StatusCode::OK);

    // A résumé written as fields is laid out from them.
    let (status, _, disposition, body) = pdf(&app, r#"{"id": 21, "kind": "resume"}"#).await;
    assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&body));
    assert!(disposition.contains("Resume%20-%20PHP-udvikler"));
}

#[tokio::test]
async fn a_pdf_without_text_or_kind_says_why() {
    let app = app().await;
    for (json, status, error) in [
        (
            r#"{"id": 22, "kind": "application"}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
            "There is no text to export yet.",
        ),
        (
            r#"{"id": 21, "kind": "letter"}"#,
            StatusCode::UNPROCESSABLE_ENTITY,
            "Choose the application, the résumé, the CV or the job post.",
        ),
        (
            r#"{"id": 99, "kind": "job"}"#,
            StatusCode::NOT_FOUND,
            "That saved match was not found.",
        ),
    ] {
        let (answer_status, mime, _, body) = pdf(&app, json).await;
        assert_eq!(answer_status, status, "{json}");
        assert_eq!(mime, "application/json");
        let answer: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(answer["error"], error, "{json}");
    }
}

/// The Jobnet extension's origin (extension::ID).
const EXTENSION: &str = "chrome-extension://jijhihfppobcoconjjllmffbohpffbll";

#[tokio::test]
async fn the_joblog_is_filled_in_from_the_saved_match() {
    let (status, entry) = send(get("/match/joblog/21")).await;
    assert_eq!(status, StatusCode::OK);
    let entry: serde_json::Value = serde_json::from_str(&entry).unwrap();
    assert_eq!(entry["sections"][0]["fields"][0]["value"], "PHP-udvikler");
    assert_eq!(entry["sections"][1]["fields"][3]["value"], true);
    assert_eq!(entry["logged_at"], serde_json::Value::Null);

    let (status, answer) = send(get("/match/joblog/99")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(answer, r#"{"error":"That saved match was not found."}"#);
}

#[tokio::test]
async fn the_application_picker_matches_the_full_desktop_shortlist() {
    let db = seeded().await;
    db.execute(sqlx::raw_sql(
        "WITH RECURSIVE ids(id) AS (SELECT 100 UNION ALL SELECT id + 1 FROM ids WHERE id < 155)
         INSERT INTO matches (id, job_title, company, job_text, score, points, max_points, tag, created_at)
         SELECT id, 'Developer', 'Acme', '', CASE id % 3 WHEN 0 THEN 1.0 WHEN 1 THEN 0.9 ELSE 0.8 END,
                9, 10, 'top', 1760000000 + id / 2
         FROM ids;
         UPDATE matches SET closed_at = 1 WHERE id = 100;
         UPDATE matches SET tag = 'poor' WHERE id = 101;
         INSERT INTO match_integrations (match_id, integration, status, created_at, updated_at)
         VALUES (102, 'jobnet_joblog', 'Soegt', 1, 1);",
    ))
    .await
    .unwrap();
    let context = Arc::new(Context::new(db, TAXONOMY.clone(), KeyVault::with_key_in_memory()));
    let app = server::router(Arc::new(AppState::new(Arc::clone(&context), PORT)));

    for sort in [SortOrder::Best, SortOrder::Newest, SortOrder::Oldest] {
        context.set_saved_match_sort(sort);
        let mut expected = Vec::new();
        let mut after = String::new();
        loop {
            let page = match_service::sorted_saved_matches(&context, Some(SHORTLIST), &after, sort)
                .await
                .unwrap();
            expected.extend(page.matches.into_iter().map(|row| row.id));
            let Some(next) = page.next_after else { break };
            after = next;
        }
        assert_eq!(expected.len(), 54);
        assert!([22, 100, 101, 102].iter().all(|id| !expected.contains(id)));

        let (status, body) = send_to(&app, get("/match/history?shortlist=true")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let answer: Value = serde_json::from_str(&body).unwrap();
        let ids: Vec<_> = answer["matches"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["id"].as_i64().unwrap())
            .collect();
        assert_eq!(ids, expected, "{sort:?}");
    }
}

#[tokio::test]
async fn the_extension_marks_a_match_as_on_jobnet() {
    let app = app().await;
    let (_, history) = send_to(&app, get("/match/history")).await;
    let history: serde_json::Value = serde_json::from_str(&history).unwrap();
    assert_eq!(history["matches"][0]["id"], 21);
    assert_eq!(history["matches"][0]["job_title"], "PHP-udvikler");
    assert_eq!(
        history["matches"].as_array().unwrap().len(),
        1,
        "rejected matches stay out"
    );
    assert_eq!(history["not_joblogged"], serde_json::json!([]));

    // Applied for: it is waiting to be logged.
    let (status, logged) = send_to(
        &app,
        post_json(
            "/match/joblog_status",
            r#"{"id": "21", "status": "Soegt", "logged": false}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{logged}");
    assert_eq!(
        serde_json::from_str::<Value>(&logged).unwrap(),
        json!({ "logged": false, "at": "" })
    );
    let (_, history) = send_to(&app, get("/match/history")).await;
    let history: serde_json::Value = serde_json::from_str(&history).unwrap();
    assert_eq!(history["not_joblogged"][0]["id"], 21);
    assert_eq!(history["not_joblogged"][0]["jobnet_status"], "Soegt");

    // The extension saved it on Jobnet.
    let request = Request::post("/match/joblog_status")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, EXTENSION)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"id": 21, "status": "Samtale", "logged": true}"#))
        .unwrap();
    let (status, logged) = send_to(&app, request).await;
    assert_eq!(status, StatusCode::OK, "{logged}");
    let logged: Value = serde_json::from_str(&logged).unwrap();
    assert_eq!(logged["logged"], true);
    assert!(!logged["at"].as_str().unwrap().is_empty(), "{logged}");
    let (_, history) = send_to(&app, get("/match/history")).await;
    let history: serde_json::Value = serde_json::from_str(&history).unwrap();
    assert_eq!(history["not_joblogged"], serde_json::json!([]));
    let (_, entry) = send_to(&app, get("/match/joblog/21")).await;
    let entry: serde_json::Value = serde_json::from_str(&entry).unwrap();
    assert_eq!(entry["sections"][2]["fields"][0]["value"], "Samtale");
    assert!(entry["logged_at"].is_i64());
    // The day applied is kept from Søgt.
    assert_eq!(
        entry["sections"][2]["fields"][1]["value"].as_str().unwrap().len(),
        "30-10-2026".len()
    );

    let (status, answer) = send_to(
        &app,
        post_json("/match/joblog_status", r#"{"id": 21, "status": "Done"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        answer,
        r#"{"error":"Pick one of Jobnet's statuses: Ikke søgt, Søgt, Samtale."}"#
    );
    let (status, _) = send_to(
        &app,
        post_json("/match/joblog_status", r#"{"id": 22000, "status": "Soegt"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn a_second_start_brings_the_window_to_the_front_or_is_told_there_is_none() {
    let front = || post_json("/instance/front", "");
    // Before the window is open, there is none to bring to the front.
    assert_eq!(send(front()).await.0, StatusCode::NOT_FOUND);

    let db = seeded().await;
    let context = Context::new(db, TAXONOMY.clone(), KeyVault::with_key_in_memory());
    let mut state = AppState::new(Arc::new(context), PORT);
    let (window, mut asked) = tokio::sync::mpsc::unbounded_channel();
    state.front = Some(window);
    let app = server::router(Arc::new(state));
    assert_eq!(send_to(&app, front()).await.0, StatusCode::NO_CONTENT);
    assert_eq!(asked.try_recv(), Ok(()));
}

/// A request from the extension: its origin, JSON.
fn extension_json(path: &str, json: &str) -> Request<Body> {
    Request::post(path)
        .header(header::HOST, HOST)
        .header(header::ORIGIN, EXTENSION)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json.to_string()))
        .unwrap()
}

#[tokio::test]
async fn the_extension_sends_the_post_the_browser_shows_to_a_search() {
    let db = db::open_in_memory().await.unwrap();
    let context = Arc::new(Context::new(
        db.clone(),
        TAXONOMY.clone(),
        KeyVault::with_key_in_memory(),
    ));
    let app = server::router(Arc::new(AppState::new(Arc::clone(&context), PORT)));
    let description = "Vi søger en erfaren PHP-udvikler til vores team i Odense. ".repeat(5);
    let html = format!(
        r#"<html><body><div class="job-details-jobs-unified-top-card__job-title"><h1>PHP-udvikler</h1></div><div id="job-details"><p>{description}</p></div></body></html>"#
    );
    let sent = serde_json::json!({
        "url": "https://www.linkedin.com/jobs/search/?keywords=php&currentJobId=4012345678",
        "html": html,
    })
    .to_string();

    let (status, answer) = send_to(&app, extension_json("/job_scan/submit_page", &sent)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{answer}");
    assert!(answer.contains("no job search to add the post to"), "{answer}");

    let search = SearchForm {
        name: "Backend".to_string(),
        keywords: "PHP".to_string(),
        tailor_from_score: "60".to_string(),
        active: "1".to_string(),
        ..SearchForm::new_search()
    };
    assert!(search_service::save(&context, 0, &search).await.is_ok());
    let (status, names) = send_to(&app, get("/job_scan/search_names")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&names).unwrap()["searches"],
        serde_json::json!([{"id": 1, "name": "Backend", "active": true}])
    );

    let (status, answer) = send_to(&app, extension_json("/job_scan/submit_page", &sent)).await;
    assert_eq!(status, StatusCode::OK, "{answer}");
    assert!(answer.contains("Sent."), "{answer}");
    let (url, title): (String, String) =
        sqlx::query_as("SELECT url, title FROM scanned_job_posts WHERE job_search_id = 1")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(url, "https://www.linkedin.com/jobs/view/4012345678/");
    assert_eq!(title, "PHP-udvikler");

    // The same post sent again is the one post.
    let (status, answer) = send_to(&app, extension_json("/job_scan/submit_page", &sent)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(answer.contains("has this post already"), "{answer}");

    // Only the extension and this app may send one.
    let request = Request::post("/job_scan/submit_page")
        .header(header::HOST, HOST)
        .header(header::ORIGIN, "https://example.com")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(sent))
        .unwrap();
    let (status, _) = send_to(&app, request).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}
