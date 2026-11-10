//! How the application for a saved match is delivered, the way the post asks for it, ready to
//! print in the Apply dialog:
//! - by email, when the post names an address to send applications to (apply_instructions): the
//!   recipient, the subject (the one the post asks for, else the job title) and the body (the
//!   written application) to copy, and a mailto link with the recipient and subject;
//! - otherwise on the employer's side (an application form, LinkedIn Easy Apply): the link to the
//!   job post and the application to paste in.
//!
//! Either way the application and résumé PDFs to attach or upload (a mailto link can't carry
//! files), what else the post asks for, and the candidate's profile links (LinkedIn, GitHub, ...)
//! to copy into forms that ask for them. from() fills it by rule from the saved match and the
//! profile links, else the links the match's CV names (cv_links), no model call.

use super::apply_instructions;
use super::cv_links::{self, CvLink};
use super::document_box::{APPLICATION, RESUME};
use super::match_model::SavedMatch;

/// The kind of PDF that is the CV the match was made with.
const CV: &str = "cv";

pub const EMAIL: &str = "email";
pub const FORM: &str = "form";
pub const FORM_INTRO: &str = "The post doesn't ask for an email, so apply where it says: its application form or Easy Apply. Paste the application into the cover letter field and upload the PDFs.";
pub const NO_LINK: &str = "This match has no link to the post: open it where you found it.";
pub const TO_HINT: &str = "The address the post asks applications to be sent to.";
pub const NOT_WRITTEN: &str = "Write the application first (Write application).";
pub const NO_CV: &str = "There is no CV saved.";
pub const LINKS_HEADING: &str = "Your profile links";
/// The desktop app has no page to add profile links on yet.
pub const NO_LINKS: &str =
    "No profile links came with your profile, and the CV of this match names none, so there are none to copy here.";
pub const CV_LINKS: &str = "From the CV of this match. Profile links that come with your profile take their place.";

/// A value to copy: its field's id, label, value, a hint under it and whether it is a text box.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    pub id: String,
    pub label: String,
    pub value: String,
    pub hint: String,
    pub multiline: bool,
}

/// A PDF to attach or upload, and why it can't be had yet.
#[derive(Debug, Clone, PartialEq)]
pub struct File {
    pub kind: &'static str,
    pub label: &'static str,
    pub ready: bool,
    pub missing: &'static str,
}

/// The button that sends the candidate on: the mailto link or the job post.
#[derive(Debug, Clone, PartialEq)]
pub struct Open {
    pub href: String,
    pub label: &'static str,
    pub target: &'static str,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ApplicationDelivery {
    pub id: i64,
    /// EMAIL or FORM.
    pub by: &'static str,
    pub heading: &'static str,
    pub intro: String,
    pub fields: Vec<Field>,
    /// "The post also asks for answers to questions." or "".
    pub asks: String,
    pub files_heading: &'static str,
    pub files: Vec<File>,
    /// "Application: Write the application first ...", for each file not ready.
    pub missing: Vec<String>,
    pub open: Option<Open>,
    pub links_heading: &'static str,
    pub links: Vec<Field>,
    /// Where the links come from, or why there are none; "" for the profile's own.
    pub links_hint: &'static str,
}

impl ApplicationDelivery {
    /// links: the candidate's profile links; with none, the ones the CV names.
    pub fn from(found: &SavedMatch, links: &[CvLink]) -> Self {
        let by = by(found);
        let cv_links = if links.is_empty() {
            cv_links::read(&found.cv_text)
        } else {
            Vec::new()
        };
        let files = files(found);
        let shown_links = if links.is_empty() { &cv_links } else { links };
        ApplicationDelivery {
            id: found.id,
            by,
            heading: if by == EMAIL {
                "Apply by email"
            } else {
                "Apply on the employer's site"
            },
            intro: intro(found, by),
            fields: if by == EMAIL {
                email_fields(found)
            } else {
                vec![body_field(found, "Application")]
            },
            asks: asks(found),
            files_heading: if by == EMAIL {
                "Files to attach"
            } else {
                "Files to upload"
            },
            missing: missing(&files),
            files,
            open: if by == EMAIL {
                Some(mailto_link(found))
            } else {
                job_link(found)
            },
            links_heading: LINKS_HEADING,
            links: link_fields(shown_links),
            links_hint: if !links.is_empty() {
                ""
            } else if cv_links.is_empty() {
                NO_LINKS
            } else {
                CV_LINKS
            },
        }
    }
}

/// EMAIL when the post names an address to send applications to, else FORM.
pub fn by(found: &SavedMatch) -> &'static str {
    if to(found).is_empty() { FORM } else { EMAIL }
}

/// The subject line the post asks for, else the job title.
pub fn subject(found: &SavedMatch) -> String {
    let asked = found
        .apply_instructions
        .as_ref()
        .map(|apply| apply.subject.trim())
        .unwrap_or("");
    if asked.is_empty() {
        found.job_title.trim().to_string()
    } else {
        asked.to_string()
    }
}

/// "mailto:job@firma.dk?subject=Backend%202026": the address with its subject line,
/// percent-encoded (rawurlencode, keeping the @).
pub fn mailto(to: &str, subject: &str) -> String {
    let link = format!("mailto:{}", rawurlencode(to).replace("%40", "@"));
    if subject.is_empty() {
        link
    } else {
        format!("{link}?subject={}", rawurlencode(subject))
    }
}

/// RFC 3986 percent-encoding: every byte but A-Z a-z 0-9 - _ . ~ as %XX.
pub fn rawurlencode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn to(found: &SavedMatch) -> &str {
    found
        .apply_instructions
        .as_ref()
        .map(|apply| apply.email.trim())
        .unwrap_or("")
}

fn intro(found: &SavedMatch, by: &str) -> String {
    if by == EMAIL {
        String::new()
    } else if job_url(found).is_empty() {
        format!("{FORM_INTRO} {NO_LINK}")
    } else {
        FORM_INTRO.to_string()
    }
}

fn email_fields(found: &SavedMatch) -> Vec<Field> {
    vec![
        field("delivery-to", "To", to(found), TO_HINT, false),
        field("delivery-subject", "Subject", &subject(found), "", false),
        body_field(found, "Body"),
    ]
}

fn body_field(found: &SavedMatch, label: &str) -> Field {
    let body = application(found);
    let hint = if body.is_empty() { NOT_WRITTEN } else { "" };
    field("delivery-body", label, body, hint, true)
}

fn link_fields(links: &[CvLink]) -> Vec<Field> {
    links
        .iter()
        .enumerate()
        .map(|(n, link)| field(&format!("delivery-link-{}", n + 1), &link.label, &link.url, "", false))
        .collect()
}

fn field(id: &str, label: &str, value: &str, hint: &str, multiline: bool) -> Field {
    Field {
        id: id.to_string(),
        label: label.to_string(),
        value: value.to_string(),
        hint: hint.to_string(),
        multiline,
    }
}

/// "The post also asks for answers to questions." or "": what it asks besides the subject, in
/// apply_instructions::ASKS' order.
fn asks(found: &SavedMatch) -> String {
    let asked: &[&str] = found
        .apply_instructions
        .as_ref()
        .map(|apply| apply.asks.as_slice())
        .unwrap_or(&[]);
    let words: Vec<&str> = apply_instructions::ASKS
        .iter()
        .filter(|(ask, _)| *ask != "subject" && asked.contains(ask))
        .map(|(_, words)| *words)
        .collect();
    if words.is_empty() {
        String::new()
    } else {
        format!("The post also asks for {}.", words.join(", "))
    }
}

/// The application, and the tailored résumé when there is one, else the CV.
fn files(found: &SavedMatch) -> Vec<File> {
    let tailored = found.resume_text.as_deref().is_some_and(|text| !text.trim().is_empty());
    vec![
        File {
            kind: APPLICATION,
            label: "Application",
            ready: !application(found).is_empty(),
            missing: NOT_WRITTEN,
        },
        File {
            kind: if tailored { RESUME } else { CV },
            label: if tailored { "Tailored résumé" } else { "CV" },
            ready: !found.cv_text.trim().is_empty(),
            missing: NO_CV,
        },
    ]
}

fn missing(files: &[File]) -> Vec<String> {
    files
        .iter()
        .filter(|file| !file.ready)
        .map(|file| format!("{}: {}", file.label, file.missing))
        .collect()
}

fn mailto_link(found: &SavedMatch) -> Open {
    Open {
        href: mailto(to(found), &subject(found)),
        label: "Open in your email app",
        target: "_self",
    }
}

fn job_link(found: &SavedMatch) -> Option<Open> {
    let url = job_url(found);
    (!url.is_empty()).then(|| Open {
        href: url.to_string(),
        label: "Open the job post",
        target: "_blank",
    })
}

fn job_url(found: &SavedMatch) -> &str {
    found.job_url.as_deref().unwrap_or("").trim()
}

fn application(found: &SavedMatch) -> &str {
    found.application_text.as_deref().unwrap_or("").trim()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::matches::apply_instructions::ApplyInstructions;

    #[test]
    fn a_mailto_link_keeps_the_at_and_encodes_the_subject() {
        assert_eq!(mailto("job@firma.dk", ""), "mailto:job@firma.dk");
        assert_eq!(
            mailto("job@firma.dk", "Backend 2026 – æ"),
            "mailto:job@firma.dk?subject=Backend%202026%20%E2%80%93%20%C3%A6"
        );
    }

    fn saved() -> SavedMatch {
        SavedMatch {
            id: 7,
            job_title: "Backend-udvikler".into(),
            company: String::new(),
            job_url: Some("https://example.dk/job/1".into()),
            job_text: String::new(),
            cv_name: String::new(),
            cv_text: "Jonas Hansen".into(),
            score: 0.0,
            points: 0,
            max_points: 0,
            tag: String::new(),
            application_text: Some("Kære Nordisk Fragt\n\nJeg søger...".into()),
            application_written_at: Some(1),
            resume_text: Some("Jonas Hansen, tailored".into()),
            resume_written_at: Some(1),
            rejected_at: None,
            feedback_note: None,
            created_at: 0,
            resume_structured: false,
            items: Vec::new(),
            jobnet_joblog: None,
            rejections: Vec::new(),
            apply_instructions: Some(ApplyInstructions {
                by_email: true,
                email: "job@nordiskfragt.dk".into(),
                subject: "Backend & drift".into(),
                keyword: String::new(),
                asks: vec!["subject", "questions"],
                lines: Vec::new(),
                trap: true,
            }),
            earlier_versions: Vec::new(),
            opening: Default::default(),
        }
    }

    /// The fields, files and links as printed lines.
    fn shown(delivery: &ApplicationDelivery) -> Vec<String> {
        let mut lines = vec![
            format!("{}: {}", delivery.by, delivery.heading),
            format!("intro: {}", delivery.intro),
        ];
        for field in &delivery.fields {
            let first = field.value.lines().next().unwrap_or("");
            let hint = if field.hint.is_empty() {
                String::new()
            } else {
                format!(" ({})", field.hint)
            };
            let multiline = if field.multiline { " [multiline]" } else { "" };
            lines.push(format!("{} {}: {first}{hint}{multiline}", field.id, field.label));
        }
        lines.push(format!("asks: {}", delivery.asks));
        let files: Vec<String> = delivery
            .files
            .iter()
            .map(|file| {
                format!(
                    "{} {} {}",
                    file.label,
                    file.kind,
                    if file.ready { "ready" } else { "missing" }
                )
            })
            .collect();
        lines.push(format!("{}: {}", delivery.files_heading, files.join(", ")));
        lines.push(format!("missing: {}", delivery.missing.join(" | ")));
        lines.push(match &delivery.open {
            Some(open) => format!("open: {} {} {}", open.label, open.href, open.target),
            None => "open: -".to_string(),
        });
        let links: Vec<String> = delivery
            .links
            .iter()
            .map(|link| format!("{} {} {}", link.id, link.label, link.value))
            .collect();
        lines.push(format!(
            "{}: {}{}",
            delivery.links_heading,
            links.join(", "),
            delivery.links_hint
        ));
        lines
    }

    #[test]
    fn an_application_is_delivered_by_email_or_on_the_form() {
        let links = [
            CvLink {
                label: "LinkedIn".into(),
                url: "https://www.linkedin.com/in/jonas".into(),
            },
            CvLink {
                label: "GitHub".into(),
                url: "https://github.com/sasin91".into(),
            },
        ];
        let mut found = saved();
        assert_eq!(
            shown(&ApplicationDelivery::from(&found, &links)),
            [
                "email: Apply by email",
                "intro: ",
                "delivery-to To: job@nordiskfragt.dk (The address the post asks applications to be sent to.)",
                "delivery-subject Subject: Backend & drift",
                "delivery-body Body: Kære Nordisk Fragt [multiline]",
                "asks: The post also asks for answers to questions.",
                "Files to attach: Application application ready, Tailored résumé resume ready",
                "missing: ",
                "open: Open in your email app mailto:job@nordiskfragt.dk?subject=Backend%20%26%20drift _self",
                "Your profile links: delivery-link-1 LinkedIn https://www.linkedin.com/in/jonas, delivery-link-2 GitHub https://github.com/sasin91",
            ]
        );

        found.apply_instructions = None;
        found.application_text = Some(String::new());
        found.resume_text = None;
        let form = shown(&ApplicationDelivery::from(&found, &[]));
        assert_eq!(form[1], format!("intro: {FORM_INTRO}"));
        assert_eq!(
            form[2],
            "delivery-body Application:  (Write the application first (Write application).) [multiline]"
        );
        assert_eq!(form[4], "Files to upload: Application application missing, CV cv ready");
        assert_eq!(
            form[5],
            "missing: Application: Write the application first (Write application)."
        );
        assert_eq!(form[6], "open: Open the job post https://example.dk/job/1 _blank");
        assert_eq!(form[7], format!("Your profile links: {NO_LINKS}"));

        found.job_url = None;
        let no_link = shown(&ApplicationDelivery::from(&found, &[]));
        assert_eq!(no_link[1], format!("intro: {FORM_INTRO} {NO_LINK}"));
        assert_eq!(no_link[6], "open: -");

        found.apply_instructions = None;
        assert_eq!(
            mailto("job@x.dk", &subject(&found)),
            "mailto:job@x.dk?subject=Backend-udvikler"
        );
    }

    #[test]
    fn the_cvs_links_stand_in_for_profile_links() {
        let mut found = saved();
        found.cv_text = "https://github.com/sasin91".into();
        let delivery = ApplicationDelivery::from(&found, &[]);
        assert_eq!(delivery.links.len(), 1);
        assert_eq!(delivery.links_hint, CV_LINKS);
    }
}
