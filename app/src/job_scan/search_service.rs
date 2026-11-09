//! The job searches' work, for whichever front end asks: the list, a search's page with the posts
//! it found, the search form, deleting, and dismissing a post. The scan itself is scan_service.

use super::found_post_card::FoundPostCard;
use super::job_scan_model::{self as model, JobSearch, SearchFields};
use super::job_search_card::JobSearchCard;
use super::job_search_rules::{self, MAX_FIELD, MAX_KEYWORDS, MAX_NAME, MAX_SITES_TEXT, MAX_TAILOR_FROM};
use super::job_search_summary::JobSearchSummary;
use super::run_steps;
use super::scan_service;
use super::scan_timeline::ScanTimeline;
use crate::context::Context;
use crate::job_sites::JobSite;
use crate::matches::match_model;
use crate::profile;
use crate::service_error::{ErrorKind, ServiceError};

/// Posts a search's page lists at a time.
const PER_PAGE: usize = 25;

/// The days a search's page counts its scans' token use over.
const USAGE_DAYS: i64 = 30;
/// Seconds in a day.
const DAY_SECONDS: i64 = 86_400;

/// The score a new search tailors from: a medium match.
const DEFAULT_TAILOR_FROM: i64 = 60;

/// The list of searches.
pub struct SearchList {
    pub searches: Vec<JobSearchCard>,
    /// Why nothing can be scanned now, when nothing can.
    pub problem: Option<String>,
    /// Why the scan leaves a signed-in site (LinkedIn, Indeed) alone, for each it does.
    pub paused_sites: Vec<String>,
}

pub async fn searches(context: &Context) -> Result<SearchList, ServiceError> {
    let searches = model::searches(context.db())
        .await?
        .iter()
        .map(JobSearchCard::from)
        .collect();
    Ok(SearchList {
        searches,
        problem: scan_service::scan_problem(context).await?,
        paused_sites: paused_sites(context),
    })
}

/// Why the scan leaves each signed-in site it does alone.
fn paused_sites(context: &Context) -> Vec<String> {
    let Some(browser) = &context.signed_in else {
        return Vec::new();
    };
    JobSite::SIGNED_IN
        .iter()
        .filter_map(|site| browser.pause_of(site.domain()))
        .collect()
}

/// Opens JobFinder's signed-in browser as a window on LinkedIn's and Indeed's sign-in pages; the
/// scan reads them again from now on. Ok is what to say.
pub fn open_sign_in(context: &Context) -> Result<&'static str, ServiceError> {
    let browser = context
        .signed_in
        .as_ref()
        .ok_or_else(|| ServiceError::new(ErrorKind::NotFound, "JobFinder has no browser of its own here."))?;
    browser
        .open_sign_in(&crate::job_sites::SIGN_IN_PAGES)
        .map_err(|why| ServiceError::new(ErrorKind::Failed, why))?;
    Ok(
        "JobFinder's browser is open. Sign in to LinkedIn (and Indeed, if you use it), then close the window: every search reads them from now on, unless it skips them.",
    )
}

/// A search the extension can send a post to.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct SearchName {
    pub id: i64,
    pub name: String,
    pub active: bool,
}

/// The searches the extension can send a post to: the active ones first, then newest first.
pub async fn search_names(context: &Context) -> Result<Vec<SearchName>, ServiceError> {
    let mut names: Vec<SearchName> = model::searches(context.db())
        .await?
        .into_iter()
        .map(|search| SearchName {
            id: search.id,
            name: search.name,
            active: search.active,
        })
        .collect();
    // Stable: newest first within active and inactive.
    names.sort_by_key(|search| !search.active);
    Ok(names)
}

/// The search a post sent without one goes to: the newest active one, else the newest.
pub async fn first_active(context: &Context) -> Result<Option<i64>, ServiceError> {
    Ok(search_names(context).await?.first().map(|search| search.id))
}

/// One search by its id; ids below 1 are none.
pub async fn search(context: &Context, id: i64) -> Result<JobSearch, ServiceError> {
    let not_found = || ServiceError::new(ErrorKind::NotFound, "That job search was not found.");
    if id <= 0 {
        return Err(not_found());
    }
    model::find(context.db(), id).await?.ok_or_else(not_found)
}

/// A search's page: the search, how its last scan went, and a batch of its posts.
pub struct SearchPage {
    pub search: JobSearch,
    pub summary: JobSearchSummary,
    pub problem: Option<String>,
    /// A scan is running, of any search: one that hasn't reached this search yet will.
    pub scan_running: bool,
    pub posts: Vec<FoundPostCard>,
    /// The next batch's cursor (search_page's after), when there is one.
    pub next_after: Option<String>,
    /// What the latest scan did, step by step.
    pub timeline: ScanTimeline,
}

/// A search's page, with the dismissed posts too when dismissed, after the cursor after ("" for
/// the first batch).
pub async fn search_page(context: &Context, id: i64, dismissed: bool, after: &str) -> Result<SearchPage, ServiceError> {
    let db = context.db();
    let search = search(context, id).await?;
    let now = chrono::Utc::now().timestamp();
    let last_run = model::last_run(db, Some(search.id)).await?;
    let tokens = model::token_use(db, search.id, now - USAGE_DAYS * DAY_SECONDS).await?;
    let scan_running = scan_service::is_running(context)?;
    let summary = JobSearchSummary::from(&search, last_run.as_ref(), tokens, scan_running);
    let steps = match &last_run {
        Some(run) => run_steps::of_run(db, run.id).await?,
        None => Vec::new(),
    };
    let batch = model::posts(db, search.id, dismissed, PER_PAGE, after).await?;
    let problem = scan_service::scan_problem(context).await?;
    Ok(SearchPage {
        search,
        summary,
        problem,
        scan_running,
        posts: batch.rows.iter().map(FoundPostCard::from).collect(),
        next_after: batch.next,
        timeline: ScanTimeline::from(&steps),
    })
}

/// What the model was asked and answered in a step of a scan's timeline.
pub struct StepExchange {
    pub asked: String,
    pub answered: String,
}

pub async fn step_exchange(context: &Context, step_id: i64) -> Result<StepExchange, ServiceError> {
    let Some((asked, answered)) = run_steps::exchange(context.db(), step_id).await? else {
        return Err(ServiceError::new(
            ErrorKind::NotFound,
            "That step is gone: only a search's latest scans keep theirs.",
        ));
    };
    Ok(StepExchange { asked, answered })
}

/// The search form as posted, or as a search fills it.
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct SearchForm {
    pub name: String,
    pub keywords: String,
    pub location: String,
    /// "1" when ticked.
    pub remote_ok: String,
    /// The sites to skip, one per line.
    pub skipped_sites: String,
    pub tailor_from_score: String,
    /// "1" when ticked.
    pub active: String,
}

impl SearchForm {
    /// A new search's form.
    pub fn new_search() -> Self {
        SearchForm {
            remote_ok: "1".to_string(),
            tailor_from_score: DEFAULT_TAILOR_FROM.to_string(),
            active: "1".to_string(),
            ..SearchForm::default()
        }
    }

    /// A saved search's form.
    pub fn of(search: &JobSearch) -> Self {
        let ticked = |on: bool| if on { "1" } else { "" }.to_string();
        SearchForm {
            name: search.name.clone(),
            keywords: search.keywords.clone(),
            location: search.location.clone(),
            remote_ok: ticked(search.remote_ok),
            skipped_sites: search.skipped_sites.join("\n"),
            tailor_from_score: search.tailor_from_score.to_string(),
            active: ticked(search.active),
        }
    }
}

/// What the form says is wrong, or the fields to save.
pub fn checked(form: &SearchForm) -> Result<SearchFields, Vec<String>> {
    let mut errors = Vec::new();
    let length = |text: &str| text.trim().chars().count();
    if length(&form.name) == 0 {
        errors.push("Give the search a name.".to_string());
    } else if length(&form.name) > MAX_NAME {
        errors.push(format!("Keep the name to {MAX_NAME} characters."));
    }
    if length(&form.keywords) == 0 {
        errors.push("Say what to look for.".to_string());
    } else if length(&form.keywords) > MAX_KEYWORDS {
        errors.push(format!("Keep what to look for to {MAX_KEYWORDS} characters."));
    }
    if length(&form.location) > MAX_FIELD {
        errors.push(format!("Keep where to {MAX_FIELD} characters."));
    }
    let tailor_from = form.tailor_from_score.trim().parse::<i64>().ok();
    if !tailor_from.is_some_and(|score| (0..=MAX_TAILOR_FROM).contains(&score)) {
        errors.push(format!("The score to tailor from is 0 to {MAX_TAILOR_FROM}."));
    }
    if form.skipped_sites.chars().count() > MAX_SITES_TEXT {
        errors.push("That's too much text for the sites to skip.".to_string());
    } else if let Some(problem) = job_search_rules::sites_problem(&form.skipped_sites) {
        errors.push(problem);
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(SearchFields {
        name: form.name.trim().to_string(),
        keywords: form.keywords.trim().to_string(),
        location: form.location.trim().to_string(),
        remote_ok: form.remote_ok == "1",
        tailor_from_score: tailor_from.unwrap_or(DEFAULT_TAILOR_FROM),
        active: form.active == "1",
        skipped_sites: job_search_rules::sites(&form.skipped_sites).0,
    })
}

/// Why a form couldn't be saved: what is wrong with it, or the app's own failure.
pub enum SaveError {
    Invalid(Vec<String>),
    Failed(ServiceError),
}

impl From<ServiceError> for SaveError {
    fn from(e: ServiceError) -> Self {
        SaveError::Failed(e)
    }
}

/// Saves the form as search id (a new one when 0); Ok is the search's id and what to say.
pub async fn save(context: &Context, id: i64, form: &SearchForm) -> Result<(i64, &'static str), SaveError> {
    if id != 0 {
        search(context, id).await?;
    }
    let fields = checked(form).map_err(SaveError::Invalid)?;
    let now = chrono::Utc::now().timestamp();
    let id = model::save(context.db(), id, &fields, now)
        .await
        .map_err(ServiceError::from)?;
    let said = if fields.active {
        "Your job search is saved. It is scanned on the job scan's schedule, or now with Scan now."
    } else {
        "Your job search is saved. It is off, so it isn't scanned."
    };
    Ok((id, said))
}

/// Deletes a search and the posts it found; their saved matches stay.
pub async fn delete(context: &Context, id: i64) -> Result<&'static str, ServiceError> {
    if !model::delete_search(context.db(), id).await? {
        return Err(ServiceError::new(ErrorKind::NotFound, "That job search was not found."));
    }
    Ok("The job search is deleted. Its saved matches are still in your match lists.")
}

/// Dismisses a post (or, with undo, brings it back). Its saved match is rejected (with no
/// reason: the card doesn't ask) or brought back with it, so the job leaves the match lists
/// too. Ok is the post's search.
pub async fn dismiss(context: &Context, post_id: i64, undo: bool) -> Result<i64, ServiceError> {
    let db = context.db();
    let Some((search_id, match_id)) = model::dismiss(db, post_id, undo).await? else {
        return Err(ServiceError::new(ErrorKind::NotFound, "That post was not found."));
    };
    if let Some(match_id) = match_id {
        if undo {
            match_model::bring_back(db, match_id).await?;
        } else {
            let now = chrono::Utc::now().timestamp();
            match_model::reject(db, match_id, &[], now).await?;
        }
        // Another search's post of the same job shares the match, and says the same.
        model::follow_match(db, match_id, undo).await?;
    }
    Ok(search_id)
}

/// Whether there is a profile to match with, for the form.
pub async fn has_profile(context: &Context) -> Result<bool, ServiceError> {
    Ok(profile::text_for_user(context.db()).await?.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_form_says_whats_wrong_with_it() {
        let errors = checked(&SearchForm::default()).unwrap_err();
        assert_eq!(
            errors,
            vec![
                "Give the search a name.",
                "Say what to look for.",
                "The score to tailor from is 0 to 101."
            ]
        );
        let form = SearchForm {
            name: " Backend ".to_string(),
            keywords: "PHP".to_string(),
            skipped_sites: "https://www.jobnet.dk/x\njobindex.dk".to_string(),
            ..SearchForm::new_search()
        };
        let fields = checked(&form).unwrap();
        assert_eq!(fields.name, "Backend");
        assert_eq!(fields.skipped_sites, vec!["jobnet.dk", "jobindex.dk"]);
        assert!(fields.remote_ok && fields.active);
        assert_eq!(fields.tailor_from_score, 60);
        let bad_site = SearchForm {
            skipped_sites: "nope".to_string(),
            ..form
        };
        assert!(checked(&bad_site).unwrap_err()[0].starts_with("These aren't sites"));
    }
}
