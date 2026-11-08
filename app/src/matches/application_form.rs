//! An employer's own application form (Recruitee, Teamtailor, Greenhouse, Workday…), filled in
//! by the extension from a saved match, best effort. The extension reads the form's fields as
//! the browser shows them (their labels, kinds and options) and asks what goes in each. The
//! contact fields come from the profile, the uploads are the match's PDFs (the written
//! application, the tailored résumé or the CV), a cover letter box gets the written
//! application, and the questions ("What made you interested in Famly?", "Which team?") are
//! answered by the form's model, from the CV, the post and the application.
//! A consent box is never ticked, and nothing is sent: the candidate reads the form and sends it.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::document_writer::{self, WriteError};
use super::match_model::SavedMatch;
use super::pdf_writer;
use crate::llm::Effort;
use crate::matches::llm_key::KeyedLlm;
use crate::profile::Contact;

/// Most fields of one form answered at once: a long form's, with room to spare. A page with more
/// is not one application form.
pub const MAX_FIELDS: usize = 80;
/// Longest label kept, in characters: a question with a paragraph of explanation.
const MAX_LABEL: usize = 600;
/// Most options kept of one choice: a list of countries is no question for the model.
const MAX_OPTIONS: usize = 60;
/// Longest option kept, in characters.
const MAX_OPTION: usize = 200;
/// The longest the answers may take, in tokens: a paragraph or two for each of a form's
/// questions.
const ANSWER_TOKENS: u32 = 6144;

/// What a field is, as the extension tells it from the element.
pub mod kind {
    pub const TEXT: &str = "text";
    pub const TEXTAREA: &str = "textarea";
    pub const EMAIL: &str = "email";
    pub const TEL: &str = "tel";
    pub const URL: &str = "url";
    pub const NUMBER: &str = "number";
    pub const DATE: &str = "date";
    /// A drop-down: one of its options.
    pub const SELECT: &str = "select";
    /// Radio buttons: one of their options.
    pub const RADIO: &str = "radio";
    /// Checkboxes sharing a question: any of their options.
    pub const CHECKBOXES: &str = "checkboxes";
    /// One checkbox: yes or no.
    pub const CHECKBOX: &str = "checkbox";
    /// A file upload.
    pub const FILE: &str = "file";
}

/// One field of the form, as the extension read it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(default)]
pub struct FormField {
    /// The extension's name for it, answered back with what goes in it.
    pub key: String,
    /// The question or label next to it, as shown.
    pub label: String,
    /// One of kind's.
    pub kind: String,
    pub required: bool,
    /// A choice's options, as shown.
    pub options: Vec<String>,
    /// What the page calls it behind the label: its autocomplete, name and id, and an upload's
    /// accepted file types, lowercase.
    pub hint: String,
}

/// What goes in a field.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FieldAnswer {
    pub key: String,
    /// The text to put in; for a choice the option as shown (checkboxes: one per line); for a
    /// checkbox "yes" to tick it. "" for an upload.
    pub value: String,
    /// For an upload, the match's PDF that goes in it (pdf_writer::KINDS).
    pub file: Option<&'static str>,
    /// Where it came from, to say next to it: "your profile", "written by the model".
    pub from: &'static str,
}

/// A field left for the candidate, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LeftField {
    pub key: String,
    pub why: String,
}

/// What the form gets: the answers, and the fields left for the candidate.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FormAnswers {
    pub answers: Vec<FieldAnswer>,
    pub left: Vec<LeftField>,
}

/// Where an answer came from.
const FROM_PROFILE: &str = "your profile";
const FROM_APPLICATION: &str = "the written application";
const FROM_RESUME: &str = "the tailored résumé";
const FROM_CV: &str = "the CV the match was made with";
const FROM_MODEL: &str = "written by the model: read it";

/// The fields as the extension sent them, made safe to work on: the first MAX_FIELDS with a key
/// and a known kind, labels and options cut to their longest, without repeated keys.
pub fn cleaned(fields: Vec<FormField>) -> Vec<FormField> {
    let known = [
        kind::TEXT,
        kind::TEXTAREA,
        kind::EMAIL,
        kind::TEL,
        kind::URL,
        kind::NUMBER,
        kind::DATE,
        kind::SELECT,
        kind::RADIO,
        kind::CHECKBOXES,
        kind::CHECKBOX,
        kind::FILE,
    ];
    let mut seen = HashSet::new();
    fields
        .into_iter()
        .filter(|field| !field.key.is_empty() && known.contains(&field.kind.as_str()))
        .filter(|field| seen.insert(field.key.clone()))
        .take(MAX_FIELDS)
        .map(|field| FormField {
            label: cut(field.label.trim(), MAX_LABEL),
            options: field
                .options
                .iter()
                .map(|option| cut(option.trim(), MAX_OPTION))
                .filter(|option| !option.is_empty())
                .take(MAX_OPTIONS)
                .collect(),
            hint: field.hint.to_lowercase(),
            ..field
        })
        .collect()
}

fn cut(text: &str, longest: usize) -> String {
    text.chars().take(longest).collect()
}

/// A contact field a form asks for, which the profile answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContactPart {
    FirstName,
    LastName,
    FullName,
    Email,
    Phone,
    Location,
    LinkedIn,
    GitHub,
    Website,
}

/// The label as compared: lowercase, without the required star and a trailing colon.
fn plain_label(label: &str) -> String {
    label
        .to_lowercase()
        .replace('*', "")
        .trim()
        .trim_end_matches(':')
        .trim()
        .to_string()
}

/// Whether text has word as a whole word (letters and digits around it end it).
fn has_word(text: &str, word: &str) -> bool {
    text.split(|c: char| !c.is_alphanumeric() && c != '-')
        .any(|part| part == word)
}

/// Whether text mentions word: as a whole word, or a phrase of words anywhere.
fn mentions(text: &str, word: &str) -> bool {
    if word.contains(' ') {
        text.contains(word)
    } else {
        has_word(text, word)
    }
}

/// The contact field a field asks for, from its autocomplete, name or id (hint) or its label.
/// Only a short label counts: "Name" is the candidate's name, "Why this team, name one thing?"
/// is a question.
fn contact_part(field: &FormField) -> Option<ContactPart> {
    if !matches!(field.kind.as_str(), kind::TEXT | kind::EMAIL | kind::TEL | kind::URL) {
        return None;
    }
    let label = plain_label(&field.label);
    // A label longer than this is a question, not a contact field's name.
    const LONGEST_CONTACT_LABEL: usize = 40;
    let short = label.chars().count() <= LONGEST_CONTACT_LABEL;
    let hint = field.hint.as_str();
    let either = |words: &[&str]| {
        words
            .iter()
            .any(|word| has_word(hint, word) || (short && mentions(&label, word)))
    };
    if either(&["given-name", "first-name", "firstname", "first_name", "fornavn"])
        || (short && label.contains("first name"))
    {
        return Some(ContactPart::FirstName);
    }
    if either(&[
        "family-name",
        "last-name",
        "lastname",
        "last_name",
        "surname",
        "efternavn",
    ]) || (short && label.contains("last name"))
    {
        return Some(ContactPart::LastName);
    }
    if field.kind == kind::EMAIL || either(&["email", "e-mail", "e-mailadresse"]) {
        return Some(ContactPart::Email);
    }
    if field.kind == kind::TEL || either(&["phone", "tel", "telefon", "mobil", "mobile", "telefonnummer"]) {
        return Some(ContactPart::Phone);
    }
    if either(&["linkedin"]) {
        return Some(ContactPart::LinkedIn);
    }
    if either(&["github"]) {
        return Some(ContactPart::GitHub);
    }
    if either(&["website", "portfolio", "homepage", "hjemmeside"]) {
        return Some(ContactPart::Website);
    }
    let name_labels = ["name", "full name", "your name", "navn", "fulde navn", "dit navn"];
    if name_labels.contains(&label.as_str()) || has_word(hint, "name") && short && label.contains("name") {
        return Some(ContactPart::FullName);
    }
    if short
        && ([
            "city",
            "location",
            "by",
            "bopæl",
            "where do you live",
            "current location",
        ]
        .contains(&label.as_str()))
    {
        return Some(ContactPart::Location);
    }
    None
}

/// The profile's answer for part, "" when it has none.
fn contact_value(part: ContactPart, contact: &Contact, links: &[(String, String)]) -> String {
    let link = |wanted: &str| {
        links
            .iter()
            .find(|(label, url)| label.to_lowercase().contains(wanted) || url.to_lowercase().contains(wanted))
            .map(|(_, url)| url.clone())
            .unwrap_or_default()
    };
    let name = contact.name.trim();
    match part {
        ContactPart::FirstName => name.rsplit_once(' ').map_or(name, |(first, _)| first).to_string(),
        ContactPart::LastName => name.rsplit_once(' ').map_or("", |(_, last)| last).to_string(),
        ContactPart::FullName => name.to_string(),
        ContactPart::Email => contact.email.trim().to_string(),
        ContactPart::Phone => contact.phone.trim().to_string(),
        ContactPart::Location => contact.location.trim().to_string(),
        ContactPart::LinkedIn => link("linkedin"),
        ContactPart::GitHub => link("github"),
        ContactPart::Website => links
            .iter()
            .map(|(_, url)| url)
            .find(|url| !url.contains("linkedin") && !url.contains("github"))
            .cloned()
            .unwrap_or_default(),
    }
}

/// Whether text names a cover letter ("Cover letter", "Motivational letter", "Ansøgning").
fn names_cover_letter(text: &str) -> bool {
    [
        "cover letter",
        "cover_letter",
        "coverletter",
        "motivation",
        "ansøgning",
        "følgebrev",
        "application letter",
    ]
    .iter()
    .any(|word| text.contains(word))
}

/// Whether text names a CV or résumé.
fn names_cv(text: &str) -> bool {
    ["resume", "résumé", "curriculum", "cv"]
        .iter()
        .any(|word| has_word(text, word) || text.contains(&format!("{word}_")) || text.contains(&format!("_{word}")))
        || text.contains("résumé")
        || text.contains("resume")
}

/// Whether a checkbox is the candidate's consent or acceptance, which they give themselves.
fn asks_consent(field: &FormField) -> bool {
    let text = format!(
        "{} {} {}",
        field.label.to_lowercase(),
        field.options.join(" ").to_lowercase(),
        field.hint
    );
    [
        "consent",
        "privacy",
        "gdpr",
        "terms",
        "agree",
        "accept",
        "policy",
        "samtykke",
        "persondata",
        "betingelser",
        "accepter",
        "retain",
        "data protection",
        "talent pool",
        "talentpool",
    ]
    .iter()
    .any(|word| text.contains(word))
}

/// What the match has to offer a form: its written application and résumé.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Written {
    pub application: bool,
    pub resume: bool,
}

impl Written {
    pub fn of(found: &SavedMatch) -> Self {
        Written {
            application: found
                .application_text
                .as_deref()
                .is_some_and(|text| !text.trim().is_empty()),
            resume: found.resume_text.as_deref().is_some_and(|text| !text.trim().is_empty()),
        }
    }
}

/// The form's fields sorted out without the model: what the profile and the match's documents
/// answer, what is left for the candidate, and the questions for the model.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub answers: FormAnswers,
    pub for_model: Vec<FormField>,
}

/// Sorts the fields: contact fields from the profile, uploads and a cover letter box from the
/// match's documents, consent boxes for the candidate, the rest for the model. A field the profile
/// or the match has nothing for is left with why.
pub fn plan(
    fields: &[FormField],
    contact: &Contact,
    links: &[(String, String)],
    written: Written,
    application_text: &str,
) -> Plan {
    let mut plan = Plan::default();
    let answer = |plan: &mut Plan, field: &FormField, value: String, file: Option<&'static str>, from| {
        plan.answers.answers.push(FieldAnswer {
            key: field.key.clone(),
            value,
            file,
            from,
        });
    };
    let leave = |plan: &mut Plan, field: &FormField, why: &str| {
        plan.answers.left.push(LeftField {
            key: field.key.clone(),
            why: why.to_string(),
        });
    };
    for field in fields {
        let about = format!("{} {}", field.label.to_lowercase(), field.hint);
        if field.kind == kind::FILE {
            let label = field.label.trim().trim_end_matches('*').trim_end();
            if label.eq_ignore_ascii_case("application") || names_cover_letter(&about) {
                if written.application {
                    answer(
                        &mut plan,
                        field,
                        String::new(),
                        Some(pdf_writer::APPLICATION),
                        FROM_APPLICATION,
                    );
                } else {
                    leave(
                        &mut plan,
                        field,
                        "Write the application in JobFinder first, then fill in again.",
                    );
                }
            } else if names_cv(&about) {
                if written.resume {
                    answer(&mut plan, field, String::new(), Some(pdf_writer::RESUME), FROM_RESUME);
                } else {
                    answer(&mut plan, field, String::new(), Some(pdf_writer::CV), FROM_CV);
                }
            } else {
                leave(
                    &mut plan,
                    field,
                    "Not sure what this upload is for: add the file yourself.",
                );
            }
            continue;
        }
        if field.kind == kind::CHECKBOX && asks_consent(field) {
            leave(&mut plan, field, "Your consent: tick it yourself if you agree.");
            continue;
        }
        if let Some(part) = contact_part(field) {
            let value = contact_value(part, contact, links);
            if value.is_empty() {
                leave(&mut plan, field, "Your profile doesn't have this.");
            } else {
                answer(&mut plan, field, value, None, FROM_PROFILE);
            }
            continue;
        }
        if field.kind == kind::TEXTAREA && names_cover_letter(&about) {
            if written.application {
                answer(
                    &mut plan,
                    field,
                    application_text.trim().to_string(),
                    None,
                    FROM_APPLICATION,
                );
            } else {
                leave(
                    &mut plan,
                    field,
                    "Write the application in JobFinder first, then fill in again.",
                );
            }
            continue;
        }
        plan.for_model.push(field.clone());
    }
    plan
}

fn system() -> &'static str {
    r#"You fill in a job application form for a candidate, from their CV, the job post and the application letter written for this job. The candidate reads every answer before sending the form, so answer as they would, in the first person, and never invent anything.

- Answer each field by its key. Write in the language the form's own questions are written in.
- Free-text questions ("What made you interested in us?", "Why would you be a fit?"): a short, specific answer of two to five sentences, from the CV, the post and the letter. Name what about this employer and role draws the candidate (from the post) and the CV facts that back the fit. Don't copy the letter: answer the question asked, and keep it consistent with the letter.
- A choice (kind select or radio): set answer to exactly one of its options, copied as written, the one the CV and the post support best. When the options are teams or areas, pick the one closest to the candidate's experience; when none fits clearly and an option says they have no preference, pick that.
- Checkboxes (kind checkboxes): set choices to the options that are true for the candidate, copied as written.
- One checkbox (kind checkbox): answer "yes" only when the CV shows it is true for the candidate, otherwise "no".
- Short fields (text, number, date, url): a short value only. Dates as YYYY-MM-DD.
- Leave answer empty when the CV, the post, the letter and the notes don't give it: salary expectations, notice period, start date, work permits, how they heard of the job, references, anything personal. An empty answer is left for the candidate; a guess is wrong."#
}

/// The fields as the prompt lists them: key, kind, whether it is required, the label, and a
/// choice's options.
fn field_lines(fields: &[FormField]) -> String {
    fields
        .iter()
        .map(|field| {
            let required = if field.required { ", required" } else { "" };
            let options = if field.options.is_empty() {
                String::new()
            } else {
                format!(
                    "\n  Options:\n{}",
                    field
                        .options
                        .iter()
                        .map(|option| format!("  - {option}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
            };
            format!(
                "- key {} ({}{required}): {}{options}",
                field.key, field.kind, field.label
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The answer's shape: an answer, and a checkbox group's choices, for each key.
fn schema() -> Value {
    json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["form_language", "answers"],
        "properties": {
            "form_language": { "type": "string" },
            "answers": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["key", "answer", "choices"],
                    "properties": {
                        "key": { "type": "string" },
                        "answer": { "type": "string" },
                        "choices": { "type": "array", "items": { "type": "string" } },
                    },
                },
            },
        },
    })
}

/// Asks the form's model to answer fields for saved match found, with the
/// letter written for it (application_text, "" when there is none).
pub async fn ask(
    db: &SqlitePool,
    found: &SavedMatch,
    llm: &KeyedLlm,
    fields: &[FormField],
    application_text: &str,
) -> Result<Value, WriteError> {
    let cv = document_writer::written_from(db, found).await?;
    let letter = if application_text.trim().is_empty() {
        "(none written yet)".to_string()
    } else {
        application_text.trim().to_string()
    };
    let user = format!(
        "Job title: {job_title}
Company: {company}

Job post:

{job_text}

CV:

{cv}

The application letter written for this job:

{letter}

The form's fields to answer:
{fields}

Set form_language to the language of the form's questions, then answer every field by its key.",
        job_title = found.job_title,
        company = found.company,
        job_text = found.job_text,
        fields = field_lines(fields),
    );
    Ok(llm
        .llm
        .structured(system(), &user, &schema(), Effort::Medium, ANSWER_TOKENS)
        .await?)
}

/// The option of options that is answer, as shown, ignoring case and spaces at the ends.
fn option_of<'a>(options: &'a [String], answer: &str) -> Option<&'a String> {
    let answer = answer.trim().to_lowercase();
    options.iter().find(|option| option.trim().to_lowercase() == answer)
}

/// The model's answer for each field it was asked, checked against the field: a choice must be
/// one of its options. A field it left empty, or answered with no option of its own, is left for
/// the candidate.
pub fn taken(fields: &[FormField], answered: &Value) -> FormAnswers {
    let mut taken = FormAnswers::default();
    let list = answered["answers"].as_array().map(Vec::as_slice).unwrap_or_default();
    for field in fields {
        let found = list
            .iter()
            .find(|answer| answer["key"].as_str() == Some(field.key.as_str()));
        let text = found.and_then(|answer| answer["answer"].as_str()).unwrap_or("").trim();
        let choices: Vec<&str> = found
            .and_then(|answer| answer["choices"].as_array())
            .map(|choices| choices.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        let value = match field.kind.as_str() {
            kind::SELECT | kind::RADIO => option_of(&field.options, text).cloned(),
            kind::CHECKBOXES => {
                let picked: Vec<String> = choices
                    .iter()
                    .filter_map(|choice| option_of(&field.options, choice).cloned())
                    .collect();
                (!picked.is_empty()).then(|| picked.join("\n"))
            }
            kind::CHECKBOX => (text.eq_ignore_ascii_case("yes")).then(|| "yes".to_string()),
            _ => (!text.is_empty()).then(|| text.to_string()),
        };
        match value {
            Some(value) => taken.answers.push(FieldAnswer {
                key: field.key.clone(),
                value,
                file: None,
                from: FROM_MODEL,
            }),
            // An unticked checkbox is the answer "no": nothing to leave.
            None if field.kind == kind::CHECKBOX => {}
            None => taken.left.push(LeftField {
                key: field.key.clone(),
                why: if text.is_empty() {
                    "Your CV doesn't say: answer it yourself.".to_string()
                } else {
                    format!("The model's answer \"{text}\" isn't one of the options: pick one yourself.")
                },
            }),
        }
    }
    taken
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(key: &str, label: &str, kind: &str) -> FormField {
        FormField {
            key: key.to_string(),
            label: label.to_string(),
            kind: kind.to_string(),
            ..FormField::default()
        }
    }

    fn contact() -> Contact {
        Contact {
            name: "Ada Lovelace King".to_string(),
            email: "ada@example.com".to_string(),
            phone: "+45 12 34 56 78".to_string(),
            location: "Aarhus".to_string(),
            ..Contact::default()
        }
    }

    fn links() -> Vec<(String, String)> {
        vec![
            ("LinkedIn".to_string(), "https://linkedin.com/in/ada".to_string()),
            ("GitHub".to_string(), "https://github.com/ada".to_string()),
            ("Site".to_string(), "https://ada.dev".to_string()),
        ]
    }

    const BOTH: Written = Written {
        application: true,
        resume: true,
    };

    fn value_of<'a>(answers: &'a FormAnswers, key: &str) -> Option<&'a FieldAnswer> {
        answers.answers.iter().find(|answer| answer.key == key)
    }

    #[test]
    fn the_profile_fills_the_contact_fields() {
        let mut first = field("1", "First name *", kind::TEXT);
        first.hint = "given-name".to_string();
        let fields = [
            first,
            field("2", "Last name", kind::TEXT),
            field("3", "Full name", kind::TEXT),
            field("4", "Email address", kind::EMAIL),
            field("5", "Phone number", kind::TEL),
            field("6", "LinkedIn profile", kind::URL),
            field("7", "GitHub", kind::URL),
            field("8", "Portfolio / website", kind::URL),
            field("9", "City", kind::TEXT),
        ];
        let plan = plan(&fields, &contact(), &links(), BOTH, "");
        let value = |key| value_of(&plan.answers, key).map(|answer| answer.value.as_str());
        assert_eq!(value("1"), Some("Ada Lovelace"));
        assert_eq!(value("2"), Some("King"));
        assert_eq!(value("3"), Some("Ada Lovelace King"));
        assert_eq!(value("4"), Some("ada@example.com"));
        assert_eq!(value("5"), Some("+45 12 34 56 78"));
        assert_eq!(value("6"), Some("https://linkedin.com/in/ada"));
        assert_eq!(value("7"), Some("https://github.com/ada"));
        assert_eq!(value("8"), Some("https://ada.dev"));
        assert_eq!(value("9"), Some("Aarhus"));
        assert!(plan.for_model.is_empty());
    }

    #[test]
    fn a_question_is_for_the_model_even_when_it_says_name() {
        let fields = [
            field(
                "q",
                "What made you interested in Famly? Name one thing.",
                kind::TEXTAREA,
            ),
            field("c", "Company name you work for now", kind::TEXT),
        ];
        let plan = plan(&fields, &contact(), &links(), BOTH, "");
        assert!(plan.answers.answers.is_empty());
        assert_eq!(plan.for_model.len(), 2);
    }

    #[test]
    fn uploads_get_the_letter_the_resume_or_the_cv() {
        let fields = [
            field("letter", "Application*", kind::FILE),
            field("cv", "CV", kind::FILE),
            field("other", "Anything else", kind::FILE),
        ];
        let plan = plan(&fields, &contact(), &links(), BOTH, "");
        assert_eq!(
            value_of(&plan.answers, "letter").unwrap().file,
            Some(pdf_writer::APPLICATION)
        );
        assert_eq!(value_of(&plan.answers, "cv").unwrap().file, Some(pdf_writer::RESUME));
        assert_eq!(plan.answers.left[0].key, "other");

        let none = Written {
            application: false,
            resume: false,
        };
        let plan = super::plan(&fields, &contact(), &links(), none, "");
        assert_eq!(value_of(&plan.answers, "cv").unwrap().file, Some(pdf_writer::CV));
        assert!(plan.answers.left.iter().any(|left| left.key == "letter"));
    }

    #[test]
    fn a_cover_letter_box_gets_the_letter_and_consent_is_left() {
        let fields = [
            field("letter", "Cover letter", kind::TEXTAREA),
            field("ok", "I agree to the privacy policy", kind::CHECKBOX),
        ];
        let plan = plan(&fields, &contact(), &links(), BOTH, " Dear Famly ");
        assert_eq!(value_of(&plan.answers, "letter").unwrap().value, "Dear Famly");
        assert_eq!(plan.answers.left[0].key, "ok");
    }

    #[test]
    fn the_models_choices_must_be_options() {
        let mut team = field("team", "Please select the team", kind::RADIO);
        team.options = vec![
            "Finance - Billing, Payments & Funding".to_string(),
            "Frameworks".to_string(),
        ];
        let mut other = team.clone();
        other.key = "other".to_string();
        let why = field("why", "Why do you feel you would be a fit?", kind::TEXTAREA);
        let salary = field("salary", "Salary expectations", kind::TEXT);
        let answered = json!({
            "form_language": "English",
            "answers": [
                { "key": "team", "answer": " frameworks", "choices": [] },
                { "key": "other", "answer": "Platform", "choices": [] },
                { "key": "why", "answer": "I built ...", "choices": [] },
                { "key": "salary", "answer": "", "choices": [] },
            ],
        });
        let taken = taken(&[team, other, why, salary], &answered);
        assert_eq!(value_of(&taken, "team").unwrap().value, "Frameworks");
        assert_eq!(value_of(&taken, "why").unwrap().value, "I built ...");
        let left: Vec<&str> = taken.left.iter().map(|left| left.key.as_str()).collect();
        assert_eq!(left, ["other", "salary"]);
    }

    /// The fields as the extension reads them from a Recruitee form like Famly's.
    #[test]
    fn a_recruitee_form_is_sorted_out() {
        let with_hint = |mut field: FormField, hint: &str| {
            field.hint = hint.to_string();
            field
        };
        let mut team = field(
            "f7",
            "Please select the team, where you see the best match in interests, skills or experience. *",
            kind::RADIO,
        );
        team.options = vec!["Frameworks".to_string()];
        let fields = [
            with_hint(field("f0", "Full name *", kind::TEXT), "name candidate[name] n"),
            with_hint(field("f1", "Email address *", kind::EMAIL), "candidate[email] e"),
            with_hint(field("f2", "Phone number", kind::TEL), "candidate[phone] p"),
            with_hint(
                field("f3", "Choose file Upload your cover letter", kind::FILE),
                "candidate[cover_letter]  .pdf,.doc",
            ),
            with_hint(
                field("f4", "What made you interested in Famly? *", kind::TEXTAREA),
                "q1",
            ),
            with_hint(field("f6", "Salary expectations", kind::TEXT), "q4"),
            team,
            with_hint(field("f8", "I agree to Famly's privacy policy", kind::CHECKBOX), "gdpr"),
        ];
        let plan = plan(&fields, &contact(), &links(), BOTH, "");
        let answered: Vec<&str> = plan.answers.answers.iter().map(|answer| answer.key.as_str()).collect();
        assert_eq!(answered, ["f0", "f1", "f2", "f3"]);
        let left: Vec<&str> = plan.answers.left.iter().map(|left| left.key.as_str()).collect();
        assert_eq!(left, ["f8"]);
        let asked: Vec<&str> = plan.for_model.iter().map(|field| field.key.as_str()).collect();
        assert_eq!(asked, ["f4", "f6", "f7"]);
    }

    #[test]
    fn cleaning_drops_unknown_kinds_and_repeated_keys() {
        let fields = vec![
            field("a", "A", kind::TEXT),
            field("a", "A again", kind::TEXT),
            field("b", "B", "password"),
            field("", "No key", kind::TEXT),
        ];
        let cleaned = cleaned(fields);
        assert_eq!(cleaned.len(), 1);
        assert_eq!(cleaned[0].label, "A");
    }
}
