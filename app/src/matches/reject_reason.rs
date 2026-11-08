//! Why a match was rejected (match_rejections):
//! one or more of a fixed list, so reasons that keep coming back can be counted, each with the
//! person's own note; 'other' needs one.

/// The reasons, in the order the form lists them.
pub const REASONS: [(&str, &str); 10] = [
    ("applied", "Already applied to this position"),
    ("commute", "Too long commute"),
    ("salary", "Salary"),
    ("tech_stack", "Tech stack"),
    ("seniority", "Seniority level"),
    ("language", "Language"),
    ("company", "The company"),
    ("job_type", "Hours or contract"),
    ("irrelevant", "Irrelevant"),
    ("other", "Other"),
];

/// Longest note per reason, in characters.
pub const NOTE_MAX: usize = 255;

/// One reason a match was rejected, with its note ('' when none).
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Rejection {
    pub reason: String,
    pub note: String,
}

fn label(reason: &str) -> Option<&'static str> {
    REASONS
        .iter()
        .find(|(value, _)| *value == reason)
        .map(|(_, label)| *label)
}

/// What's wrong with the reasons ticked and the notes by reason as posted, for the person to
/// read, or '' when they can be saved.
pub fn problem(reasons: &[String], notes: &[(String, String)]) -> String {
    if reasons.is_empty() || reasons.iter().any(|reason| label(reason).is_none()) {
        return "Tick why you are not interested.".to_string();
    }
    let rows = rows(reasons, notes);
    if rows.iter().any(|row| row.reason == "other" && row.note.is_empty()) {
        return "Say in a few words what the other reason is.".to_string();
    }
    if rows.iter().any(|row| row.note.chars().count() > NOTE_MAX) {
        return format!("Keep each note under {NOTE_MAX} characters.");
    }
    String::new()
}

/// The rejections to save, one per reason ticked (each once) with its note, trimmed.
pub fn rows(reasons: &[String], notes: &[(String, String)]) -> Vec<Rejection> {
    let mut rows: Vec<Rejection> = Vec::new();
    for reason in reasons {
        if rows.iter().any(|row| &row.reason == reason) {
            continue;
        }
        let note = notes
            .iter()
            .find(|(name, _)| name == reason)
            .map(|(_, note)| note.trim())
            .unwrap_or("");
        rows.push(Rejection {
            reason: reason.clone(),
            note: note.to_string(),
        });
    }
    rows
}

/// "Too long commute (1h each way), Salary, Recruiter agency": each rejection by its label and
/// note, Other by its note alone; '' for none.
pub fn describe(rejections: &[Rejection]) -> String {
    rejections.iter().map(one).collect::<Vec<_>>().join(", ")
}

fn one(rejection: &Rejection) -> String {
    let label = label(&rejection.reason)
        .map(str::to_string)
        .unwrap_or_else(|| rejection.reason.clone());
    let note = rejection.note.trim();
    if note.is_empty() {
        label
    } else if rejection.reason == "other" {
        note.to_string()
    } else {
        format!("{label} ({note})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn reasons_are_checked_and_described() {
        assert_eq!(problem(&[], &[]), "Tick why you are not interested.");
        assert_eq!(problem(&strings(&["nope"]), &[]), "Tick why you are not interested.");
        assert_eq!(
            problem(&strings(&["other"]), &[]),
            "Say in a few words what the other reason is."
        );
        let notes = vec![
            ("commute".to_string(), " 1h each way ".to_string()),
            ("other".to_string(), "Agency".to_string()),
        ];
        assert_eq!(problem(&strings(&["commute", "salary", "other"]), &notes), "");
        let rows = rows(&strings(&["commute", "salary", "commute", "other"]), &notes);
        assert_eq!(describe(&rows), "Too long commute (1h each way), Salary, Agency");
        assert_eq!(problem(&strings(&["applied"]), &[]), "");
        let applied = super::rows(&strings(&["applied"]), &[]);
        assert_eq!(describe(&applied), "Already applied to this position");
    }
}
