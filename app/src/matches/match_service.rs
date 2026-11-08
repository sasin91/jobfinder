//! The match page's work, for whichever front end asks: the web pages (pages.rs) today, a desktop
//! window later. Each function takes the app's shared state (Context) and plain inputs, and
//! answers a view model or data, or a ServiceError to show. Nothing here knows about HTTP: the
//! pages read the request, call one of these, and print what comes back.

use std::sync::Arc;

use super::ai_match::{self, AiMatch, Matching};
use super::application_delivery::ApplicationDelivery;
use super::application_form;
use super::cv_links::CvLink;
use super::document_box::{self, DocumentBox, RESUME};
use super::document_question::{self, Answer as QuestionAnswer, Question};
use super::document_writer::{self, Revision, WriteError};
use super::free_reader::{FreeReader, YearMonth};
use super::joblog::{self, Entry};
use super::joblog_panel::{JoblogPanel, Logged};
use super::llm_key;
use super::match_documents::{self, MatchDocuments};
use super::match_model::{self, JOBNET_JOBLOG, LISTS, ListedMatch, NewMatch, SHORTLIST, SavedMatch, SortOrder};
use super::match_result::{Fresh, MatchResult};
use super::matcher::{self, Matcher, Verdict, Verdicts};
use super::page_reader::{self, Page};
use super::resume_comparison::ResumeComparison;
use super::saved_match_row::{SavedMatchRow, TAILORED_CV};
use super::{Job, Profile, apply_instructions, match_prompts, pdf_writer, reject_reason, seniority, taxonomy_lessons};
use crate::account::account_model::JOB_EVALUATION;
use crate::account::task_model;
use crate::context::Context;
use crate::laya::laya_client;
use crate::pdf_text::child;
use crate::profile;
use crate::service_error::{ErrorKind, ServiceError};
use crate::task_manager::running_work::{Stopped, WorkKind};
use crate::taxonomy::Taxonomy;
use crate::taxonomy_store;

/// Longest CV or job post the free match reads, and longest text made into a PDF, in characters.
pub const MAX_TEXT: usize = 40_000;

/// Saved matches a list gives at a time; the next ones come after the last one's cursor.
const PER_PAGE: usize = 25;

/// Most pages fetch_job reads in FETCH_WINDOW_SECONDS: a batch of links a few times over, not a
/// crawler.
pub const MAX_FETCHES: usize = 30;
/// The window fetches are counted in, in seconds.
pub const FETCH_WINDOW_SECONDS: i64 = 600;

/// What the free match says of a requirement the taxonomy didn't find in the CV.
const NOT_FOUND_REASON: &str = "Not named in the CV. The AI match also looks for related experience.";

/// The extension's history: the latest matches, and the ones to log on Jobnet first.
const HISTORY_SIZE: usize = 50;

/// A batch of a list's saved matches.
pub struct SavedMatchList {
    pub matches: Vec<SavedMatchRow>,
    /// The cursor of the next batch, when there is one.
    pub next_after: Option<String>,
}

/// A batch of the list named list (the shortlist when None or unknown), after the cursor after
/// ("" for the first batch).
pub async fn saved_matches(context: &Context, list: Option<&str>, after: &str) -> Result<SavedMatchList, ServiceError> {
    sorted_saved_matches(context, list, after, SortOrder::Best).await
}

/// A batch in the selected order, after a cursor from the same order.
pub async fn sorted_saved_matches(
    context: &Context,
    list: Option<&str>,
    after: &str,
    sort: SortOrder,
) -> Result<SavedMatchList, ServiceError> {
    let list = LISTS
        .iter()
        .map(|(name, ..)| *name)
        .find(|name| list == Some(*name))
        .unwrap_or(SHORTLIST);
    let found = match_model::list_matches(context.db(), PER_PAGE, after, list, sort).await?;
    Ok(SavedMatchList {
        matches: found.rows.iter().map(SavedMatchRow::from).collect(),
        next_after: found.next,
    })
}

/// One of the user's saved matches by its id; ids below 1 are no match.
pub async fn saved_match(context: &Context, id: i64) -> Result<SavedMatch, ServiceError> {
    if id <= 0 {
        return Err(ServiceError::not_found());
    }
    match_model::find(context.db(), id)
        .await?
        .ok_or_else(ServiceError::not_found)
}

/// A saved match as its result shows it, with its application and résumé panel.
pub async fn saved_result(context: &Context, id: i64) -> Result<MatchResult, ServiceError> {
    let found = saved_match(context, id).await?;
    let uses = llm_key::ai_uses(context).await?;
    let documents = MatchDocuments::from(
        Some(found.id),
        "",
        &match_documents::written_of(&found),
        uses,
        found.apply_instructions.as_ref(),
    );
    Ok(MatchResult::from_saved(&found, true, documents))
}

/// The free match: both texts read with the free reader (no model), matched with the taxonomy
/// and scored like the full match; what the taxonomy can't find in the CV counts as missing.
/// Nothing is saved. With Laya's model, the result has its answer on these verdicts. job_url:
/// where the post was read from, empty for a pasted post.
pub async fn quick_match(
    context: &Context,
    job_text: &str,
    job_url: &str,
    cv_text: &str,
) -> Result<MatchResult, ServiceError> {
    let job_text = job_text.trim();
    let cv_text = cv_text.trim();
    if job_text.is_empty() || cv_text.is_empty() {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Paste or upload both the CV and the job post.",
        ));
    }
    if job_text.chars().count() > MAX_TEXT || cv_text.chars().count() > MAX_TEXT {
        return Err(ServiceError::new(
            ErrorKind::TooLarge,
            "That text is too long. Paste only the CV and the job post.",
        ));
    }
    let taxonomy = context.taxonomy();
    let taxonomy = taxonomy.as_ref();
    let mut reader = FreeReader::new(taxonomy, this_month());
    let job = seniority::with_level(reader.job(job_text));
    let profile = reader.cv(cv_text);
    let matcher = Matcher::new(taxonomy, profile.clone(), reader.known.clone());
    let groups = matcher::criteria_for_post(&job, job_text);
    let mut verdicts = Verdicts::new();
    for unit in matcher::units(&groups) {
        let verdict = matcher.decide(unit).unwrap_or_else(not_found_verdict);
        verdicts.insert(unit.id.clone(), verdict);
    }
    let verdicts = matcher::combine(&groups, &verdicts);
    if groups
        .iter()
        .all(|(criterion, items)| criterion == matcher::LOCATION || items.is_empty())
    {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "The free match found no skills it knows in this post. The AI match reads requirements written as sentences too.",
        ));
    }
    let score = matcher::score(&groups, &verdicts);
    let apply = apply_instructions::read(job_text);
    let laya = laya_client::reading(context.laya.as_ref(), &matcher::laya_summary(&job, &groups, &verdicts)).await;
    let fresh = Fresh {
        job: &job,
        groups: &groups,
        verdicts: &verdicts,
        score: &score,
        soft_skills: matcher::soft_skills(&job),
        apply_instructions: apply.as_ref(),
        saved_id: None,
        laya: &laya,
        job_url,
    };
    let writer = llm_key::ai_uses(context).await?.any();
    Ok(MatchResult::from(&fresh, writer, None, "").with_cv_found(&profile))
}

/// The free match's verdict on a requirement the taxonomy didn't find in the CV.
fn not_found_verdict() -> Verdict {
    Verdict {
        verdict: "missing".to_string(),
        by: "taxonomy".to_string(),
        method: "not_found".to_string(),
        evidence: String::new(),
        reason: NOT_FOUND_REASON.to_string(),
    }
}

/// This month, which the free reader counts "until now" to.
fn this_month() -> YearMonth {
    use chrono::Datelike;
    let today = chrono::Local::now().date_naive();
    YearMonth {
        year: i64::from(today.year()),
        month: i64::from(today.month()),
    }
}

/// What a reject or its undo did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RejectOutcome {
    /// Done, with what to tell the user; None when the match was already where it was asked to be.
    Done(Option<&'static str>),
    /// Not done: what is missing from the reasons, to show back on the match.
    Unfinished(String),
}

/// Rejects a saved match (not interested) for reasons, with notes per reason, so it leaves every
/// list but Rejected; undo brings it back instead.
pub async fn reject(
    context: &Context,
    id: i64,
    reasons: &[String],
    notes: &[(String, String)],
    undo: bool,
) -> Result<RejectOutcome, ServiceError> {
    if !undo {
        let problem = reject_reason::problem(reasons, notes);
        if !problem.is_empty() {
            return Ok(RejectOutcome::Unfinished(problem));
        }
    }
    let done = if undo {
        match_model::bring_back(context.db(), id).await?
    } else {
        let now = chrono::Utc::now().timestamp();
        match_model::reject(context.db(), id, &reject_reason::rows(reasons, notes), now).await?
    };
    if done {
        // Its found post, if the job scan found it, says the same.
        crate::job_scan::job_scan_model::follow_match(context.db(), id, undo).await?;
    }
    let message = match (done, undo) {
        (false, _) => None,
        (true, true) => Some("The match is back in your lists."),
        (true, false) => Some("Rejected. It is in the Rejected list if you change your mind."),
    };
    Ok(RejectOutcome::Done(message))
}

/// The saved matches for the Jobnet extension.
pub struct ExtensionHistory {
    /// The latest ones, newest first.
    pub matches: Vec<ListedMatch>,
    /// The ones applied for but not in Jobnet's joblog yet ("Applied, not on Jobnet yet"), best
    /// first, which the extension offers first.
    pub not_joblogged: Vec<ListedMatch>,
}

pub async fn application_shortlist(context: &Context) -> Result<Vec<ListedMatch>, ServiceError> {
    Ok(match_model::shortlist(context.db(), context.saved_match_sort()).await?)
}

pub async fn extension_history(context: &Context) -> Result<ExtensionHistory, ServiceError> {
    let db = context.db();
    Ok(ExtensionHistory {
        matches: match_model::recent(db, HISTORY_SIZE).await?,
        not_joblogged: match_model::best_first(db, HISTORY_SIZE, "", "not_joblogged")
            .await?
            .rows,
    })
}

/// A saved match as an entry for Jobnet's joblog (joblog::read).
pub async fn joblog_entry(context: &Context, id: i64) -> Result<Entry, ServiceError> {
    Ok(joblog::read(&saved_match(context, id).await?))
}

/// The same entry as the match prints it under the result.
pub async fn joblog_panel(context: &Context, id: i64) -> Result<JoblogPanel, ServiceError> {
    let found = saved_match(context, id).await?;
    Ok(JoblogPanel::from(found.id, &joblog::read(&found)))
}

/// Saves how far the candidate is with the job in Jobnet's words (joblog::SEARCH_STATUSES), and
/// whether they entered it in their Jobnet joblog; answers the "In your Jobnet joblog" section.
pub async fn save_joblog_status(
    context: &Context,
    id: i64,
    status: &str,
    logged: bool,
) -> Result<Logged, ServiceError> {
    let found = saved_match(context, id).await?;
    if joblog::search_status(status).is_none() {
        let statuses: Vec<&str> = joblog::SEARCH_STATUSES.iter().map(|(_, text)| *text).collect();
        return Err(ServiceError::new(
            ErrorKind::Malformed,
            format!("Pick one of Jobnet's statuses: {}.", statuses.join(", ")),
        ));
    }
    let now = chrono::Utc::now().timestamp();
    let saved = match_model::save_integration(context.db(), found.id, JOBNET_JOBLOG, status, logged, now).await?;
    Ok(Logged::from(
        saved.as_ref().and_then(|saved| saved.logged_at),
        joblog::applied_on(saved.as_ref()),
    ))
}

/// A match marked as applied for: its status in Jobnet's words ("Søgt") and the day it was
/// applied for, dd-mm-yyyy.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Applied {
    pub status: String,
    pub applied_on: String,
}

/// Marks saved match id as applied for: Søgt, unless it is further along already (Samtale),
/// keeping whether it is in the Jobnet joblog. Applied for once, it keeps the first day.
pub async fn mark_applied(context: &Context, id: i64) -> Result<Applied, ServiceError> {
    let found = saved_match(context, id).await?;
    let saved = found.jobnet_joblog.as_ref();
    let status = saved
        .map(|saved| saved.status.as_str())
        .filter(|status| match_model::APPLIED_STATUSES.contains(status))
        .unwrap_or(joblog::APPLIED_STATUS);
    let logged = saved.is_some_and(|saved| saved.logged_at.is_some());
    let now = chrono::Utc::now().timestamp();
    let saved = match_model::save_integration(context.db(), found.id, JOBNET_JOBLOG, status, logged, now).await?;
    Ok(Applied {
        status: joblog::search_status(status).unwrap_or(status).to_string(),
        applied_on: joblog::applied_on(saved.as_ref()),
    })
}

/// Copy as prompt: saved match id as a prompt for a chatbot of the candidate's own
/// (document_writer::chat_prompt), with the application and résumé as shown (empty when not
/// written).
pub async fn chat_prompt(context: &Context, id: i64, application: &str, resume: &str) -> Result<String, ServiceError> {
    let found = saved_match(context, id).await?;
    Ok(document_writer::chat_prompt(context.db(), &found, application, resume).await?)
}

/// A PDF of a saved match's document, named for the job.
#[derive(Debug, Clone)]
pub struct PdfFile {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// A saved match's job application (kind application), tailored résumé (resume), the CV it was
/// matched with (cv) or its job post (job) as a PDF (pdf_writer). A résumé with fields
/// (tailored_resume) is laid out from them. Otherwise edited is the text as the user sees it,
/// edits included; without it the saved text is used; the CV and the job post are always the
/// saved text. document_id, an earlier version, is its text (and fields) as written.
pub async fn pdf(
    context: &Context,
    id: i64,
    kind: &str,
    edited: &str,
    document_id: Option<i64>,
) -> Result<PdfFile, ServiceError> {
    if !pdf_writer::KINDS.contains(&kind) {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Choose the application, the résumé, the CV or the job post.",
        ));
    }
    let found = saved_match(context, id).await?;
    let version = match document_id {
        Some(document_id) => Some(
            match_model::version(context.db(), found.id, document_id)
                .await?
                .filter(|version| version.kind == kind)
                .ok_or_else(|| ServiceError::new(ErrorKind::NotFound, "That version was not found."))?,
        ),
        None => None,
    };
    let text = match &version {
        Some(version) => version.text.trim().to_string(),
        None => pdf_text(&found, kind, edited.trim()),
    };
    if text.is_empty() {
        return Err(ServiceError::new(ErrorKind::Invalid, "There is no text to export yet."));
    }
    if text.chars().count() > MAX_TEXT {
        return Err(ServiceError::new(
            ErrorKind::TooLarge,
            "That text is too long for a PDF.",
        ));
    }
    let title = pdf_title(&found, kind);
    // A résumé written with fields is laid out from them; the text is only for one written
    // before they existed.
    let fields = match (&version, kind) {
        (Some(version), RESUME) => match_model::resume_fields_of(context.db(), version.id).await?,
        (None, RESUME) => match_model::resume_fields(context.db(), found.id).await?,
        _ => None,
    };
    let made = match &fields {
        Some(fields) => pdf_writer::resume_pdf(fields, &title),
        None => pdf_writer::pdf(kind, &text, &title),
    };
    let bytes = made.map_err(|e| ServiceError::internal("match/pdf", format!("The PDF couldn't be made: {e}")))?;
    Ok(PdfFile {
        name: pdf_writer::file_name(kind, &found.job_title, &found.company),
        bytes,
    })
}

/// The text a PDF is made from: what the user edited, or the saved text when there's none; the
/// CV and the job post are always the saved text, the post under its link.
fn pdf_text(found: &SavedMatch, kind: &str, edited: &str) -> String {
    let saved = |text: Option<&str>| text.unwrap_or("").trim().to_string();
    let text = if edited.is_empty() || kind == "cv" || kind == "job" {
        match kind {
            "application" => saved(found.application_text.as_deref()),
            "resume" => saved(found.resume_text.as_deref()),
            "cv" => saved(Some(&found.cv_text)),
            _ => saved(Some(&found.job_text)),
        }
    } else {
        edited.to_string()
    };
    let job_url = found.job_url.as_deref().unwrap_or("");
    if kind == "job" && !text.is_empty() && !job_url.is_empty() {
        format!("{job_url}\n\n{text}")
    } else {
        text
    }
}

/// The PDF's title: "Job application: Acme", "Résumé: Acme", "CV: Acme", or the job post's
/// "Title · Company".
fn pdf_title(found: &SavedMatch, kind: &str) -> String {
    if kind == "job" {
        let title: Vec<&str> = [found.job_title.as_str(), found.company.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect();
        return if title.is_empty() {
            "Job post".to_string()
        } else {
            title.join(" · ")
        };
    }
    let label = match kind {
        "application" => "Job application",
        "resume" => "Résumé",
        _ => "CV",
    };
    if found.company.is_empty() {
        label.to_string()
    } else {
        format!("{label}: {}", found.company)
    }
}

/// The newest saved match of this CV with the same job post text, if any, so a post matched
/// before can be skipped.
pub async fn already_matched(context: &Context, cv_text: &str, job_text: &str) -> Result<Option<i64>, ServiceError> {
    Ok(match_model::existing(context.db(), cv_text.trim(), job_text.trim()).await?)
}

/// The job post at a pasted link (page_reader). Fetches are counted (MAX_FETCHES in
/// FETCH_WINDOW_SECONDS), so a batch of links can't become a crawler.
pub async fn fetch_job(context: &Context, url: &str) -> Result<Page, ServiceError> {
    if let Err(minutes) = context.count_fetch(chrono::Utc::now().timestamp()) {
        let plural = if minutes == 1 { "" } else { "s" };
        return Err(ServiceError::new(
            ErrorKind::TooMany,
            format!(
                "That's a lot of links in a short time. Try again in {minutes} minute{plural}, or paste the post text."
            ),
        ));
    }
    page_reader::read(url, context.readers()).await.map_err(|why| {
        crate::log_line!("match (fetch_job): {url}: {why}");
        ServiceError::new(ErrorKind::Invalid, why)
    })
}

/// The biggest PDF read for its text. A CV is rarely over a few MB; this leaves room for one
/// with photos while keeping a wrong file from being held in memory.
pub const MAX_PDF_BYTES: usize = 20 * 1024 * 1024;

/// An uploaded PDF's text (a CV, or a job post saved as PDF), read by the app's PDF reader
/// (pdf_text) in a child process, so a file that breaks the reader ends only that process. A PDF
/// that holds no text, as a scan, is said so: its text must be pasted.
pub async fn read_pdf(context: &Context, bytes: Vec<u8>) -> Result<String, ServiceError> {
    if bytes.len() > MAX_PDF_BYTES {
        return Err(ServiceError::new(
            ErrorKind::TooLarge,
            format!("The PDF is over {} MB.", MAX_PDF_BYTES / (1024 * 1024)),
        ));
    }
    let Some(reader) = &context.pdf_reader else {
        return Err(ServiceError::internal(
            "match/pdf_text",
            "The app couldn't find its own program to read the PDF with.",
        ));
    };
    match child::text_of(reader, bytes).await {
        Ok(text) if text.is_empty() => Err(ServiceError::new(
            ErrorKind::Invalid,
            "The PDF has no text to read; it may be a scan. Paste its text instead.",
        )),
        Ok(text) => Ok(text),
        Err(child::Failed::Unreadable(why)) => {
            crate::log_line!("match/pdf_text: {why}");
            Err(ServiceError::new(
                ErrorKind::Invalid,
                format!("The PDF couldn't be read ({why}). Paste its text instead."),
            ))
        }
        Err(child::Failed::Stopped(why)) => {
            crate::log_line!("match/pdf_text: {why}");
            Err(ServiceError::new(
                ErrorKind::Invalid,
                format!("The PDF couldn't be read: {why}. Paste its text instead."),
            ))
        }
    }
}

/// The job post as structured requirements, read by the model, or by the free reader when the
/// key is off for evaluating job posts.
pub async fn extract_job(context: &Context, text: &str) -> Result<Job, ServiceError> {
    if llm_key::llm_settings(context, JOB_EVALUATION).await?.is_none() {
        return Ok(seniority::with_level(
            FreeReader::new(&context.taxonomy(), this_month()).job(text),
        ));
    }
    let keyed = llm_key::llm(context, &task_model::READ_JOB).await?;
    match_prompts::read_job(&keyed.llm, text)
        .await
        .map(seniority::with_level)
        .map_err(|e| ServiceError::llm("match/extract_job", &e))
}

/// The CV as a structured profile, read by the model, or by the free reader when the key is off
/// for evaluating job posts.
pub async fn extract_cv(context: &Context, text: &str) -> Result<Profile, ServiceError> {
    if llm_key::llm_settings(context, JOB_EVALUATION).await?.is_none() {
        // The free reader takes the stated years itself, for quick_match too.
        return Ok(FreeReader::new(&context.taxonomy(), this_month()).cv(text));
    }
    let keyed = llm_key::llm(context, &task_model::READ_CV).await?;
    let read = match_prompts::read_cv(&keyed.llm, text)
        .await
        .map_err(|e| ServiceError::llm("match/extract_cv", &e))?;
    Ok(with_stated_years(read, text))
}

/// The model's read of the CV with the years of work its text states (the profile's line, which
/// the candidate confirms on the setup screen) in place of the years the model counted, when it
/// states them.
fn with_stated_years(mut read: Profile, text: &str) -> Profile {
    if let Some(years) = profile::stated_years_of_work(text) {
        read.experience_years = years;
    }
    read
}

/// A read job post and a read CV, with their texts, to match.
#[derive(Debug, Default, Clone)]
pub struct MatchRequest {
    pub job: Job,
    pub profile: Profile,
    pub job_text: String,
    pub job_url: String,
    pub cv_name: String,
    pub cv_text: String,
}

/// The match of a read job post with a read CV (ai_match), saved when both texts are given.
pub async fn ai_match(context: &Context, request: &MatchRequest) -> Result<AiMatch, ServiceError> {
    let keyed = match llm_key::llm_settings(context, JOB_EVALUATION).await? {
        Some(_) => Some(llm_key::llm(context, &task_model::JUDGE).await?),
        None => None,
    };
    let taxonomy = context.taxonomy();
    let matching = Matching {
        db: context.db(),
        taxonomy: &taxonomy,
        now: chrono::Utc::now().timestamp(),
        laya: context.laya.as_ref(),
    };
    let new = NewMatch {
        job: &request.job,
        job_url: &request.job_url,
        job_text: &request.job_text,
        cv_name: &request.cv_name,
        cv_text: &request.cv_text,
    };
    let llm = keyed.as_ref().map(|keyed| &keyed.llm);
    let found = ai_match::ai_match(&matching, llm, new, &request.profile, Default::default())
        .await
        .map_err(|e| ServiceError::llm("match/match", &e))?;
    if let (Some(keyed), Some(match_id)) = (&keyed, found.saved_id) {
        learn_from_match(context, &taxonomy, request, &found, match_id, &keyed.written_by).await;
    }
    Ok(found)
}

/// What a saved AI match teaches the taxonomy (taxonomy_lessons), recorded, and the taxonomy
/// built again when a label turned active. A failure here only costs the lesson, so it is logged
/// and the match goes on.
async fn learn_from_match(
    context: &Context,
    taxonomy: &Taxonomy,
    request: &MatchRequest,
    found: &AiMatch,
    match_id: i64,
    model: &str,
) {
    let cv_text = if request.profile.text_en.is_empty() {
        &request.cv_text
    } else {
        &request.profile.text_en
    };
    let lessons = taxonomy_lessons::lessons(taxonomy, &request.job, &request.profile, cv_text, &found.verdicts);
    if lessons.is_empty() {
        return;
    }
    let now = chrono::Utc::now().timestamp();
    match taxonomy_store::learn(context.db(), &lessons, match_id, model, now).await {
        Ok(activated) if !activated.is_empty() => {
            if let Err(why) = taxonomy_store::reload(context).await {
                crate::log_line!("match (learning): {why}");
            }
        }
        Ok(_) => {}
        Err(e) => crate::log_line!("match (learning): the lessons of match {match_id} weren't saved: {e}"),
    }
}

/// The AI match as its result shows it, with the application and résumé panel of its saved match.
pub async fn ai_match_result(
    context: &Context,
    request: &MatchRequest,
    found: &AiMatch,
) -> Result<MatchResult, ServiceError> {
    let uses = llm_key::ai_uses(context).await?;
    let documents = MatchDocuments::from(
        found.saved_id,
        found.save_error.as_deref().unwrap_or(""),
        &Default::default(),
        uses,
        found.apply_instructions.as_ref(),
    );
    let fresh = Fresh {
        job: &request.job,
        groups: &found.groups,
        verdicts: &found.verdicts,
        score: &found.result,
        soft_skills: found.soft_skills.clone(),
        apply_instructions: found.apply_instructions.as_ref(),
        saved_id: found.saved_id,
        laya: &found.laya_reading,
        job_url: &request.job_url,
    };
    let laya_input = ai_match::laya_input(
        &request.job,
        &request.job_text,
        &request.profile,
        &request.cv_text,
        &found.laya_summary,
    );
    Ok(MatchResult::from(&fresh, true, Some(documents), &laya_input))
}

/// A CV read for matching: its text, and the profile read from it.
#[derive(Debug, Clone)]
pub struct ReadCv {
    pub text: String,
    pub profile: Profile,
}

/// What to match, as a front end has it: the job post's text or a link to it, and the CV.
pub struct MatchAsk<'a> {
    /// The job post's text, or a link to it.
    pub job_text: &'a str,
    /// A link to the post, fetched when job_text is empty.
    pub job_url: &'a str,
    pub cv_text: &'a str,
    /// The CV read the last time, used again while its text is cv_text, so an unchanged CV costs
    /// no model call.
    pub read_cv: Option<&'a ReadCv>,
}

/// Most job posts compared against one CV in one go (match_jobs, and the match page).
pub const MAX_JOBS: usize = 10;

/// A job post of several matched with one CV: its text or a link to it, and a link to it.
pub struct JobAsk {
    pub job_text: String,
    pub job_url: String,
}

/// Matches each of jobs with one CV in turn (match_job), saying "Job 2 of 3: …" for each step.
/// The CV is read once, for the first post that gets that far. A post that fails gives its
/// error, and the rest carry on.
pub async fn match_jobs(
    context: &Context,
    jobs: &[JobAsk],
    cv_text: &str,
    read_cv: Option<ReadCv>,
    say: Say<'_>,
) -> Result<Vec<Result<MatchAnswer, ServiceError>>, ServiceError> {
    if jobs.len() > MAX_JOBS {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            format!("Compare up to {MAX_JOBS} job posts at a time."),
        ));
    }
    let mut read_cv = read_cv;
    let mut answers = Vec::with_capacity(jobs.len());
    for (index, job) in jobs.iter().enumerate() {
        let step = format!("Job {} of {}: ", index + 1, jobs.len());
        let say_step = |line: &str| say(&format!("{step}{line}"));
        let ask = MatchAsk {
            job_text: &job.job_text,
            job_url: &job.job_url,
            cv_text,
            read_cv: read_cv.as_ref(),
        };
        let answer = match_job(context, ask, &say_step).await;
        if let Ok(MatchAnswer {
            read_cv: Some(read), ..
        }) = &answer
        {
            read_cv = Some(read.clone());
        }
        answers.push(answer);
    }
    Ok(answers)
}

/// What match_job came to.
#[derive(Debug)]
pub struct MatchAnswer {
    /// The post read from its link, for the job box.
    pub fetched: Option<Page>,
    /// The CV as read, to give back next time; None for the free match, which reads it itself.
    pub read_cv: Option<ReadCv>,
    pub result: MatchResult,
    /// The same post was matched with the same CV before: result is that saved match.
    pub matched_before: bool,
    /// Why the match couldn't be saved, when it couldn't.
    pub save_error: Option<String>,
}

/// What says each step of match_job as it starts ("Reading the job post…").
pub type Say<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// Whether text is one link and nothing else.
pub fn is_link(text: &str) -> bool {
    let text = text.trim();
    let lower = text.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://"))
        && text.len() > "https://".len()
        && !text.contains(char::is_whitespace)
}

/// Matches a job post with a CV, as the match page does: a link is fetched first; with an AI
/// model, the CV is read (unless it was already), a post matched with this CV before opens its
/// saved match, and otherwise the post is read and matched (ai_match) and saved; without one,
/// it is the free match (quick_match), which saves nothing.
pub async fn match_job(context: &Context, ask: MatchAsk<'_>, say: Say<'_>) -> Result<MatchAnswer, ServiceError> {
    let typed = ask.job_text.trim();
    let link = if is_link(typed) {
        Some(typed)
    } else if typed.is_empty() {
        Some(ask.job_url.trim()).filter(|url| !url.is_empty())
    } else {
        None
    };
    let fetched = match link {
        Some(url) if !is_link(url) => {
            return Err(ServiceError::new(
                ErrorKind::Invalid,
                "Paste a link starting with http:// or https://.",
            ));
        }
        Some(url) => {
            say("Fetching the job post…");
            Some(fetch_job(context, url).await?)
        }
        None => None,
    };
    let (job_text, job_url) = match &fetched {
        Some(page) => (page.text.trim().to_string(), page.url.clone()),
        None => (typed.to_string(), ask.job_url.trim().to_string()),
    };
    if job_text.is_empty() {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Paste a link, the text or choose a file with the job post first.",
        ));
    }
    let cv_text = ask.cv_text.trim();
    if cv_text.is_empty() {
        return Err(ServiceError::new(ErrorKind::Invalid, "Paste or choose a CV first."));
    }
    let uses = llm_key::ai_uses(context).await?;
    if !uses.any() {
        say("Matching…");
        return Ok(MatchAnswer {
            result: quick_match(context, &job_text, &job_url, cv_text).await?,
            fetched,
            read_cv: None,
            matched_before: false,
            save_error: None,
        });
    }
    let read_cv = match ask.read_cv.filter(|read| read.text == cv_text) {
        Some(read) => read.clone(),
        None => {
            say("Reading the CV…");
            ReadCv {
                text: cv_text.to_string(),
                profile: extract_cv(context, cv_text).await?,
            }
        }
    };
    say("Checking your saved matches…");
    if let Some(id) = already_matched(context, cv_text, &job_text).await? {
        return Ok(MatchAnswer {
            result: saved_result(context, id).await?,
            fetched,
            read_cv: Some(read_cv),
            matched_before: true,
            save_error: None,
        });
    }
    say("Reading the job post…");
    let job = extract_job(context, &job_text).await?;
    say(if uses.job_evaluation {
        "Matching: the taxonomy first, then the AI for what it can't decide…"
    } else {
        "Matching with the taxonomy…"
    });
    let request = MatchRequest {
        job,
        cv_name: read_cv.profile.name.clone(),
        profile: read_cv.profile.clone(),
        job_text,
        job_url,
        cv_text: cv_text.to_string(),
    };
    let found = ai_match(context, &request).await?;
    let result = ai_match_result(context, &request, &found).await?;
    Ok(MatchAnswer {
        result,
        fetched,
        read_cv: Some(read_cv),
        matched_before: false,
        save_error: found.save_error.clone().filter(|why| !why.is_empty()),
    })
}

/// What a writing does when the one who asked for it stops waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WhenAbandoned {
    /// Finishes and is saved: a page reloaded or a window closed still gets its document.
    KeepWriting,
    /// Stops with the waiter: the job scan, which gives up on a post past its time limit and
    /// must not leave the model writing behind it.
    StopWriting,
}

/// Ends a task when dropped.
struct AbortOnDrop(tokio::task::AbortHandle);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// What the task manager calls a document being written for saved: "The application for
/// Backend developer, Acme".
fn writing_label(kind: &str, saved: &SavedMatch) -> String {
    let document = if kind == RESUME {
        "The tailored résumé"
    } else {
        "The application"
    };
    format!("{document} for {}", job_label(saved))
}

/// Writes kind (document_box::APPLICATION or RESUME) for a saved match from its CV and post, with
/// the candidate's notes, keeps it as the match's newest version, and answers its box. The
/// writing runs as its own task, which abandoned says the fate of when the one who asked stops
/// waiting.
pub async fn write(
    context: Arc<Context>,
    id: i64,
    notes: &str,
    kind: &'static str,
    revision: Option<Revision>,
    abandoned: WhenAbandoned,
) -> Result<DocumentBox, ServiceError> {
    let found = saved_match(&context, id).await?;
    let keyed = llm_key::llm(&context, task_model::writing(kind)).await?;
    let notes = document_writer::notes(notes);
    let task_context = context.clone();
    let task_found = found.clone();
    // In a task of its own (abandoned says when it stops), listed and recorded as the work
    // asking for it is.
    let listed = crate::task_scope::carried(async move {
        let list = Arc::clone(&task_context.running);
        let what = writing_label(kind, &task_found);
        let writing = async {
            let db = task_context.db();
            let now = chrono::Utc::now().timestamp();
            if kind == RESUME {
                document_writer::tailor_resume(db, &task_found, &keyed, &notes, revision.as_ref(), now)
                    .await
                    .map(|tailored| (tailored.keywords, tailored.left_out))
            } else {
                document_writer::write_application(db, &task_found, &keyed, &notes, revision.as_ref(), now)
                    .await
                    .map(|_| (Vec::new(), Vec::new()))
            }
        };
        list.run(WorkKind::Writing, what, writing)
            .await
            .unwrap_or_else(|Stopped| Err(WriteError::Empty(Stopped::SAID)))
    });
    let writing = tokio::spawn(listed);
    let _stopped_with_waiter = (abandoned == WhenAbandoned::StopWriting).then(|| AbortOnDrop(writing.abort_handle()));
    let work = if kind == RESUME {
        "match/tailor_resume"
    } else {
        "match/write_application"
    };
    let written = writing
        .await
        .map_err(|e| ServiceError::failed(work, format!("The writing stopped: {e}")))?;
    let (keywords, left_out) = written.map_err(|e| match e {
        WriteError::Llm(e) => ServiceError::llm(work, &e),
        WriteError::Database(e) => ServiceError::from(e),
        WriteError::Empty(why) => ServiceError::failed(work, why),
    })?;
    let found = saved_match(&context, found.id).await?;
    let uses = llm_key::ai_uses(&context).await?;
    let (application, resume) = match_documents::written_of(&found);
    let written = if kind == RESUME { resume } else { application };
    Ok(DocumentBox::from(
        kind,
        found.id,
        &written,
        uses.of(kind),
        found.apply_instructions.as_ref(),
    )
    .with_keywords(keywords, &left_out))
}

/// The model that writes question.kind answers the question in a comment thread on a passage
/// of saved match id's document, with a change for its next revision. Listed in the task manager as the
/// writing is; nothing is saved.
pub async fn ask_about(context: &Context, id: i64, question: &Question) -> Result<QuestionAnswer, ServiceError> {
    const WORK: &str = "match/ask_about";
    if question.comment.passage.trim().is_empty() {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Highlight a part of the text first.",
        ));
    }
    if question.asked().is_empty() {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Write what you want to know about it.",
        ));
    }
    if question.comment.passage.chars().count() > document_question::MAX_PASSAGE {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            format!("Highlight at most {} characters.", document_question::MAX_PASSAGE),
        ));
    }
    if question.asked().chars().count() > document_question::MAX_QUESTION {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            format!("Ask in at most {} characters.", document_question::MAX_QUESTION),
        ));
    }
    let found = saved_match(context, id).await?;
    let keyed = llm_key::llm(context, task_model::writing(question.kind)).await?;
    let what = format!("Question on {}", writing_label(question.kind, &found).to_lowercase());
    let asking = document_question::ask(context.db(), &found, &keyed, question);
    let answered = Arc::clone(&context.running)
        .run(WorkKind::Writing, what, asking)
        .await
        .unwrap_or_else(|Stopped| Err(WriteError::Empty(Stopped::SAID)));
    answered.map_err(|e| match e {
        WriteError::Llm(e) => ServiceError::llm(WORK, &e),
        WriteError::Database(e) => ServiceError::from(e),
        WriteError::Empty(why) => ServiceError::failed(WORK, why),
    })
}

/// What write_document came to: the boxes as they are now, and the tailored résumé's own match.
#[derive(Debug)]
pub struct WriteAnswer {
    /// The application and résumé boxes, in document_box::KINDS's order; the one just tailored
    /// with the keywords it took over.
    pub boxes: Vec<DocumentBox>,
    /// The tailored résumé matched against the post, next to the CV's score; Err says why it
    /// couldn't be. None when no résumé was tailored.
    pub comparison: Option<Result<ResumeComparison, String>>,
    /// What was done, to say: "Résumé tailored and application written".
    pub done: &'static str,
    /// The application written with the résumé failed, and why.
    pub problem: Option<String>,
}

/// Writes kind for saved match id, as the match page does: tailoring the résumé writes the
/// application too when there is none yet and the key may write it, and then matches the
/// résumé against the post (compare_resume). Each of those runs when the other fails, so
/// neither has to be written again to get it. revision: the version it rewrites, with the
/// candidate's comments on it; None writes it from the CV alone.
pub async fn write_document(
    context: Arc<Context>,
    id: i64,
    notes: &str,
    kind: &'static str,
    revision: Option<Revision>,
    say: Say<'_>,
) -> Result<WriteAnswer, ServiceError> {
    say(match (kind == RESUME, revision.is_some()) {
        (true, false) => "Tailoring the résumé…",
        (true, true) => "Revising the résumé…",
        (false, false) => "Writing the application…",
        (false, true) => "Revising the application…",
    });
    let written = write(
        Arc::clone(&context),
        id,
        notes,
        kind,
        revision,
        WhenAbandoned::KeepWriting,
    )
    .await?;
    if kind != RESUME {
        return Ok(WriteAnswer {
            boxes: boxes_with(&context, id, written).await?,
            comparison: None,
            done: "Application written",
            problem: None,
        });
    }
    let found = saved_match(&context, id).await?;
    let application_too =
        found.application_text.is_none() && llm_key::ai_uses(&context).await?.of(document_box::APPLICATION);
    let mut problem = None;
    if application_too {
        say("Writing the application…");
        if let Err(e) = write(
            Arc::clone(&context),
            id,
            notes,
            document_box::APPLICATION,
            None,
            WhenAbandoned::KeepWriting,
        )
        .await
        {
            problem = Some(format!("The application wasn't written: {}", e.message));
        }
    }
    say("Matching the tailored résumé against the post…");
    let comparison = compare_resume(&context, id, &written.text).await.map_err(|e| e.message);
    Ok(WriteAnswer {
        boxes: boxes_with(&context, id, written).await?,
        comparison: Some(comparison),
        done: if application_too && problem.is_none() {
            "Résumé tailored and application written"
        } else {
            "Résumé tailored"
        },
        problem,
    })
}

/// The saved match's boxes as they are now, with written in place of its kind's.
async fn boxes_with(context: &Context, id: i64, written: DocumentBox) -> Result<Vec<DocumentBox>, ServiceError> {
    let documents = saved_result(context, id).await?.documents;
    Ok(documents
        .map(|documents| documents.boxes)
        .unwrap_or_default()
        .into_iter()
        .map(|current| {
            if current.kind == written.kind {
                written.clone()
            } else {
                current
            }
        })
        .collect())
}

/// The tailored résumé resume_text matched against saved match id's post as a match of its
/// own (named after the CV, ending saved_match_row::TAILORED_CV), next to the CV's score. A
/// résumé matched before with this post is that match.
pub async fn compare_resume(context: &Context, id: i64, resume_text: &str) -> Result<ResumeComparison, ServiceError> {
    let found = saved_match(context, id).await?;
    let resume_id = match already_matched(context, resume_text, &found.job_text).await? {
        Some(resume_id) => resume_id,
        None => {
            let job = extract_job(context, &found.job_text).await?;
            let mut profile = extract_cv(context, resume_text).await?;
            let name = [profile.name.as_str(), found.cv_name.as_str()]
                .into_iter()
                .find(|name| !name.is_empty())
                .unwrap_or("CV");
            profile.name = format!("{name} {TAILORED_CV}");
            let request = MatchRequest {
                job,
                cv_name: profile.name.clone(),
                profile,
                job_text: found.job_text.clone(),
                job_url: found.job_url.clone().unwrap_or_default(),
                cv_text: resume_text.trim().to_string(),
            };
            let matched = ai_match(context, &request).await?;
            matched.saved_id.ok_or_else(|| {
                ServiceError::failed(
                    "match/compare_resume",
                    matched
                        .save_error
                        .unwrap_or_else(|| "The tailored résumé was matched, but its match wasn't saved.".to_string()),
                )
            })?
        }
    };
    resume_comparison(context, id, resume_id).await
}

/// The tailored résumé's saved match (resume_id) against the CV's (id).
pub async fn resume_comparison(context: &Context, id: i64, resume_id: i64) -> Result<ResumeComparison, ServiceError> {
    let cv_match = saved_match(context, id).await?;
    let resume_match = saved_match(context, resume_id).await?;
    Ok(ResumeComparison::from(&cv_match, &resume_match))
}

/// What goes in the fields of an employer's application form for saved match id
/// (application_form): the profile's contact details, the match's PDFs and letter, and the
/// model's answers to the questions, listed in the task manager as writing is. When the model
/// can't answer (no key, a failed call), the rest is still answered and its questions are left
/// with why. Nothing is saved.
pub async fn application_form(
    context: &Context,
    id: i64,
    fields: Vec<application_form::FormField>,
) -> Result<application_form::FormAnswers, ServiceError> {
    const WORK: &str = "match/application_form";
    let fields = application_form::cleaned(fields);
    if fields.is_empty() {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "No form fields to fill in were found on this page.",
        ));
    }
    let found = saved_match(context, id).await?;
    let db = context.db();
    let contact = profile::contact(db).await?.unwrap_or_default();
    let links = profile::links_for_user(db).await?;
    let letter = found.application_text.clone().unwrap_or_default();
    let written = application_form::Written::of(&found);
    let mut plan = application_form::plan(&fields, &contact, &links, written, &letter);
    if plan.for_model.is_empty() {
        return Ok(plan.answers);
    }
    let answered = match llm_key::llm(context, &task_model::APPLICATION_FORM).await {
        Ok(keyed) => {
            let what = format!("Answering the application form for {}", job_label(&found));
            let asking = application_form::ask(db, &found, &keyed, &plan.for_model, &letter);
            Arc::clone(&context.running)
                .run(WorkKind::Writing, what, asking)
                .await
                .unwrap_or_else(|Stopped| Err(WriteError::Empty(Stopped::SAID)))
                .map_err(|e| match e {
                    WriteError::Llm(e) => ServiceError::llm(WORK, &e),
                    WriteError::Database(e) => ServiceError::from(e),
                    WriteError::Empty(why) => ServiceError::failed(WORK, why),
                })
        }
        Err(e) => Err(e),
    };
    match answered {
        Ok(answered) => {
            let taken = application_form::taken(&plan.for_model, &answered);
            plan.answers.answers.extend(taken.answers);
            plan.answers.left.extend(taken.left);
        }
        Err(e) => {
            crate::log_line!("{WORK}: the questions were left: {}", e.message);
            let why = format!("Not answered: {}", e.message);
            plan.answers
                .left
                .extend(plan.for_model.iter().map(|field| application_form::LeftField {
                    key: field.key.clone(),
                    why: why.clone(),
                }));
        }
    }
    Ok(plan.answers)
}

/// The job as the task manager names it: "Backend developer, Acme", or "match 21".
fn job_label(saved: &SavedMatch) -> String {
    let job = [saved.job_title.trim(), saved.company.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ");
    if job.is_empty() {
        format!("match {}", saved.id)
    } else {
        job
    }
}

/// The Apply dialog's content for a saved match: how the application is delivered, by email or on
/// the employer's form, with the PDFs and the profile links to copy.
pub async fn application_delivery(context: &Context, id: i64) -> Result<ApplicationDelivery, ServiceError> {
    let found = saved_match(context, id).await?;
    let links: Vec<CvLink> = profile::links_for_user(context.db())
        .await?
        .into_iter()
        .map(|(label, url)| CvLink { label, url })
        .collect();
    Ok(ApplicationDelivery::from(&found, &links))
}
