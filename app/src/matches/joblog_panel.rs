//! A saved match's Jobnet joblog entry (joblog::read) as the Apply screen prints it: Jobnet's
//! sections with each field to copy, a choice with its options and the one to pick, the three
//! PDFs, the "Fill it in with an AI" prompt and whether the candidate has entered it in their
//! joblog.
//!
//! Each option carries what the AI prompt says for it; js/app.js reads typed values from the
//! inputs, edits included.

use super::dates;
use super::joblog::{self, Entry, Field, File, Value};

// The candidate picks how far they are here, saved with the match (joblog_status).
pub const CHOOSE: &str = "searchStatus";
pub const CHOOSE_HINT: &str = "Pick it here as you get along: it's saved with the match.";
pub const CHECKBOX_YES: &str = "Ja";
pub const CHECKBOX_NO: &str = "Nej";
pub const CHARACTERS_LEFT: &str = "Antal tegn tilbage:";
pub const LOGGED_LABEL: &str = "I've entered this job in my joblog on Jobnet";
pub const LOGGED_ON: &str = "Entered on";

#[derive(Debug, Clone, PartialEq)]
pub struct JoblogPanel {
    pub id: i64,
    pub sections: Vec<PanelSection>,
    pub files: Vec<PanelFile>,
    /// "Jobansøgning: Skriv ansøgningen først (Write application)." for each PDF that can't be
    /// made yet.
    pub missing: Vec<String>,
    pub logged: Logged,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PanelSection {
    pub heading: &'static str,
    pub fields: Vec<PanelField>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PanelField {
    pub name: &'static str,
    pub label: &'static str,
    pub required: bool,
    pub hint: String,
    /// Picked from options rather than typed.
    pub choice: bool,
    /// The choice the candidate makes here (CHOOSE).
    pub choose: bool,
    pub options: Vec<PanelOption>,
    pub value: String,
    pub multiline: bool,
    pub numeric: bool,
    /// Characters left of Jobnet's limit, for the notes.
    pub left: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PanelOption {
    pub value: &'static str,
    pub text: &'static str,
    pub chosen: bool,
    pub prompt: String,
}

/// A PDF button: its title says what it is, or why it can't be made yet.
#[derive(Debug, Clone, PartialEq)]
pub struct PanelFile {
    pub kind: &'static str,
    pub label: String,
    pub ready: bool,
    pub title: &'static str,
}

/// Whether, and when (ISO time or ''), it is in the candidate's joblog, and the day applied as
/// the date field says it (joblog::applied_on), which the page copies there when the status is
/// saved.
#[derive(Debug, Clone, PartialEq)]
pub struct Logged {
    pub checked: bool,
    pub at: String,
    pub applied_on: String,
}

impl JoblogPanel {
    pub fn from(id: i64, entry: &Entry) -> Self {
        Self::from_status(id, entry, None)
    }

    /// Builds the panel for the Apply screen, using `actual_status` (when given) so JobFinder's
    /// full status list (including `Afslag` and `Fået job`) is selectable even though Jobnet's
    /// create form (`entry`) only carries the three `FORM_STATUSES` radio buttons.
    pub fn from_status(id: i64, entry: &Entry, actual_status: Option<&str>) -> Self {
        JoblogPanel {
            id,
            sections: entry
                .sections
                .iter()
                .map(|section| PanelSection {
                    heading: section.heading,
                    fields: section.fields.iter().map(|f| field(f, actual_status)).collect(),
                })
                .collect(),
            files: entry.files.iter().map(file).collect(),
            missing: entry
                .files
                .iter()
                .filter(|file| !file.ready)
                .map(|file| format!("{}: {}", file.label, file.missing))
                .collect(),
            logged: Logged::from(entry.logged_at, applied_on(entry)),
        }
    }
}

impl Logged {
    pub fn from(logged_at: Option<i64>, applied_on: String) -> Self {
        Logged {
            checked: logged_at.is_some(),
            at: logged_at.map(dates::iso).unwrap_or_default(),
            applied_on,
        }
    }
}

/// The entry's submissionDate value.
fn applied_on(entry: &Entry) -> String {
    entry
        .sections
        .iter()
        .flat_map(|section| &section.fields)
        .find(|field| field.name == "submissionDate")
        .map(|field| field.value.text().to_string())
        .unwrap_or_default()
}

fn field(field: &Field, actual_status: Option<&str>) -> PanelField {
    let choice = matches!(field.kind, "radio" | "checkbox");
    let choose = field.name == CHOOSE;
    let multiline = field.kind == "textarea";
    let value = if choice {
        String::new()
    } else {
        field.value.text().to_string()
    };
    PanelField {
        name: field.name,
        label: field.label,
        required: field.required,
        hint: if choose {
            CHOOSE_HINT.to_string()
        } else {
            field.hint.clone().unwrap_or_default()
        },
        choice,
        choose,
        options: if choice {
            options(field, choose, actual_status)
        } else {
            Vec::new()
        },
        left: multiline.then(|| joblog::MAX_NOTES.saturating_sub(value.chars().count())),
        value,
        multiline,
        numeric: field.kind == "number",
    }
}

fn options(field: &Field, choose: bool, actual_status: Option<&str>) -> Vec<PanelOption> {
    if field.kind == "checkbox" {
        let ticked = field.value == Value::Ticked(true);
        return vec![
            option("yes", CHECKBOX_YES, ticked, "tick it".to_string()),
            option("no", CHECKBOX_NO, !ticked, "leave it unticked".to_string()),
        ];
    }
    if choose {
        let chosen = actual_status.unwrap_or(field.value.text());
        return joblog::SEARCH_STATUSES
            .iter()
            .map(|&(value, text)| {
                let prompt = match value {
                    "Afslag" => "choose \"Søgt\" and tick \"Afsluttet\" on the Jobnet card".to_string(),
                    "FaaetJob" => "choose \"Samtale\" and tick \"Afsluttet\" on the Jobnet card".to_string(),
                    _ => format!("choose \"{text}\""),
                };
                option(value, text, chosen == value, prompt)
            })
            .collect();
    }
    field
        .options
        .map(|choices| choices.0)
        .unwrap_or_default()
        .iter()
        .map(|&(value, text)| option(value, text, field.value.text() == value, format!("choose \"{text}\"")))
        .collect()
}

fn option(value: &'static str, text: &'static str, chosen: bool, prompt: String) -> PanelOption {
    PanelOption {
        value,
        text,
        chosen,
        prompt,
    }
}

fn file(file: &File) -> PanelFile {
    PanelFile {
        kind: file.kind,
        label: format!("{} (PDF)", file.label),
        ready: file.ready,
        title: if file.ready {
            file.note.unwrap_or("")
        } else {
            file.missing
        },
    }
}

/// What the AI prompt starts with: what to fill in, and how Jobnet's fields want it.
const PROMPT_START: [&str; 3] = [
    "Fill in a new entry in my joblog on Jobnet (jobnet.dk, \"Opret joblog\"). I am signed in. Use exactly these values, field by field, in Jobnet's order.",
    "Stilling eller arbejdsområde, Virksomhedens navn and Postnummer og by only keep a value picked from Jobnet's suggestions; typed text is cleared. Type the value and pick the matching suggestion; if none shows, search for a shorter part (\"Backend\" for \"Backend-udvikler\" finds \"Softwareudvikler, backend\"), pick the closest and tell me which you picked.",
    "Antal timer pr. uge only shows for Deltid. Ansøgningsfrist is typed as dd-mm-yyyy.",
];
/// What the AI prompt ends with: the PDFs are the candidate's, and it stops before saving.
const PROMPT_END: &str = "Don't upload any files: I'll add the application, CV and job ad PDFs myself. Don't press Gem: stop when the form is filled and tell me what you couldn't fill.";

/// The prompt for an AI that drives the browser (Claude in Chrome and the like): every value as
/// the panel shows it now, edits included, by section: (heading, [(label, value)]), where a
/// choice's value is what its chosen option says to do (PanelOption::prompt). It stops before
/// saving and leaves the PDFs to the candidate, since such an agent can't pick files on the
/// computer.
pub fn ai_prompt(sections: &[(&str, Vec<(&str, String)>)]) -> String {
    let mut lines: Vec<String> = PROMPT_START.iter().map(|line| line.to_string()).collect();
    for (heading, fields) in sections.iter().filter(|(_, fields)| !fields.is_empty()) {
        lines.push(String::new());
        lines.push(format!("{heading}:"));
        for (label, value) in fields {
            let value = value.trim();
            if value.is_empty() {
                lines.push(format!("- {label}: leave empty"));
            } else if value.contains('\n') {
                lines.push(format!("- {label}:"));
                lines.extend(value.split('\n').map(|line| format!("    {line}")));
            } else {
                lines.push(format!("- {label}: {value}"));
            }
        }
    }
    lines.push(String::new());
    lines.push(PROMPT_END.to_string());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_logged_entry_says_when() {
        assert_eq!(
            Logged::from(Some(1_760_000_000), "09-10-2025".to_string()),
            Logged {
                checked: true,
                at: "2025-10-09T08:53:20Z".to_string(),
                applied_on: "09-10-2025".to_string(),
            }
        );
        assert!(!Logged::from(None, String::new()).checked);
    }

    #[test]
    fn the_prompt_lists_each_value_and_says_what_is_empty() {
        let prompt = ai_prompt(&[
            (
                "Job",
                vec![
                    ("Stilling", "Backend".to_string()),
                    ("Adresse", " ".to_string()),
                    ("Noter", "Score 80\nPHP".to_string()),
                ],
            ),
            ("Empty", Vec::new()),
        ]);
        assert!(
            prompt.contains("\nJob:\n- Stilling: Backend\n- Adresse: leave empty\n- Noter:\n    Score 80\n    PHP\n")
        );
        assert!(!prompt.contains("Empty:"));
        assert!(prompt.ends_with(PROMPT_END));
    }
}
