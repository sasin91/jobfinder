//! A saved match as a row of the Match screen's lists: its id, tag, title and meta line, ready to
//! print.

use super::match_model::{APPLIED_STATUSES, ListedMatch};
use super::{dates, match_score, reject_reason, still_open};

/// How a match of a tailored résumé is named (its cv_name ends with this).
pub const TAILORED_CV: &str = "(tailored résumé)";

#[derive(Debug, Clone, PartialEq)]
pub struct SavedMatchRow {
    pub id: i64,
    pub tag: String,
    pub title: String,
    /// "81% good · likely disqualifier: Located close to Copenhagen".
    pub about: String,
    /// The score, 0 to 100.
    pub percent: i64,
    /// What about says after the score: "likely disqualifier: Located close to Copenhagen", or "".
    pub notes: String,
    pub saved_at: String,
    pub saved_on: String,
    /// "application · applied · on Jobnet", ending "rejected: Too long commute" when rejected.
    pub done: String,
    /// "Deadline 15 Nov (in 3 days)" or "Closed 2 Nov: the post was taken down" (still_open), ""
    /// when nothing is known.
    pub opening: String,
    /// The job no longer takes applications.
    pub closed: bool,
}

impl SavedMatchRow {
    pub fn from(m: &ListedMatch) -> Self {
        let mut notes = Vec::new();
        if m.cv_name.ends_with(TAILORED_CV) {
            notes.push("tailored résumé".to_string());
        }
        if let Some(missed) = &m.missed_disqualifier {
            notes.push(format!("likely disqualifier: {missed}"));
        }
        let about: Vec<String> = std::iter::once(match_score::short_label(m.score, &m.tag))
            .chain(notes.iter().cloned())
            .collect();
        SavedMatchRow {
            id: m.id,
            tag: m.tag.clone(),
            title: title(m),
            about: about.join(" · "),
            percent: match_score::percent(m.score),
            notes: notes.join(" · "),
            saved_at: dates::iso(m.created_at),
            saved_on: dates::day(m.created_at),
            done: done(m).join(" · "),
            opening: still_open::describe(&m.opening, still_open::today()),
            closed: still_open::is_closed(&m.opening),
        }
    }
}

/// "Job title · Company", or "Untitled" when the post named neither.
fn title(m: &ListedMatch) -> String {
    let parts: Vec<&str> = [m.job_title.as_str(), m.company.as_str()]
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect();
    if parts.is_empty() {
        "Untitled".to_string()
    } else {
        parts.join(" · ")
    }
}

/// What has been done for the match: written application, résumé, applied, logged on Jobnet,
/// rejected and why.
fn done(m: &ListedMatch) -> Vec<String> {
    let status = m.jobnet_status.as_deref();
    let applied = status.is_some_and(|status| APPLIED_STATUSES.contains(&status));
    let mut done: Vec<String> = [
        ("application", m.has_application),
        ("résumé", m.has_resume),
        ("applied", applied),
        ("samtale", status == Some("Samtale")),
        ("afslag", status == Some("Afslag")),
        ("fået job", status == Some("FaaetJob")),
        ("on Jobnet", m.jobnet_logged_at.is_some()),
        (
            "update on Jobnet",
            m.jobnet_logged_at.is_some() && status.is_some() && m.jobnet_logged_status.as_deref() != status,
        ),
    ]
    .into_iter()
    .filter(|(_, is)| *is)
    .map(|(label, _)| label.to_string())
    .collect();
    if m.rejected_at.is_some() {
        let why = reject_reason::describe(&m.rejections);
        done.push(if why.is_empty() {
            "rejected".to_string()
        } else {
            format!("rejected: {why}")
        });
    }
    done
}
