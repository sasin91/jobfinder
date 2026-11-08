//! The likely disqualifiers of a match, as the result prints them above the requirements: where
//! the post asks the candidate to live or show up when they likely don't, a language they don't
//! speak (one the post asks for, or the one the job is done in), and a career level their years
//! don't fit (a student job for a senior). A workplace that still needs checking has its own
//! notice. Each line gives the post's words and the reason.

use super::match_model::SavedItem;
use super::matcher::{self, Groups, Verdicts};

/// The heading and note when the only disqualifier is where the candidate lives.
pub const LOCATION_HEADING: &str = "Likely disqualifier: where you live";
pub const LOCATION_NOTE: &str = "The score is capped below medium for this, and a job scan skips automatic tailoring.";
pub const LOCATION_CHECK_HEADING: &str = "Workplace to check";
pub const LOCATION_CHECK_NOTE: &str =
    "Confirm the commute or residency requirement. Job scans skip automatic tailoring while this needs checking.";
/// The heading and note when the only disqualifier is a language the candidate doesn't speak.
pub const LANGUAGE_HEADING: &str = "Likely disqualifier: a language you don't speak";
pub const LANGUAGE_NOTE: &str =
    "The score is capped below medium for this, and a job scan doesn't write a résumé or application for it.";
/// The heading when the only disqualifier is the post's career level.
pub const LEVEL_HEADING: &str = "Likely disqualifier: the post's level";
/// The heading when there are several kinds.
pub const BOTH_HEADING: &str = "Likely disqualifiers";
/// The note on a level, alone or with others.
pub const NOTE: &str = "The score is capped below medium for this.";

#[derive(Debug, Clone, PartialEq)]
pub struct DisqualifierLine {
    pub text: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DisqualifierNotice {
    pub heading: &'static str,
    pub note: &'static str,
    pub lines: Vec<DisqualifierLine>,
}

impl DisqualifierNotice {
    /// A match just made: its groups (matcher::criteria) and combined verdicts.
    pub fn from(groups: &Groups, verdicts: &Verdicts) -> Option<Self> {
        let missed = matcher::missed_disqualifiers(groups, verdicts);
        let criteria: Vec<&str> = missed.iter().map(|(criterion, _)| *criterion).collect();
        let lines = missed
            .into_iter()
            .map(|(_, item)| DisqualifierLine {
                text: item.text.clone(),
                reason: verdicts.get(&item.id).map(|v| v.reason.clone()).unwrap_or_default(),
            })
            .collect();
        notice(&criteria, lines).or_else(|| {
            let lines = groups
                .get(matcher::LOCATION)
                .into_iter()
                .flatten()
                .filter_map(|item| {
                    let verdict = verdicts.get(&item.id)?;
                    (verdict.verdict == "partial").then(|| DisqualifierLine {
                        text: item.text.clone(),
                        reason: verdict.reason.clone(),
                    })
                })
                .collect();
            location_check(lines)
        })
    }

    /// A saved match's items (match_items).
    pub fn from_saved(items: &[SavedItem]) -> Option<Self> {
        let missed: Vec<&SavedItem> = items.iter().filter(|item| is_missed_disqualifier(item)).collect();
        let criteria: Vec<&str> = missed.iter().map(|item| item.criterion.as_str()).collect();
        let lines = missed
            .into_iter()
            .map(|item| DisqualifierLine {
                text: item.text.clone(),
                reason: item.reason.clone(),
            })
            .collect();
        notice(&criteria, lines).or_else(|| {
            let lines = items
                .iter()
                .filter(|item| item.criterion == matcher::LOCATION && item.verdict == "partial")
                .map(|item| DisqualifierLine {
                    text: item.text.clone(),
                    reason: item.reason.clone(),
                })
                .collect();
            location_check(lines)
        })
    }
}

/// Whether a saved match item is a disqualifier the candidate missed, one that capped the score: a
/// location or language nobody weighed is saved as partial (ai_match::unweighed).
pub fn is_missed_disqualifier(item: &SavedItem) -> bool {
    matcher::DISQUALIFIERS.contains(&item.criterion.as_str()) && item.verdict == "missing"
}

/// A required workplace needs to be feasible, and the candidate must speak the working language.
pub fn blocks_tailoring(items: &[SavedItem]) -> bool {
    items.iter().any(|item| {
        (item.criterion == matcher::LOCATION && item.verdict != "met")
            || (item.criterion == matcher::LANGUAGE && is_missed_disqualifier(item))
    })
}

fn location_check(lines: Vec<DisqualifierLine>) -> Option<DisqualifierNotice> {
    (!lines.is_empty()).then_some(DisqualifierNotice {
        heading: LOCATION_CHECK_HEADING,
        note: LOCATION_CHECK_NOTE,
        lines,
    })
}

/// The notice for the missed lines, under the criterion of each.
fn notice(criteria: &[&str], lines: Vec<DisqualifierLine>) -> Option<DisqualifierNotice> {
    let mut kinds = criteria.to_vec();
    kinds.sort_unstable();
    kinds.dedup();
    let (heading, note) = match kinds.as_slice() {
        [matcher::LOCATION] => (LOCATION_HEADING, LOCATION_NOTE),
        [matcher::LANGUAGE] => (LANGUAGE_HEADING, LANGUAGE_NOTE),
        [matcher::LEVEL] => (LEVEL_HEADING, NOTE),
        _ => (BOTH_HEADING, NOTE),
    };
    (!lines.is_empty()).then_some(DisqualifierNotice { heading, note, lines })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn missing(criterion: &str, text: &str) -> SavedItem {
        SavedItem {
            criterion: criterion.into(),
            text: text.into(),
            verdict: "missing".into(),
            decided_by: "llm".into(),
            reason: String::new(),
            evidence: String::new(),
        }
    }

    #[test]
    fn a_missed_language_has_its_own_heading() {
        let notice = DisqualifierNotice::from_saved(&[missing(matcher::LANGUAGE, "Finnish")]).unwrap();
        assert_eq!(notice.heading, LANGUAGE_HEADING);
        assert_eq!(notice.note, LANGUAGE_NOTE);
        assert_eq!(notice.lines[0].text, "Finnish");
    }

    #[test]
    fn several_kinds_share_one_heading() {
        let items = [
            missing(matcher::LOCATION, "On site in Hämeenlinna"),
            missing(matcher::LANGUAGE, "Finnish"),
        ];
        let notice = DisqualifierNotice::from_saved(&items).unwrap();
        assert_eq!(notice.heading, BOTH_HEADING);
        assert_eq!(notice.lines.len(), 2);
    }

    #[test]
    fn two_locations_keep_the_location_heading() {
        let items = [
            missing(matcher::LOCATION, "Aarhus"),
            missing(matcher::LOCATION, "Odense"),
        ];
        assert_eq!(
            DisqualifierNotice::from_saved(&items).unwrap().heading,
            LOCATION_HEADING
        );
    }

    #[test]
    fn a_missed_language_or_an_unconfirmed_workplace_stops_the_writing() {
        assert!(blocks_tailoring(&[missing(matcher::LANGUAGE, "Finnish")]));
        assert!(blocks_tailoring(&[missing(matcher::LOCATION, "Aarhus")]));
        let unconfirmed = SavedItem {
            verdict: "partial".into(),
            ..missing(matcher::LOCATION, "Hybrid work")
        };
        let notice = DisqualifierNotice::from_saved(std::slice::from_ref(&unconfirmed)).unwrap();
        assert_eq!(notice.heading, LOCATION_CHECK_HEADING);
        assert!(blocks_tailoring(&[unconfirmed]));
        let spoken = SavedItem {
            verdict: "met".into(),
            ..missing(matcher::LANGUAGE, "Danish")
        };
        assert!(!blocks_tailoring(&[spoken]));
    }
}
