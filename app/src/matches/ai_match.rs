//! One job post through the match, for the page's match endpoint (and the job scan to come): the
//! requirements decided by the taxonomy against the CV, the rest judged by the model (or, without
//! one, left missing as the free match leaves them), then scored and saved for the user. The job
//! post and CV come read by match_prompts or by the free reader. Never a URL.

use indexmap::IndexMap;
use serde::Serialize;
use sqlx::SqlitePool;

use super::apply_instructions::{self, ApplyInstructions};
use super::match_model::{self, NewMatch};
use super::match_prompts::{self, Undecided};
use super::matcher::{self, Groups, LANGUAGE, LOCATION, Matcher, Score, Verdict, Verdicts};
use super::{Job, KnownTerm, Profile};
use crate::laya::laya_client::{self, LayaClient, LayaReading};
use crate::llm::{Llm, LlmException};
use crate::taxonomy::Taxonomy;

/// What a requirement the taxonomy didn't find in the CV says, when no model looked further.
const NOT_FOUND_REASON: &str =
    "Not named in the CV. With an AI key on for evaluating job posts, the AI also looks for related experience.";

/// What a location requirement nobody weighed says.
const UNWEIGHED_LOCATION_REASON: &str =
    "Check this yourself: with an AI key on for evaluating job posts, the AI weighs it against where you live.";

/// What a language requirement the taxonomy couldn't find, and nobody weighed, says.
const UNWEIGHED_LANGUAGE_REASON: &str = "Check this yourself: with an AI key on for evaluating job posts, the AI checks it against the languages your CV shows.";

/// The match's answer: the criteria, the combined verdicts and the score, with the soft skills,
/// how the post asks to be applied to, the Laya summary and, with Laya's model, its answer on that
/// summary (laya), or laya_error; saved_id once saved, or save_error when the database couldn't
/// take it (the score still comes back).
#[derive(Debug, Clone, Serialize)]
pub struct AiMatch {
    pub groups: Groups,
    pub verdicts: Verdicts,
    pub result: Score,
    pub soft_skills: Vec<String>,
    pub apply_instructions: Option<ApplyInstructions>,
    pub laya_summary: String,
    /// laya and laya_error.
    #[serde(flatten)]
    pub laya_reading: LayaReading,
    pub saved_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub save_error: Option<String>,
}

/// What the match needs besides the post and the CV as read.
pub struct Matching<'a> {
    pub db: &'a SqlitePool,
    pub taxonomy: &'a Taxonomy,
    pub now: i64,
    pub laya: Option<&'a LayaClient>,
}

/// The match of job with profile, judged on llm where the taxonomy can't decide, and saved when
/// the post's and the CV's texts are given. new.cv_name "" is the CV's own name.
pub async fn ai_match(
    matching: &Matching<'_>,
    llm: Option<&Llm>,
    new: NewMatch<'_>,
    profile: &Profile,
    known: IndexMap<String, KnownTerm>,
) -> Result<AiMatch, LlmException> {
    let job = new.job;
    let matcher = Matcher::new(matching.taxonomy, profile.clone(), known);
    let groups = matcher::criteria_for_post(job, new.job_text);
    let mut verdicts = Verdicts::new();
    let mut undecided = Vec::new();
    for unit in matcher::units(&groups) {
        match matcher.decide(unit) {
            Some(verdict) => {
                verdicts.insert(unit.id.clone(), verdict);
            }
            None => undecided.push(Undecided {
                id: unit.id.clone(),
                text: unit.text.clone(),
                kind: unit.kind.clone(),
                min_years: unit.min_years,
            }),
        }
    }
    match llm {
        Some(llm) if !undecided.is_empty() => {
            let cv_text = if new.cv_text.trim().is_empty() {
                &profile.text_en
            } else {
                new.cv_text
            };
            let judged = match_prompts::judge(
                llm,
                &job.job_title,
                &undecided,
                cv_text,
                profile.location.trim(),
                &profile.language,
            )
            .await?;
            // Only the requirements it was asked about: an id it made up, or one the taxonomy
            // decided, would put its verdict on the wrong requirement.
            let cv_for_evidence = evidence_text(cv_text);
            for unit in undecided {
                if let Some(answer) = judged.iter().find(|answer| answer.id == unit.id) {
                    let verdict = grounded_verdict(&unit, answer, &cv_for_evidence);
                    verdicts.insert(unit.id, verdict);
                } else if unit.kind == LOCATION {
                    verdicts.insert(
                        unit.id,
                        matcher::location_to_check("The match could not verify the commute."),
                    );
                }
            }
        }
        _ => {
            for unit in undecided {
                let verdict = match unit.kind.as_str() {
                    LOCATION => matcher::location_to_check(UNWEIGHED_LOCATION_REASON),
                    LANGUAGE => unweighed(UNWEIGHED_LANGUAGE_REASON),
                    _ => not_found(),
                };
                verdicts.insert(unit.id, verdict);
            }
        }
    }
    let cv_name = if new.cv_name.is_empty() {
        profile.name.as_str()
    } else {
        new.cv_name
    };
    Ok(score_and_save(matching, NewMatch { cv_name, ..new }, groups, &verdicts).await)
}

fn evidence_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn grounded_verdict(unit: &Undecided, answer: &match_prompts::Judged, cv: &str) -> Verdict {
    let mut verdict = Verdict {
        verdict: answer.verdict.clone(),
        by: "llm".into(),
        method: "judgement".into(),
        evidence: answer.evidence.clone(),
        reason: answer.reason.clone(),
    };
    // Location and language use the structured candidate details, rather than CV excerpts.
    if matches!(answer.verdict.as_str(), "met" | "partial") && !matches!(unit.kind.as_str(), LOCATION | LANGUAGE) {
        let quote = evidence_text(answer.evidence.trim().trim_matches(['"', '“', '”', '«', '»']));
        if quote.is_empty() || !cv.contains(&quote) {
            verdict.verdict = "missing".into();
            verdict.evidence.clear();
            verdict.reason = "The model did not provide a verifiable excerpt from the CV.".into();
        }
    }
    verdict
}

/// The verdicts scored, and saved as the user's match.
async fn score_and_save(matching: &Matching<'_>, new: NewMatch<'_>, groups: Groups, verdicts: &Verdicts) -> AiMatch {
    let verdicts = matcher::combine(&groups, verdicts);
    let score = matcher::score(&groups, &verdicts);
    let laya_summary = matcher::laya_summary(new.job, &groups, &verdicts);
    // Laya's own reading of the verdicts, with its model; the score comes back without it if
    // Laya fails.
    let laya_reading = laya_client::reading(matching.laya, &laya_summary).await;
    let mut answer = AiMatch {
        laya_summary,
        soft_skills: matcher::soft_skills(new.job),
        apply_instructions: apply_instructions::read(new.job_text),
        laya_reading,
        saved_id: None,
        save_error: None,
        groups,
        verdicts,
        result: score,
    };
    if new.job_text.trim().is_empty() || new.cv_text.trim().is_empty() {
        return answer;
    }
    let saved = match_model::save(
        matching.db,
        new,
        &answer.groups,
        &answer.verdicts,
        &answer.result,
        matching.now,
    )
    .await;
    match saved {
        Ok(id) => answer.saved_id = Some(id),
        Err(e) => {
            crate::log_line!("match (saving a match): {e}");
            answer.save_error = Some(format!("The match wasn't saved: {e}"));
        }
    }
    answer
}

fn not_found() -> Verdict {
    Verdict {
        verdict: "missing".to_string(),
        by: "taxonomy".to_string(),
        method: "not_found".to_string(),
        evidence: String::new(),
        reason: NOT_FOUND_REASON.to_string(),
    }
}

/// A language requirement nobody weighed (no AI key on for evaluating job posts):
/// partial, so it's shown without capping the score, with the reason saying why.
fn unweighed(reason: &str) -> Verdict {
    Verdict {
        verdict: "partial".to_string(),
        reason: reason.to_string(),
        ..not_found()
    }
}

/// What Laya's English checkpoint reads, shown under the AI match: both texts and the verdicts, in
/// the saved order, pretty-printed with four spaces. The post is in its own language when it
/// wasn't translated (a model's read no longer is).
pub fn laya_input(job: &Job, job_text: &str, profile: &Profile, cv_text: &str, summary: &str) -> String {
    let cv = if profile.text_en.is_empty() {
        cv_text
    } else {
        &profile.text_en
    };
    let quoted = |text: &str| serde_json::Value::from(text).to_string();
    format!(
        "{{\n    \"job\": {},\n    \"cv\": {},\n    \"summary\": {}\n}}",
        quoted(if job.text_en.is_empty() { job_text } else { &job.text_en }),
        quoted(cv),
        quoted(summary)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_model_verdict_needs_an_excerpt_from_the_cv() {
        let unit = Undecided {
            id: "a0".into(),
            text: "Lead a municipal home-care team".into(),
            kind: "responsibility".into(),
            min_years: None,
        };
        let answer = match_prompts::Judged {
            id: unit.id.clone(),
            verdict: "met".into(),
            evidence: "Led development in the municipal home-care team".into(),
            reason: "Shown in the CV.".into(),
        };
        let cv = evidence_text("Software developer\nLed a team of\n four software engineers.");
        let verdict = grounded_verdict(&unit, &answer, &cv);
        assert_eq!(verdict.verdict, "missing");
        assert!(verdict.evidence.is_empty());

        let transferable = match_prompts::Judged {
            verdict: "partial".into(),
            evidence: "“Led a team of four software engineers”".into(),
            ..answer
        };
        assert_eq!(grounded_verdict(&unit, &transferable, &cv).verdict, "partial");
    }
}
