//! The setup wizard's steps as data and its forms' work, outside the web pages, so another front end (a native UI
//! later) can show the same steps from the same functions: where the database is, which model
//! the AI runs on, the profile read from the CV, and the job scan's schedule.
//!
//! Completion and deliberate skips are saved per database (progress). Importing a CV does
//! not finish a new setup; migration 0018 preserves existing populated databases.

use crate::account::account_model::{self, RESUME};
use crate::account::model_advice::ModelAdvice;
use crate::context::Context;
use crate::job_scan::{job_scan_model, scan_service, search_service};
use crate::matches::free_reader::YearMonth;
use crate::matches::llm_key;
pub use crate::profile::YearsOfWork;
use crate::profile::profile_model::{self, FilledFrom};
use crate::profile::{self, MAX_YEARS_OF_WORK, profile_fill};
use crate::scan_schedule::INTERVALS;
use crate::service_error::{ErrorKind, ServiceError};

pub struct SearchStep {
    pub id: i64,
    pub form: search_service::SearchForm,
    pub scan_summary: Option<String>,
    pub scan_problem: Option<String>,
    pub progress: super::progress::Progress,
    pub profile_text: Option<String>,
    pub source_status: Vec<String>,
}

pub async fn search_step(context: &Context) -> Result<SearchStep, ServiceError> {
    let searches = job_scan_model::searches(context.db()).await?;
    let selected = searches.iter().find(|s| s.active).or_else(|| searches.first());
    let search = match selected {
        Some(s) => job_scan_model::find(context.db(), s.id).await?,
        None => None,
    };
    let search = search.as_ref();
    let last = job_scan_model::last_run(context.db(), search.map(|s| s.id)).await?;
    let scan_summary = last.map(|run| {
        format!(
            "Last scan: {} posts found, {} new. {}",
            run.posts_found,
            run.posts_new,
            if run.error.is_empty() {
                "Open Job searches to review matches and any post failures."
            } else {
                &run.error
            }
        )
    });
    let mut form = search
        .map(search_service::SearchForm::of)
        .unwrap_or_else(search_service::SearchForm::new_search);
    if search.is_none() {
        form.skipped_sites = "linkedin.com\nindeed.com".to_string();
        form.tailor_from_score = "101".to_string();
    }
    Ok(SearchStep {
        id: search.map_or(0, |s| s.id),
        scan_summary,
        scan_problem: scan_service::scan_problem(context).await?,
        progress: super::progress::read(context.db()).await?,
        profile_text: profile::text_for_user(context.db()).await?,
        source_status: crate::job_sites::JobSite::SIGNED_IN
            .iter()
            .map(|site| {
                if form.skipped_sites.lines().any(|domain| domain == site.domain()) {
                    format!("{}: skipped in this search.", site.domain())
                } else if let Some(browser) = &context.signed_in {
                    browser.pause_of(site.domain()).unwrap_or_else(|| {
                        format!(
                            "{}: browser available; sign-in is verified when the scan reads it.",
                            site.domain()
                        )
                    })
                } else {
                    format!("{}: sign-in browser unavailable here.", site.domain())
                }
            })
            .collect(),
        form,
    })
}

pub async fn choose_manual(context: &Context) -> Result<String, ServiceError> {
    // Remove an existing schedule too: the selected choice must match actual behavior.
    context
        .scan_schedule
        .remove()
        .await
        .map_err(|why| ServiceError::failed("setup/manual", why))?;
    let mut progress = super::progress::read(context.db()).await?;
    progress.manual_scan = true;
    super::progress::save(context.db(), &progress).await?;
    Ok("Manual scanning selected. Use Scan now whenever you want to check for jobs.".into())
}

pub async fn skip_scan(context: &Context) -> Result<String, ServiceError> {
    let mut progress = super::progress::read(context.db()).await?;
    progress.skipped_scan = true;
    super::progress::save(context.db(), &progress).await?;
    Ok("First scan skipped for now. Your search is saved; you can run it from Job searches.".into())
}

pub async fn visit(context: &Context, step: usize) -> Result<(), ServiceError> {
    if step >= 6 {
        return Err(ServiceError::new(ErrorKind::Invalid, "Unknown setup step."));
    }
    let mut progress = super::progress::read(context.db()).await?;
    progress.current_step = step;
    super::progress::save(context.db(), &progress).await?;
    Ok(())
}

pub async fn finish(context: &Context) -> Result<(), ServiceError> {
    let step = search_step(context).await?;
    if let Some(problem) = step.scan_problem {
        return Err(ServiceError::new(ErrorKind::Invalid, problem));
    }
    if step.id == 0 || step.form.active != "1" {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Save an active job search before finishing setup.",
        ));
    }
    if step.scan_summary.is_none() && !step.progress.skipped_scan {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Try the first scan, or choose Skip for now.",
        ));
    }
    if context.scan_schedule.interval_hours().await.unwrap_or(None).is_none() && !step.progress.manual_scan {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Choose a scan schedule or select manual scanning.",
        ));
    }
    let mut progress = step.progress;
    progress.completed = true;
    super::progress::save(context.db(), &progress).await?;
    Ok(())
}

/// The database step: the file the app keeps everything in.
#[derive(Debug, Clone, PartialEq)]
pub struct DatabaseStep {
    pub path: String,
    /// Made by this start of the app, rather than found.
    pub made_now: bool,
    pub system: String,
    pub storage: String,
}

/// The AI step: the saved keys, and the suggestion while there are none.
pub struct AiStep {
    /// Each saved key as "This app's own model (llama.cpp), Qwen3-8B-Q4_K_M.gguf".
    pub saved: Vec<String>,
    pub advice: ModelAdvice,
}

/// The CV step: what the profile was filled from, and whether a CV can be read now.
pub struct CvStep {
    pub filled: Option<FilledFrom>,
    /// The years of professional work saved and counted, None without a profile.
    pub years: Option<YearsOfWork>,
    /// A key is picked for tailoring résumés, which reading the CV runs on.
    pub can_read: bool,
}

/// The schedule step: how often the job scan runs by itself, if it does.
pub struct ScheduleStep {
    /// The task's interval in hours, None when there is no task.
    pub interval_hours: Option<u32>,
    /// Why Task Scheduler couldn't be read, when it couldn't.
    pub error: Option<String>,
    /// The command the task runs.
    pub task_run: String,
}

/// Whether this database's first-run setup has not been explicitly completed.
pub async fn is_first_run(context: &Context) -> sqlx::Result<bool> {
    Ok(!super::progress::read(context.db()).await?.completed)
}

/// is_first_run, or false (logged) when the database can't say: the pages that only link back
/// to the wizard then go on without the link.
pub async fn first_run_or_logged(context: &Context) -> bool {
    is_first_run(context).await.unwrap_or_else(|e| {
        crate::log_line!("setup: whether this is the first run couldn't be read: {e}");
        false
    })
}

/// "Mette Sørensen, read from cv.pdf on 31 October 2026"; without the parts it doesn't have.
pub fn filled_line(filled: &FilledFrom) -> String {
    let mut line = filled.name.clone();
    if !filled.cv_name.is_empty() {
        line.push_str(&format!(", read from {}", filled.cv_name));
    }
    let day = filled
        .filled_at
        .and_then(|at| chrono::DateTime::from_timestamp(at, 0))
        .map(|at| at.format("%-d %B %Y").to_string());
    if let Some(day) = day {
        line.push_str(&format!(" on {day}"));
    }
    line
}

/// "Every 4 hours" for 4; "Every N hours" for one that isn't offered (set by hand).
pub fn interval_label(hours: u32) -> &'static str {
    INTERVALS
        .iter()
        .find(|(offered, _)| *offered == hours)
        .map(|(_, label)| *label)
        .unwrap_or("Every few hours")
}

pub fn database(context: &Context) -> DatabaseStep {
    let folder = std::path::Path::new(&context.database.path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let storage = fs2::available_space(folder)
        .map(|bytes| {
            format!(
                "{} free where your data is kept. Model downloads show their additional space requirements in Models.",
                crate::llama::hardware::gigabytes(bytes)
            )
        })
        .unwrap_or_else(|_| {
            "Available storage could not be checked. Choose a writable folder before downloading models.".into()
        });
    let system = format!(
        "{} / {}. {}",
        std::env::consts::OS,
        std::env::consts::ARCH,
        context
            .llama
            .as_ref()
            .map(|s| s.inventory().to_string())
            .unwrap_or_else(|| "Local hardware setup is unavailable.".into())
    );
    DatabaseStep {
        path: context.database.path.clone(),
        made_now: context.database.made_now,
        system,
        storage,
    }
}

pub async fn ai(context: &Context) -> sqlx::Result<AiStep> {
    let saved = account_model::saved(context.db()).await?;
    Ok(AiStep {
        saved: saved.iter().map(saved_key_line).collect(),
        advice: ModelAdvice::from(context.llama.as_ref()),
    })
}

/// "OpenAI, gpt-5.4-mini", or the provider alone without a model.
fn saved_key_line(saved: &account_model::Saved) -> String {
    let provider = account_model::provider_label(&saved.provider);
    if saved.model.is_empty() {
        provider.to_string()
    } else {
        format!("{provider}, {}", saved.model)
    }
}

pub async fn cv(context: &Context) -> Result<CvStep, String> {
    let filled = profile_model::filled_from(context.db())
        .await
        .map_err(|e| format!("The database couldn't answer: {e}"))?;
    let can_read = llm_key::llm_settings(context, RESUME)
        .await
        .map_err(|e| e.message)?
        .is_some();
    let years = profile::years_of_work(context.db(), YearMonth::of(chrono::Utc::now().timestamp()))
        .await
        .map_err(|e| format!("The database couldn't answer: {e}"))?;
    Ok(CvStep {
        filled,
        years,
        can_read,
    })
}

/// The years box's text: the saved years, else the counted suggestion, else empty.
pub fn years_box(years: &YearsOfWork) -> String {
    years.years().map(|years| years.to_string()).unwrap_or_default()
}

/// The line under the years box: whether the number is saved, and what the jobs' dates count.
pub fn years_line(years: &YearsOfWork) -> String {
    let counted = match years.counted {
        Some(counted) => {
            format!("Your jobs' dates count {counted} years; education and your own projects don't count.")
        }
        None => "Your profile has no dated jobs to count the years from.".to_string(),
    };
    match years.saved {
        Some(_) => format!("Saved. {counted}"),
        None if years.counted.is_some() => {
            format!("A suggestion, not saved yet. {counted} Letters and résumés use this number.")
        }
        None => counted,
    }
}

/// Saves the years of professional work typed in the years box; empty clears them, so the
/// count from the jobs is used again. Ok is what to say.
pub async fn save_years_of_work(context: &Context, typed: &str) -> Result<String, ServiceError> {
    let typed = typed.trim();
    let years = if typed.is_empty() {
        None
    } else {
        Some(parse_years(typed)?)
    };
    profile_model::save_years_of_work(context.db(), years).await?;
    Ok(match years {
        Some(years) => format!("Saved: {years} years of professional work."),
        None => "Cleared: the years are counted from your jobs again.".to_string(),
    })
}

/// A typed number of years, 0 to MAX_YEARS_OF_WORK.
fn parse_years(typed: &str) -> Result<i64, ServiceError> {
    typed
        .parse::<i64>()
        .ok()
        .filter(|years| (0..=MAX_YEARS_OF_WORK).contains(years))
        .ok_or_else(|| {
            ServiceError::new(
                ErrorKind::Invalid,
                format!("Type the years as a whole number from 0 to {MAX_YEARS_OF_WORK}."),
            )
        })
}

pub async fn schedule(context: &Context) -> ScheduleStep {
    let (interval_hours, error) = match context.scan_schedule.interval_hours().await {
        Ok(interval) => (interval, None),
        Err(e) => (None, Some(e)),
    };
    ScheduleStep {
        interval_hours,
        error,
        task_run: context.scan_schedule.task_run(),
    }
}

/// Reads the CV into the profile, replacing it; replace must be true when there is a profile
/// already. Ok is what to say.
pub async fn read_cv(context: &Context, cv_text: &str, cv_name: &str, replace: bool) -> Result<String, ServiceError> {
    let has_profile = cv(context)
        .await
        .map_err(|why| ServiceError::internal("setup/read_cv", why))?
        .filled
        .is_some();
    if has_profile && !replace {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            "Tick \"Replace my profile\" to fill it in again from this CV.",
        ));
    }
    profile_fill::fill_from_cv(context, cv_text, cv_name).await
}

/// Registers the job scan's task to run every hours. Ok is what to say.
pub async fn schedule_scan(context: &Context, hours: u32) -> Result<String, ServiceError> {
    context.scan_schedule.register(hours).await.map_err(|why| {
        ServiceError::failed(
            "setup/schedule_scan",
            format!("The job scan couldn't be scheduled: {why}"),
        )
    })?;
    Ok(format!(
        "The job scan runs {} while you're logged in to this computer.",
        interval_label(hours).to_lowercase()
    ))
}

/// Removes the job scan's task. Ok is what to say.
pub async fn unschedule_scan(context: &Context) -> Result<&'static str, ServiceError> {
    context.scan_schedule.remove().await.map_err(|why| {
        ServiceError::failed(
            "setup/unschedule_scan",
            format!("The job scan's task couldn't be removed: {why}"),
        )
    })?;
    Ok("The job scan no longer runs by itself.")
}
