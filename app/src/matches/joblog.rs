//! A saved match as an entry for Jobnet's joblog ("Opret joblog" on jobnet.dk), the log of
//! applications Danish job seekers keep for the job centre. Jobnet has no way in for other sites,
//! so the page (and the Jobnet extension) shows each field in Jobnet's own order and words, filled
//! in from the match, for the candidate to copy over.
//!
//! read() fills the fields by rule from the saved match (no model call):
//! - the job title, company and link come from the match;
//! - the deadline, working hours, address, postcode and town, and contact person are read from
//!   the post text (Danish or English). A postcode only counts when the town after it is that
//!   postcode's town (assets/danish_postcodes.json), so "2026 Vi tilbyder" isn't an address;
//! - how far the application is: what the candidate last said (Ikke søgt until then) and, once
//!   applied, the day they said so; the post was found as a posted job and is applied for
//!   digitally;
//! - the notes: the score, what the CV meets and misses, and how the post asks to be applied to.
//!
//! Anything not found is left empty, for the candidate to fill in on Jobnet.

use std::collections::{BTreeMap, HashMap};
use std::sync::LazyLock;

use chrono::NaiveDate;
use fancy_regex::{Captures, Regex};
use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

use super::apply_instructions::ApplyInstructions;
use super::match_model::{APPLIED_STATUSES, Integration, SavedMatch};
use super::{dates, match_score};

/// Jobnet's choices, by the value its form posts.
pub const JOB_TYPES: [(&str, &str); 2] = [("Fuldtid", "Fuldtid"), ("Deltid", "Deltid")];
/// The three status radio options Jobnet's own "Opret/Rediger joblog" form accepts.
pub const FORM_STATUSES: [(&str, &str); 3] = [("IkkeSoegt", "Ikke søgt"), ("Soegt", "Søgt"), ("Samtale", "Samtale")];
/// All application statuses JobFinder tracks (Jobnet's three form statuses plus closed outcomes
/// that tick Jobnet's "Afsluttet" checkbox on the joblog card).
pub const SEARCH_STATUSES: [(&str, &str); 5] = [
    ("IkkeSoegt", "Ikke søgt"),
    ("Soegt", "Søgt"),
    ("Samtale", "Samtale"),
    ("Afslag", "Afslag"),
    ("FaaetJob", "Fået job"),
];
pub const FOUND: [(&str, &str); 3] = [
    ("OpslaaetStilling", "Opslået stilling"),
    ("Uopfordret", "Uopfordret"),
    ("GennemNetvaerk", "Gennem Netværk"),
];
pub const APPLIED: [(&str, &str); 3] = [
    ("Digitalt", "Digitalt"),
    ("Personligt", "Personligt"),
    ("Telefon", "Telefonisk"),
];

/// The status until the candidate says otherwise.
pub const NOT_APPLIED: &str = "IkkeSoegt";
/// The status a match gets when the candidate says they applied (Søgt).
pub const APPLIED_STATUS: &str = "Soegt";

/// The radio value Jobnet's "Opret/Rediger joblog" form takes for a JobFinder status.
/// Closed outcomes (`Afslag`, `FaaetJob`) map to an applied radio value; on Jobnet's card they
/// also tick the "Afsluttet" checkbox (`Lukket`).
pub fn form_status(status: &str) -> &'static str {
    match status {
        "Samtale" | "FaaetJob" => "Samtale",
        "Soegt" | "Afslag" => "Soegt",
        _ => NOT_APPLIED,
    }
}

// Jobnet's limits: part time is at most 36 hours, notes 2000 characters.
pub const MAX_PART_TIME_HOURS: i64 = 36;
pub const MAX_NOTES: usize = 2000;

/// Hours a week below which a job is part time (full time in Denmark is 37).
const FULL_TIME_HOURS: f64 = 37.0;

/// How far after a deadline's cue its date is looked for, in bytes.
const DEADLINE_WINDOW: usize = 120;

/// How far after a deadline's cue "løbende" counts, in characters.
const ONGOING_WINDOW: usize = 30;

/// How long before the match was saved a deadline without a year may lie and still be this
/// year's: a post read a couple of months after its deadline.
const PAST_DEADLINE_SECONDS: i64 = 60 * 86_400;

/// Lines from a contact cue that count as its block: its details are often on the next lines.
const CONTACT_BLOCK_LINES: usize = 3;

/// Most parts of a street address before a postcode: "Vestergade 12, 2. th.".
const MAX_STREET_PARTS: usize = 3;

const MONTHS: &[(&str, u32)] = &[
    ("januar", 1),
    ("january", 1),
    ("jan", 1),
    ("februar", 2),
    ("february", 2),
    ("feb", 2),
    ("marts", 3),
    ("march", 3),
    ("mar", 3),
    ("april", 4),
    ("apr", 4),
    ("maj", 5),
    ("may", 5),
    ("juni", 6),
    ("june", 6),
    ("jun", 6),
    ("juli", 7),
    ("july", 7),
    ("jul", 7),
    ("august", 8),
    ("aug", 8),
    ("september", 9),
    ("sept", 9),
    ("sep", 9),
    ("oktober", 10),
    ("october", 10),
    ("okt", 10),
    ("oct", 10),
    ("november", 11),
    ("nov", 11),
    ("december", 12),
    ("dec", 12),
];

/// Capitalised words that start a sentence or name a role, not a person.
const NOT_NAMES: &[&str] = &[
    "Har",
    "Hvis",
    "Vil",
    "Du",
    "Kontakt",
    "Kontakte",
    "Ring",
    "Send",
    "Skriv",
    "Spørgsmål",
    "For",
    "Yderligere",
    "Information",
    "Oplysninger",
    "If",
    "Please",
    "Contact",
    "Feel",
    "You",
    "We",
    "Vi",
    "Questions",
    "Call",
    "Mail",
    "Email",
    "E-mail",
    "Tlf",
    "Telefon",
    "Phone",
    "Mobil",
    "Mobile",
    "Head",
    "Chief",
    "Senior",
    "Lead",
    "Manager",
    "Director",
    "Direktør",
    "Leder",
    "Afdelingsleder",
    "Teamleder",
    "Chef",
    "HR",
    "CTO",
    "CEO",
    "COO",
    "CFO",
    "Recruiter",
    "Rekrutteringspartner",
    "Ansøgningsfrist",
    "Mere",
    "More",
    "Learn",
    "Read",
    "Og",
    "And",
    "Eller",
    "Or",
    "Så",
    "Til",
    "To",
    "Med",
    "With",
    "På",
    "At",
    "Om",
    "About",
    "Engineering",
    "Development",
    "Udvikling",
    "IT",
    "Hej",
    "Hello",
    "Hi",
    "Mvh",
    "Venlig",
    "Hilsen",
    "Best",
    "Regards",
    "Danmark",
    "Denmark",
    "Aarhus",
    "København",
    "Copenhagen",
    "Odense",
    "Aalborg",
];

fn pattern(source: &str) -> Regex {
    Regex::new(source).unwrap_or_else(|e| panic!("{source}: {e}"))
}

// What comes right before an application deadline.
static DEADLINE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)(?:ansøgningsfrist\w*|sidste ansøgningsdag|frist\w*|deadline|(?:søg|ansøg|send\w*)[^.\n]{0,40}?senest|senest|apply (?:by|before|no later than)|applications? (?:by|before)|closing date|last day to apply)",
    )
});
// "Løbende": applications are read as they come, there is no deadline.
static ONGOING: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"(?i)(?:løbende|hurtigst muligt|asap|as soon as possible|on a rolling basis|ongoing basis)")
});
// The end of the sentence or line after a deadline's cue: a date further on is about something else.
static SENTENCE_END: LazyLock<Regex> = LazyLock::new(|| pattern(r"\n|(?<=[a-zæøå])\.\s"));
// A location line on its own, as LinkedIn and Jobindex print it:
// "Odense, Region Syddanmark, Danmark" or "Aarhus C, Denmark".
static TOWN_LINE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(r"^\s*(\p{Lu}[\p{L} .\-]{1,40}),\s*(?:Region\s+\p{L}+(?:\s+\p{L}+)?,\s*)?(?:Danmark|Denmark)\s*$")
});
// Lines that say where the job is.
static PLACE: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)(?:arbejdssted|arbejdsplads|lokation|location|placering|beliggende|adresse|address|kontor(?:et)? (?:i|ligger)|office in|based in|job ?location|\bsted\b)\s*:?",
    )
});
// A postcode and the words after it: "8000 Aarhus C", "DK-2100 København Ø".
static POSTCODE: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?<![\d\p{L}])(?:DK-?\s?)?(\d{4})\s+(\p{Lu}[\p{L}.\-]*(?:\s+[\p{L}.\-]+)?)"));
static KONGENS: LazyLock<Regex> = LazyLock::new(|| pattern(r"\bkgs\.?\s*"));
static COPENHAGEN: LazyLock<Regex> = LazyLock::new(|| pattern(r"\bcopenhagen\b"));
static NOT_TOWN_LETTERS: LazyLock<Regex> = LazyLock::new(|| pattern(r"[^\p{L}\d]+"));
static LABEL: LazyLock<Regex> = LazyLock::new(|| pattern(r"^.*:\s*"));
static STREET: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"^\p{Lu}[\p{L}.\-]*(?:\s+[\p{L}.\-]+){0,3}\s+\d{1,4}\s*\p{L}?\b"));
// The rest of an address: "2. th.", "st. tv".
static FLOOR: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)^\d|\b(?:sal|th|tv|mf|st)\b"));
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"[\p{L}0-9._%+-]+@[\p{L}0-9-]+(?:\.[\p{L}0-9-]+)*\.\p{L}{2,}"));
static PHONE: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?<![\d+])(?:\+45[\s-]?)?(\d{2}[\s-]?\d{2}[\s-]?\d{2}[\s-]?\d{2})(?![\d])"));
static PHONE_CUE: LazyLock<Regex> =
    LazyLock::new(|| pattern(r"(?i)(?:tlf|telefon|phone|mobil|mobile|\bring\b|\bcall\b|\btel\b|\+45)"));
static CVR: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)\bcvr\b"));
// Lines with a contact person: who to ask about the job.
static CONTACT: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)(?:kontakt\w*|contact|spørgsmål|questions|vil du vide mere|hører du gerne mere|høre mere|information|oplysninger)",
    )
});
// Two or three capitalised words, with a middle initial: "Mette Hansen", "Peter J. Olsen".
static PERSON: LazyLock<Regex> = LazyLock::new(|| {
    let word = r"\p{Lu}\p{Ll}+(?:-\p{Lu}\p{Ll}+)?";
    pattern(&format!(
        r"(?<![\p{{L}}]){word}(?:\s+\p{{Lu}}\.)?(?:\s+{word}){{1,2}}(?![\p{{L}}])"
    ))
});
static WHITESPACE: LazyLock<Regex> = LazyLock::new(|| pattern(r"\s+"));
static EMAIL_PARTS: LazyLock<Regex> = LazyLock::new(|| pattern(r"[._-]"));
static HOURS_A_WEEK: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)(?<![\d.,])(\d{1,2}(?:[.,]\d{1,2})?)\s*(?:-\s*\d{1,2}\s*)?(?:timer|timers|t\.|hours|hrs|h)\b[^\n.]{0,20}?(?:uge\w*|week\w*|ugentlig\w*)",
    )
});
static WEEKLY_HOURS: LazyLock<Regex> = LazyLock::new(|| {
    pattern(
        r"(?i)(?:ugentlig\w*|weekly|om ugen|pr\.? uge|per week)[^\n.\d]{0,30}?(\d{1,2}(?:[.,]\d{1,2})?)\s*(?:timer|t\.|hours|hrs)\b",
    )
});
static FULL_TIME: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)\bfuldtid\w*|\bfull[- ]?time\b"));
static PART_TIME: LazyLock<Regex> = LazyLock::new(|| pattern(r"(?i)\bdeltid\w*|\bpart[- ]?time\b"));

/// How a date is written, and which groups are its day, month and year.
enum DateForm {
    /// 2026-10-31
    Iso,
    /// 31.10.2026, 31/10-26, 31-10-2026
    Numeric,
    /// 31. oktober 2026, 31 Oct
    DayMonthName,
    /// October 31, 2026
    MonthNameDay,
    /// 31/10 (no year), but not a time of day: "kl. 12.00"
    DayMonth,
}

static DATES: LazyLock<Vec<(Regex, DateForm)>> = LazyLock::new(|| {
    let months = MONTHS
        .iter()
        .map(|(name, _)| fancy_regex::escape(name).into_owned())
        .collect::<Vec<_>>()
        .join("|");
    vec![
        (pattern(r"(?<!\d)(\d{4})-(\d{1,2})-(\d{1,2})(?!\d)"), DateForm::Iso),
        (
            pattern(r"(?<![\d.])(\d{1,2})[./-](\d{1,2})[./-](\d{4}|\d{2})(?![\d])"),
            DateForm::Numeric,
        ),
        (
            pattern(&format!(
                r"(?i)(?<!\d)(\d{{1,2}})\.?\s*(?:af\s+)?({months})\.?(?![\p{{L}}])(?:,?\s*(\d{{4}}))?"
            )),
            DateForm::DayMonthName,
        ),
        (
            pattern(&format!(
                r"(?i)(?<![\p{{L}}])({months})\.?\s+(\d{{1,2}})(?:st|nd|rd|th)?(?!\d)(?:,?\s*(\d{{4}}))?"
            )),
            DateForm::MonthNameDay,
        ),
        (
            pattern(r"(?i)(?<![\d.])(?<!kl\. )(?<!kl )(\d{1,2})[./](\d{1,2})(?!\d|[./]\d)"),
            DateForm::DayMonth,
        ),
    ]
});

/// Danish postcodes and their towns as [postcode, town] pairs, in the list's order: PostNord's
/// list, via the ISC-licensed npm package dk-postals 1.0.0.
static POSTCODES: LazyLock<Vec<(String, String)>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../assets/danish_postcodes.json"))
        .expect("assets/danish_postcodes.json is a list of [postcode, town] pairs")
});
static TOWN_OF: LazyLock<HashMap<&'static str, &'static str>> = LazyLock::new(|| {
    POSTCODES
        .iter()
        .map(|(code, town)| (code.as_str(), town.as_str()))
        .collect()
});

/// A town's postcode ("Odense" or "odense c" is 5000), or a postcode as written ("5000"): the
/// first of the town's postcodes, or of the first town named "{town} …" (Odense C for Odense).
pub fn postcode_of(place: &str) -> Option<&'static str> {
    let place = place.trim();
    if let Some((code, _)) = POSTCODES.iter().find(|(code, _)| code == place) {
        return Some(code.as_str());
    }
    let lower = place.to_lowercase();
    if lower.is_empty() {
        return None;
    }
    let district = format!("{lower} ");
    let named = |town: &str| town.to_lowercase() == lower;
    let in_district = |town: &str| town.to_lowercase().starts_with(&district);
    POSTCODES
        .iter()
        .find(|(_, town)| named(town))
        .or_else(|| POSTCODES.iter().find(|(_, town)| in_district(town)))
        .map(|(code, _)| code.as_str())
}

/// The joblog entry: Jobnet's sections in order, each with its fields, then the three PDFs it
/// keeps, and when the candidate entered it in their joblog.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Entry {
    pub sections: Vec<Section>,
    pub files: Vec<File>,
    pub logged_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Section {
    pub heading: &'static str,
    pub fields: Vec<Field>,
}

/// One of Jobnet's fields: its form name and label, its type (text, radio, number, select,
/// checkbox or textarea), the value to give it, and its choices by the value Jobnet posts.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Field {
    pub name: &'static str,
    pub label: &'static str,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub value: Value,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Choices>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// A field's value: text, or ticked or not for a checkbox.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub enum Value {
    Text(String),
    Ticked(bool),
}

impl Value {
    pub fn text(&self) -> &str {
        match self {
            Value::Text(text) => text,
            Value::Ticked(_) => "",
        }
    }
}

/// A choice field's options, posted value => text, as one JSON object in Jobnet's order.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Choices(pub &'static [(&'static str, &'static str)]);

impl Serialize for Choices {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (value, text) in self.0 {
            map.serialize_entry(value, text)?;
        }
        map.end()
    }
}

/// One of the PDFs Jobnet keeps: whether it can be made, and if not, why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct File {
    pub kind: &'static str,
    pub label: &'static str,
    pub ready: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<&'static str>,
    pub missing: &'static str,
}

/// The joblog entry for a saved match.
pub fn read(saved: &SavedMatch) -> Entry {
    let text = saved
        .job_text
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\u{a0}', " ");
    let (job_type, hours) = working_hours(&text);
    let (address, postal) = place(&text);
    let contact = contact(&text, &saved.company, saved.apply_instructions.as_ref());
    let written = !saved.application_text.as_deref().unwrap_or("").trim().is_empty();
    let resume = !saved.resume_text.as_deref().unwrap_or("").trim().is_empty();
    let jobnet = saved.jobnet_joblog.as_ref();
    let status = jobnet.map_or(NOT_APPLIED, |jobnet| jobnet.status.as_str());
    let part_time_hours = if job_type == "Deltid" { hours } else { String::new() };

    Entry {
        sections: vec![
            Section {
                heading: "Om jobbet",
                fields: vec![
                    text_field(
                        "occupation",
                        "Stilling eller arbejdsområde",
                        saved.job_title.trim(),
                        true,
                        "",
                    ),
                    text_field(
                        "submissionDeadline",
                        "Ansøgningsfrist",
                        &deadline(&text, saved.created_at),
                        false,
                        "dd-mm-åååå",
                    ),
                    choice_field("jobType", "Arbejdstid", job_type, &JOB_TYPES),
                    Field {
                        kind: "number",
                        ..text_field(
                            "hoursPerWeek",
                            "Antal timer pr. uge",
                            &part_time_hours,
                            true,
                            &format!("Kun ved deltid, højst {MAX_PART_TIME_HOURS}"),
                        )
                    },
                ],
            },
            Section {
                heading: "Om arbejdspladsen",
                fields: vec![
                    text_field("name", "Virksomhedens navn", saved.company.trim(), true, ""),
                    text_field("companyAddress", "Adresse", &address, false, ""),
                    Field {
                        kind: "select",
                        ..text_field(
                            "companyCountry",
                            "Land",
                            if postal.is_empty() { "" } else { "Danmark" },
                            false,
                            "",
                        )
                    },
                    Field {
                        name: "unknownPostalOrCity",
                        label: "Jeg kender hverken postnummer eller by",
                        kind: "checkbox",
                        value: Value::Ticked(postal.is_empty()),
                        required: false,
                        options: None,
                        hint: None,
                    },
                    text_field("postalCodeAndCity", "Postnummer og by", &postal, true, ""),
                    text_field("companyContactName", "Navn på kontaktperson", &contact.name, false, ""),
                    text_field("companyContactPhone", "Telefonnummer", &contact.phone, false, ""),
                    text_field("companyContactEmail", "E-mail", &contact.email, false, ""),
                    text_field(
                        "jobAdLink",
                        "Link til jobannonce",
                        saved.job_url.as_deref().unwrap_or("").trim(),
                        false,
                        "",
                    ),
                ],
            },
            Section {
                heading: "Om din jobsøgning",
                fields: vec![
                    // What the candidate said last (match_integrations); a written
                    // application alone doesn't mean it was sent.
                    choice_field(
                        "searchStatus",
                        "Hvor langt er du med at søge dette job?",
                        form_status(status),
                        &FORM_STATUSES,
                    ),
                    text_field(
                        "submissionDate",
                        "Hvilken dato har du søgt jobbet?",
                        &applied_on(jobnet),
                        true,
                        "dd-mm-åååå, kun ved Søgt eller Samtale",
                    ),
                    choice_field(
                        "applicationFormTypeFound",
                        "Hvordan fandt du jobbet?",
                        "OpslaaetStilling",
                        &FOUND,
                    ),
                    choice_field(
                        "applicationFormTypeApplied",
                        "Hvordan søger du jobbet?",
                        "Digitalt",
                        &APPLIED,
                    ),
                    Field {
                        kind: "textarea",
                        ..text_field("jobNotes", "Skriv evt. noter om jobbet", &notes(saved), false, "")
                    },
                ],
            },
        ],
        files: vec![
            File {
                kind: "application",
                label: "Jobansøgning",
                ready: written,
                note: None,
                missing: "Skriv ansøgningen først (Write application).",
            },
            File {
                kind: if resume { "resume" } else { "cv" },
                label: "CV",
                ready: !saved.cv_text.trim().is_empty(),
                note: Some(if resume {
                    "Det tilpassede CV"
                } else {
                    "Dit CV som tekst"
                }),
                missing: "Der er ikke gemt noget CV.",
            },
            File {
                kind: "job",
                label: "Jobannonce",
                ready: !text.trim().is_empty(),
                note: None,
                missing: "Der er ikke gemt noget jobopslag.",
            },
        ],
        // Whether the candidate has entered it in their joblog, and when.
        logged_at: jobnet.and_then(|jobnet| jobnet.logged_at),
    }
}

fn text_field(name: &'static str, label: &'static str, value: &str, required: bool, hint: &str) -> Field {
    Field {
        name,
        label,
        kind: "text",
        value: Value::Text(value.to_string()),
        required,
        options: None,
        hint: (!hint.is_empty()).then(|| hint.to_string()),
    }
}

/// A radio field, which Jobnet always requires.
fn choice_field(
    name: &'static str,
    label: &'static str,
    value: &str,
    options: &'static [(&'static str, &'static str)],
) -> Field {
    Field {
        kind: "radio",
        options: Some(Choices(options)),
        ..text_field(name, label, value, true, "")
    }
}

/// The day the candidate applied as Jobnet wants it, dd-mm-yyyy (Danish time): when they first
/// set Søgt or Samtale (match_integrations applied_at). '' when they haven't applied, which
/// is when Jobnet doesn't ask.
pub fn applied_on(jobnet: Option<&Integration>) -> String {
    match jobnet {
        Some(jobnet) if APPLIED_STATUSES.contains(&jobnet.status.as_str()) => jobnet
            .applied_at
            .and_then(dates::in_copenhagen)
            .map(|at| at.format("%d-%m-%Y").to_string())
            .unwrap_or_default(),
        _ => String::new(),
    }
}

/// The application deadline as Jobnet wants it, dd-mm-yyyy, or '' when the post gives none (or
/// reads applications as they come). A date without a year is the next one after the match was
/// saved, give or take a couple of months for a post read after its deadline.
pub fn deadline(text: &str, saved_at: i64) -> String {
    for cue in DEADLINE.find_iter(text).flatten() {
        let window = whole_letters(&text[cue.end()..], DEADLINE_WINDOW);
        let after = sentence(window);
        if ONGOING
            .is_match(&after.chars().take(ONGOING_WINDOW).collect::<String>())
            .unwrap_or(false)
            && !has_date(after)
        {
            continue;
        }
        let date = first_date(after, saved_at);
        if !date.is_empty() {
            return date;
        }
    }
    String::new()
}

/// The first `bytes` bytes of text, cut back to a whole letter.
fn whole_letters(text: &str, bytes: usize) -> &str {
    let mut end = bytes.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Text up to the end of its first sentence or line.
fn sentence(text: &str) -> &str {
    match SENTENCE_END.find(text).ok().flatten() {
        Some(end) => &text[..end.start()],
        None => text,
    }
}

fn has_date(text: &str) -> bool {
    !first_date(text, chrono::Utc::now().timestamp()).is_empty()
}

/// The first date in a short piece of text, as dd-mm-yyyy, or ''.
fn first_date(text: &str, saved_at: i64) -> String {
    // By where each form's first date starts; a later form found at the same place wins.
    let mut found: BTreeMap<usize, (i64, i64, String)> = BTreeMap::new();
    for (regex, form) in DATES.iter() {
        if let Ok(Some(m)) = regex.captures(text) {
            let start = m.get(0).map_or(0, |whole| whole.start());
            found.insert(start, date_parts(&m, form));
        }
    }
    for (day, month, year) in found.into_values() {
        let year = if year.is_empty() {
            let mut year = i64::from(dates::utc_year(saved_at));
            // Saved in December for a deadline in January.
            if dates::mktime_utc(month, day, year) < saved_at - PAST_DEADLINE_SECONDS {
                year += 1;
            }
            year
        } else if year.len() == 2 {
            2000 + year.parse::<i64>().unwrap_or(0)
        } else {
            year.parse().unwrap_or(0)
        };
        if checkdate(month, day, year) {
            return format!("{day:02}-{month:02}-{year:04}");
        }
    }
    String::new()
}

/// A date's day, month and year ('' when it has none) as the form writes them.
fn date_parts(m: &Captures<str>, form: &DateForm) -> (i64, i64, String) {
    let group = |i: usize| m.get(i).map_or("", |g| g.as_str());
    let number = |i: usize| group(i).parse::<i64>().unwrap_or(0);
    let month_named = |i: usize| month_number(group(i));
    match form {
        DateForm::Iso => (number(3), number(2), group(1).to_string()),
        DateForm::Numeric => (number(1), number(2), group(3).to_string()),
        DateForm::DayMonthName => (number(1), month_named(2), group(3).to_string()),
        DateForm::MonthNameDay => (number(2), month_named(1), group(3).to_string()),
        DateForm::DayMonth => (number(1), number(2), String::new()),
    }
}

fn month_number(name: &str) -> i64 {
    let name = name.to_lowercase();
    MONTHS
        .iter()
        .find(|(month, _)| *month == name)
        .map_or(0, |(_, number)| i64::from(*number))
}

/// A real day of the Gregorian calendar, in years 1 to 32767.
fn checkdate(month: i64, day: i64, year: i64) -> bool {
    (1..=32_767).contains(&year)
        && u32::try_from(month).is_ok_and(|month| {
            u32::try_from(day).is_ok_and(|day| NaiveDate::from_ymd_opt(year as i32, month, day).is_some())
        })
}

/// Full or part time, and the hours a week for part time. Hours below 37 a week, or the word
/// itself, make it part time; full time otherwise, the usual case.
pub fn working_hours(text: &str) -> (&'static str, String) {
    let hours = [&*HOURS_A_WEEK, &*WEEKLY_HOURS]
        .iter()
        .find_map(|regex| regex.captures(text).ok().flatten())
        .and_then(|m| m.get(1).map(|hours| hours.as_str().replace(',', ".")))
        .map(|hours| hours.parse::<f64>().unwrap_or(0.0));
    if let Some(hours) = hours
        && hours > 0.0
        && hours < FULL_TIME_HOURS
    {
        return ("Deltid", MAX_PART_TIME_HOURS.min(hours.round() as i64).to_string());
    }
    let full = FULL_TIME.is_match(text).unwrap_or(false);
    let part = PART_TIME.is_match(text).unwrap_or(false);
    (if part && !full { "Deltid" } else { "Fuldtid" }, String::new())
}

/// The workplace's street address and "postcode town" as Jobnet's typeahead lists it ("8000
/// Aarhus C"). A postcode counts when the town after it is the postcode's own; without one, a
/// known town named after "Arbejdssted:" or the like gives its first postcode.
pub fn place(text: &str) -> (String, String) {
    for m in POSTCODE.captures_iter(text).flatten() {
        let (Some(whole), Some(code), Some(words)) = (m.get(0), m.get(1), m.get(2)) else {
            continue;
        };
        let Some(town) = TOWN_OF.get(code.as_str()) else {
            continue;
        };
        if !names_town(words.as_str(), town) {
            continue;
        }
        // The street is on the same line, before the postcode.
        let line_start = text[..whole.start()].rfind('\n').map_or(0, |at| at + 1);
        return (
            street(&text[line_start..whole.start()]),
            format!("{} {town}", code.as_str()),
        );
    }
    for line in text.split('\n') {
        let rest = if let Some(cue) = PLACE.find(line).ok().flatten() {
            format!(" {} ", town_words(&line[cue.end()..]))
        } else if let Some(m) = TOWN_LINE.captures(line).ok().flatten() {
            format!(" {} ", town_words(m.get(1).map_or("", |g| g.as_str())))
        } else {
            continue;
        };
        // The longest town named in full ("Kongens Lyngby" over "Lyngby"), else a town's first
        // word ("Odense" for 5000 Odense C).
        let mut full: Option<(&str, &str)> = None;
        let mut first: Option<(&str, &str)> = None;
        for (code, town) in POSTCODES.iter() {
            let name = town.to_lowercase();
            let longest = full.map_or(0, |(_, town)| town.chars().count());
            if rest.contains(&format!(" {name} ")) && name.chars().count() > longest {
                full = Some((code, town));
            } else if first.is_none() && rest.contains(&format!(" {} ", name.split(' ').next().unwrap_or(""))) {
                first = Some((code, town));
            }
        }
        if let Some((code, town)) = full.or(first) {
            return (String::new(), format!("{code} {town}"));
        }
    }
    (String::new(), String::new())
}

/// Lower case words with "Kgs." and "Copenhagen" as the postcode list writes them.
fn town_words(text: &str) -> String {
    let text = text.to_lowercase();
    let text = KONGENS.replace_all(&text, "kongens ");
    let text = COPENHAGEN.replace_all(&text, "københavn");
    NOT_TOWN_LETTERS.replace_all(&text, " ").trim().to_string()
}

/// Whether the words after a postcode name its town ("Aarhus" for "Aarhus C").
fn names_town(words: &str, town: &str) -> bool {
    let words = town_words(words);
    let town = town.to_lowercase();
    let first = town.split(' ').next().unwrap_or("");
    format!("{words} ").starts_with(&format!("{town} "))
        || words.starts_with(&town)
        || pattern(&format!(r"^{}(?![\p{{L}}])", fancy_regex::escape(first)))
            .is_match(&words)
            .unwrap_or(false)
}

/// The street address at the end of the text before a postcode: "Vestergade 12, 2. th.".
fn street(before: &str) -> String {
    let before = before.trim_matches(|c| " \t,".contains(c));
    // The last part that starts with a street name and has a house number.
    let mut street: Vec<String> = Vec::new();
    for part in before.split(',').rev().map(str::trim) {
        // "Adresse: Vestergade 12": only the address.
        let part = LABEL.replace(part, "").into_owned();
        street.insert(0, part.clone());
        if STREET.is_match(&part).unwrap_or(false) {
            return street.join(", ");
        }
        if street.len() >= MAX_STREET_PARTS || !FLOOR.is_match(&part).unwrap_or(false) {
            break;
        }
    }
    String::new()
}

/// The contact person: name, phone (8 digits, as Jobnet's example) and email.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Contact {
    pub name: String,
    pub phone: String,
    pub email: String,
}

/// The contact person from the post's contact lines ("Har du spørgsmål, så kontakt Mette Hansen
/// på 12 34 56 78"). The email falls back to the address the post wants applications sent to.
pub fn contact(text: &str, company: &str, apply: Option<&ApplyInstructions>) -> Contact {
    let lines: Vec<&str> = text
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let mut found = Contact::default();
    for (i, line) in lines.iter().enumerate() {
        let is_contact = CONTACT.is_match(line).unwrap_or(false);
        // A contact sentence often ends a line and its details are on the next.
        let block = if is_contact {
            lines[i..(i + CONTACT_BLOCK_LINES).min(lines.len())].join("\n")
        } else {
            line.to_string()
        };
        if found.phone.is_empty() && (is_contact || PHONE_CUE.is_match(line).unwrap_or(false)) {
            for part in block.split('\n') {
                if !PHONE_CUE.is_match(part).unwrap_or(false) || CVR.is_match(part).unwrap_or(false) {
                    continue;
                }
                if let Some(number) = PHONE.captures(part).ok().flatten().and_then(|m| m.get(1)) {
                    found.phone = number.as_str().chars().filter(char::is_ascii_digit).collect();
                    break;
                }
            }
        }
        if is_contact
            && found.email.is_empty()
            && let Some(email) = EMAIL.find(&block).ok().flatten()
        {
            found.email = email.as_str().to_string();
        }
        if is_contact && found.name.is_empty() {
            found.name = person(&block, company, &found.email);
        }
    }
    if found.email.is_empty() {
        found.email = apply.map(|apply| apply.email.clone()).unwrap_or_default();
    }
    found
}

/// The first person's name in a contact line, or ''.
fn person(line: &str, company: &str, email: &str) -> String {
    let line = EMAIL.replace_all(line, " ");
    // "peter.olsen@" names the person the line is about.
    let local: Vec<String> = if email.is_empty() {
        Vec::new()
    } else {
        let name = email.split('@').next().unwrap_or("").to_lowercase();
        EMAIL_PARTS.split(&name).flatten().map(str::to_string).collect()
    };
    let company = company.to_lowercase();
    let mut candidates: Vec<String> = Vec::new();
    for found in PERSON.find_iter(line.as_ref()).flatten() {
        let mut words: Vec<&str> = WHITESPACE.split(found.as_str()).flatten().collect();
        // "Kontakt Mette Hansen": drop the words that aren't names.
        while words.first().is_some_and(|word| NOT_NAMES.contains(word)) {
            words.remove(0);
        }
        let name = words.join(" ");
        if words.len() < 2
            || words.iter().any(|word| NOT_NAMES.contains(word))
            || (!company.is_empty() && company.contains(&name.to_lowercase()))
        {
            continue;
        }
        candidates.push(name);
    }
    if !local.is_empty()
        && let Some(named) = candidates.iter().find(|candidate| {
            let first = candidate.split(' ').next().unwrap_or("").to_lowercase();
            local.contains(&first)
        })
    {
        return named.clone();
    }
    candidates.into_iter().next().unwrap_or_default()
}

/// Notes for the job centre: the match score, what the CV meets and misses, and how the post
/// asks to be applied to, within Jobnet's limit.
pub fn notes(saved: &SavedMatch) -> String {
    let (mut met, mut missing) = (Vec::new(), Vec::new());
    for item in saved.items.iter().filter(|item| item.criterion != "title") {
        if item.verdict == "missing" {
            missing.push(item.text.as_str());
        } else {
            met.push(item.text.as_str());
        }
    }
    let tag = if saved.tag.is_empty() {
        String::new()
    } else {
        format!(" ({})", saved.tag)
    };
    let mut lines = vec![format!("Match: {} %{tag}.", match_score::percent(saved.score))];
    if !met.is_empty() {
        lines.push(format!("Opfylder: {}.", met.join(", ")));
    }
    if !missing.is_empty() {
        lines.push(format!("Mangler: {}.", missing.join(", ")));
    }
    if let Some(apply) = &saved.apply_instructions
        && !apply.email.is_empty()
    {
        let subject = if apply.subject.is_empty() {
            String::new()
        } else {
            format!(" med emnet \"{}\"", apply.subject)
        };
        lines.push(format!("Ansøgningen sendes til {}{subject}.", apply.email));
    }
    let notes = lines.join("\n");
    if notes.chars().count() > MAX_NOTES {
        let cut = notes.chars().take(MAX_NOTES - 1).collect::<String>();
        return format!("{}…", cut.trim_end_matches([' ', '\t', '\n', '\r', '\0', '\x0B']));
    }
    notes
}

/// The text a status is shown as, by the value Jobnet posts, or None for one it doesn't know.
pub fn search_status(status: &str) -> Option<&'static str> {
    SEARCH_STATUSES
        .iter()
        .find(|(value, _)| *value == status)
        .map(|(_, text)| *text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_postcode_list_is_read_in_full() {
        assert_eq!(POSTCODES.len(), 1111);
        assert_eq!(TOWN_OF.get("1000"), Some(&"København K"));
        assert_eq!(postcode_of("Odense"), Some("5000"));
        assert_eq!(postcode_of(" aarhus c "), Some("8000"));
        assert_eq!(postcode_of("5230"), Some("5230"));
        assert_eq!(postcode_of("Atlantis"), None);
        assert_eq!(postcode_of(""), None);
        assert_eq!(TOWN_OF.get("4200"), Some(&"Slagelse"));
    }

    #[test]
    fn a_month_name_gives_its_number() {
        assert_eq!(month_number("Oktober"), 10);
        assert_eq!(month_number("sept"), 9);
        assert_eq!(month_number(""), 0);
    }
}
