//! The AI match and writing, as the window calls them (match_service, account_service), on a
//! stand-in for a local model: what is read, judged, saved and written, and on which model.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock};

use jobfinder::account::account_model::{self, UseChoices};
use jobfinder::account::account_service;
use jobfinder::account::key_vault::KeyVault;
use jobfinder::context::Context;
use jobfinder::db;
use jobfinder::matches::document_box::{APPLICATION, DocumentBox, RESUME};
use jobfinder::matches::document_question::Question;
use jobfinder::matches::document_writer::{By, Comment, Revision, Said};
use jobfinder::matches::match_model;
use jobfinder::matches::match_service::{self, MatchRequest};
use jobfinder::service_error::ErrorKind;
use jobfinder::taxonomy::Taxonomy;
use serde_json::{Value, json};
use sqlx::{Executor, SqlitePool};

static TAXONOMY: LazyLock<Arc<Taxonomy>> = LazyLock::new(|| Arc::new(Taxonomy::load()));

/// A saved match with an email instruction and a résumé written as fields, and a poor one.
const SEED: &str = "
INSERT INTO matches (id, job_title, company, job_url, job_text, score, points, max_points, tag, created_at)
VALUES (21, 'PHP-udvikler', 'Firma <A/S>', 'https://www.jobindex.dk/job/1', 'Vi søger en PHP-udvikler.\nSend din ansøgning til job@firma.dk. Skriv \"Backend\" i emnefeltet.', 0.86, 9, 10, 'top', 1760000000),
       (22, 'Mekaniker', '', NULL, 'Mekaniker søges', 0.2, 1, 10, 'poor', 1760000100);
INSERT INTO documents (id, kind, name, text, written_at) VALUES (1, 'cv', 'cv', 'PHP', 1760000000);
INSERT INTO match_documents (match_id, document_id) VALUES (21, 1), (22, 1);
INSERT INTO match_items (match_id, criterion, text, verdict, decided_by, reason, sort_order)
VALUES (21, 'requirements', 'PHP', 'met', 'exact', 'Named in the CV.', 0);
INSERT INTO documents (id, kind, text, written_at) VALUES (5, 'resume', 'Mette Sørensen', 1760000200);
INSERT INTO match_documents (match_id, document_id) VALUES (21, 5);
INSERT INTO match_resumes (document_id, name, intro) VALUES (5, 'Mette Sørensen', 'Backend-udvikler.');
INSERT INTO match_resume_entries (id, document_id, section, title, sort_order) VALUES (1, 5, 'experience', 'Udvikler', 0);
INSERT INTO match_resume_lines (document_id, entry_id, kind, text, sort_order)
VALUES (5, 1, 'bullet', 'Byggede en API', 0), (5, NULL, 'skill', 'PHP', 1);
";

/// A stand-in for an OpenAI-compatible model on 127.0.0.1: it answers each kind of request by
/// the schema it asks for, and counts the requests.
async fn stand_in_model() -> (String, Arc<AtomicUsize>) {
    use axum::routing::post;
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = calls.clone();
    let app = axum::Router::new().route(
        "/v1/chat/completions",
        post(move |axum::Json(body): axum::Json<Value>| {
            let counted = counted.clone();
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                let required = &body["response_format"]["json_schema"]["schema"]["required"];
                let asks = |key: &str| required.as_array().unwrap().iter().any(|k| k == key);
                let user = body["messages"][1]["content"].as_str().unwrap_or("").to_string();
                let answer = if asks("entries") {
                    profile_answer()
                } else if asks("requirements") {
                    job_answer()
                } else if asks("experience_years") {
                    cv_answer()
                } else if asks("verdicts") {
                    judged(&user)
                } else if asks("change") {
                    asked_about(&user)
                } else if asks("application") && user.contains("This is a revision") {
                    json!({ "post_language": "Danish", "employer_needs": "x", "standout": [], "application": revised(&user) })
                } else if asks("application") {
                    json!({ "post_language": "Danish", "employer_needs": "x", "standout": [], "application": "Kære Firma\n\nJeg søger stillingen." })
                } else {
                    resume_answer()
                };
                axum::Json(json!({
                    "choices": [{ "finish_reason": "stop", "message": { "content": answer.to_string(), "refusal": null } }]
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = format!("http://{}/v1", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (address, calls)
}

/// A question about a passage: the answer names the passage it was asked about.
fn asked_about(user: &str) -> Value {
    let passage = user
        .split("The highlighted passage:\n\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\n").next())
        .unwrap_or("");
    json!({ "answer": format!("Nothing in the CV backs “{passage}”."), "change": "Say 9 years, never 16." })
}

/// A revision: the previous version with 16 changed to 9, when the comments ask for it.
fn revised(user: &str) -> String {
    let previous = user
        .split("as the candidate has it:\n\n")
        .nth(1)
        .and_then(|rest| rest.split("\n\nThe candidate's comments on it:").next())
        .unwrap_or("");
    if user.contains("- On “16 år”:\n  Candidate: I have 9 years") {
        previous.replace("16 år", "9 år")
    } else {
        previous.to_string()
    }
}

fn job_answer() -> Value {
    let requirement = |value: &str, kind: &str| json!({ "value": value, "english": value, "kind": kind, "required": true, "alt_group": "", "min_years": 0, "min_level": "" });
    json!({
        "text_en": "We seek a PHP developer who lives near Copenhagen.",
        "job_title": "PHP-udvikler",
        "job_title_en": "PHP developer",
        "company": "Firma A/S",
        "requirements": [requirement("PHP", "skill"), requirement("Erfaring med betalingsløsninger", "experience"), requirement("Bosat nær København", "location")],
        "responsibilities": ["Udvikle API'er"],
    })
}

fn profile_answer() -> Value {
    json!({
        "name": "Mette Sørensen", "email": "mette@example.com", "phone": "", "title": "Udvikler", "location": "Slagelse",
        "about": ["Backend-udvikler."],
        "entries": [{ "section": "experience", "title": "Udvikler", "organisation": "Firma", "location": "", "url": "",
                      "started": "2017-01", "ended": "", "summary": "", "bullets": ["Byggede betalingsløsninger"], "note": "" }],
        "skills": ["PHP"], "languages": [], "certificates": [],
    })
}

fn cv_answer() -> Value {
    json!({
        "name": "Mette Sørensen", "location": "Slagelse, 4200", "title": ["Udvikler"], "skill": ["PHP", "Laravel"],
        "language": ["Danish"], "certificate": [], "education": [], "experience_years": 8, "responsibilities": [],
        "text_en": "Mette Sørensen, developer. PHP, Laravel. Built payment flows.",
    })
}

/// Every requirement the user message lists ("- [id] (kind) text") as met.
fn judged(user: &str) -> Value {
    let unrelated = user.contains("earthworks");
    let evidence = if unrelated {
        "Led development in close collaboration with the CTO"
    } else {
        "Built payment flows"
    };
    let verdicts: Vec<Value> = user
        .lines()
        .filter_map(|line| line.strip_prefix("- [")?.split(']').next())
        .map(|id| json!({ "id": id, "verdict": "met", "evidence": evidence, "reason": "Shown in the CV." }))
        .collect();
    if unrelated {
        json!({
            "candidate_field": "software development",
            "job_field": "civil construction, earthworks and sewer",
            "role_field_fit": "unrelated",
            "verdicts": verdicts,
        })
    } else {
        json!({
            "candidate_field": "software development",
            "job_field": "software development",
            "role_field_fit": "same",
            "verdicts": verdicts,
        })
    }
}

fn resume_answer() -> Value {
    json!({
        "post_language": "Danish", "employer_needs": "x", "standout": [],
        "keywords": [{ "term": "PHP", "basis": "8 years of PHP" }],
        "left_out": ["Go"],
        "resume": {
            "name": "Mette Sørensen", "title": "PHP-udvikler", "location": "Slagelse", "phone": "", "email": "", "links": [],
            "intro": ["Backend-udvikler med 8 års PHP."],
            "experience": [{ "title": "Udvikler", "organisation": "Firma", "location": "", "starts": "2017", "ends": "nu", "summary": "", "bullets": ["Byggede betalingsløsninger"], "note": "" }],
            "projects": [], "skills": ["PHP"], "languages": ["Dansk"], "education_note": "", "education": [],
            "experience_heading": "Erfaring", "skills_heading": "Kompetencer", "education_heading": "Uddannelse",
            "languages_heading": "Sprog", "projects_heading": "",
        },
    })
}

/// The work on the seeded database with a local model saved as the user's key, and the model's
/// count of requests.
async fn with_model() -> (Arc<Context>, SqlitePool, Arc<AtomicUsize>) {
    let db = db::open_in_memory().await.unwrap();
    db.execute(sqlx::raw_sql(SEED)).await.unwrap();
    let vault = KeyVault::with_key_in_memory();
    let (base_url, calls) = stand_in_model().await;
    account_model::save(&db, &vault, "local", "stand-in", &base_url, "")
        .await
        .unwrap();
    let context = Context::new(db.clone(), TAXONOMY.clone(), vault);
    (Arc::new(context), db, calls)
}

/// The box of kind among boxes.
fn box_of<'a>(boxes: &'a [DocumentBox], kind: &str) -> &'a DocumentBox {
    boxes.iter().find(|document| document.kind == kind).unwrap()
}

async fn write(context: &Arc<Context>, notes: &str, kind: &'static str) -> match_service::WriteAnswer {
    match_service::write_document(Arc::clone(context), 21, notes, kind, None, &|_| {})
        .await
        .unwrap()
}

#[tokio::test]
async fn the_ai_match_reads_judges_scores_and_saves() {
    let (context, db, _) = with_model().await;
    let job = match_service::extract_job(&context, "PHP-udvikler søges")
        .await
        .unwrap();
    assert_eq!(job.requirements[1].kind, "experience");
    let profile = match_service::extract_cv(&context, "Mette Sørensen").await.unwrap();
    assert_eq!(profile.location, "Slagelse, 4200");

    let request = MatchRequest {
        job,
        profile,
        job_text: "PHP-udvikler søges".to_string(),
        cv_text: "Mette Sørensen, PHP".to_string(),
        ..MatchRequest::default()
    };
    let found = match_service::ai_match(&context, &request).await.unwrap();
    let id = found.saved_id.expect("saved");
    assert!(found.result.index > 0.0);
    let result = match_service::ai_match_result(&context, &request, &found)
        .await
        .unwrap();
    let documents = result.documents.expect("a saved match's documents");
    assert!(documents.boxes.iter().any(|document| document.kind == APPLICATION));
    let saved = match_model::find(&db, id).await.unwrap().unwrap();
    assert_eq!(saved.cv_name, "Mette Sørensen", "the CV's own name");
    assert_eq!(saved.company, "Firma A/S");
    let judged: Vec<&str> = saved.items.iter().map(|item| item.decided_by.as_str()).collect();
    assert!(judged.contains(&"llm"), "{judged:?}");
}

#[tokio::test]
async fn writing_again_keeps_the_version_before_in_its_list() {
    let (context, ..) = with_model().await;
    let first = write(&context, "kort", APPLICATION).await;
    let application = box_of(&first.boxes, APPLICATION);
    assert_eq!(application.text, "Kære Firma\n\nJeg søger stillingen.");
    assert!(application.earlier.is_empty());
    let second = write(&context, "", APPLICATION).await;
    let earlier = &box_of(&second.boxes, APPLICATION).earlier;
    assert_eq!(earlier.len(), 1);
    assert_eq!(earlier[0].written_by, "local/stand-in");
    assert_eq!(earlier[0].notes, "kort");

    let saved = match_service::saved_match(&context, 21).await.unwrap();
    assert_eq!(
        saved.application_text.as_deref(),
        Some("Kære Firma\n\nJeg søger stillingen.")
    );
    assert!(saved.application_written_at.is_some());
    let pdf = match_service::pdf(&context, 21, APPLICATION, "", Some(earlier[0].id))
        .await
        .unwrap();
    assert!(pdf.bytes.starts_with(b"%PDF-"), "an earlier version's PDF");
    let other_kind = match_service::pdf(&context, 21, RESUME, "", Some(earlier[0].id))
        .await
        .unwrap_err();
    assert_eq!(other_kind.kind, ErrorKind::NotFound, "a version is only its own kind's");
}

#[tokio::test]
async fn a_task_runs_on_the_model_picked_for_it() {
    let (context, ..) = with_model().await;
    let pick = |model: &str| vec![("application".to_string(), model.to_string())];
    let refused = account_service::save_task_models(&context, &pick("writer model"))
        .await
        .unwrap_err();
    assert!(
        refused
            .message
            .contains("Writing job applications: A model name is letters"),
        "{}",
        refused.message
    );
    account_service::save_task_models(&context, &pick("writer-model"))
        .await
        .unwrap();
    write(&context, "", APPLICATION).await;
    let again = write(&context, "", APPLICATION).await;
    assert_eq!(
        box_of(&again.boxes, APPLICATION).earlier[0].written_by,
        "local/writer-model",
        "the document names the model it ran on"
    );
}

#[tokio::test]
async fn a_tailored_resume_comes_back_with_its_keywords_and_compares() {
    let (context, db, _) = with_model().await;
    let answer = write(&context, "", RESUME).await;
    let resume = box_of(&answer.boxes, RESUME);
    let keywords = resume.keywords.as_ref().expect("the keywords it took over");
    assert_eq!(keywords.summary, "Uses 1 of the post's keywords: PHP");
    assert!(
        resume.left_out.contains("Left out, since your CV doesn"),
        "{}",
        resume.left_out
    );
    assert!(resume.structured);
    assert_eq!(resume.earlier.len(), 1, "the seeded résumé stays");
    let fields = match_model::resume_fields(&db, 21).await.unwrap().unwrap();
    assert_eq!(fields.experience_heading, "Erfaring");
    assert_eq!(
        fields.projects_heading, "Projects",
        "an empty heading is the English one"
    );

    let comparison = match_service::resume_comparison(&context, 21, 22).await.unwrap();
    assert_eq!(
        comparison.line,
        "The tailored résumé scores 20% poor against this post; your CV scored 86% top."
    );
    assert_eq!(comparison.open_id, 22);
}

#[tokio::test]
async fn a_key_switched_off_for_a_use_says_so() {
    let (context, db, calls) = with_model().await;
    let uses = UseChoices {
        resume: Some("local".to_string()),
        application: None,
        job_evaluation: None,
    };
    account_model::save_uses(&db, &uses).await.unwrap();
    let refused = match_service::write_document(Arc::clone(&context), 21, "", APPLICATION, None, &|_| {})
        .await
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::NeedKey);
    assert_eq!(
        refused.message,
        "None of your API keys is picked for writing job applications. Pick one on the API key page."
    );
    // Evaluating job posts off: the free reader reads, and the match is still saved.
    let job = match_service::extract_job(&context, "PHP-udvikler\nDin profil\n- Erfaring med PHP")
        .await
        .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 0, "no model call");
    let request = MatchRequest {
        job,
        profile: jobfinder::matches::Profile {
            skill: vec!["PHP".to_string()],
            ..Default::default()
        },
        job_text: "PHP-udvikler".to_string(),
        cv_text: "PHP".to_string(),
        ..MatchRequest::default()
    };
    let found = match_service::ai_match(&context, &request).await.unwrap();
    assert!(found.saved_id.is_some());
    let result = match_service::saved_result(&context, 21).await.unwrap();
    let documents = result.documents.expect("a saved match's documents");
    assert_eq!(
        box_of(&documents.boxes, APPLICATION).off_note,
        "Your API key isn't on for writing the application."
    );
}

#[tokio::test]
async fn a_question_about_a_highlighted_passage_is_answered_with_a_comment() {
    let (context, _db, _calls) = with_model().await;
    let question = Question {
        kind: APPLICATION,
        document: "Kære Firma\n\nJeg har arbejdet med software i 16 år.".to_string(),
        comment: Comment {
            passage: "16 år".to_string(),
            said: vec![said(By::Candidate, "Where does this come from?")],
        },
        notes: String::new(),
    };
    let answer = match_service::ask_about(&context, 21, &question).await.unwrap();
    assert_eq!(answer.answer, "Nothing in the CV backs “16 år”.");
    assert_eq!(
        answer.said(),
        "Nothing in the CV backs “16 år”.\nSuggested change: Say 9 years, never 16."
    );
    let nothing_highlighted = Question {
        comment: Comment {
            passage: " ".to_string(),
            ..question.comment.clone()
        },
        ..question
    };
    let refused = match_service::ask_about(&context, 21, &nothing_highlighted)
        .await
        .unwrap_err();
    assert_eq!(refused.kind, ErrorKind::Invalid);
}

#[tokio::test]
async fn a_revision_rewrites_the_version_on_its_comments_and_keeps_them_in_its_notes() {
    let (context, db, _calls) = with_model().await;
    let revision = Revision {
        previous: "Kære Firma\n\nJeg har arbejdet med software i 16 år.".to_string(),
        comments: vec![Comment {
            passage: "16 år".to_string(),
            said: vec![
                said(By::Candidate, "I have 9 years, hired in 2017."),
                said(By::Writer, "Then it should say 9."),
            ],
        }],
    };
    let answer = match_service::write_document(Arc::clone(&context), 21, "Kort.", APPLICATION, Some(revision), &|_| {})
        .await
        .unwrap();
    assert_eq!(
        box_of(&answer.boxes, APPLICATION).text,
        "Kære Firma\n\nJeg har arbejdet med software i 9 år."
    );
    let notes: String =
        sqlx::query_scalar("SELECT notes FROM documents WHERE kind = 'application' ORDER BY id DESC LIMIT 1")
            .fetch_one(&db)
            .await
            .unwrap();
    assert_eq!(
        notes,
        "Kort.\nRevised on these comments:\n- On “16 år”:\n  Candidate: I have 9 years, hired in 2017.\n  Writer: Then it should say 9."
    );
}

fn said(by: By, text: &str) -> Said {
    Said {
        by,
        text: text.to_string(),
    }
}

#[tokio::test]
async fn an_unrelated_field_leadership_role_never_matches_a_tech_lead() {
    let (context, ..) = with_model().await;
    let cv_text = "Tech Lead\nLed development in close collaboration with the CTO, incorporating regular feedback and code review\nKørekort B";
    let profile = jobfinder::matches::Profile {
        title: vec!["Tech Lead".into(), "Team Lead".into()],
        skill: vec!["PHP".into(), "Kørekort B".into()],
        language: vec!["Danish".into()],
        ..Default::default()
    };

    // Case 1: The exact teaser-ad scenario (only title + working language).
    let teaser = MatchRequest {
        job: jobfinder::matches::Job {
            job_title: "Projektleder til jord/kloak og anlæg".into(),
            job_title_en: "Project manager for earthworks, sewer and civil engineering".into(),
            company: "Jysk Erhverv & Råhus A/S".into(),
            field: "civil construction, earthworks and sewer".into(),
            working_language: "Danish".into(),
            ..Default::default()
        },
        profile: profile.clone(),
        job_text: "Projektleder til jord/kloak og anlæg".into(),
        cv_text: cv_text.into(),
        ..Default::default()
    };
    let found = match_service::ai_match(&context, &teaser).await.unwrap();
    assert_eq!(found.verdicts["t0"].verdict, "missing");
    assert_eq!(found.result.index, 0.0);
    assert_eq!(found.result.tag, "poor");

    // Case 2: Generic title ("Teamleder") that taxonomy shares with "Team Lead", plus a generic
    // requirement ("Kørekort B"): the title still goes to the model and the unrelated field caps
    // the score below medium.
    let generic = MatchRequest {
        job: jobfinder::matches::Job {
            job_title: "Teamleder".into(),
            job_title_en: "Team leader".into(),
            company: "Jysk Erhverv & Råhus A/S".into(),
            field: "civil construction, earthworks and sewer".into(),
            requirements: vec![jobfinder::matches::Requirement {
                value: "Kørekort B".into(),
                english: "Driving license B".into(),
                kind: "skill".into(),
                required: true,
                ..Default::default()
            }],
            ..Default::default()
        },
        profile,
        job_text: "Teamleder til jord/kloak og anlæg".into(),
        cv_text: cv_text.into(),
        ..Default::default()
    };
    let capped = match_service::ai_match(&context, &generic).await.unwrap();
    assert_eq!(capped.verdicts["t0"].by, "llm");
    assert_eq!(capped.verdicts["t0"].verdict, "missing");
    assert_eq!(capped.result.index, jobfinder::matches::matcher::DISQUALIFIED_INDEX);
    assert_eq!(capped.result.tag, "poor");
    assert!(
        capped
            .result
            .feedback_note
            .as_deref()
            .unwrap_or("")
            .contains("different field")
    );
}
