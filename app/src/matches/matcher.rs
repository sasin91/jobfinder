//! Matching a job post against a CV, and the legacy ranking of the result.
//!
//! A job post is the extract_job answer and a profile the extract_cv answer (see `Job` and
//! `Profile`). Verdicts are keyed by item id: { verdict: met|partial|missing, by, method,
//! evidence, reason }.

use std::sync::LazyLock;

use indexmap::IndexMap;
use regex::Regex;
use serde::{Deserialize, Serialize};

use super::{Job, KnownTerm, Profile, seniority};
use crate::taxonomy::{self, Method, Taxonomy, Term};

/// The legacy ranking engine's weights: formal requirements weigh 150, relevant skills 50, and
/// every other criterion (job title, responsibility area) 30.
pub mod weights {
    pub const REQUIREMENTS: i64 = 150;
    pub const SKILLS: i64 = 50;
    pub const TITLE: i64 = 30;
    pub const RESPONSIBILITIES: i64 = 30;
}

/// How much a verdict counts towards a criterion. The legacy ranker only knew in or out; a
/// partial match counts half.
pub mod credit {
    pub const MET: f64 = 1.0;
    pub const PARTIAL: f64 = 0.5;
    pub const MISSING: f64 = 0.0;
}

/// The most a match may score when it has a likely disqualifier: the post asks the candidate to
/// live somewhere they don't (a missing required location), to speak a language they don't (a
/// missing required language, or the language the job is done in), or is for another career
/// level (a student job for someone with ten years of work). Just under "medium", so it ranks
/// below real fits and isn't tailored right away.
pub const DISQUALIFIED_INDEX: f64 = 0.59;
/// A required workplace that still needs checking ranks below a good match.
pub const UNCONFIRMED_LOCATION_INDEX: f64 = 0.79;

/// The legacy ranker's tags, by the lowest match index that earns them.
pub mod tags {
    pub const TOP_FROM: f64 = 0.85;
    pub const GOOD_FROM: f64 = 0.8;
    pub const MEDIUM_FROM: f64 = 0.6;
}

/// Taxonomy match methods precise enough to decide a requirement on their own. An
/// embedding-only hit is a guess, so those go to the model.
const DECISIVE: [Method; 3] = [Method::Exact, Method::Synonym, Method::Fuzzy];

/// How many ranked terms map_term() looks through.
const MAP_TERM_CANDIDATES: usize = 3;

/// Most edits two spellings of a requirement may differ by ("Kubernets").
const SPELLING_EDITS: usize = 2;

/// The criteria, in the order the score adds them up.
pub const REQUIREMENTS: &str = "requirements";
pub const SKILLS: &str = "skills";
pub const TITLE: &str = "title";
pub const RESPONSIBILITIES: &str = "responsibilities";
pub const LOCATION: &str = "location";
pub const LANGUAGE: &str = "language";
pub const LEVEL: &str = "level";

/// The criteria that aren't weighed, but whose miss caps the score at DISQUALIFIED_INDEX.
pub const DISQUALIFIERS: [&str; 3] = [LOCATION, LANGUAGE, LEVEL];

/// The id of the item for the language the job is done in (Job::working_language).
const WORKING_LANGUAGE_ID: &str = "w0";
const UNCONFIRMED_WORKPLACE_ID: &str = "p0";

// Recognize standalone job-board labels without treating a mention in prose as a work mode.
static OFFICE_LABEL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?im)(?:^|\|)[ \t]*(?:📍[ \t]*)?(hybrid(?: work|arbejde)?|on[- ]?site|på kontoret)[ \t]*(?:\||\r?$)")
        .expect("office work labels")
});

/// One thing a verdict is given on, or a group of alternatives met by the best of them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub english: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_years: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alternatives: Option<Vec<Item>>,
}

/// The job post's items by criterion: requirements, skills, title, responsibilities and, when
/// the post has any required location, location.
pub type Groups = IndexMap<String, Vec<Item>>;

/// A verdict on one item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Verdict {
    /// met, partial or missing.
    pub verdict: String,
    /// Who decided: rule, taxonomy or llm.
    pub by: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub evidence: String,
    #[serde(default)]
    pub reason: String,
}

pub type Verdicts = IndexMap<String, Verdict>;

/// One criterion's share of the score.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Part {
    pub percentage: i64,
    pub value: i64,
    pub weight: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Score {
    pub rank: i64,
    pub total: i64,
    pub index: f64,
    pub tag: String,
    pub parts: IndexMap<String, Part>,
}

/// What a CV phrase names in the taxonomy.
struct CvTerm {
    index: usize,
    phrase: String,
}

pub struct Matcher<'a> {
    taxonomy: &'a Taxonomy,
    profile: Profile,
    known: IndexMap<String, KnownTerm>,
    cv_phrases: Vec<String>,
    /// By term id, in the order the CV names them.
    cv_terms: IndexMap<i64, CvTerm>,
}

impl<'a> Matcher<'a> {
    /// `known` holds phrases whose term is already known, phrase => term (with its match method),
    /// so map_term() needn't rank the taxonomy for them: Free_reader found the CV's and job
    /// post's phrases by label.
    pub fn new(taxonomy: &'a Taxonomy, profile: Profile, known: IndexMap<String, KnownTerm>) -> Self {
        let cv_phrases: Vec<String> = [
            &profile.skill,
            &profile.language,
            &profile.certificate,
            &profile.education,
            &profile.title,
        ]
        .into_iter()
        .flatten()
        .cloned()
        .collect();
        let mut matcher = Self {
            taxonomy,
            profile,
            known,
            cv_phrases,
            cv_terms: IndexMap::new(),
        };
        for phrase in matcher.cv_phrases.clone() {
            if let Some(found) = matcher.map_term(&phrase, None) {
                let id = taxonomy.terms[found.index].id;
                matcher.cv_terms.entry(id).or_insert(CvTerm {
                    index: found.index,
                    phrase,
                });
            }
        }
        matcher
    }

    /// The taxonomy term a phrase names, when the lookup is sure: the phrase is a label of the
    /// term, a synonym, or one with typos. None otherwise.
    pub fn map_term(&self, phrase: &str, kind: Option<&str>) -> Option<KnownTerm> {
        if let Some(&known) = self.known.get(phrase)
            && kind.is_none_or(|k| self.taxonomy.terms[known.index].kind == k)
        {
            return Some(known);
        }
        for result in self.taxonomy.rank(phrase, kind, MAP_TERM_CANDIDATES) {
            let method = self.taxonomy.match_method(&result, phrase);
            if DECISIVE.contains(&method) {
                let index = self.taxonomy.terms.iter().position(|t| std::ptr::eq(t, result.term))?;
                return Some(KnownTerm { index, method });
            }
        }
        None
    }

    /// A verdict the taxonomy (or plain arithmetic) can give without the model, or None when it
    /// can't decide.
    pub fn decide(&self, item: &Item) -> Option<Verdict> {
        if item.kind == LOCATION {
            if item.id == UNCONFIRMED_WORKPLACE_ID
                || item.english.as_deref().is_some_and(|text| text.to_lowercase().contains("office location not stated"))
            {
                return Some(location_to_check("Check where the office is and whether the commute works for you."));
            }
            if self.profile.location.trim().is_empty() {
                return Some(location_to_check("The CV is missing your home location."));
            }
        }
        if item.kind == LEVEL {
            let level = item.min_level.as_deref().unwrap_or("");
            return Some(seniority::verdict(level, self.profile.experience_years));
        }
        let min_years = item.min_years.unwrap_or(0);
        if min_years > 0 && is_total_experience(item, min_years) {
            let have = self.profile.experience_years;
            let verdict = if have >= min_years {
                "met"
            } else if have as f64 >= min_years as f64 / 2.0 {
                "partial"
            } else {
                "missing"
            };
            return Some(verdict_of(
                verdict,
                "rule",
                "years",
                format!("{have} years"),
                format!("Needs {min_years}, CV shows {have}."),
            ));
        }
        // A level ("fluent", "master's degree") is more than the taxonomy knows, and where
        // someone lives is the model's to weigh.
        if min_years > 0
            || matches!(item.kind.as_str(), "location" | "responsibility" | "experience")
            || item.min_level.as_deref().is_some_and(|l| !l.is_empty())
        {
            return None;
        }

        // Spelled the same, give or take a typo ("Kubernets").
        let wanted = self.taxonomy.content_words(&item.text).join(" ");
        for phrase in &self.cv_phrases {
            let have = self.taxonomy.content_words(phrase).join(" ");
            if wanted.is_empty() || have.is_empty() {
                continue;
            }
            let distance = taxonomy::edit_distance(&wanted, &have, SPELLING_EDITS);
            if distance <= taxonomy::typo_budget(&wanted) {
                let method = if distance == 0 { "exact" } else { "fuzzy" };
                return Some(verdict_of(
                    "met",
                    "taxonomy",
                    method,
                    phrase.clone(),
                    "Named in the CV.".into(),
                ));
            }
        }

        // The taxonomy's language terms are too loose to go by ("dansk" has "english" as a
        // synonym), and a language decides whether the job can be done at all: one not named in
        // the CV goes to the model.
        if item.kind == LANGUAGE {
            return None;
        }
        let found = self.map_term(&item.text, None)?;
        let term = &self.taxonomy.terms[found.index];
        let label = term.preferred_da();

        if let Some(same) = self.cv_terms.get(&term.id) {
            return Some(verdict_of(
                "met",
                "taxonomy",
                found.method.as_str(),
                same.phrase.clone(),
                format!("Both are \"{label}\"."),
            ));
        }
        // A broader or narrower skill in the same family, or a sibling.
        for cv_term in self.cv_terms.values() {
            let have = &self.taxonomy.terms[cv_term.index];
            if related(term, have) {
                return Some(verdict_of(
                    "partial",
                    "taxonomy",
                    "related",
                    cv_term.phrase.clone(),
                    format!("\"{}\" is related to \"{label}\".", have.preferred_da()),
                ));
            }
        }
        None
    }
}

/// The free reader and older generic requirements use these labels for total years.
fn is_total_experience(item: &Item, years: i64) -> bool {
    item.kind == "experience"
        && [format!("{years} years of experience"), format!("{years} års erfaring")]
            .iter()
            .any(|label| item.text.eq_ignore_ascii_case(label))
}

pub(super) fn location_to_check(reason: &str) -> Verdict {
    verdict_of("partial", "rule", "location_unconfirmed", String::new(), reason.into())
}

/// Whether `have` is the parent, a child or a sibling of `term`.
fn related(term: &Term, have: &Term) -> bool {
    let parent = term.parent_id.filter(|&p| p != 0);
    let have_parent = have.parent_id.filter(|&p| p != 0);
    parent.is_some_and(|p| p == have.id || Some(p) == have.parent_id) || have_parent == Some(term.id)
}

fn verdict_of(verdict: &str, by: &str, method: &str, evidence: String, reason: String) -> Verdict {
    Verdict {
        verdict: verdict.into(),
        by: by.into(),
        method: method.into(),
        evidence,
        reason,
    }
}

/// The verdict a stored credit (CREDIT, as match_score_details.rank) stands for.
pub fn verdict_of_credit(value: f64) -> &'static str {
    if value >= credit::MET {
        "met"
    } else if value > credit::MISSING {
        "partial"
    } else {
        "missing"
    }
}

/// The legacy ranker's tag for a match index.
pub fn match_tag(index: f64) -> &'static str {
    if index >= tags::TOP_FROM {
        "top"
    } else if index >= tags::GOOD_FROM {
        "good"
    } else if index >= tags::MEDIUM_FROM {
        "medium"
    } else {
        "poor"
    }
}

/// What a whole percent is out of.
const WHOLE_PERCENT: i64 = 100;

/// percentage % of weight, rounded half up, in whole numbers so no floating-point error can move
/// a half: 57 % of 150 is 85.5, which is 86.
fn share_of_weight(percentage: i64, weight: i64) -> i64 {
    (2 * percentage * weight + WHOLE_PERCENT) / (2 * WHOLE_PERCENT)
}

fn credit_of(verdict: Option<&Verdict>) -> f64 {
    match verdict.map(|v| v.verdict.as_str()) {
        Some("met") => credit::MET,
        Some("partial") => credit::PARTIAL,
        _ => credit::MISSING,
    }
}

fn weight_of(criterion: &str) -> i64 {
    match criterion {
        REQUIREMENTS => weights::REQUIREMENTS,
        SKILLS => weights::SKILLS,
        TITLE => weights::TITLE,
        _ => weights::RESPONSIBILITIES,
    }
}

/// The job post as the legacy ranker's criteria: one list of items per weight. The post's one
/// requirements list splits into required (the formal requirements) and nice-to-have (relevant
/// skills). Requirements sharing an alt_group are alternatives and make one item, met by its
/// best alternative.
///
/// Soft skills are left out: they are not scored (see soft_skills()). Required location
/// requirements are their own group, location, when the post has any: not weighed, but a missing
/// one caps the score. A nice-to-have location is a relevant skill like any other. Required
/// languages, and the language the job is done in (Job::working_language), are the group
/// language the same way: a job in a language the candidate doesn't speak isn't one they can do,
/// however well the rest fits. The post's career level, when it states one, is the group level:
/// not weighed either, but a mismatch with the candidate's years caps the score too.
pub fn criteria(job: &Job) -> Groups {
    let mut lists: IndexMap<&str, Vec<Item>> = IndexMap::from([
        (REQUIREMENTS, vec![]),
        (SKILLS, vec![]),
        (LOCATION, vec![]),
        (LANGUAGE, vec![]),
    ]);
    // "list:alt_group" => (list, position)
    let mut groups: IndexMap<String, (&str, usize)> = IndexMap::new();
    for (i, r) in job.requirements.iter().enumerate() {
        if r.kind == "soft_skill" {
            continue;
        }
        let level = if r.min_level.is_empty() {
            String::new()
        } else {
            format!(" ({})", r.min_level)
        };
        let english = if r.english.is_empty() { &r.value } else { &r.english };
        let unit = Item {
            id: format!("r{i}"),
            kind: r.kind.clone(),
            text: format!("{}{level}", r.value),
            english: Some(format!("{english}{level}")),
            min_years: Some(r.min_years),
            min_level: Some(r.min_level.clone()),
            alternatives: None,
        };
        let list = if !r.required {
            SKILLS
        } else if r.kind == LOCATION {
            LOCATION
        } else if r.kind == LANGUAGE {
            LANGUAGE
        } else {
            REQUIREMENTS
        };
        if r.alt_group.is_empty() {
            lists[list].push(unit);
            continue;
        }
        let key = format!("{list}:{}", r.alt_group);
        let (list, position) = match groups.get(&key) {
            Some(&found) => found,
            None => {
                let position = lists[list].len();
                lists[list].push(Item {
                    id: format!("g{}", groups.len()),
                    kind: "alternatives".into(),
                    text: String::new(),
                    english: None,
                    min_years: None,
                    min_level: None,
                    alternatives: Some(vec![]),
                });
                groups.insert(key, (list, position));
                (list, position)
            }
        };
        if let Some(alternatives) = lists[list][position].alternatives.as_mut() {
            alternatives.push(unit);
        }
    }
    for &(list, position) in groups.values() {
        let item = &mut lists[list][position];
        let alternatives = item.alternatives.as_deref().unwrap_or_default();
        let text = alternatives
            .iter()
            .map(|a| a.text.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        let english = alternatives
            .iter()
            .map(|a| a.english.as_deref().unwrap_or(""))
            .collect::<Vec<_>>()
            .join(" or ");
        item.text = text;
        item.english = Some(english);
    }

    let responsibilities = job
        .responsibilities
        .iter()
        .enumerate()
        .map(|(i, text)| Item {
            id: format!("a{i}"),
            kind: "responsibility".into(),
            text: text.clone(),
            english: None,
            min_years: None,
            min_level: None,
            alternatives: None,
        })
        .collect();
    let title = if job.job_title.is_empty() {
        vec![]
    } else {
        vec![Item {
            id: "t0".into(),
            kind: "title".into(),
            text: job.job_title.clone(),
            english: Some(job.job_title_en.clone()),
            min_years: None,
            min_level: None,
            alternatives: None,
        }]
    };
    let location = lists.swap_remove(LOCATION).unwrap_or_default();
    let mut languages = lists.swap_remove(LANGUAGE).unwrap_or_default();
    if let Some(working) = working_language_item(job, &languages) {
        languages.push(working);
    }
    let mut out = Groups::new();
    out.insert(REQUIREMENTS.into(), lists.swap_remove(REQUIREMENTS).unwrap_or_default());
    out.insert(SKILLS.into(), lists.swap_remove(SKILLS).unwrap_or_default());
    out.insert(TITLE.into(), title);
    out.insert(RESPONSIBILITIES.into(), responsibilities);
    if !location.is_empty() {
        out.insert(LOCATION.into(), location);
    }
    if !languages.is_empty() {
        out.insert(LANGUAGE.into(), languages);
    }
    if let Some(level) = level_item(job) {
        out.insert(LEVEL.into(), vec![level]);
    }
    out
}

/// Keep a workplace check when the job board labels the role hybrid or on-site and the
/// reader returned no required location.
pub(super) fn criteria_for_post(job: &Job, post: &str) -> Groups {
    let mut groups = criteria(job);
    if !groups.contains_key(LOCATION) && let Some(label) = OFFICE_LABEL.captures(post) {
        groups.insert(
            LOCATION.into(),
            vec![Item {
                id: UNCONFIRMED_WORKPLACE_ID.into(),
                kind: LOCATION.into(),
                text: format!("{} · office location to check", &label[1]),
                english: None,
                min_years: None,
                min_level: None,
                alternatives: None,
            }],
        );
    }
    groups
}

/// The post's career level as an item, when it was read with one (seniority::with_level).
fn level_item(job: &Job) -> Option<Item> {
    let level = job.level.clone();
    let label = seniority::label(&level);
    (!label.is_empty()).then(|| Item {
        id: "l0".into(),
        kind: LEVEL.into(),
        text: label.into(),
        english: Some(label.into()),
        min_years: None,
        min_level: Some(level),
        alternatives: None,
    })
}

/// The language the job is done in as an item of the group language, unless a required language
/// already names it ("Finsk (fluent)" for a Finnish post) or the post wasn't read with one.
fn working_language_item(job: &Job, languages: &[Item]) -> Option<Item> {
    let working = job.working_language.trim();
    if working.is_empty() || languages.iter().any(|item| names_language(item, working)) {
        return None;
    }
    Some(Item {
        id: WORKING_LANGUAGE_ID.into(),
        kind: LANGUAGE.into(),
        text: working.into(),
        english: Some(working.into()),
        min_years: None,
        min_level: None,
        alternatives: None,
    })
}

/// Whether a language item (or one of its alternatives) names the language, in English.
fn names_language(item: &Item, language: &str) -> bool {
    let language = language.to_lowercase();
    let named = |item: &Item| {
        [Some(item.text.as_str()), item.english.as_deref()]
            .into_iter()
            .flatten()
            .any(|text| text.to_lowercase().contains(&language))
    };
    named(item) || item.alternatives.iter().flatten().any(named)
}

/// The post's likely disqualifiers the candidate misses, with the criterion each is under: a
/// location or a language the model judged they miss, and a career level their years don't fit.
/// They cap the score.
pub fn missed_disqualifiers<'g>(groups: &'g Groups, verdicts: &Verdicts) -> Vec<(&'g str, &'g Item)> {
    let mut missed = Vec::new();
    for (criterion, items) in groups {
        if !DISQUALIFIERS.contains(&criterion.as_str()) {
            continue;
        }
        let misses = |item: &&Item| {
            verdicts
                .get(&item.id)
                .is_some_and(|v| v.verdict == "missing" && judged_enough(criterion, v))
        };
        missed.extend(items.iter().filter(misses).map(|item| (criterion.as_str(), item)));
    }
    missed
}

/// Whether a verdict may disqualify under the criterion. Only the model weighs where someone lives
/// and which languages they speak, so a location or language nobody judged (no AI key) never caps
/// the score; a career level is plain arithmetic on the CV's years.
fn judged_enough(criterion: &str, verdict: &Verdict) -> bool {
    criterion == LEVEL || verdict.by == "llm"
}

/// The post's items under the criteria only the model judges (location, language): to Laya they
/// are required qualifications too, once the model has weighed them.
fn judged_disqualifiers<'g>(groups: &'g Groups, verdicts: &'g Verdicts) -> impl Iterator<Item = &'g Item> {
    [LOCATION, LANGUAGE]
        .into_iter()
        .filter_map(|criterion| groups.get(criterion))
        .flatten()
        .filter(|item| verdicts.get(&item.id).is_some_and(|v| v.by == "llm"))
}

/// The post's soft skills, shown as tags but not scored.
pub fn soft_skills(job: &Job) -> Vec<String> {
    job.requirements
        .iter()
        .filter(|r| r.kind == "soft_skill")
        .map(|r| r.value.clone())
        .collect()
}

/// What gets a verdict: every item, or each of its alternatives.
pub fn units(groups: &Groups) -> Vec<&Item> {
    groups
        .values()
        .flatten()
        .flat_map(|item| match &item.alternatives {
            Some(alternatives) => alternatives.iter().collect::<Vec<_>>(),
            None => vec![item],
        })
        .collect()
}

/// Verdicts on units, plus one per alternatives item: its best alternative.
pub fn combine(groups: &Groups, verdicts: &Verdicts) -> Verdicts {
    let rank = |v: &Verdict| match v.verdict.as_str() {
        "met" => 2,
        "partial" => 1,
        _ => 0,
    };
    let mut out = verdicts.clone();
    for item in groups.values().flatten() {
        let Some(alternatives) = &item.alternatives else {
            continue;
        };
        let mut top: Option<&Verdict> = None;
        for a in alternatives {
            if let Some(v) = verdicts.get(&a.id)
                && top.is_none_or(|t| rank(v) > rank(t))
            {
                top = Some(v);
            }
        }
        if let Some(top) = top {
            out.insert(item.id.clone(), top.clone());
        }
    }
    out
}

/// The verdicts on the requirements as one English sentence pair, the state Laya's English
/// checkpoint ranked well in the bake-off (eval/laya/run_laya_summary.py). A partial match counts
/// as missing there, marked "(partly)".
pub fn laya_summary(job: &Job, groups: &Groups, verdicts: &Verdicts) -> String {
    // A required location or language is a required qualification to Laya too, once the model
    // has weighed it: unjudged, it isn't known to be missing. So is the post's career level.
    let judged = judged_disqualifiers(groups, verdicts);
    let level = groups.get(LEVEL).into_iter().flatten();
    let items: Vec<&Item> = groups
        .get(REQUIREMENTS)
        .into_iter()
        .flatten()
        .chain(judged)
        .chain(level)
        .collect();
    let mut met = 0;
    let mut missing = Vec::new();
    for item in &items {
        let verdict = verdicts.get(&item.id).map(|v| v.verdict.as_str());
        let name = item.english.as_deref().filter(|e| !e.is_empty()).unwrap_or(&item.text);
        match verdict {
            Some("met") => met += 1,
            Some("partial") => missing.push(format!("{name} (partly)")),
            _ => missing.push(name.to_string()),
        }
    }
    let title = if job.job_title_en.is_empty() {
        &job.job_title
    } else {
        &job.job_title_en
    };
    let missing = if missing.is_empty() {
        "none".to_string()
    } else {
        missing.join(", ")
    };
    format!(
        "Job: {title}. The candidate meets {met} of {} required qualifications. Missing requirements: {missing}.",
        items.len()
    )
}

/// The legacy ranking over the criteria: each non-empty criterion scores the share of its items
/// met (as a whole percent), times its weight; the match index is the sum over the sum of
/// weights. A missed disqualifier caps the index below medium. An unconfirmed location caps it
/// below good.
pub fn score(groups: &Groups, verdicts: &Verdicts) -> Score {
    /// A share as a whole percent.
    const PERCENT: f64 = 100.0;
    let mut rank = 0;
    let mut total = 0;
    let mut parts = IndexMap::new();
    for (key, items) in groups {
        if items.is_empty() || DISQUALIFIERS.contains(&key.as_str()) {
            continue;
        }
        let weight = weight_of(key);
        let credit: f64 = items.iter().map(|item| credit_of(verdicts.get(&item.id))).sum();
        let percentage = (credit / items.len() as f64 * PERCENT) as i64;
        let value = share_of_weight(percentage, weight);
        rank += value;
        total += weight;
        parts.insert(
            key.clone(),
            Part {
                percentage,
                value,
                weight,
            },
        );
    }
    let mut index = if total > 0 { rank as f64 / total as f64 } else { 0.0 };
    if !missed_disqualifiers(groups, verdicts).is_empty() {
        index = index.min(DISQUALIFIED_INDEX);
    } else if groups.get(LOCATION).into_iter().flatten().any(|item| {
        verdicts.get(&item.id).is_none_or(|verdict| verdict.verdict != "met")
    }) {
        index = index.min(UNCONFIRMED_LOCATION_INDEX);
    }
    Score {
        rank,
        total,
        index,
        tag: match_tag(index).into(),
        parts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_half_of_a_weight_rounds_up() {
        assert_eq!(share_of_weight(57, weights::REQUIREMENTS), 86);
        assert_eq!(share_of_weight(41, weights::REQUIREMENTS), 62);
        assert_eq!(share_of_weight(29, weights::SKILLS), 15);
        assert_eq!(share_of_weight(100, weights::REQUIREMENTS), 150);
        assert_eq!(share_of_weight(0, weights::SKILLS), 0);
    }

    /// A post asking only for PHP, at the given level, with the PHP requirement met.
    fn php_post(level: &str) -> (Groups, Verdicts) {
        let job = Job {
            job_title: "Udvikler".into(),
            requirements: vec![super::super::Requirement {
                value: "PHP".into(),
                kind: "skill".into(),
                required: true,
                ..Default::default()
            }],
            level: level.into(),
            ..Job::default()
        };
        let groups = criteria(&job);
        let mut verdicts = Verdicts::new();
        verdicts.insert(
            "r0".into(),
            verdict_of("met", "taxonomy", "exact", "PHP".into(), String::new()),
        );
        verdicts.insert(
            "t0".into(),
            verdict_of("met", "taxonomy", "exact", "Udvikler".into(), String::new()),
        );
        (groups, verdicts)
    }

    #[test]
    fn a_hybrid_label_without_a_workplace_requires_a_commute_check() {
        let job = Job { job_title: "Senior Backend Engineer".into(), ..Job::default() };
        let post = "Senior Backend Engineer (PHP)\r\n\r\nUp to €6.5K/month | 📍Hybrid | PHP (Symfony)";
        let groups = criteria_for_post(&job, post);
        let workplace = &groups[LOCATION][0];
        let taxonomy = Taxonomy::from_records(vec![]);
        let matcher = Matcher::new(&taxonomy, Profile::default(), IndexMap::new());
        let workplace_verdict = matcher.decide(workplace).unwrap();
        assert_eq!(workplace_verdict.verdict, "partial");
        let verdicts = Verdicts::from([
            (workplace.id.clone(), workplace_verdict),
            ("t0".into(), verdict_of("met", "taxonomy", "exact", String::new(), String::new())),
        ]);
        assert_eq!(score(&groups, &verdicts).index, UNCONFIRMED_LOCATION_INDEX);
        assert!(!criteria_for_post(&job, "Senior Backend Engineer | Fully remote").contains_key(LOCATION));
    }

    #[test]
    fn total_experience_does_not_decide_a_scoped_minimum() {
        let taxonomy = Taxonomy::from_records(vec![]);
        let profile = Profile { experience_years: 9, ..Profile::default() };
        let matcher = Matcher::new(&taxonomy, profile, IndexMap::new());
        let job = Job {
            requirements: [
                "2 years in a senior role making architecture decisions",
                "2 years of experience",
            ].map(|value| super::super::Requirement {
                value: value.into(),
                kind: "experience".into(),
                required: true,
                min_years: 2,
                ..Default::default()
            }).to_vec(),
            ..Job::default()
        };
        let groups = criteria(&job);
        let items = &groups[REQUIREMENTS];
        assert!(matcher.decide(&items[0]).is_none());
        assert_eq!(matcher.decide(&items[1]).unwrap().verdict, "met");
    }

    #[test]
    fn a_student_job_for_a_senior_is_capped_below_medium() {
        let (groups, mut verdicts) = php_post(seniority::STUDENT);
        assert_eq!(groups[LEVEL][0].text, "Student job");
        verdicts.insert("l0".into(), seniority::verdict(seniority::STUDENT, 10));
        let score = score(&groups, &verdicts);
        assert_eq!(score.index, DISQUALIFIED_INDEX);
        assert_eq!(score.tag, "poor");
        assert!(!score.parts.contains_key(LEVEL), "the level isn't weighed");
    }

    #[test]
    fn a_level_that_fits_leaves_the_score_alone() {
        let (groups, mut verdicts) = php_post(seniority::SENIOR);
        verdicts.insert("l0".into(), seniority::verdict(seniority::SENIOR, 10));
        assert_eq!(score(&groups, &verdicts).index, 1.0);
    }

    #[test]
    fn a_post_without_a_level_has_no_level_group() {
        let (groups, _) = php_post("");
        assert!(!groups.contains_key(LEVEL));
    }

    /// A PHP post written in Finnish, with PHP and the title met.
    fn finnish_post(requirements: Vec<super::super::Requirement>) -> (Groups, Verdicts) {
        let php = super::super::Requirement {
            value: "PHP".into(),
            kind: "skill".into(),
            required: true,
            ..Default::default()
        };
        let job = Job {
            job_title: "Lehtori".into(),
            requirements: std::iter::once(php).chain(requirements).collect(),
            working_language: "Finnish".into(),
            ..Job::default()
        };
        let groups = criteria(&job);
        let mut verdicts = Verdicts::new();
        for id in ["r0", "t0"] {
            verdicts.insert(
                id.into(),
                verdict_of("met", "taxonomy", "exact", String::new(), String::new()),
            );
        }
        (groups, verdicts)
    }

    #[test]
    fn a_job_in_a_language_the_candidate_doesnt_speak_is_capped_below_medium() {
        let (groups, mut verdicts) = finnish_post(vec![]);
        assert_eq!(groups[LANGUAGE][0].text, "Finnish");
        verdicts.insert(
            WORKING_LANGUAGE_ID.into(),
            verdict_of(
                "missing",
                "llm",
                "judgement",
                String::new(),
                "The CV names no Finnish.".into(),
            ),
        );
        let score = score(&groups, &verdicts);
        assert_eq!(score.index, DISQUALIFIED_INDEX);
        assert!(!score.parts.contains_key(LANGUAGE), "languages aren't weighed");
        let missed = missed_disqualifiers(&groups, &verdicts);
        assert_eq!(missed.len(), 1);
        assert_eq!(missed[0].0, LANGUAGE);
    }

    #[test]
    fn a_language_the_candidate_speaks_leaves_the_score_alone() {
        let (groups, mut verdicts) = finnish_post(vec![]);
        verdicts.insert(
            WORKING_LANGUAGE_ID.into(),
            verdict_of("met", "taxonomy", "exact", "Finnish".into(), String::new()),
        );
        assert_eq!(score(&groups, &verdicts).index, 1.0);
    }

    #[test]
    fn a_language_nobody_judged_never_caps_the_score() {
        let (groups, mut verdicts) = finnish_post(vec![]);
        verdicts.insert(
            WORKING_LANGUAGE_ID.into(),
            verdict_of("missing", "taxonomy", "not_found", String::new(), String::new()),
        );
        assert_eq!(score(&groups, &verdicts).index, 1.0);
    }

    #[test]
    fn a_required_language_is_a_disqualifier_and_names_the_working_language() {
        let finnish = super::super::Requirement {
            value: "Suomen kieli".into(),
            english: "Finnish".into(),
            kind: LANGUAGE.into(),
            required: true,
            min_level: "fluent".into(),
            ..Default::default()
        };
        let (groups, _) = finnish_post(vec![finnish]);
        assert_eq!(groups[REQUIREMENTS].len(), 1, "only PHP is weighed");
        let languages: Vec<&str> = groups[LANGUAGE].iter().map(|item| item.text.as_str()).collect();
        assert_eq!(languages, ["Suomen kieli (fluent)"], "Finnish is asked for once");
    }

    #[test]
    fn a_nice_to_have_language_stays_a_relevant_skill() {
        let swedish = super::super::Requirement {
            value: "Swedish".into(),
            kind: LANGUAGE.into(),
            required: false,
            ..Default::default()
        };
        let (groups, _) = finnish_post(vec![swedish]);
        assert_eq!(groups[SKILLS][0].text, "Swedish");
        assert_eq!(groups[LANGUAGE].len(), 1, "only the working language");
    }

    #[test]
    fn a_post_read_without_a_language_has_no_language_group() {
        let (groups, _) = php_post("");
        assert!(!groups.contains_key(LANGUAGE));
    }
}
