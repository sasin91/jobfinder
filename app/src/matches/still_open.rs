//! The still-open check: whether a saved match's job still takes applications.
//!
//! A job is closed when the deadline its post gives has passed (read with joblog::deadline, as
//! the Jobnet joblog panel reads it), when its post was taken down (the page answers 404 or 410,
//! or Jobnet no longer has the ad), when its page says it is closed, or when its link now lands
//! on the site's front page or search page. A page that can't be read says nothing: the job
//! stays as it was, and is read again next time. No language model is asked.
//!
//! The check runs after every scan (scan_service) and from the "Check now" button of Saved
//! matches. Each post is read at most once in READ_EVERY_SECONDS; the deadline is looked at
//! every time, since that costs no fetch.

use std::time::Duration;

use chrono::NaiveDate;
use url::Url;

use super::match_model::{self, OpenToCheck, Opening};
use super::page_reader::{self, GONE_STATUSES, Got, Readers};
use super::{dates, joblog};
use crate::context::Context;
use crate::job_sites;
use crate::service_error::{ErrorKind, ServiceError};
use crate::task_manager::running_work;

/// What closed_why says when the user said a job is still open: the check leaves it alone.
pub const BY_HAND_OPEN: &str = "by_hand_open";

/// How long after a post was read it is read again: a day, less a few hours, so a scan at the
/// same time each day reads it again.
const READ_EVERY_SECONDS: i64 = 20 * 60 * 60;

/// The pause between reading two posts, so the sites aren't asked for many pages at once.
pub const PAUSE_BETWEEN_READS: Duration = Duration::from_secs(2);

/// How long reading one post may take before it counts as unreadable this time.
const READ_TIME_LIMIT: Duration = Duration::from_secs(90);

/// What the sites write on a post that no longer takes applications, in lower case.
const CLOSED_PHRASES: [&str; 16] = [
    "annoncen er udløbet",
    "annoncen er ikke længere aktiv",
    "jobannoncen ikke længere er aktuel",
    "jobbet er ikke længere aktivt",
    "opslaget er udløbet",
    "stillingen er besat",
    "stillingen er blevet besat",
    "ansøgningsfristen er udløbet",
    "no longer accepting applications",
    "this job has expired",
    "this job ad has expired",
    "this job is no longer available",
    "this job posting has expired",
    "this position has been filled",
    "the position has been filled",
    "this job is closed",
];

/// The start of the path of a site's search page: a post's link that lands there was taken down
/// (Jobindex's search, LinkedIn's and Indeed's job search).
const SEARCH_PAGES: [&str; 3] = ["/jobsoegning", "/jobs/search", "/jobs?"];

/// The deadline as joblog::deadline writes it (Jobnet's form wants dd-mm-yyyy).
const JOBLOG_DATE: &str = "%d-%m-%Y";

/// The deadline as matches.deadline keeps it.
const KEPT_DATE: &str = "%Y-%m-%d";

/// What gave away that a job is closed, as matches.closed_why keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    /// Its deadline passed.
    Deadline,
    /// Its post was taken down.
    Gone,
    /// Its page says so.
    SaysClosed,
    /// Its link now lands on the site's front page or search page.
    Moved,
}

impl Closed {
    const ALL: [Closed; 4] = [Closed::Deadline, Closed::Gone, Closed::SaysClosed, Closed::Moved];

    pub fn as_str(self) -> &'static str {
        match self {
            Closed::Deadline => "deadline",
            Closed::Gone => "gone",
            Closed::SaysClosed => "says_closed",
            Closed::Moved => "moved",
        }
    }

    pub fn of(closed_why: &str) -> Option<Closed> {
        Closed::ALL.into_iter().find(|why| why.as_str() == closed_why)
    }

    /// Why, in words: "the deadline passed".
    pub fn says(self) -> &'static str {
        match self {
            Closed::Deadline => "the deadline passed",
            Closed::Gone => "the post was taken down",
            Closed::SaysClosed => "the post says it is closed",
            Closed::Moved => "the link now goes to the site's front page",
        }
    }
}

/// What reading a post found.
#[derive(Debug, Clone, PartialEq)]
pub enum Finding {
    Open,
    Closed(Closed),
    /// It couldn't be told, and why.
    Unknown(String),
}

/// The application deadline the post's text gives, as matches.deadline keeps it (yyyy-mm-dd),
/// or None when it gives none; a date without a year is read as of saved_at (joblog::deadline).
pub fn deadline_of(job_text: &str, saved_at: i64) -> Option<String> {
    let day = NaiveDate::parse_from_str(&joblog::deadline(job_text, saved_at), JOBLOG_DATE).ok()?;
    Some(day.format(KEPT_DATE).to_string())
}

/// Whether a kept deadline (yyyy-mm-dd) is before today; the deadline's own day is still open.
pub fn has_passed(deadline: &str, today: NaiveDate) -> bool {
    NaiveDate::parse_from_str(deadline, KEPT_DATE).is_ok_and(|day| day < today)
}

/// Today in Danish time.
pub fn today() -> NaiveDate {
    day_of(chrono::Utc::now().timestamp())
}

/// The Danish day of a moment.
fn day_of(at: i64) -> NaiveDate {
    dates::in_copenhagen(at).map_or(NaiveDate::MIN, |at| at.date())
}

/// Whether a post's text says it no longer takes applications.
pub fn says_closed(text: &str) -> bool {
    let text = text.to_lowercase();
    CLOSED_PHRASES.iter().any(|phrase| text.contains(phrase))
}

/// Whether a post's link (asked) now lands on its site's front page or search page (landed),
/// where it didn't point itself.
pub fn has_moved(asked: &str, landed: &str) -> bool {
    let (Ok(asked), Ok(landed)) = (Url::parse(asked), Url::parse(landed)) else {
        return false;
    };
    let front_page = |url: &Url| url.path().trim_matches('/').is_empty() && url.query().is_none();
    let search_page = |url: &Url| {
        let path_and_query = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_string(),
        };
        SEARCH_PAGES.iter().any(|start| path_and_query.starts_with(start))
    };
    (front_page(&landed) && !front_page(&asked)) || (search_page(&landed) && !search_page(&asked))
}

/// What a post's page, read from asked and landed on landed, says: closed when it says so or
/// has moved, open when it shows a post, else unknown (a page that needs its JavaScript shows
/// next to nothing without it).
pub fn judge_page(asked: &str, landed: &str, text: &str) -> Finding {
    if says_closed(text) {
        Finding::Closed(Closed::SaysClosed)
    } else if has_moved(asked, landed) {
        Finding::Closed(Closed::Moved)
    } else if page_reader::is_too_short(text) {
        Finding::Unknown("The page shows next to no text without its JavaScript.".to_string())
    } else {
        Finding::Open
    }
}

/// Reads the post at url to tell whether it is still open: a Jobnet post from Jobnet's API, a
/// LinkedIn or Indeed post in JobFinder's signed-in browser (unknown without one), any other as
/// its page, fetched under page_reader's rules.
pub async fn read_post(url: &str, readers: Readers<'_>) -> Finding {
    if let Some(id) = job_sites::jobnet::post_id(url) {
        return match job_sites::jobnet::find_post(&id).await {
            Ok(Some(page)) if says_closed(&page.text) => Finding::Closed(Closed::SaysClosed),
            Ok(Some(_)) => Finding::Open,
            Ok(None) => Finding::Closed(Closed::Gone),
            Err(why) => Finding::Unknown(why),
        };
    }
    if job_sites::is_signed_in_post(url) {
        if readers.signed_in.is_none() {
            return Finding::Unknown("It is read in JobFinder's signed-in browser, which isn't set up.".to_string());
        }
        return match job_sites::read_post(url, readers).await {
            Some(Ok(page)) => judge_page(url, &page.url, &page.text),
            Some(Err(why)) => Finding::Unknown(why),
            None => Finding::Unknown("JobFinder's signed-in browser couldn't read it.".to_string()),
        };
    }
    let resolved = job_sites::jobindex::post_link(url);
    let fetch_url = resolved.as_deref().unwrap_or(url);
    match page_reader::get_or_status(fetch_url, &page_reader::PAGE).await {
        Ok(Got::Page(fetched)) => match page_reader::to_text(&fetched.body, &fetched.content_type, &fetched.url) {
            Ok(page) => judge_page(url, &fetched.url, &page.text),
            Err(why) => Finding::Unknown(why),
        },
        Ok(Got::Status { status, .. }) if GONE_STATUSES.contains(&status) => Finding::Closed(Closed::Gone),
        Ok(Got::Status { status, .. }) => Finding::Unknown(format!("The page answered HTTP {status}.")),
        Err(why) => Finding::Unknown(why),
    }
}

/// What a check did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CheckReport {
    /// Posts read.
    pub read: usize,
    /// Jobs found closed, by their deadline or by reading their post.
    pub closed: usize,
    /// Posts that couldn't be read, so nothing was told of them.
    pub unknown: usize,
    /// The check was stopped in the task manager before it was done.
    pub stopped: bool,
}

impl CheckReport {
    /// Whether the check found or tried anything worth saying after a scan.
    pub fn did_anything(&self) -> bool {
        self.read > 0 || self.closed > 0 || self.unknown > 0
    }

    /// "Checked 12 saved jobs: 3 have closed, 1 couldn't be read."
    pub fn summary(&self) -> String {
        if !self.did_anything() {
            return "Every saved job was checked within the last day, and none has passed its deadline since."
                .to_string();
        }
        let jobs = if self.read == 1 { "post" } else { "posts" };
        let mut said = format!(
            "Checked whether your saved jobs are still open: read {} {jobs}, {} closed",
            self.read, self.closed
        );
        if self.unknown > 0 {
            said.push_str(&format!(", {} couldn't be read", self.unknown));
        }
        said.push('.');
        if self.stopped {
            said.push_str(" It was stopped before it was done.");
        }
        said
    }
}

/// Checks the user's saved matches that are open as far as the app knows (not rejected, not
/// found closed, not said to be open by the user): each one's deadline, then its post when it
/// wasn't read in READ_EVERY_SECONDS, pause apart. What couldn't be read is logged.
pub async fn check(context: &Context, pause: Duration) -> Result<CheckReport, ServiceError> {
    let db = context.db();
    let mut report = CheckReport::default();
    let today = today();
    let mut read_one = false;
    for saved in match_model::open_to_check(db, BY_HAND_OPEN).await? {
        // Paused in the task manager: the next check waits until it is resumed.
        running_work::until_resumed().await;
        if running_work::stop_asked() {
            report.stopped = true;
            break;
        }
        let now = chrono::Utc::now().timestamp();
        if deadline_passed(context, &saved, today).await? {
            match_model::close(db, saved.id, Closed::Deadline.as_str(), now).await?;
            report.closed += 1;
            continue;
        }
        let Some(url) = url_to_read(&saved, now) else {
            continue;
        };
        if read_one {
            tokio::select! {
                () = tokio::time::sleep(pause) => {}
                () = running_work::until_stop_asked() => {
                    report.stopped = true;
                    break;
                }
            }
        }
        read_one = true;
        // The time it is paused in the task manager doesn't count toward the read's limit.
        let finding = running_work::timeout_unpaused(READ_TIME_LIMIT, read_post(url, context.readers()))
            .await
            .unwrap_or_else(|_| Finding::Unknown(format!("It took over {} s to read.", READ_TIME_LIMIT.as_secs())));
        match_model::mark_open_checked(db, saved.id, now).await?;
        report.read += 1;
        match finding {
            Finding::Open => {}
            Finding::Closed(why) => {
                crate::say!("still_open: match {}: closed, {}.", saved.id, why.says());
                match_model::close(db, saved.id, why.as_str(), now).await?;
                report.closed += 1;
            }
            Finding::Unknown(why) => {
                crate::log_line!(
                    "still_open: match {} ({url}): couldn't tell whether it is open: {why}",
                    saved.id
                );
                report.unknown += 1;
            }
        }
    }
    Ok(report)
}

/// Whether saved's deadline has passed by today. A deadline not kept yet (a match saved before
/// the check) is read from its post and kept.
async fn deadline_passed(context: &Context, saved: &OpenToCheck, today: NaiveDate) -> Result<bool, ServiceError> {
    let deadline = match &saved.deadline {
        Some(deadline) => Some(deadline.clone()),
        None => {
            let read = deadline_of(&saved.job_text, saved.created_at);
            if let Some(deadline) = &read {
                match_model::save_deadline(context.db(), saved.id, deadline).await?;
            }
            read
        }
    };
    Ok(deadline.is_some_and(|deadline| has_passed(&deadline, today)))
}

/// saved's link, when it has one and it wasn't read in READ_EVERY_SECONDS.
fn url_to_read(saved: &OpenToCheck, now: i64) -> Option<&str> {
    let url = saved.job_url.as_deref().map(str::trim).filter(|url| !url.is_empty())?;
    let read_lately = saved
        .still_open_checked_at
        .is_some_and(|at| now - at < READ_EVERY_SECONDS);
    (!read_lately).then_some(url)
}

/// The check, by hand ("Check now" on Saved matches): what it found, in words.
pub async fn check_now(context: &Context) -> Result<String, ServiceError> {
    Ok(check(context, PAUSE_BETWEEN_READS).await?.summary())
}

/// The user saying saved match id's job is still open: it leaves the No longer open list, and
/// the check leaves it alone.
pub async fn keep_open(context: &Context, id: i64) -> Result<(), ServiceError> {
    if match_model::keep_open(context.db(), id, BY_HAND_OPEN).await? {
        Ok(())
    } else {
        Err(ServiceError::new(
            ErrorKind::NotFound,
            "That saved match was not found.",
        ))
    }
}

/// What a list row and a saved match's page say of whether its job is open: "Closed 2 Nov: the
/// deadline passed", "Deadline 15 Nov (in 3 days)", or "" when nothing is known.
pub fn describe(opening: &Opening, today: NaiveDate) -> String {
    if let Some(closed_at) = opening.closed_at {
        let why = Closed::of(&opening.closed_why).map_or("", Closed::says);
        let day = short_day(day_of(closed_at));
        return if why.is_empty() {
            format!("Closed {day}")
        } else {
            format!("Closed {day}: {why}")
        };
    }
    let Some(day) = opening
        .deadline
        .as_deref()
        .and_then(|deadline| NaiveDate::parse_from_str(deadline, KEPT_DATE).ok())
    else {
        return String::new();
    };
    let days = (day - today).num_days();
    let when = match days {
        ..0 => "passed".to_string(),
        0 => "today".to_string(),
        1 => "tomorrow".to_string(),
        days => format!("in {days} days"),
    };
    format!("Deadline {} ({when})", short_day(day))
}

/// "15 Nov", or "15 Nov 2025" in another year than this one.
fn short_day(day: NaiveDate) -> String {
    if day.format("%Y").to_string() == today().format("%Y").to_string() {
        day.format("%-d %b").to_string()
    } else {
        day.format("%-d %b %Y").to_string()
    }
}

/// Whether opening says the job is closed.
pub fn is_closed(opening: &Opening) -> bool {
    opening.closed_at.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use sqlx::Executor;

    fn day(text: &str) -> NaiveDate {
        NaiveDate::parse_from_str(text, KEPT_DATE).unwrap()
    }

    #[test]
    fn the_deadline_is_read_from_the_post_and_kept_as_iso() {
        let saved_at = dates::mktime_utc(10, 1, 2026);
        assert_eq!(
            deadline_of("Ansøgningsfrist: 15. november 2026", saved_at).as_deref(),
            Some("2026-11-15")
        );
        // Jobnet's API, as job_sites::jobnet writes it.
        assert_eq!(
            deadline_of("Ansøgningsfrist: 2026-11-30", saved_at).as_deref(),
            Some("2026-11-30")
        );
        assert_eq!(deadline_of("Ansøgningsfrist: løbende", saved_at), None);
        assert_eq!(deadline_of("We look forward to hearing from you.", saved_at), None);
    }

    #[test]
    fn a_deadline_passes_the_day_after() {
        assert!(!has_passed("2026-11-15", day("2026-11-15")));
        assert!(has_passed("2026-11-15", day("2026-11-16")));
        assert!(!has_passed("not a date", day("2026-11-16")));
    }

    #[test]
    fn a_page_that_says_so_is_closed() {
        let long = "Om jobbet ".repeat(50);
        let url = "https://www.jobindex.dk/vis-job/h123";
        assert_eq!(
            judge_page(url, url, &format!("Annoncen er udløbet.\n{long}")),
            Finding::Closed(Closed::SaysClosed)
        );
        assert_eq!(
            judge_page(url, url, "No longer accepting applications"),
            Finding::Closed(Closed::SaysClosed)
        );
        assert_eq!(
            judge_page(url, url, "Please note that this job ad has expired."),
            Finding::Closed(Closed::SaysClosed)
        );
        assert_eq!(judge_page(url, url, &long), Finding::Open);
        assert!(matches!(judge_page(url, url, "Loading…"), Finding::Unknown(_)));
    }

    #[test]
    fn a_link_that_lands_on_the_front_or_search_page_has_moved() {
        let post = "https://www.jobindex.dk/vis-job/h123";
        assert!(has_moved(post, "https://www.jobindex.dk/"));
        assert!(has_moved(post, "https://www.jobindex.dk/jobsoegning?q=rust"));
        assert!(has_moved("https://acme.dk/jobs/42", "https://acme.dk"));
        assert!(!has_moved(post, post));
        // Jobindex's robot ads redirect to the employer's own page.
        assert!(!has_moved(
            "https://www.jobindex.dk/jobannonce/r1",
            "https://acme.dk/career/42"
        ));
        assert!(!has_moved("https://acme.dk/", "https://acme.dk/"));
    }

    #[test]
    fn closed_why_is_kept_and_read_back() {
        for why in Closed::ALL {
            assert_eq!(Closed::of(why.as_str()), Some(why));
        }
        assert_eq!(Closed::of(BY_HAND_OPEN), None);
    }

    #[test]
    fn a_row_says_when_the_job_closes_or_closed() {
        let today = day("2026-11-12");
        let deadline = |deadline: &str| Opening {
            deadline: Some(deadline.to_string()),
            ..Opening::default()
        };
        assert_eq!(describe(&Opening::default(), today), "");
        assert!(describe(&deadline("2026-11-15"), today).ends_with("(in 3 days)"));
        assert!(describe(&deadline("2026-11-13"), today).ends_with("(tomorrow)"));
        assert!(describe(&deadline("2026-11-12"), today).ends_with("(today)"));
        let closed = Opening {
            closed_at: Some(dates::mktime_utc(11, 2, 2026) + 12 * 3600),
            closed_why: Closed::Gone.as_str().to_string(),
            ..deadline("2026-11-15")
        };
        assert!(
            describe(&closed, today).starts_with("Closed 2 Nov"),
            "{}",
            describe(&closed, today)
        );
        assert!(describe(&closed, today).ends_with("the post was taken down"));
    }

    #[test]
    fn a_post_read_lately_is_not_read_again() {
        let now = 1_000_000;
        let saved = OpenToCheck {
            id: 1,
            job_url: Some("https://acme.dk/jobs/1".to_string()),
            job_text: String::new(),
            created_at: 0,
            deadline: None,
            still_open_checked_at: Some(now - 60),
        };
        assert_eq!(url_to_read(&saved, now), None);
        let long_ago = OpenToCheck {
            still_open_checked_at: Some(now - READ_EVERY_SECONDS),
            ..saved.clone()
        };
        assert_eq!(url_to_read(&long_ago, now), Some("https://acme.dk/jobs/1"));
        let no_link = OpenToCheck {
            job_url: Some(" ".to_string()),
            still_open_checked_at: None,
            ..saved
        };
        assert_eq!(url_to_read(&no_link, now), None);
    }

    /// Two matches without links: one whose deadline passed, one whose deadline is to come.
    const SEED: &str = r#"
        INSERT INTO matches (id, job_title, job_text, score, points, max_points, tag, created_at)
        VALUES (1, 'Passed', 'Ansøgningsfrist: 2020-01-15', 0.9, 9, 10, 'top', 1577836800),
               (2, 'To come', 'Ansøgningsfrist: 2999-01-15', 0.8, 8, 10, 'top', 1577836800),
               (3, 'Rejected', 'Ansøgningsfrist: 2020-01-15', 0.7, 7, 10, 'top', 1577836800);
        UPDATE matches SET rejected_at = 1 WHERE id = 3;
    "#;

    #[tokio::test]
    async fn a_passed_deadline_closes_the_job_without_reading_it() {
        let pool = db::open_in_memory().await.unwrap();
        pool.execute(sqlx::raw_sql(SEED)).await.unwrap();
        let context = Context::new(
            pool.clone(),
            std::sync::Arc::new(crate::taxonomy::Taxonomy::load()),
            crate::account::key_vault::KeyVault::with_key_in_memory(),
        );
        let report = check(&context, Duration::ZERO).await.unwrap();
        assert_eq!(
            report,
            CheckReport {
                closed: 1,
                ..CheckReport::default()
            }
        );
        let closed = match_model::best_first(&pool, 10, "", match_model::CLOSED)
            .await
            .unwrap();
        assert_eq!(closed.rows.iter().map(|row| row.id).collect::<Vec<_>>(), vec![1]);
        assert_eq!(closed.rows[0].opening.closed_why, "deadline");
        let all = match_model::best_first(&pool, 10, "", "all").await.unwrap();
        assert_eq!(all.rows.iter().map(|row| row.id).collect::<Vec<_>>(), vec![2]);
        assert_eq!(all.rows[0].opening.deadline.as_deref(), Some("2999-01-15"));
        // Said to be open by hand: back in All, and left alone by the next check.
        keep_open(&context, 1).await.unwrap();
        assert_eq!(check(&context, Duration::ZERO).await.unwrap(), CheckReport::default());
        let all = match_model::best_first(&pool, 10, "", "all").await.unwrap();
        assert_eq!(all.rows.len(), 2);
    }
}
