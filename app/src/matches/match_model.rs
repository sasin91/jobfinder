//! Saved matches in the database (matches with match_items, match_integrations, match_rejections,
//! the documents written for them and the tailored résumé tables). A document written again is a
//! new version (documents) rather than written over the one before; the newest is the one shown.

use std::collections::HashMap;

use serde::Serialize;
use sqlx::SqlitePool;
use sqlx::sqlite::SqliteConnection;

use super::Job;
use super::apply_instructions::{self, ApplyInstructions};
use super::document_box::{APPLICATION, CV, RESUME};
use super::match_score;
use super::matcher::{Groups, Score, Verdicts};
use super::reject_reason::Rejection;
use super::score_cursor;
use super::tailored_resume::{ResumeEntry, TailoredResume};

/// The sort order for saved matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortOrder {
    #[default]
    Best = 0,
    Newest = 1,
    Oldest = 2,
}

impl std::fmt::Display for SortOrder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Best => "Best match",
            Self::Newest => "Newest saved",
            Self::Oldest => "Oldest saved",
        })
    }
}

/// The integration a match's Jobnet joblog status is kept under.
pub const JOBNET_JOBLOG: &str = "jobnet_joblog";

/// How much of a résumé's links its head keeps, in characters (the web app's
/// match_resumes.links was a varchar(1000)).
const LINKS_MAX_CHARS: usize = 1000;

/// The longest each of a saved match's texts is kept, in characters: the web app's MariaDB
/// column lengths.
mod lengths {
    /// job_title, company and cv_name.
    pub const NAME: usize = 255;
    pub const JOB_URL: usize = 2048;
    /// An item's text, reason and evidence.
    pub const ITEM: usize = 500;
    pub const DECIDED_BY: usize = 16;
}

/// The decimals a match's score is kept with.
const SCORE_DECIMALS: i32 = 4;

/// The verdicts an item can be saved with; anything else is saved as missing.
const VERDICTS: [&str; 3] = ["met", "partial", "missing"];

/// What an item without a verdict is saved with.
const NO_VERDICT_REASON: &str = "No verdict returned.";

/// The Jobnet statuses that mean the candidate applied (joblog's APPLIED_STATUSES).
pub const APPLIED_STATUSES: [&str; 2] = ["Soegt", "Samtale"];

/// The lists of saved matches by ?list=, each with its tab label and what the page says when it
/// has none: the shortlist (top matches not applied for yet), all of them, applied for but not
/// on Jobnet yet, on Jobnet, and rejected.
pub const SHORTLIST: &str = "shortlist";
/// The list of jobs that no longer take applications (still_open).
pub const CLOSED: &str = "closed";
pub const LISTS: [(&str, &str, &str); 6] = [
    (
        SHORTLIST,
        "Shortlist",
        "No top matches left to apply for. All has the rest.",
    ),
    ("all", "All", "No saved matches yet."),
    (
        "not_joblogged",
        "Applied, not on Jobnet yet",
        "Nothing to log: every match marked Søgt or Samtale in its Jobnet joblog panel is on Jobnet.",
    ),
    (
        "joblogged",
        "On Jobnet",
        "Nothing on Jobnet yet: tick \"entered on Jobnet\" in a match's joblog panel once you have logged it there.",
    ),
    (
        CLOSED,
        "No longer open",
        "None of your saved jobs has closed: after each scan JobFinder checks whether they still take applications.",
    ),
    (
        "rejected",
        "Rejected",
        "Nothing rejected. Reject a match you are not interested in to keep it out of the other lists.",
    ),
];

/// One scored item of a saved match: the "why" behind its score.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SavedItem {
    pub criterion: String,
    pub text: String,
    pub verdict: String,
    pub decided_by: String,
    pub reason: String,
    pub evidence: String,
}

/// A saved match's state in an outside service (Jobnet's joblog).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Integration {
    pub status: String,
    pub external_id: Option<String>,
    pub applied_at: Option<i64>,
    pub logged_at: Option<i64>,
    pub updated_at: i64,
}

/// One version of a document written for a saved match.
#[derive(Debug, Clone, PartialEq)]
pub struct Document {
    pub id: i64,
    pub text: String,
    pub written_at: i64,
}

/// A version of a document with who wrote it and the notes it was written with, as the
/// "Earlier versions" list shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Version {
    pub id: i64,
    pub kind: String,
    pub text: String,
    pub written_by: String,
    pub notes: String,
    pub written_at: i64,
}

/// A match about to be saved: the post as read, where it came from, and the CV it was matched
/// with.
#[derive(Debug, Clone, Copy)]
pub struct NewMatch<'a> {
    pub job: &'a Job,
    pub job_url: &'a str,
    pub job_text: &'a str,
    pub cv_name: &'a str,
    pub cv_text: &'a str,
}

/// Whether a saved match's job still takes applications, as the still-open check (still_open)
/// last found it: matches.deadline, closed_at and closed_why.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Opening {
    /// The application deadline the post gives, yyyy-mm-dd.
    pub deadline: Option<String>,
    /// When the job was found closed; None while it is open.
    pub closed_at: Option<i64>,
    /// What gave it away (still_open::Closed), or still_open::BY_HAND_OPEN.
    pub closed_why: String,
}

/// A saved match the still-open check looks at (queries/match_model/open_to_check.sql).
#[derive(Debug, Clone, PartialEq)]
pub struct OpenToCheck {
    pub id: i64,
    pub job_url: Option<String>,
    pub job_text: String,
    pub created_at: i64,
    pub deadline: Option<String>,
    pub still_open_checked_at: Option<i64>,
}

/// A saved match with everything the page shows of it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SavedMatch {
    pub id: i64,
    pub job_title: String,
    pub company: String,
    pub job_url: Option<String>,
    pub job_text: String,
    pub cv_name: String,
    pub cv_text: String,
    pub score: f64,
    pub points: i64,
    pub max_points: i64,
    pub tag: String,
    /// The newest application's text and when it was written (documents), None when none is.
    pub application_text: Option<String>,
    pub application_written_at: Option<i64>,
    pub resume_text: Option<String>,
    pub resume_written_at: Option<i64>,
    pub rejected_at: Option<i64>,
    pub created_at: i64,
    /// Its newest tailored résumé was written as fields (match_resumes).
    pub resume_structured: bool,
    pub items: Vec<SavedItem>,
    pub jobnet_joblog: Option<Integration>,
    pub rejections: Vec<Rejection>,
    /// How the post asks to be applied to (apply_instructions::read of job_text).
    pub apply_instructions: Option<ApplyInstructions>,
    /// The application's and résumé's versions before the newest, newest first.
    pub earlier_versions: Vec<Version>,
    pub opening: Opening,
}

/// A saved match as a list shows it (and the Jobnet extension lists it, as JSON).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ListedMatch {
    pub id: i64,
    pub job_title: String,
    pub company: String,
    pub job_url: Option<String>,
    pub cv_name: String,
    pub score: f64,
    pub points: i64,
    pub max_points: i64,
    pub tag: String,
    pub has_application: bool,
    pub has_resume: bool,
    pub created_at: i64,
    /// The first likely disqualifier the match misses: a location, a language or the career level.
    pub missed_disqualifier: Option<String>,
    pub jobnet_status: Option<String>,
    pub jobnet_logged_at: Option<i64>,
    #[serde(skip)]
    pub rejected_at: Option<i64>,
    #[serde(skip)]
    pub rejections: Vec<Rejection>,
    #[serde(skip)]
    pub opening: Opening,
}

/// A batch of a list, and the cursor of the batch after it (None at the end).
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub rows: Vec<ListedMatch>,
    pub next: Option<String>,
}

/// A row of queries/match_model/recent.sql and best_first.sql, which select the same columns.
struct ListedRow {
    id: i64,
    job_title: String,
    company: String,
    job_url: Option<String>,
    cv_name: String,
    score: f64,
    points: i64,
    max_points: i64,
    tag: String,
    has_application: bool,
    has_resume: bool,
    created_at: i64,
    missed_disqualifier: Option<String>,
    jobnet_status: Option<String>,
    jobnet_logged_at: Option<i64>,
    rejected_at: Option<i64>,
    deadline: Option<String>,
    closed_at: Option<i64>,
    closed_why: String,
}

impl From<ListedRow> for ListedMatch {
    fn from(row: ListedRow) -> Self {
        ListedMatch {
            id: row.id,
            job_title: row.job_title,
            company: row.company,
            job_url: row.job_url,
            cv_name: row.cv_name,
            score: row.score,
            points: row.points,
            max_points: row.max_points,
            tag: row.tag,
            has_application: row.has_application,
            has_resume: row.has_resume,
            created_at: row.created_at,
            missed_disqualifier: row.missed_disqualifier,
            jobnet_status: row.jobnet_status,
            jobnet_logged_at: row.jobnet_logged_at,
            rejected_at: row.rejected_at,
            rejections: Vec::new(),
            opening: Opening {
                deadline: row.deadline,
                closed_at: row.closed_at,
                closed_why: row.closed_why,
            },
        }
    }
}

/// Saves a match for the user: the post and CV, the score, and an item per criterion item with
/// its verdict; the new match's id.
pub async fn save(
    db: &SqlitePool,
    new: NewMatch<'_>,
    groups: &Groups,
    verdicts: &Verdicts,
    score: &Score,
    now: i64,
) -> sqlx::Result<i64> {
    let job_title = cut(&new.job.job_title, lengths::NAME);
    let company = cut(&new.job.company, lengths::NAME);
    let job_url = (!new.job_url.is_empty()).then(|| cut(new.job_url, lengths::JOB_URL));
    let cv_name = cut(new.cv_name, lengths::NAME);
    let deadline = super::still_open::deadline_of(new.job_text, now);
    let index = match_score::round_to(score.index, SCORE_DECIMALS);
    let mut tx = crate::db::write_transaction(db).await?;
    let id = sqlx::query_file!(
        "queries/match_model/save.sql",
        job_title,
        company,
        job_url,
        new.job_text,
        index,
        score.rank,
        score.total,
        score.tag,
        now,
        deadline
    )
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();
    let cv_id = saved_cv(&mut tx, &cv_name, new.cv_text, now).await?;
    link_document(&mut tx, id, cv_id).await?;
    let mut order: i64 = 0;
    for (criterion, list) in groups {
        for item in list {
            let saved = SavedItem::of(criterion, &item.text, verdicts.get(&item.id));
            sqlx::query!(
                r#"
                INSERT INTO match_items (match_id, criterion, text, verdict, decided_by, reason, evidence, sort_order)
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                "#,
                id,
                saved.criterion,
                saved.text,
                saved.verdict,
                saved.decided_by,
                saved.reason,
                saved.evidence,
                order
            )
            .execute(&mut *tx)
            .await?;
            order += 1;
        }
    }
    tx.commit().await?;
    Ok(id)
}

impl SavedItem {
    /// An item as it is saved: its verdict (missing when it has none it may have), who decided
    /// it ("llm", or the taxonomy's method) and why, cut to MariaDB's lengths.
    fn of(criterion: &str, text: &str, verdict: Option<&super::matcher::Verdict>) -> Self {
        let Some(verdict) = verdict else {
            return SavedItem {
                criterion: criterion.to_string(),
                text: cut(text, lengths::ITEM),
                verdict: "missing".to_string(),
                decided_by: "llm".to_string(),
                reason: NO_VERDICT_REASON.to_string(),
                evidence: String::new(),
            };
        };
        let known = VERDICTS.contains(&verdict.verdict.as_str());
        let by_model = verdict.by.is_empty() || verdict.by == "llm";
        let decided_by = if by_model {
            "llm"
        } else if verdict.method.is_empty() {
            &verdict.by
        } else {
            &verdict.method
        };
        SavedItem {
            criterion: criterion.to_string(),
            text: cut(text, lengths::ITEM),
            verdict: if known {
                verdict.verdict.clone()
            } else {
                "missing".to_string()
            },
            decided_by: cut(decided_by, lengths::DECIDED_BY),
            reason: cut(&verdict.reason, lengths::ITEM),
            evidence: cut(&verdict.evidence, lengths::ITEM),
        }
    }
}

/// The first length characters of text.
fn cut(text: &str, length: usize) -> String {
    text.chars().take(length).collect()
}

/// The user's newest matches not rejected, for the extension's history.
pub async fn recent(db: &SqlitePool, limit: usize) -> sqlx::Result<Vec<ListedMatch>> {
    let limit = limit.max(1) as i64;
    let rows = sqlx::query_file_as!(ListedRow, "queries/match_model/recent.sql", JOBNET_JOBLOG, limit)
        .fetch_all(db)
        .await?;
    Ok(rows.into_iter().map(ListedMatch::from).collect())
}

/// A batch of the user's matches in list `list` (LISTS), best first (highest score, then
/// newest), after cursor `after` (score_cursor) when it is one.
pub async fn best_first(db: &SqlitePool, limit: usize, after: &str, list: &str) -> sqlx::Result<Batch> {
    list_matches(db, limit, after, list, SortOrder::Best).await
}

/// A batch in the selected order. The cursor must come from a batch in that same order.
pub async fn list_matches(
    db: &SqlitePool,
    limit: usize,
    after: &str,
    list: &str,
    sort_order: SortOrder,
) -> sqlx::Result<Batch> {
    // One more than the batch, to tell whether another batch follows.
    let mut rows = list_rows(db, limit as i64 + 1, after, list, sort_order).await?;
    let mut next = None;
    if rows.len() > limit {
        rows.truncate(limit);
        let last = rows.last().expect("limit rows");
        next = Some(score_cursor::after(Some(last.score), last.created_at, last.id));
    }
    let rejected: Vec<i64> = rows
        .iter()
        .filter(|row| row.rejected_at.is_some())
        .map(|row| row.id)
        .collect();
    let mut by_match = rejections(db, &rejected).await?;
    for row in &mut rows {
        row.rejections = by_match.remove(&row.id).unwrap_or_default();
    }
    Ok(Batch { rows, next })
}

/// The full shortlist for the browser's application picker. SQLite's LIMIT -1 reads all rows.
pub(super) async fn shortlist(db: &SqlitePool, sort: SortOrder) -> sqlx::Result<Vec<ListedMatch>> {
    list_rows(db, -1, "", SHORTLIST, sort).await
}

async fn list_rows(
    db: &SqlitePool,
    limit: i64,
    after: &str,
    list: &str,
    sort: SortOrder,
) -> sqlx::Result<Vec<ListedMatch>> {
    let place = score_cursor::place(after);
    let after_score = place.as_ref().map(|place| place.score);
    let after_at = place.as_ref().map(|place| place.at);
    let after_id = place.as_ref().map(|place| place.id);
    let applied = applied_statuses_json();
    let rows = sqlx::query_file_as!(
        ListedRow,
        "queries/match_model/best_first.sql",
        JOBNET_JOBLOG,
        list,
        applied,
        after_score,
        after_at,
        after_id,
        limit,
        sort as i32,
    )
    .fetch_all(db)
    .await?;
    Ok(rows.into_iter().map(ListedMatch::from).collect())
}

/// How many matches each list (LISTS) holds, in LISTS's order, as (list, count).
pub async fn list_counts(db: &SqlitePool) -> sqlx::Result<Vec<(&'static str, i64)>> {
    let applied = applied_statuses_json();
    let row = sqlx::query_file!("queries/match_model/list_counts.sql", JOBNET_JOBLOG, applied)
        .fetch_one(db)
        .await?;
    let counts = [
        row.shortlist,
        row.every,
        row.not_joblogged,
        row.joblogged,
        row.closed,
        row.rejected,
    ];
    Ok(LISTS.iter().map(|(list, ..)| *list).zip(counts).collect())
}

/// APPLIED_STATUSES as the JSON array the queries read with json_each.
fn applied_statuses_json() -> String {
    serde_json::to_string(&APPLIED_STATUSES).expect("a list of strings is JSON")
}

/// The user's saved matches the still-open check looks at, newest first; by_hand_open is what
/// closed_why says when the user said a job is still open.
pub async fn open_to_check(db: &SqlitePool, by_hand_open: &str) -> sqlx::Result<Vec<OpenToCheck>> {
    sqlx::query_file_as!(OpenToCheck, "queries/match_model/open_to_check.sql", by_hand_open)
        .fetch_all(db)
        .await
}

/// Keeps the application deadline read from match id's post.
pub async fn save_deadline(db: &SqlitePool, id: i64, deadline: &str) -> sqlx::Result<()> {
    sqlx::query!("UPDATE matches SET deadline = ?2 WHERE id = ?1", id, deadline)
        .execute(db)
        .await?;
    Ok(())
}

/// Notes that match id's post was read by the still-open check at `at`.
pub async fn mark_open_checked(db: &SqlitePool, id: i64, at: i64) -> sqlx::Result<()> {
    sqlx::query!("UPDATE matches SET still_open_checked_at = ?2 WHERE id = ?1", id, at)
        .execute(db)
        .await?;
    Ok(())
}

/// Marks match id's job closed at `at`, for why (still_open::Closed), unless it is already.
pub async fn close(db: &SqlitePool, id: i64, why: &str, at: i64) -> sqlx::Result<()> {
    sqlx::query!(
        "UPDATE matches SET closed_at = ?3, closed_why = ?2 WHERE id = ?1 AND closed_at IS NULL",
        id,
        why,
        at
    )
    .execute(db)
    .await?;
    Ok(())
}

/// Marks match id open again, with closed_why by_hand_open so the check leaves it alone; false
/// when there is no such match.
pub async fn keep_open(db: &SqlitePool, id: i64, by_hand_open: &str) -> sqlx::Result<bool> {
    let done = sqlx::query!(
        "UPDATE matches SET closed_at = NULL, closed_why = ?2 WHERE id = ?1",
        id,
        by_hand_open
    )
    .execute(db)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Why matches `ids` were rejected, by match id, in the order ticked.
async fn rejections(db: &SqlitePool, ids: &[i64]) -> sqlx::Result<HashMap<i64, Vec<Rejection>>> {
    let mut by_match: HashMap<i64, Vec<Rejection>> = HashMap::new();
    if ids.is_empty() {
        return Ok(by_match);
    }
    let ids = serde_json::to_string(ids).expect("a list of numbers is JSON");
    let rows = sqlx::query!(
        r#"
        SELECT match_id, reason, note
        FROM match_rejections
        WHERE match_id IN (SELECT value FROM json_each(?1))
        ORDER BY id
        "#,
        ids
    )
    .fetch_all(db)
    .await?;
    for row in rows {
        by_match.entry(row.match_id).or_default().push(Rejection {
            reason: row.reason,
            note: row.note,
        });
    }
    Ok(by_match)
}

/// The id of the user's newest match of this CV with the same job post text, or None. By text,
/// not link: a page can change, and a post read wrongly before is matched again once it reads
/// right.
pub async fn existing(db: &SqlitePool, cv_text: &str, job_text: &str) -> sqlx::Result<Option<i64>> {
    if cv_text.is_empty() || job_text.is_empty() {
        return Ok(None);
    }
    sqlx::query_scalar!(
        r#"
        SELECT m.id FROM matches m
        JOIN match_documents md ON md.match_id = m.id
        JOIN documents d ON d.id = md.document_id AND d.kind = ?3
        WHERE d.text = ?1 AND m.job_text = ?2
        ORDER BY m.created_at DESC, m.id DESC
        LIMIT 1
        "#,
        cv_text,
        job_text,
        CV
    )
    .fetch_optional(db)
    .await
}

/// The id of the CV document with this name and text, saved now if there is none yet: each CV
/// is kept once, however many matches are made with it.
async fn saved_cv(tx: &mut SqliteConnection, name: &str, text: &str, now: i64) -> sqlx::Result<i64> {
    let kept = sqlx::query_scalar!(
        r#"SELECT id AS "id!" FROM documents WHERE kind = ?1 AND name = ?2 AND text = ?3"#,
        CV,
        name,
        text
    )
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(id) = kept {
        return Ok(id);
    }
    Ok(sqlx::query!(
        r#"INSERT INTO documents (kind, name, text, written_at) VALUES (?1, ?2, ?3, ?4)"#,
        CV,
        name,
        text,
        now
    )
    .execute(&mut *tx)
    .await?
    .last_insert_rowid())
}

/// Puts the document on the match (match_documents).
async fn link_document(tx: &mut SqliteConnection, match_id: i64, document_id: i64) -> sqlx::Result<()> {
    sqlx::query!(
        r#"INSERT INTO match_documents (match_id, document_id) VALUES (?1, ?2)"#,
        match_id,
        document_id
    )
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// One of the user's saved matches with its items, joblog status and rejections, or None.
pub async fn find(db: &SqlitePool, id: i64) -> sqlx::Result<Option<SavedMatch>> {
    let Some(row) = sqlx::query_file!("queries/match_model/find.sql", id)
        .fetch_optional(db)
        .await?
    else {
        return Ok(None);
    };
    let items = sqlx::query_as!(
        SavedItem,
        r#"
        SELECT criterion, text, verdict, decided_by, reason, evidence
        FROM match_items
        WHERE match_id = ?1
        ORDER BY sort_order, id
        "#,
        id
    )
    .fetch_all(db)
    .await?;
    let application = newest_document(db, id, APPLICATION).await?;
    let resume = newest_document(db, id, RESUME).await?;
    let resume_structured = match &resume {
        Some(resume) => has_resume_fields(db, resume.id).await?,
        None => false,
    };
    Ok(Some(SavedMatch {
        id: row.id,
        job_title: row.job_title,
        company: row.company,
        job_url: row.job_url,
        cv_name: row.cv_name,
        cv_text: row.cv_text,
        score: row.score,
        points: row.points,
        max_points: row.max_points,
        tag: row.tag,
        application_written_at: application.as_ref().map(|document| document.written_at),
        application_text: application.map(|document| document.text),
        resume_written_at: resume.as_ref().map(|document| document.written_at),
        resume_text: resume.map(|document| document.text),
        rejected_at: row.rejected_at,
        created_at: row.created_at,
        resume_structured,
        items,
        jobnet_joblog: integration(db, id, JOBNET_JOBLOG).await?,
        rejections: rejections(db, &[id]).await?.remove(&id).unwrap_or_default(),
        apply_instructions: apply_instructions::read(&row.job_text),
        job_text: row.job_text,
        earlier_versions: earlier_versions(db, id).await?,
        opening: Opening {
            deadline: row.deadline,
            closed_at: row.closed_at,
            closed_why: row.closed_why,
        },
    }))
}

/// The newest version of one kind (document_box's APPLICATION, RESUME) of a match's documents, or
/// None when none is written.
pub async fn newest_document(db: &SqlitePool, id: i64, kind: &str) -> sqlx::Result<Option<Document>> {
    sqlx::query_file_as!(Document, "queries/match_model/newest_document.sql", id, kind)
        .fetch_optional(db)
        .await
}

/// The versions of a match's documents before the newest of their kind, newest first.
async fn earlier_versions(db: &SqlitePool, id: i64) -> sqlx::Result<Vec<Version>> {
    let versions = sqlx::query_file_as!(Version, "queries/match_model/versions.sql", id)
        .fetch_all(db)
        .await?;
    let mut newest_seen: Vec<String> = Vec::new();
    Ok(versions
        .into_iter()
        .filter(|version| {
            let newest = !newest_seen.contains(&version.kind);
            if newest {
                newest_seen.push(version.kind.clone());
            }
            !newest
        })
        .collect())
}

/// One version of a match's documents by its id, or None when it isn't one of that match's.
pub async fn version(db: &SqlitePool, id: i64, document_id: i64) -> sqlx::Result<Option<Version>> {
    sqlx::query_as!(
        Version,
        r#"
        SELECT d.id, d.kind, d.text, d.written_by, d.notes, d.written_at
        FROM documents d
        JOIN match_documents md ON md.document_id = d.id AND md.match_id = ?2
        WHERE d.id = ?1 AND d.kind != ?3
        "#,
        document_id,
        id,
        CV
    )
    .fetch_optional(db)
    .await
}

/// Whether the résumé `document_id` was written as fields (match_resumes).
async fn has_resume_fields(db: &SqlitePool, document_id: i64) -> sqlx::Result<bool> {
    sqlx::query_scalar!(
        r#"SELECT EXISTS (SELECT 1 FROM match_resumes WHERE document_id = ?1) AS "has: bool""#,
        document_id
    )
    .fetch_one(db)
    .await
}

/// Saves a new version of a match's application; the one before stays. `written_by` is the
/// provider and model that wrote it, `notes` what the user asked for.
pub async fn save_application(
    db: &SqlitePool,
    id: i64,
    text: &str,
    written_by: &str,
    notes: &str,
    now: i64,
) -> sqlx::Result<Document> {
    let mut conn = db.acquire().await?;
    save_document(&mut conn, id, APPLICATION, text, written_by, notes, now).await
}

/// Saves a new version of a match's tailored résumé: its fields (TailoredResume) and its plain
/// text, in one transaction. The version before keeps its own.
#[allow(clippy::too_many_arguments)]
pub async fn save_resume_fields(
    db: &SqlitePool,
    id: i64,
    resume: &TailoredResume,
    text: &str,
    written_by: &str,
    notes: &str,
    now: i64,
) -> sqlx::Result<Document> {
    let mut tx = crate::db::write_transaction(db).await?;
    let document = save_document(&mut tx, id, RESUME, text, written_by, notes, now).await?;
    save_resume_head(&mut tx, document.id, resume).await?;
    let sections = [
        ("experience", &resume.experience),
        ("projects", &resume.projects),
        ("education", &resume.education),
    ];
    for (section, entries) in sections {
        for (order, entry) in entries.iter().enumerate() {
            save_resume_entry(&mut tx, document.id, section, order as i64, entry).await?;
        }
    }
    for (kind, lines) in [("skill", &resume.skills), ("language", &resume.languages)] {
        for (order, line) in lines.iter().enumerate() {
            save_resume_line(&mut tx, document.id, None, kind, line, order as i64).await?;
        }
    }
    tx.commit().await?;
    Ok(document)
}

/// Adds a version of one kind of a match's documents.
async fn save_document(
    db: &mut SqliteConnection,
    id: i64,
    kind: &str,
    text: &str,
    written_by: &str,
    notes: &str,
    now: i64,
) -> sqlx::Result<Document> {
    let document_id = sqlx::query!(
        r#"
        INSERT INTO documents (kind, text, written_by, notes, written_at)
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        kind,
        text,
        written_by,
        notes,
        now
    )
    .execute(&mut *db)
    .await?
    .last_insert_rowid();
    link_document(db, id, document_id).await?;
    Ok(Document {
        id: document_id,
        text: text.to_string(),
        written_at: now,
    })
}

/// A résumé's head: links " · "-joined (cut as MariaDB would), intro paragraphs blank-line-joined.
async fn save_resume_head(db: &mut SqliteConnection, document_id: i64, resume: &TailoredResume) -> sqlx::Result<()> {
    let links: String = resume.links.join(" · ").chars().take(LINKS_MAX_CHARS).collect();
    let intro = resume.intro.join("\n\n");
    sqlx::query_file!(
        "queries/match_model/save_resume_head.sql",
        document_id,
        resume.name,
        resume.title,
        resume.location,
        resume.phone,
        resume.email,
        links,
        intro,
        resume.education_note,
        resume.experience_heading,
        resume.skills_heading,
        resume.education_heading,
        resume.languages_heading,
        resume.projects_heading
    )
    .execute(db)
    .await?;
    Ok(())
}

/// One of a résumé's roles, projects or schools, with its bullets.
async fn save_resume_entry(
    db: &mut SqliteConnection,
    document_id: i64,
    section: &str,
    order: i64,
    entry: &ResumeEntry,
) -> sqlx::Result<()> {
    let entry_id = sqlx::query!(
        r#"
        INSERT INTO match_resume_entries
          (document_id, section, title, organisation, location, starts, ends, summary, note, sort_order)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
        "#,
        document_id,
        section,
        entry.title,
        entry.organisation,
        entry.location,
        entry.starts,
        entry.ends,
        entry.summary,
        entry.note,
        order
    )
    .execute(&mut *db)
    .await?
    .last_insert_rowid();
    for (n, bullet) in entry.bullets.iter().enumerate() {
        save_resume_line(db, document_id, Some(entry_id), "bullet", bullet, n as i64).await?;
    }
    Ok(())
}

/// One of a résumé's bullets (entry_id set), skills or languages.
async fn save_resume_line(
    db: &mut SqliteConnection,
    document_id: i64,
    entry_id: Option<i64>,
    kind: &str,
    text: &str,
    order: i64,
) -> sqlx::Result<()> {
    sqlx::query!(
        r#"
        INSERT INTO match_resume_lines (document_id, entry_id, kind, text, sort_order)
        VALUES (?1, ?2, ?3, ?4, ?5)
        "#,
        document_id,
        entry_id,
        kind,
        text,
        order
    )
    .execute(db)
    .await?;
    Ok(())
}

/// A match's state in one integration, or None.
pub async fn integration(db: &SqlitePool, id: i64, integration: &str) -> sqlx::Result<Option<Integration>> {
    sqlx::query_as!(
        Integration,
        r#"
        SELECT status, external_id, applied_at, logged_at, updated_at
        FROM match_integrations
        WHERE match_id = ?1 AND integration = ?2
        "#,
        id,
        integration
    )
    .fetch_optional(db)
    .await
}

/// Saves a match's status in an outside service. `logged` says whether the candidate has
/// entered it there; the first time keeps its date. The first applied status
/// (APPLIED_STATUSES) keeps its date too (applied_at), through Søgt -> Samtale; any other status
/// clears it.
pub async fn save_integration(
    db: &SqlitePool,
    id: i64,
    integration: &str,
    status: &str,
    logged: bool,
    now: i64,
) -> sqlx::Result<Option<Integration>> {
    let applied = APPLIED_STATUSES.contains(&status);
    let applied_at = applied.then_some(now);
    let logged_at = logged.then_some(now);
    sqlx::query_file!(
        "queries/match_model/save_integration.sql",
        id,
        integration,
        status,
        applied_at,
        logged_at,
        now,
        applied,
        logged
    )
    .execute(db)
    .await?;
    self::integration(db, id, integration).await
}

/// Rejects one of the user's matches (not interested), with why; false when it isn't theirs.
pub async fn reject(db: &SqlitePool, id: i64, rows: &[Rejection], now: i64) -> sqlx::Result<bool> {
    let mut tx = crate::db::write_transaction(db).await?;
    if !mark_rejected(&mut tx, id, Some(now)).await? {
        return Ok(false);
    }
    sqlx::query!(r#"DELETE FROM match_rejections WHERE match_id = ?1"#, id)
        .execute(&mut *tx)
        .await?;
    for row in rows {
        sqlx::query!(
            r#"
            INSERT INTO match_rejections (match_id, reason, note, created_at)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            id,
            row.reason,
            row.note,
            now
        )
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

/// Brings back one of the user's rejected matches; false when it isn't theirs.
pub async fn bring_back(db: &SqlitePool, id: i64) -> sqlx::Result<bool> {
    let mut tx = crate::db::write_transaction(db).await?;
    if !mark_rejected(&mut tx, id, None).await? {
        return Ok(false);
    }
    sqlx::query!(r#"DELETE FROM match_rejections WHERE match_id = ?1"#, id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Sets when a saved match was rejected (None: not rejected); false when there is no such match. The reject and bring-back transactions hold the write lock from their start
/// (db::write_transaction), so overlapping ones wait for each other.
async fn mark_rejected(db: &mut SqliteConnection, id: i64, at: Option<i64>) -> sqlx::Result<bool> {
    let updated = sqlx::query!(r#"UPDATE matches SET rejected_at = ?1 WHERE id = ?2"#, at, id)
        .execute(db)
        .await?;
    Ok(updated.rows_affected() > 0)
}

/// A saved match's newest tailored résumé as fields (match_resumes with its entries and
/// lines), or None when none is written or it was written as text only.
pub async fn resume_fields(db: &SqlitePool, id: i64) -> sqlx::Result<Option<TailoredResume>> {
    match newest_document(db, id, RESUME).await? {
        Some(resume) => resume_fields_of(db, resume.id).await,
        None => Ok(None),
    }
}

/// The résumé `document_id` as fields, or None when it was written as text only.
pub async fn resume_fields_of(db: &SqlitePool, document_id: i64) -> sqlx::Result<Option<TailoredResume>> {
    let Some(head) = sqlx::query_file!("queries/match_model/resume_head.sql", document_id)
        .fetch_optional(db)
        .await?
    else {
        return Ok(None);
    };
    let mut resume = TailoredResume {
        name: head.name,
        title: head.title,
        location: head.location,
        phone: head.phone,
        email: head.email,
        links: split_unless_empty(&head.links, " · "),
        intro: split_unless_empty(&head.intro, "\n\n"),
        education_note: head.education_note,
        experience_heading: head.experience_heading,
        skills_heading: head.skills_heading,
        education_heading: head.education_heading,
        languages_heading: head.languages_heading,
        projects_heading: head.projects_heading,
        ..TailoredResume::default()
    };

    let mut entries: Vec<(i64, String, ResumeEntry)> =
        sqlx::query_file!("queries/match_model/resume_entries.sql", document_id)
            .fetch_all(db)
            .await?
            .into_iter()
            .map(|row| {
                let entry = ResumeEntry {
                    title: row.title,
                    organisation: row.organisation,
                    location: row.location,
                    starts: row.starts,
                    ends: row.ends,
                    summary: row.summary,
                    bullets: Vec::new(),
                    note: row.note,
                };
                (row.id, row.section, entry)
            })
            .collect();

    let lines = sqlx::query!(
        r#"
        SELECT entry_id, kind, text
        FROM match_resume_lines
        WHERE document_id = ?1
        ORDER BY sort_order, id
        "#,
        document_id
    )
    .fetch_all(db)
    .await?;
    for line in lines {
        match line.kind.as_str() {
            "bullet" => {
                if let Some((.., entry)) = entries.iter_mut().find(|(id, ..)| Some(*id) == line.entry_id) {
                    entry.bullets.push(line.text);
                }
            }
            "skill" => resume.skills.push(line.text),
            "language" => resume.languages.push(line.text),
            _ => {}
        }
    }
    for (_, section, entry) in entries {
        match section.as_str() {
            "experience" => resume.experience.push(entry),
            "projects" => resume.projects.push(entry),
            "education" => resume.education.push(entry),
            _ => {}
        }
    }
    Ok(Some(resume))
}

/// The text split at separator, or no parts for "".
fn split_unless_empty(text: &str, separator: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split(separator).map(str::to_string).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use sqlx::Executor;

    /// One match per kind of list: scores fall with the id, so best first is by id.
    const SEED: &str = r#"
        INSERT INTO matches (id, job_title, job_text, score, points, max_points, tag,
                                rejected_at, created_at)
        VALUES (1, 'Top, not applied', 'post', 0.9, 9, 10, 'top', NULL, 0),
               (2, 'Applied, not logged', 'post', 0.8, 8, 10, 'good', NULL, 0),
               (3, 'Logged', 'post', 0.7, 7, 10, 'top', NULL, 0),
               (4, 'Rejected', 'post', 0.6, 6, 10, 'good', 100, 0),
               (5, 'Poor', 'post', 0.5, 5, 10, 'poor', NULL, 0),
               (6, 'Good, interested', 'post', 0.4, 4, 10, 'good', NULL, 0);
        INSERT INTO match_integrations (match_id, integration, status, logged_at, created_at, updated_at)
        VALUES (2, 'jobnet_joblog', 'Soegt', NULL, 0, 0),
               (3, 'jobnet_joblog', 'Samtale', 50, 0, 0),
               (6, 'jobnet_joblog', 'Interesse', NULL, 0, 0);
        INSERT INTO match_rejections (match_id, reason, note, created_at) VALUES (4, 'salary', 'low', 100);
    "#;

    async fn ids(db: &SqlitePool, list: &str, limit: usize, after: &str) -> (Vec<i64>, Option<String>) {
        let batch = best_first(db, limit, after, list).await.unwrap();
        (batch.rows.iter().map(|row| row.id).collect(), batch.next)
    }

    #[tokio::test]
    async fn the_counts_are_the_lists_lengths() {
        let db = db::open_in_memory().await.unwrap();
        db.execute(sqlx::raw_sql(SEED)).await.unwrap();
        for (list, count) in list_counts(&db).await.unwrap() {
            assert_eq!(count as usize, ids(&db, list, 100, "").await.0.len(), "{list}");
        }
    }

    #[tokio::test]
    async fn each_list_holds_its_matches_best_first() {
        let db = db::open_in_memory().await.unwrap();
        db.execute(sqlx::raw_sql(SEED)).await.unwrap();
        assert_eq!(ids(&db, SHORTLIST, 10, "").await, (vec![1, 6], None));
        assert_eq!(ids(&db, "all", 10, "").await, (vec![1, 2, 3, 5, 6], None));
        assert_eq!(ids(&db, "not_joblogged", 10, "").await, (vec![2], None));
        assert_eq!(ids(&db, "joblogged", 10, "").await, (vec![3], None));
        assert_eq!(ids(&db, "rejected", 10, "").await, (vec![4], None));
        let rejected = best_first(&db, 10, "", "rejected").await.unwrap();
        assert_eq!(rejected.rows[0].rejections[0].note, "low");
    }

    #[tokio::test]
    async fn a_list_comes_in_batches_after_its_cursor() {
        let db = db::open_in_memory().await.unwrap();
        db.execute(sqlx::raw_sql(SEED)).await.unwrap();
        let (first, next) = ids(&db, "all", 2, "").await;
        assert_eq!(first, [1, 2]);
        let (second, next) = ids(&db, "all", 2, &next.unwrap()).await;
        assert_eq!(second, [3, 5]);
        assert_eq!(ids(&db, "all", 2, &next.unwrap()).await, (vec![6], None));
    }

    #[tokio::test]
    async fn date_sort_pages_through_matches_with_equal_saved_times() {
        let db = db::open_in_memory().await.unwrap();
        db.execute(sqlx::raw_sql(SEED)).await.unwrap();
        db.execute("UPDATE matches SET created_at = 100 WHERE id IN (1, 5, 6)")
            .await
            .unwrap();

        for (sort, expected) in [
            (SortOrder::Newest, [6, 5, 1, 3, 2]),
            (SortOrder::Oldest, [2, 3, 1, 5, 6]),
        ] {
            let first = list_matches(&db, 2, "", "all", sort).await.unwrap();
            let second = list_matches(&db, 3, &first.next.unwrap(), "all", sort).await.unwrap();
            let ids: Vec<_> = first.rows.iter().chain(&second.rows).map(|row| row.id).collect();
            assert_eq!(ids, expected);
            assert!(second.next.is_none());
        }
    }

    #[tokio::test]
    async fn writing_again_keeps_the_version_before_and_shows_the_newest() {
        let db = db::open_in_memory().await.unwrap();
        db.execute(sqlx::raw_sql(SEED)).await.unwrap();
        save_application(&db, 1, "Dear Acme", "openai/gpt-5.4-mini", "", 10)
            .await
            .unwrap();
        save_application(&db, 1, "Dear Acme team", "openai/gpt-5.4-mini", "shorter", 20)
            .await
            .unwrap();
        let first = TailoredResume {
            name: "Me".into(),
            skills: vec!["PHP".into()],
            experience: vec![ResumeEntry {
                title: "Developer".into(),
                bullets: vec!["Built things".into()],
                ..ResumeEntry::default()
            }],
            ..TailoredResume::default()
        };
        let second = TailoredResume {
            name: "Me".into(),
            skills: vec!["Rust".into()],
            ..TailoredResume::default()
        };
        let older = save_resume_fields(&db, 1, &first, "Me, PHP", "", "", 10).await.unwrap();
        save_resume_fields(&db, 1, &second, "Me, Rust", "", "", 20)
            .await
            .unwrap();

        let found = find(&db, 1).await.unwrap().unwrap();
        assert_eq!(
            (found.application_text.as_deref(), found.application_written_at),
            (Some("Dear Acme team"), Some(20))
        );
        assert_eq!(found.resume_text.as_deref(), Some("Me, Rust"));
        assert!(found.resume_structured);
        assert_eq!(resume_fields(&db, 1).await.unwrap(), Some(second));
        assert_eq!(resume_fields_of(&db, older.id).await.unwrap(), Some(first));
        let versions = sqlx::query_scalar!(r#"SELECT COUNT(*) FROM match_documents md JOIN documents d ON d.id = md.document_id WHERE md.match_id = 1 AND d.kind != 'cv'"#)
            .fetch_one(&db)
            .await
            .unwrap();
        assert_eq!(versions, 4);
        let listed = best_first(&db, 10, "", "all").await.unwrap();
        assert!(listed.rows[0].has_application && listed.rows[0].has_resume);
        assert!(!listed.rows[1].has_application && !listed.rows[1].has_resume);
    }

    /// Rejects and bring-backs of one match sent at once, as from two open tabs.
    const OVERLAPPING_REQUESTS: i64 = 40;

    #[tokio::test(flavor = "multi_thread")]
    async fn overlapping_rejects_and_bring_backs_all_save() {
        // A file, not :memory:, so the pool has several connections, as the app does.
        let path = std::env::temp_dir().join(format!("jobfinder-overlap-{}.sqlite", std::process::id()));
        let db = db::open(&path).await.unwrap();
        db.execute(sqlx::raw_sql(SEED)).await.unwrap();
        let requests: Vec<_> = (0..OVERLAPPING_REQUESTS)
            .map(|n| {
                let db = db.clone();
                tokio::spawn(async move {
                    if n % 2 == 0 {
                        let why = Rejection {
                            reason: "salary".to_string(),
                            note: String::new(),
                        };
                        reject(&db, 1, &[why], n).await
                    } else {
                        bring_back(&db, 1).await
                    }
                })
            })
            .collect();
        let mut answers = Vec::new();
        for request in requests {
            answers.push(request.await.unwrap());
        }
        db.close().await;
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
        for answer in answers {
            assert!(answer.unwrap(), "every request is for the user's own match");
        }
    }
}
