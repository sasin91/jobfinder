//! match: matching a job post against a CV, ported from modules/cv_match.
//!
//! A job post and a CV come in the shapes of the language model's extract_job and extract_cv
//! answers, whether the model read them or Free_reader did, so Cv_matcher scores both the same.

pub mod ai_match;
pub mod application_delivery;
pub mod application_form;
pub mod apply_instructions;
pub mod apply_notice;
pub mod batch_row;
pub mod cv_links;
pub mod cvr;
pub mod dates;
pub mod disqualifier_notice;
pub mod document_box;
pub mod document_question;
pub mod document_writer;
pub mod free_reader;
pub mod joblog;
pub mod joblog_panel;
pub mod llm_key;
pub mod match_documents;
pub mod match_model;
pub mod match_prompts;
pub mod match_result;
pub mod match_score;
pub mod match_service;
pub mod matcher;
pub mod page_reader;
pub mod pdf_writer;
pub mod reject_button;
pub mod reject_reason;
mod rejection_feedback;
pub mod resume_comparison;
pub mod saved_match_row;
pub mod score_cursor;
pub mod seniority;
pub mod still_open;
pub mod tailored_resume;
pub mod taxonomy_lessons;
pub mod text_changes;
pub mod verdict_row;

use serde::{Deserialize, Serialize};

use crate::taxonomy::Method;

/// A job post as extract_job answers it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    /// The post in English. The free reader keeps the post's own text here; a model's read leaves
    /// it empty.
    pub text_en: String,
    pub job_title: String,
    pub job_title_en: String,
    pub heading: String,
    pub company: String,
    pub requirements: Vec<Requirement>,
    pub responsibilities: Vec<String>,
    /// The career level the post states, one of seniority::LEVELS ("student", "senior"), or ""
    /// when it states none (seniority::level_of).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub level: String,
    /// The language the job is done in, in English ("Danish", "Finnish"): the one the post is
    /// written in, unless the post names another working language. "" when the post wasn't read
    /// by a model (Free_reader doesn't tell languages apart). matcher::criteria makes it a
    /// requirement the candidate must speak.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub working_language: String,
}

/// One thing a job post asks for. Requirements sharing an `alt_group` are alternatives
/// ("pædagog eller pædagogisk assistent").
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Requirement {
    pub value: String,
    pub english: String,
    /// skill, education, certificate, experience, language, soft_skill or location.
    pub kind: String,
    pub required: bool,
    pub alt_group: String,
    pub min_years: i64,
    pub min_level: String,
}

/// A CV as extract_cv answers it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Profile {
    pub name: String,
    /// Where the candidate lives, as the CV gives it ("Slagelse, 4200"); the free reader leaves
    /// it empty, and so does its JSON, as Free_reader has no location.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub location: String,
    pub text_en: String,
    pub title: Vec<String>,
    pub skill: Vec<String>,
    pub language: Vec<String>,
    pub certificate: Vec<String>,
    pub education: Vec<String>,
    pub experience_years: i64,
    pub responsibilities: Vec<String>,
}

/// A taxonomy term a phrase is known to name: its position in Taxonomy::terms and how the
/// phrase matched it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct KnownTerm {
    pub index: usize,
    pub method: Method,
}
