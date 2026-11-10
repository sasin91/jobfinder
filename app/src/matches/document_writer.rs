//! The job application and the tailored résumé for a saved match, written by the language model
//! from the CV and the post, for the page (and the job scan to come). Each is saved as a new
//! version of the match's document (documents), with the model that wrote it and the candidate's
//! notes. Never a URL.

use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::apply_instructions;
use super::document_box::Keyword;
use super::free_reader::YearMonth;
use super::match_model::{self, Document, SavedItem, SavedMatch};
use super::match_score::MatchScore;
use super::tailored_resume;
use crate::llm::{Effort, LlmException};
use crate::matches::llm_key::KeyedLlm;
use crate::profile;

/// Longest notes the candidate can add for an application or résumé, in characters.
pub const MAX_NOTES: usize = 2000;

/// The longest answer each document may take, in tokens: about twice the longest a local model wrote
/// in the model bake-off, so one that repeats itself stops in minutes, not at the call's time
/// limit. The résumé holds the whole CV again, so it gets more.
const APPLICATION_TOKENS: u32 = 4096;
const RESUME_TOKENS: u32 = 8192;

/// What the strengths list says when the CV meets nothing.
const NO_STRENGTHS: &str = "- (none found)";
/// What the gaps list says when the CV misses nothing.
const NO_GAPS: &str = "- (none)";

/// Why a document couldn't be written.
#[derive(Debug)]
pub enum WriteError {
    Llm(LlmException),
    Database(sqlx::Error),
    /// The model answered without the document; trying again usually works.
    Empty(&'static str),
}

impl From<LlmException> for WriteError {
    fn from(e: LlmException) -> Self {
        WriteError::Llm(e)
    }
}

impl From<sqlx::Error> for WriteError {
    fn from(e: sqlx::Error) -> Self {
        WriteError::Database(e)
    }
}

/// A tailored résumé just written: its version, and the post's keywords it took over (each with
/// the CV fact behind it) and the ones it left out because the CV doesn't back them.
#[derive(Debug, Clone, PartialEq)]
pub struct Tailored {
    pub document: Document,
    pub keywords: Vec<Keyword>,
    pub left_out: Vec<String>,
}

/// The candidate's notes as they are kept: trimmed, at most MAX_NOTES characters.
pub fn notes(sent: &str) -> String {
    let cut: String = sent.chars().take(MAX_NOTES).collect();
    cut.trim().to_string()
}

/// The writing prompts' line with the candidate's years of professional work: the number they
/// saved, else the one counted from the profile's jobs (profile::years_of_work), or saying there
/// is none. The model is told to use this number and never count itself: counting from the
/// whole CV, it took the earliest date in it (school, own projects) and wrote "16 years in
/// software development" for 9 years of work.
fn years_line(years: Option<i64>) -> String {
    let label = profile::YEARS_OF_WORK_LABEL;
    match years {
        Some(years) => format!("{label}{years} (education and own projects not included)"),
        None => format!("{label}not given"),
    }
}

/// The writing prompts' rule on years of experience, which goes with years_line.
const YEARS_RULE: &str = "Never count years of experience yourself. When the text says how long the candidate has worked (in total, or as a developer, in the field, in software), use exactly the number given as \"Years of professional work\", and only for work; years with one skill or at one employer only as the CV's own dates show them. Education, school projects and the candidate's own or hobby projects are not years of work, however early they start. When no number is given, don't state a total number of years.";

/// The years line for a saved match's documents: from the profile when there is one.
async fn years_of_work(db: &SqlitePool, now: i64) -> sqlx::Result<String> {
    let years = profile::years_of_work(db, YearMonth::of(now)).await?;
    Ok(years_line(years.and_then(|years| years.years())))
}

/// A rewrite of a written version the candidate commented on, as a pull request's review is
/// sent: the version as it was in its box (edits included), and the comment threads on its
/// passages.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Revision {
    pub previous: String,
    pub comments: Vec<Comment>,
}

/// A comment thread on one passage of a written version: what the candidate said, and the
/// writer's answers when they asked (document_question).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Comment {
    /// The text they highlighted, as it stands in the version.
    pub passage: String,
    pub said: Vec<Said>,
}

/// One line of a comment thread.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    pub by: By,
    pub text: String,
}

/// Who said a line of a comment thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    Candidate,
    /// The model that writes the document, answering a question.
    Writer,
}

impl By {
    /// As the prompt and the panel name them.
    pub fn label(self) -> &'static str {
        match self {
            By::Candidate => "Candidate",
            By::Writer => "Writer",
        }
    }
}

/// A thread's lines as the prompt has them: "  Candidate: …", one per line.
pub(super) fn thread_lines(said: &[Said]) -> String {
    said.iter()
        .map(|line| format!("  {}: {}", line.by.label(), line.text.trim().replace('\n', " ")))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The comments as a list: "- On “passage”:" and its thread under it.
fn comment_lines(comments: &[Comment]) -> String {
    comments
        .iter()
        .map(|c| format!("- On “{}”:\n{}", c.passage.trim(), thread_lines(&c.said)))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The previous version and the comments on it, as a block to end the prompt with, or "" for a
/// first writing.
fn revision_block(revision: Option<&Revision>, document: &str) -> String {
    let Some(revision) = revision else {
        return String::new();
    };
    format!(
        "\nThe previous version of the {document}, as the candidate has it:\n\n{previous}\n\nThe candidate's comments on it:\n{comments}\n\nThis is a revision: write an improved version of the previous one. Keep what the comments don't touch close to how it was, change what each comment thread asks for or agrees on, and never write again what a thread says is wrong. Treat facts the candidate states in the threads as true, like facts in the CV.",
        previous = revision.previous.trim(),
        comments = comment_lines(&revision.comments),
    )
}

/// The notes a version is saved with: the candidate's, and the comments it was revised on, so
/// Earlier versions says why it changed.
pub fn saved_notes(notes: &str, revision: Option<&Revision>) -> String {
    let Some(revision) = revision.filter(|revision| !revision.comments.is_empty()) else {
        return notes.to_string();
    };
    let comments = format!("Revised on these comments:\n{}", comment_lines(&revision.comments));
    if notes.is_empty() {
        comments
    } else {
        format!("{notes}\n{comments}")
    }
}

/// The notes as a block to end the prompt with, or "" when there are none.
pub(super) fn notes_block(notes: &str) -> String {
    if notes.is_empty() {
        String::new()
    } else {
        format!("\nNotes from the candidate:\n\n{notes}")
    }
}

/// A job application for a saved match, written from the CV and the post in the post's
/// language when the candidate speaks it (English otherwise), and saved with the match. notes: what the candidate wants stressed, or facts the CV
/// leaves out.
pub async fn write_application(
    db: &SqlitePool,
    found: &SavedMatch,
    llm: &KeyedLlm,
    notes: &str,
    revision: Option<&Revision>,
    now: i64,
) -> Result<Document, WriteError> {
    let cv = written_from(db, found).await?;
    let years = years_of_work(db, now).await?;
    let system = application_system();
    let user = format!(
        "{brief}

{years}
{notes_block}
{revision_block}

Write in written_language: the job post's own language (post_language) when the candidate speaks it, otherwise English. Name employer_needs and standout before writing, and build the letter on them.",
        brief = match_brief(found, &cv),
        notes_block = notes_block(notes),
        revision_block = revision_block(revision, "application"),
    );
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["post_language", "written_language", "employer_needs", "standout", "application"],
        "properties": {
            "post_language": { "type": "string" },
            "written_language": { "type": "string" },
            "employer_needs": { "type": "string" },
            "standout": standout_schema(),
            "application": { "type": "string" },
        },
    });
    let answer = llm
        .llm
        .structured(&system, &user, &schema, Effort::Medium, APPLICATION_TOKENS)
        .await?;
    let text = answer["application"].as_str().unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Err(WriteError::Empty("The model returned an empty application. Try again."));
    }
    Ok(match_model::save_application(db, found.id, &text, &llm.written_by, &saved_notes(notes, revision), now).await?)
}

/// The CV rewritten for a saved match's post, from the CV's own facts and the candidate's notes
/// only, and saved with the match as fields (tailored_resume) and the plain text made from them.
/// The rewrite takes over the post's own words wherever the CV backs them, also by reasonable
/// inference, so screeners and keyword filters find them.
pub async fn tailor_resume(
    db: &SqlitePool,
    found: &SavedMatch,
    llm: &KeyedLlm,
    notes: &str,
    revision: Option<&Revision>,
    now: i64,
) -> Result<Tailored, WriteError> {
    let (strengths, gaps) = strengths_and_gaps(&found.items);
    let apply = apply_instructions::for_prompt(found.apply_instructions.as_ref());
    let cv = written_from(db, found).await?;
    let years = years_of_work(db, now).await?;
    let system = resume_system();
    let user = format!(
        "Job title: {job_title}
Company: {company}

Requirements the CV meets:
{strengths}

Requirements the matcher marked missing (use only if the CV backs them, also by inference):
{gaps}

{years}

{apply}

Job post:

{job_text}

CV:

{cv}
{notes_block}
{revision_block}

Write in written_language: the job post's own language (post_language) when the candidate speaks it, otherwise English. Name employer_needs and standout before writing, and build the CV on them.",
        job_title = found.job_title,
        company = found.company,
        job_text = found.job_text,
        notes_block = notes_block(notes),
        revision_block = revision_block(revision, "CV"),
    );
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["post_language", "written_language", "employer_needs", "standout", "keywords", "left_out", "resume"],
        "properties": {
            "post_language": { "type": "string" },
            "written_language": { "type": "string" },
            "employer_needs": { "type": "string" },
            "standout": standout_schema(),
            "keywords": {
                "type": "array",
                "items": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["term", "basis"],
                    "properties": {
                        "term": { "type": "string" },
                        "basis": { "type": "string" },
                    },
                },
            },
            "left_out": { "type": "array", "items": { "type": "string" } },
            "resume": tailored_resume::schema(),
        },
    });
    let answer = llm
        .llm
        .structured(&system, &user, &schema, Effort::Medium, RESUME_TOKENS)
        .await?;
    let Some(resume) = tailored_resume::clean(&answer["resume"]) else {
        return Err(WriteError::Empty("The model returned an empty résumé. Try again."));
    };
    let text = tailored_resume::text(&resume);
    let saved = saved_notes(notes, revision);
    let document = match_model::save_resume_fields(db, found.id, &resume, &text, &llm.written_by, &saved, now).await?;
    Ok(Tailored {
        document,
        keywords: resume_keywords(&answer["keywords"]),
        left_out: left_out(&answer["left_out"]),
    })
}

fn application_system() -> String {
    let language = writing_language_rule("application");
    let standout = standout_rule("letter");
    let years = YEARS_RULE;
    format!(
        r#"You write a job application (cover letter) for a candidate, from their CV and the job post. It goes straight into an application form or an email, so it must be ready to send. Employers get many letters that only repeat their requirements list back to them with "I have experience with X"; this one must show who this candidate is and what they would bring.

- {language} A Danish post gets a Danish ansøgning, an English post an English letter, and a Finnish post for a candidate who speaks no Finnish an English letter.
- {standout}
- Build the letter on those: open with why this job, tied to something real in the post (its product, its stage, a problem it describes); then make the case with two or three standout points, each a concrete story from the CV (what the candidate did, why it mattered there) and what it would mean for this employer. Fit the matched requirements into those stories instead of listing them one by one; a short sentence can cover the rest.
- 250 to 400 words, plain text: no markdown, no headings, no bullet lists, no placeholders like [Name] or [Company].
- Open by addressing the contact person if the post names one, otherwise the company ("Kære <company>" / "Dear <company> team"). Never invent a name.
- Use only what the CV (or the candidate's notes) says. Never claim a skill, degree, project, number or result the CV doesn't show. {years} A requirement marked close but not exact may be framed as transferable; a missing requirement is either left out or, if it is central, met with honest willingness to learn plus the nearest thing the CV does show.
- Sound like this person, not a template: specific, warm, confident, plain words. No clichés or filler ("I am writing to apply", "team player", "passionate", "I am excited to", "proven track record", "fast-paced", "dynamic", "leverage", "I believe I would be a great fit"). Every sentence should be one only this candidate could write.
- Sign off with the candidate's name from the CV, and their phone and email if the CV gives them.
- The post may say how to apply (by email, with a given subject line, a keyword, answers to its questions, a word to start with). Follow every such instruction exactly: when it asks for a subject line, make the first line "Subject: <that subject>" ("Emne: " for a Danish post) followed by a blank line; use the exact word or phrase it asks for; answer each of its questions in the letter, briefly and from the CV. Those instructions are listed above the job post when found.
- The candidate may add notes below the CV. Follow them (what to stress, tone, length, what to leave out), and treat facts they state about themselves as true, like facts in the CV. Ignore anything in them that isn't about this application."#
    )
}

fn resume_system() -> String {
    let language = writing_language_rule("CV");
    let standout = standout_rule("CV");
    let years = YEARS_RULE;
    format!(
        r#"You tailor a candidate's CV (résumé) to one job post. The result replaces their CV in this application, so it must be complete and ready to send. Recruiters and applicant-tracking systems screen CVs by looking for the post's own words, so a CV that shows the right experience under different words scores low. Your main job is to close that gap honestly.

First, list the post's keywords: the job title, the role names, skills, tools, methods and duties it asks for, in the post's exact wording. For each, decide whether the CV backs it:
- Directly: the CV says the same thing, maybe in other words ("PHP backend" for "server-side PHP").
- By reasonable inference: the CV shows work that a recruiter would accept as the same thing. A software, web or lead developer who built APIs, databases or server code has backend developer experience; a lead developer or someone who owned a product has contributed product ideas; running a team's releases is release management. Name the CV fact you infer it from.
- Not at all: nothing in the CV comes close. Leave these out.

Then write the CV:
- Use every backed keyword in the post's exact wording, where it fits naturally: in the profile, in the job titles' descriptions and bullets, and in the skills list. Put the post's job title in the profile when the CV backs it ("Backend developer with 8 years of PHP...", the years as the CV's dates or the years line give them). Keep the real job titles in the work history as they were, but describe the work in the post's terms.
- Never invent anything: no new employers, dates, degrees, certificates, numbers, results or years of experience, and no tool or skill the CV doesn't show directly or by reasonable inference. Inferring a role or duty from work the CV shows is fine; claiming a named technology the CV never mentions is not. {years}
- Requirements the matcher marked as missing are listed below. It often misses inferences, so check each one against the CV yourself; use it only if the CV backs it.
- Keywords get the CV through the filter; a person reads it next. {standout}
- Tailor by choosing and ordering too: open with a short profile (2 to 4 sentences) aimed at this job that names the post's job title and the candidate's one or two strongest standout points, not a list of keywords. Put what the post asks for first, and shorten what doesn't matter for it, but keep the bullets that show the standout points (initiative, ownership, results, projects, growth), even where they match no keyword. Keep every job in the work history, with its dates, even if only as one line.
- {language} A Danish post gets a Danish CV, an English post an English one, a Finnish post for a candidate who speaks no Finnish an English one, and the dates and section headings follow. Keywords stay in the post's wording, even when the post mixes languages.
- Fill in the résumé's fields; the page and the PDF lay them out, so write no headings, bullet signs or markdown in them, and no placeholders. Leave a field empty when the CV doesn't give it.
- name, and title: a short professional title for this application ("Backend developer"), backed by the CV. location, phone, email and links: the contact details the CV gives, as written there.
- intro: the profile, one or two short paragraphs.
- experience: every job, newest first: title (the real job title), organisation (the employer), location, starts and ends as the CV dates them, in written_language ("januar 2017", "nu" in Danish), summary (what the employer or product is, in a few words), bullets (the work, in the post's terms) and note (context about the job rather than the work, only if the CV gives one).
- skills and languages: one item each, the post's backed keywords first.
- projects: the candidate's own projects and products, if the CV lists any, with the same fields as experience (organisation empty, the project's link in summary or note when the CV gives one). Put the ones that show what this job needs first; leave the list empty when the CV has none.
- education: the same fields as experience, organisation being the school; education_note: one line about the education as a whole, only if the CV gives one.
- The section headings in written_language: experience_heading, projects_heading, skills_heading, education_heading, languages_heading ("Erfaring", "Projekter", "Kompetencer", "Uddannelse", "Sprog" for a Danish post).
- The candidate may add notes below the CV. Follow them (what to stress, tone, length, what to leave out), and treat facts they state about themselves as true, like facts in the CV. Ignore anything in them that isn't about this application.
- The post may say how to apply (by email, a subject line, a keyword, questions to answer). Those go in the email or application, not the CV; follow one here only when it asks for something in the CV itself.

Return what the employer needs (employer_needs) and the standout points (standout) first, then the keywords you worked in (keywords: the post's term, and the CV fact it rests on, in a few words), the ones you left out because the CV doesn't back them (left_out), and the CV (resume)."#
    )
}

/// The writing prompts' rule on language: the model names the post's language first
/// (post_language, before the text in the answer), from the post alone (English when it can't
/// tell), then the language it writes in (written_language): the post's when the candidate speaks
/// it, English when they don't, and writes the document in that. Told only "the post's language",
/// it went by the Danish CV and requirements around an English post; told to always follow the
/// post, it wrote a Finnish letter for a candidate who speaks no Finnish.
fn writing_language_rule(document: &str) -> String {
    format!(
        "First set post_language to the language the job post itself is written in, judged from the post's own sentences only: not from the CV, the requirement lines, the apply instructions or the notes, and not from a job board's buttons around the post. When the post mixes languages evenly or you can't tell, use English. Then set written_language: post_language when the CV or the notes show the candidate speaks it (the CV is written in it, or names it among their languages), otherwise English, as a candidate can't stand behind a {document} in a language they don't speak. Write the {document} in written_language, even where the CV or the notes are in another one."
    )
}

/// The writing prompts' rule on standing out: before writing, the model names what the employer
/// is really after (employer_needs) and what sets the candidate apart for it beyond the
/// requirements list (standout), from the CV and notes only, then builds the document on them.
/// Asked only to show fit, it wrote the requirements list back with "I have experience with X",
/// like any generic letter.
fn standout_rule(document: &str) -> String {
    format!(
        "Before writing, read past the requirements list. Set employer_needs to what this employer is really after, in two or three sentences from the post: the product, the stage the company or team is at, the problems or goals the post hints at. Then set standout to the two to four things in the CV (or the notes) that set this candidate apart for that, beyond the listed requirements: things they built or started on their own initiative, problems they solved and what changed, ownership (sole developer, lead, end-to-end), growth (junior to lead, rehired, trusted with more), breadth that helps here, side projects and products of their own. For each, give the CV fact as it stands and why it matters to this employer. Use only what the CV or the notes say; never add a project, number or result. Then write the {document} so a reader remembers those points, not only that the requirements are met."
    )
}

/// The answer's standout list: the CV fact and why it matters here.
fn standout_schema() -> Value {
    json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "required": ["fact", "why_it_matters"],
            "properties": {
                "fact": { "type": "string" },
                "why_it_matters": { "type": "string" },
            },
        },
    })
}

/// What a prompt says about a saved match: the job, the requirements the CV meets and doesn't
/// show, how to apply, the post and the CV (cv). The application's prompt and the chat prompt
/// (chat_prompt) both start from it.
fn match_brief(found: &SavedMatch, cv: &str) -> String {
    let (strengths, gaps) = strengths_and_gaps(&found.items);
    let apply = apply_instructions::for_prompt(found.apply_instructions.as_ref());
    format!(
        "Job title: {job_title}
Company: {company}

Requirements the CV meets:
{strengths}

Requirements the CV doesn't show:
{gaps}

{apply}

Job post:

{job_text}

CV:

{cv}",
        job_title = found.job_title,
        company = found.company,
        job_text = found.job_text,
    )
}

/// A prompt to paste into a chatbot of the candidate's own (Copy as prompt): the match's brief
/// (match_brief), its score, and the application and résumé as written (application, resume:
/// the boxes' text, empty when not written), so the chat can help with this job from all of it.
pub async fn chat_prompt(db: &SqlitePool, found: &SavedMatch, application: &str, resume: &str) -> sqlx::Result<String> {
    let cv = written_from(db, found).await?;
    Ok(chat_prompt_from(found, &cv, application, resume))
}

/// chat_prompt from the CV it is written from.
fn chat_prompt_from(found: &SavedMatch, cv: &str, application: &str, resume: &str) -> String {
    let score = MatchScore::of(found.score, &found.tag);
    let mut prompt = format!(
        "I'm thinking of applying for the job below. JobFinder matched my CV with it: {percent}% ({tag}), {points}/{max_points} points. Here is the match, the job post and my CV. Help me with this application: answer my questions about it, and when I ask, write or improve my application or résumé from my CV's facts only. Ask me before you assume anything the CV doesn't say.

{brief}",
        percent = score.percent,
        tag = score.tag_name,
        points = found.points,
        max_points = found.max_points,
        brief = match_brief(found, cv),
    );
    for (heading, text) in [("My application so far", application), ("My résumé so far", resume)] {
        let text = text.trim();
        if !text.is_empty() {
            prompt.push_str(&format!("\n\n{heading}:\n\n{text}"));
        }
    }
    prompt
}

/// A saved match's verdicts as two bullet lists for a prompt: what the CV meets (with its
/// evidence) and what it doesn't show. The job title isn't a requirement to write about.
fn strengths_and_gaps(items: &[SavedItem]) -> (String, String) {
    let mut strengths = Vec::new();
    let mut gaps = Vec::new();
    for item in items.iter().filter(|item| item.criterion != "title") {
        let evidence = if item.evidence.is_empty() {
            String::new()
        } else {
            format!(" (CV: {})", item.evidence)
        };
        let line = format!("- {}{evidence}", item.text);
        match item.verdict.as_str() {
            "missing" => gaps.push(line),
            "partial" => strengths.push(format!("{line} [close, not exact]")),
            _ => strengths.push(line),
        }
    }
    let list = |lines: Vec<String>, none: &str| {
        if lines.is_empty() {
            none.to_string()
        } else {
            lines.join("\n")
        }
    };
    (list(strengths, NO_STRENGTHS), list(gaps, NO_GAPS))
}

/// What a match's application and résumé are written from: the user's profile when they have
/// one, else the CV the match was made with.
pub(super) async fn written_from(db: &SqlitePool, found: &SavedMatch) -> sqlx::Result<String> {
    Ok(profile::text_for_user(db)
        .await?
        .unwrap_or_else(|| found.cv_text.clone()))
}

/// The model's {term, basis} keyword list, cleaned for the page.
fn resume_keywords(keywords: &Value) -> Vec<Keyword> {
    let Some(list) = keywords.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter(|keyword| keyword.is_object())
        .map(|keyword| Keyword {
            term: keyword["term"].as_str().unwrap_or("").trim().to_string(),
            basis: keyword["basis"].as_str().unwrap_or("").trim().to_string(),
        })
        .filter(|keyword| !keyword.term.is_empty())
        .collect()
}

/// The terms the résumé left out, trimmed, without empty ones.
fn left_out(terms: &Value) -> Vec<String> {
    let Some(list) = terms.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(Value::as_str)
        .map(|term| term.trim().to_string())
        .filter(|term| !term.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chat_prompt_has_the_score_the_brief_and_only_what_is_written() {
        let found = SavedMatch {
            id: 3,
            job_title: "Backend developer".into(),
            company: "Nordisk Fragt".into(),
            job_url: None,
            job_text: "We need Rust.".into(),
            cv_name: String::new(),
            cv_text: String::new(),
            score: 0.86,
            points: 6,
            max_points: 7,
            tag: "top".into(),
            application_text: None,
            application_written_at: None,
            resume_text: None,
            resume_written_at: None,
            rejected_at: None,
            feedback_note: None,
            created_at: 0,
            resume_structured: false,
            items: vec![item("skill", "Rust", "met", "5 years of Rust")],
            jobnet_joblog: None,
            rejections: Vec::new(),
            apply_instructions: None,
            earlier_versions: Vec::new(),
            opening: Default::default(),
        };
        let prompt = chat_prompt_from(&found, "Jonas, Rust developer", "Dear Nordisk Fragt", " ");
        assert!(prompt.contains("86% (Top match), 6/7 points"), "{prompt}");
        assert!(prompt.contains(&match_brief(&found, "Jonas, Rust developer")));
        assert!(prompt.contains("- Rust (CV: 5 years of Rust)"));
        assert!(prompt.ends_with("My application so far:\n\nDear Nordisk Fragt"));
        assert!(!prompt.contains("My résumé so far"));
    }

    fn item(criterion: &str, text: &str, verdict: &str, evidence: &str) -> SavedItem {
        SavedItem {
            criterion: criterion.into(),
            text: text.into(),
            verdict: verdict.into(),
            decided_by: "exact".into(),
            reason: String::new(),
            evidence: evidence.into(),
        }
    }

    #[test]
    fn the_verdicts_become_the_strengths_and_gaps_the_prompt_lists() {
        let items = [
            item("title", "Backend-udvikler", "met", ""),
            item("requirements", "PHP", "met", "8 years of PHP"),
            item("requirements", "React", "partial", "Vue"),
            item("requirements", "Go", "missing", ""),
        ];
        assert_eq!(
            strengths_and_gaps(&items),
            (
                "- PHP (CV: 8 years of PHP)\n- React (CV: Vue) [close, not exact]".to_string(),
                "- Go".to_string()
            )
        );
        assert_eq!(
            strengths_and_gaps(&[]),
            ("- (none found)".to_string(), "- (none)".to_string())
        );
    }

    #[test]
    fn the_years_line_gives_the_count_or_says_there_is_none() {
        assert_eq!(
            years_line(Some(9)),
            "Years of professional work: 9 (education and own projects not included)"
        );
        assert_eq!(years_line(None), "Years of professional work: not given");
        assert!(application_system().contains(YEARS_RULE));
        assert!(resume_system().contains(YEARS_RULE));
    }

    #[test]
    fn notes_are_cut_and_end_the_prompt_as_a_block() {
        assert_eq!(notes("  keep it short \n"), "keep it short");
        assert_eq!(notes(&"x".repeat(MAX_NOTES + 10)).chars().count(), MAX_NOTES);
        assert_eq!(notes_block(""), "");
        assert_eq!(notes_block("short"), "\nNotes from the candidate:\n\nshort");
    }

    #[test]
    fn the_keywords_are_cleaned_for_the_page() {
        let answer = json!([{ "term": " PHP ", "basis": "8 years" }, { "term": "", "basis": "x" }, "Go"]);
        assert_eq!(
            resume_keywords(&answer),
            [Keyword {
                term: "PHP".into(),
                basis: "8 years".into()
            }]
        );
        assert_eq!(left_out(&json!([" Go ", "", 3])), ["Go"]);
    }
}
