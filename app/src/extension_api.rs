//! What the Jobnet extension asks the app for, as JSON on 127.0.0.1 (server): the saved matches,
//! a match as a joblog entry and its PDFs, the joblog status it saved on Jobnet, the answers for
//! an employer's application form, and the job post its "Send to JobFinder" button sends. The work is in match_service and scan_service; errors
//! answer JSON {error} (api_error), which the extension shows.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::api_error::ApiError;
use crate::job_scan::{scan_service, search_service};
use crate::matches::application_delivery::rawurlencode;
use crate::matches::application_form;
use crate::matches::match_service;
use crate::server::AppState;

type Answer = Result<Response, ApiError>;

/// An id from the address; anything but a number is no match.
fn id_of(segment: &str) -> i64 {
    segment.parse().unwrap_or(0)
}

/// An id in a JSON body: a number, or a number as text.
fn id_in_body(id: &serde_json::Value) -> i64 {
    match id {
        serde_json::Value::Number(id) => id.as_i64().or_else(|| id.as_f64().map(|id| id as i64)).unwrap_or(0),
        serde_json::Value::String(id) => id_of(id.trim()),
        _ => 0,
    }
}

#[derive(Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    shortlist: bool,
}

/// GET match/history?shortlist=true gives the application picker the app's shortlist and order.
/// Without that flag: matches, the latest ones, newest first; not_joblogged,
/// the ones applied for but not in Jobnet's joblog yet (the list "Applied, not on Jobnet yet"),
/// best first, which the extension offers first.
pub async fn history(State(state): State<Arc<AppState>>, Query(query): Query<HistoryQuery>) -> Answer {
    if query.shortlist {
        let matches = match_service::application_shortlist(&state.context).await?;
        return Ok(Json(json!({ "matches": matches })).into_response());
    }
    let history = match_service::extension_history(&state.context).await?;
    Ok(Json(json!({ "matches": history.matches, "not_joblogged": history.not_joblogged })).into_response())
}

/// GET match/joblog/{id}: a saved match as an entry for Jobnet's joblog (joblog::read): its
/// fields in Jobnet's order, filled in from the match, and which of the three PDFs Jobnet keeps
/// can be downloaded.
pub async fn joblog(State(state): State<Arc<AppState>>, Path(id): Path<String>) -> Answer {
    let entry = match_service::joblog_entry(&state.context, id_of(&id)).await?;
    Ok(Json(entry).into_response())
}

/// joblog_status's body.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct JoblogStatusInput {
    id: serde_json::Value,
    status: serde_json::Value,
    logged: serde_json::Value,
}

/// POST match/joblog_status {id, status, logged}: how far the candidate is with the job in
/// Jobnet's words (joblog::SEARCH_STATUSES), and whether they have entered it in their Jobnet
/// joblog. Answers {logged, at}: whether it is in the joblog now, and since when.
pub async fn joblog_status(State(state): State<Arc<AppState>>, Json(input): Json<JoblogStatusInput>) -> Answer {
    // Anything but a known status's text is answered with the list of them.
    let status = input.status.as_str().unwrap_or("");
    let logged = input.logged == serde_json::Value::Bool(true);
    let logged = match_service::save_joblog_status(&state.context, id_in_body(&input.id), status, logged).await?;
    Ok(Json(json!({ "logged": logged.checked, "at": logged.at })).into_response())
}

/// pdf's body.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct PdfInput {
    id: serde_json::Value,
    kind: String,
    text: String,
    /// An earlier version of the application or résumé (documents), whose PDF is made as it was
    /// written instead.
    document_id: Option<i64>,
}

/// POST match/pdf {id, kind, text?, document_id?}: a saved match's application, résumé, CV or job
/// post as a PDF download (match_service::pdf), for Jobnet's joblog.
pub async fn pdf(State(state): State<Arc<AppState>>, Json(input): Json<PdfInput>) -> Answer {
    let made = match_service::pdf(
        &state.context,
        id_in_body(&input.id),
        &input.kind,
        &input.text,
        input.document_id,
    )
    .await?;
    Ok((
        [
            (header::CONTENT_TYPE, "application/pdf".to_string()),
            (header::CONTENT_DISPOSITION, content_disposition(&made.name)),
            (header::CACHE_CONTROL, "private, no-store".to_string()),
        ],
        made.bytes,
    )
        .into_response())
}

/// The download's Content-Disposition: the name in ASCII (each other byte and " as _) for old
/// clients, and in full as filename*=UTF-8''.
fn content_disposition(name: &str) -> String {
    let ascii: String = name
        .bytes()
        .map(|byte| {
            if (0x20..=0x7e).contains(&byte) && byte != b'"' {
                byte as char
            } else {
                '_'
            }
        })
        .collect();
    format!(
        "attachment; filename=\"{ascii}\"; filename*=UTF-8''{}",
        rawurlencode(name)
    )
}

/// application_form's body.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ApplicationFormInput {
    id: serde_json::Value,
    fields: Vec<application_form::FormField>,
}

/// POST match/application_form {id, fields}: what goes in each field of the employer's
/// application form the browser shows, from saved match id (match_service::application_form).
/// Answers {answers: [{key, value, file, from}], left: [{key, why}]}; the extension fetches the
/// PDFs named in file with match/pdf.
pub async fn application_form(State(state): State<Arc<AppState>>, Json(input): Json<ApplicationFormInput>) -> Answer {
    let answers = match_service::application_form(&state.context, id_in_body(&input.id), input.fields).await?;
    Ok(Json(answers).into_response())
}

/// applied's body.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct AppliedInput {
    id: serde_json::Value,
}

/// POST match/applied {id}: the candidate sent their application (on the employer's form, say):
/// the match is marked as applied for (match_service::mark_applied), so it shows under "Applied,
/// not on Jobnet yet". Answers {status, applied_on}.
pub async fn applied(State(state): State<Arc<AppState>>, Json(input): Json<AppliedInput>) -> Answer {
    let applied = match_service::mark_applied(&state.context, id_in_body(&input.id)).await?;
    Ok(Json(applied).into_response())
}

/// GET job_scan/search_names: the searches the extension can send a post to, as JSON
/// {searches: [{id, name, active}]}, active ones first.
pub async fn search_names(State(state): State<Arc<AppState>>) -> Answer {
    let searches = search_service::search_names(&state.context).await?;
    Ok(Json(json!({ "searches": searches })).into_response())
}

/// What the extension sends of the page the user's browser shows.
#[derive(Deserialize)]
pub struct SentPage {
    /// The search to add the post to; none for the newest active one.
    #[serde(default)]
    pub search_id: Option<i64>,
    pub url: String,
    /// The page's HTML as the browser shows it, its JavaScript run.
    pub html: String,
}

/// POST job_scan/submit_page {search_id?, url, html}: adds the job post the user's browser shows
/// to a search and matches it. Answers {said}.
pub async fn submit_page(State(state): State<Arc<AppState>>, Json(page): Json<SentPage>) -> Answer {
    let said = scan_service::add_page(Arc::clone(&state.context), page.search_id, &page.url, &page.html).await?;
    Ok(Json(json!({ "said": said })).into_response())
}

/// Every other address: not something the extension asks for.
pub async fn not_here() -> ApiError {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "The JobFinder app has no web pages: open it from its window.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_id_comes_as_a_number_or_as_text() {
        assert_eq!(id_in_body(&json!(21)), 21);
        assert_eq!(id_in_body(&json!(" 21 ")), 21);
        assert_eq!(id_in_body(&json!(null)), 0);
        assert_eq!(id_of("x"), 0);
    }

    #[test]
    fn a_downloads_name_is_ascii_and_in_full() {
        assert_eq!(
            content_disposition("Ansøgning \"PHP\".pdf"),
            "attachment; filename=\"Ans__gning _PHP_.pdf\"; filename*=UTF-8''Ans%C3%B8gning%20%22PHP%22.pdf"
        );
    }
}
