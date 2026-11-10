//! The Setup screen: the setup wizard (setup) in the window. Where the data is, which model the
//! AI runs on, the profile read from the CV, and how often the job scan runs by itself, all
//! through setup_steps, the functions the web page uses. The window opens on it while the app
//! hasn't been set up (setup_steps::is_first_run).

use std::sync::Arc;

use iced::widget::{Column, button, checkbox, column, container, pick_list, row, text, text_editor, text_input};
use iced::{Center, Element, Fill, Task};

use super::matching::{Picked, pick_file};
use super::style;
use super::widgets::{self, CARD_PADDING};
use super::{GAP, Runner};
use super::{drawings, local_model};
use crate::dashboard::Health;
use crate::job_scan::{
    scan_lock, scan_service,
    search_service::{self, SaveError, SearchForm},
};
use crate::scan_schedule::{DEFAULT_INTERVAL_HOURS, INTERVALS};
use crate::setup::setup_steps::{self, CvStep, DatabaseStep, ScheduleStep};

/// The CV box's height.
const CV_BOX_HEIGHT: f32 = 220.0;
/// The interval list's width.
const PICK_WIDTH: f32 = 220.0;
/// The years box's width: room for a two-digit number.
const YEARS_BOX_WIDTH: f32 = 80.0;

/// The four steps as read.
pub struct Steps {
    database: DatabaseStep,
    cv: CvStep,
    schedule: ScheduleStep,
    search: setup_steps::SearchStep,
}

/// An interval the job scan can run at, for the list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Interval {
    hours: u32,
    label: &'static str,
}

impl std::fmt::Display for Interval {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label)
    }
}

/// The intervals offered (scan_schedule::INTERVALS).
fn intervals() -> Vec<Interval> {
    INTERVALS
        .iter()
        .map(|&(hours, label)| Interval { hours, label })
        .collect()
}

/// The interval the list starts on: the task's, or the default.
fn interval_of(hours: Option<u32>) -> Option<Interval> {
    let hours = hours.unwrap_or(DEFAULT_INTERVAL_HOURS);
    intervals().into_iter().find(|interval| interval.hours == hours)
}

#[derive(Default)]
pub struct State {
    pub(super) models: local_model::State,
    /// None until read; Err says why it couldn't be.
    steps: Option<Result<Arc<Steps>, String>>,
    cv_text: text_editor::Content,
    /// The file the CV was picked from, saved with the profile.
    cv_name: String,
    /// "Replace my profile" is ticked.
    replace: bool,
    /// Why the CV couldn't be read, under its box.
    cv_error: Option<String>,
    /// The years of professional work box: the saved years or the suggestion, as typed.
    years_text: String,
    /// Why the years couldn't be saved, under the box.
    years_error: Option<String>,
    interval: Option<Interval>,
    /// Work is running (reading the CV, the scheduler), so the buttons wait.
    busy: bool,
    said: Option<(Health, String)>,
    search_form: SearchForm,
    search_dirty: bool,
    search_errors: Vec<String>,
    search_profile_open: bool,
    scanning: bool,
    stage: usize,
    stage_loaded: bool,
}

#[derive(Clone)]
pub enum Msg {
    Models(local_model::Msg),
    Load,
    LoadSteps,
    Stage(usize),
    StageRecorded(Result<(), String>),
    SearchName(String),
    SearchKeywords(String),
    SearchLocation(String),
    SearchRemote(bool),
    SearchSite(&'static str, bool),
    SearchThreshold(String),
    ToggleSearchProfile,
    SearchSaved(usize, Result<String, Vec<String>>),
    SignIn,
    FirstScan,
    ScanDone(Result<String, String>),
    StopScan,
    SkipScan,
    Manual,
    Complete,
    Completed(Result<(), String>),
    Later,
    ShowSearches,
    Loaded(Result<Arc<Steps>, String>),
    ChooseCv,
    CvPicked(Result<Picked, String>),
    CvEdit(text_editor::Action),
    Replace(bool),
    ReadCv,
    CvRead(Result<String, String>),
    YearsEdit(String),
    SaveYears,
    /// What saving the years came to: Ok what to say, or Err why not.
    YearsSaved(Result<String, String>),
    Interval(Interval),
    Schedule,
    Unschedule,
    /// What the scheduler said: Ok what to say, or Err why not.
    Scheduled(Result<String, String>),
    /// For the shell: go to Language models.
    ShowLanguageModels,
    /// For the shell: go to Local model.
    ShowLocalModel,
    /// For the shell: go to the Match screen.
    Finish,
    /// What another screen's work came to, said here (a key saved during setup).
    Tell(String),
}

pub fn update(state: &mut State, msg: Msg, runner: &Runner) -> Task<Msg> {
    match msg {
        Msg::Models(local_model::Msg::ShowLanguageModels) => return Task::done(Msg::ShowLanguageModels),
        Msg::Models(local_model::Msg::ShowFullModels) => return Task::done(Msg::ShowLocalModel),
        Msg::Models(msg) => {
            let refresh = matches!(msg, local_model::Msg::ChoicesSaved(Ok(())));
            let task = local_model::update(&mut state.models, msg, runner).map(Msg::Models);
            return if refresh {
                Task::batch([task, Task::done(Msg::LoadSteps)])
            } else {
                task
            };
        }
        Msg::Load => {
            return Task::batch([
                Task::done(Msg::LoadSteps),
                Task::done(Msg::Models(local_model::Msg::Load)),
            ]);
        }
        Msg::LoadSteps => {
            return runner.run(
                |context| async move {
                    let cv = setup_steps::cv(&context).await?;
                    Ok(Arc::new(Steps {
                        database: setup_steps::database(&context),
                        cv,
                        schedule: setup_steps::schedule(&context).await,
                        search: setup_steps::search_step(&context).await.map_err(|e| e.message)?,
                    }))
                },
                |read| Msg::Loaded(read.and_then(|steps| steps)),
            );
        }
        Msg::Loaded(steps) => {
            if let Ok(steps) = &steps {
                if !state.stage_loaded {
                    state.stage = steps.search.progress.current_step.min(5);
                    state.stage_loaded = true;
                }
                state.interval = interval_of(steps.schedule.interval_hours);
                state.replace = false;
                state.years_text = steps.cv.years.as_ref().map(setup_steps::years_box).unwrap_or_default();
                if !state.search_dirty {
                    state.search_form = steps.search.form.clone();
                }
            }
            if let Err(why) = &steps {
                crate::log_line!("setup: the steps couldn't be read: {why}");
            }
            // A read or the scheduler may still be running: its own answer frees the buttons.
            state.steps = Some(steps);
        }
        Msg::ChooseCv => {
            state.busy = true;
            return runner.run(
                |context| async move { pick_file(context).await },
                |picked| Msg::CvPicked(picked.and_then(|picked| picked.map_err(|e| e.message))),
            );
        }
        Msg::SearchName(value) => {
            state.search_form.name = value;
            state.search_dirty = true;
        }
        Msg::Stage(stage) => {
            if state.busy {
                return Task::none();
            }
            if state.stage == 3
                && stage > 3
                && let Some(Ok(steps)) = &state.steps
                && (steps.search.id == 0 || state.search_dirty || state.search_form.active != "1")
            {
                let id = steps.search.id;
                let mut form = state.search_form.clone();
                form.active = "1".into();
                state.busy = true;
                state.search_errors.clear();
                return runner.run(
                    move |context| async move {
                        search_service::save(&context, id, &form)
                            .await
                            .map(|(_, said)| said.to_string())
                            .map_err(|e| match e {
                                SaveError::Invalid(why) => why,
                                SaveError::Failed(e) => vec![e.message],
                            })
                    },
                    move |answer| Msg::SearchSaved(stage, answer.unwrap_or_else(|e| Err(vec![e]))),
                );
            }
            let next_stage = stage.min(5);
            if state.stage != next_stage {
                state.search_profile_open = false;
            }
            state.stage = next_stage;
            return runner.run(
                move |context| async move { setup_steps::visit(&context, stage).await },
                |r| Msg::StageRecorded(r.and_then(|s| s.map_err(|e| e.message))),
            );
        }
        Msg::StageRecorded(Err(why)) => state.said = Some((Health::Attention, why)),
        Msg::StageRecorded(Ok(())) => {}
        Msg::SearchKeywords(value) => {
            state.search_form.keywords = value;
            state.search_dirty = true;
        }
        Msg::SearchLocation(value) => {
            state.search_form.location = value;
            state.search_dirty = true;
        }
        Msg::SearchThreshold(value) => {
            state.search_form.tailor_from_score = value;
            state.search_dirty = true;
        }
        Msg::ToggleSearchProfile => state.search_profile_open = !state.search_profile_open,
        Msg::SearchRemote(value) => {
            state.search_form.remote_ok = if value { "1" } else { "" }.into();
            state.search_dirty = true;
        }
        Msg::SearchSite(domain, enabled) => {
            let mut skipped = state
                .search_form
                .skipped_sites
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>();
            skipped.retain(|s| s != domain);
            if !enabled {
                skipped.push(domain.to_string());
            }
            state.search_form.skipped_sites = skipped.join("\n");
            state.search_dirty = true;
        }
        Msg::SearchSaved(stage, result) => {
            state.busy = false;
            match result {
                Ok(said) => {
                    state.search_dirty = false;
                    state.search_errors.clear();
                    state.said = Some((Health::Ready, said));
                    // Wait for fresh search data before exposing the next step's controls.
                    state.steps = None;
                    return update(state, Msg::Stage(stage), runner).chain(Task::done(Msg::Load));
                }
                Err(errors) => state.search_errors = errors,
            }
        }
        Msg::SignIn => {
            state.busy = true;
            return runner.run(
                |context| async move { search_service::open_sign_in(&context).map(str::to_string) },
                |r| Msg::Scheduled(r.and_then(|s| s.map_err(|e| e.message))),
            );
        }
        Msg::FirstScan => {
            state.busy = true;
            state.scanning = true;
            state.said = Some((Health::Neutral, "Trying one search, matching up to three posts, for up to five minutes. Configured cloud tasks may incur provider charges. Task manager shows each stage.".into()));
            return runner.run(
                |context| async move {
                    scan_service::scan(context, scan_service::Started::Setup)
                        .await
                        .map(|report| {
                            let mut text = report.summary();
                            if report.posts_found == 0 && report.problems.is_empty() {
                                text.push_str(" No posts found: try broader keywords or another location.");
                            }
                            if report.matched > 0 {
                                text.push_str(" Open the dashboard to review your matches and their reasons.");
                            }
                            text
                        })
                },
                |r| Msg::ScanDone(r.and_then(|s| s.map_err(|e| e.message))),
            );
        }
        Msg::ScanDone(result) => {
            state.busy = false;
            state.scanning = false;
            state.said = Some(match result {
                Ok(s) => (Health::Ready, s),
                Err(e) => (Health::Attention, e),
            });
            return Task::done(Msg::Load);
        }
        Msg::StopScan => {
            return runner.run(
                |context| async move {
                    scan_lock::ask_to_stop(&context)
                        .map(|_| "Stopping the trial scan…".to_string())
                        .map_err(|e| e.to_string())
                },
                |r| Msg::Tell(r.and_then(|s| s).unwrap_or_else(|e| e)),
            );
        }
        Msg::SkipScan => {
            return runner.run(
                |context| async move { setup_steps::skip_scan(&context).await },
                |r| Msg::Scheduled(r.and_then(|s| s.map_err(|e| e.message))),
            );
        }
        Msg::Manual => {
            return runner.run(
                |context| async move { setup_steps::choose_manual(&context).await },
                |r| Msg::Scheduled(r.and_then(|s| s.map_err(|e| e.message))),
            );
        }
        Msg::Complete => {
            state.busy = true;
            return runner.run(
                |context| async move { setup_steps::finish(&context).await },
                |r| Msg::Completed(r.and_then(|s| s.map_err(|e| e.message))),
            );
        }
        Msg::Completed(Ok(())) => {
            state.busy = false;
            return Task::done(Msg::Finish);
        }
        Msg::Completed(Err(e)) => {
            state.busy = false;
            state.said = Some((Health::Attention, e));
        }
        Msg::CvPicked(picked) => {
            state.busy = false;
            match picked {
                Ok(None) => {}
                Ok(Some((name, text))) => {
                    state.cv_text = text_editor::Content::with_text(&text);
                    state.cv_name = name;
                    state.cv_error = None;
                }
                Err(why) => state.cv_error = Some(why),
            }
        }
        Msg::CvEdit(action) => state.cv_text.perform(action),
        Msg::Replace(replace) => state.replace = replace,
        Msg::ReadCv => {
            state.busy = true;
            state.cv_error = None;
            state.said = Some((Health::Neutral, "Your AI model is reading the CV…".to_string()));
            let (cv_text, cv_name, replace) = (state.cv_text.text(), state.cv_name.clone(), state.replace);
            return runner.run(
                move |context| async move { setup_steps::read_cv(&context, &cv_text, &cv_name, replace).await },
                |answer| Msg::CvRead(answer.and_then(|said| said.map_err(|e| e.message))),
            );
        }
        Msg::CvRead(Ok(said)) => {
            state.busy = false;
            state.cv_text = text_editor::Content::new();
            state.cv_name.clear();
            state.said = Some((Health::Ready, said));
            return Task::done(Msg::Load);
        }
        Msg::CvRead(Err(why)) => {
            state.busy = false;
            state.said = None;
            state.cv_error = Some(why);
        }
        Msg::YearsEdit(typed) => state.years_text = typed,
        Msg::SaveYears => {
            state.busy = true;
            state.years_error = None;
            let typed = state.years_text.clone();
            return runner.run(
                move |context| async move { setup_steps::save_years_of_work(&context, &typed).await },
                |answer| Msg::YearsSaved(answer.and_then(|said| said.map_err(|e| e.message))),
            );
        }
        Msg::YearsSaved(Ok(said)) => {
            state.busy = false;
            state.said = Some((Health::Ready, said));
            return Task::done(Msg::Load);
        }
        Msg::YearsSaved(Err(why)) => {
            state.busy = false;
            state.years_error = Some(why);
        }
        Msg::Interval(interval) => state.interval = Some(interval),
        Msg::Schedule => {
            let Some(interval) = state.interval else {
                return Task::none();
            };
            state.busy = true;
            return runner.run(
                move |context| async move { setup_steps::schedule_scan(&context, interval.hours).await },
                |answer| Msg::Scheduled(answer.and_then(|said| said.map_err(|e| e.message))),
            );
        }
        Msg::Unschedule => {
            state.busy = true;
            return runner.run(
                |context| async move { setup_steps::unschedule_scan(&context).await.map(str::to_string) },
                |answer| Msg::Scheduled(answer.and_then(|said| said.map_err(|e| e.message))),
            );
        }
        Msg::Scheduled(said) => {
            state.busy = false;
            state.said = Some(match said {
                Ok(said) => (Health::Ready, said),
                Err(why) => (Health::Attention, why),
            });
            return Task::done(Msg::Load);
        }
        // The shell changes screens.
        Msg::Tell(said) => state.said = Some((Health::Ready, said)),
        Msg::ShowLanguageModels | Msg::ShowLocalModel | Msg::Finish | Msg::Later | Msg::ShowSearches => {}
    }
    Task::none()
}

pub fn view(state: &State) -> Element<'_, Msg> {
    let title = row![
        widgets::page_title(
            "Set up JobFinder",
            widgets::note("Choose where to keep your data and models, then set up your CV, jobs and sites. You can leave and resume later."),
        ),
        drawings::drawing(&drawings::HERO),
    ]
    .spacing(GAP)
    .align_y(Center);
    let mut screen = Column::new().spacing(GAP * 1.5).push(title);
    if let Some((health, said)) = &state.said {
        screen = screen.push(widgets::message_bar(*health, said.as_str()));
    }
    let steps = match &state.steps {
        None => return screen.push(widgets::note("Reading the setup…")).into(),
        Some(Err(why)) => {
            return screen
                .push(widgets::message_bar(Health::Attention, why.as_str()))
                .into();
        }
        Some(Ok(steps)) => steps,
    };
    let labels = ["Storage", "Models", "Your CV", "Jobs & sites", "First scan", "Schedule"];
    let models_configured = local_model::configured(&state.models);
    let done = [
        true,
        local_model::ready(&state.models),
        steps.cv.filled.is_some(),
        steps.search.id > 0 && !state.search_dirty,
        steps.search.scan_summary.is_some() || steps.search.progress.skipped_scan,
        steps.schedule.interval_hours.is_some() || steps.search.progress.manual_scan,
    ];
    let mut stages = row![].spacing(6);
    for (index, label) in labels.iter().enumerate() {
        stages = stages.push(
            button(
                text(format!(
                    "{}. {label}{}",
                    index + 1,
                    if done[index] && index > 0 { " ✓" } else { "" }
                ))
                .size(style::CAPTION_SIZE),
            )
            .padding(8)
            .style(if index == state.stage {
                style::primary_button
            } else {
                style::standard_button
            })
            .on_press_maybe(
                (!state.busy
                    && (index <= 1 || models_configured)
                    && (index <= 3 || state.stage == 3 || !state.search_dirty))
                    .then_some(Msg::Stage(index)),
            ),
        );
    }
    let content = match state.stage {
        0 => step("Choose where JobFinder stores files", true, column![
            database_step(&steps.database),
            widgets::divider(),
            local_model::setup_folder(&state.models).map(Msg::Models),
        ].spacing(12).into()),
        1 => step("Choose your models", done[1], column![
            widgets::message_bar(Health::Attention, crate::dashboard::PRIVACY_GUIDANCE),
            local_model::setup_view(&state.models).map(Msg::Models),
        ].spacing(12).into()),
        2 => step("Import and review your CV", done[2], cv_step(&steps.cv, state)),
        3 => step("Define your jobs and choose sites", done[3], search_step(&steps.search, state)),
        4 => step("Try your first scan", done[4], first_scan(&steps.search, state)),
        _ => step("Choose when to check for jobs", done[5], column![
            schedule_step(&steps.schedule, state),
            button(text(if steps.search.progress.manual_scan && steps.schedule.interval_hours.is_none() { "Manual scanning selected" } else { "I'll scan manually" }).size(style::BODY_SIZE))
                .style(style::standard_button).on_press_maybe((!state.busy).then_some(Msg::Manual)),
            widgets::caption("Finish saves this setup choice. If anything is missing, you'll see what needs attention. You can revisit Setup from the sidebar at any time."),
        ].spacing(8).into()),
    };
    let mut navigation = row![
        button(text("Continue later").size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press_maybe((!state.busy).then_some(Msg::Later)),
        space_fill()
    ]
    .spacing(8)
    .align_y(Center);
    if state.stage > 0 {
        navigation = navigation.push(
            button(text("Back").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press_maybe((!state.busy).then_some(Msg::Stage(state.stage - 1))),
        );
    }
    if state.stage < 5 {
        navigation = navigation.push(
            button(text("Continue").size(style::BODY_SIZE))
                .padding([8.0, 16.0])
                .style(style::primary_button)
                .on_press_maybe(
                    (!state.busy
                        && (state.stage != 1 || models_configured)
                        && (state.stage <= 3 || !state.search_dirty))
                        .then_some(Msg::Stage(state.stage + 1)),
                ),
        );
    } else {
        navigation = navigation.push(
            button(text("Finish setup").size(style::BODY_SIZE))
                .padding([8.0, 16.0])
                .style(style::primary_button)
                .on_press_maybe((!state.busy && !state.search_dirty).then_some(Msg::Complete)),
        );
    }
    screen.push(stages).push(content).push(navigation).into()
}

fn search_step<'a>(step: &'a setup_steps::SearchStep, state: &'a State) -> Element<'a, Msg> {
    let form = &state.search_form;
    let input = |example, value, on_input: fn(String) -> Msg| {
        text_input(example, value)
            .on_input_maybe((!state.busy).then_some(on_input))
            .padding(8)
            .style(style::text_field)
            .into()
    };
    let search = column![
        widgets::section_title("Job search"),
        widgets::field(
            "Search name",
            input("e.g. Backend roles", &form.name, Msg::SearchName),
            None
        ),
        widgets::field(
            "Job titles or keywords",
            input(
                "e.g. Rust developer, backend developer",
                &form.keywords,
                Msg::SearchKeywords
            ),
            Some("Separate multiple titles or keywords with commas."),
        ),
        widgets::field(
            "Location",
            input("e.g. Aarhus or 8000", &form.location, Msg::SearchLocation),
            None
        ),
        checkbox(form.remote_ok == "1")
            .label("Include remote jobs")
            .on_toggle_maybe((!state.busy).then_some(Msg::SearchRemote))
            .style(style::check_box),
    ]
    .spacing(10);

    let mut sites = column![widgets::section_title("Job sites")].spacing(8);
    for (domain, label) in [
        ("jobnet.dk", "Jobnet — public search"),
        ("jobindex.dk", "Jobindex — public search"),
        ("linkedin.com", "LinkedIn — sign-in required"),
        ("indeed.com", "Indeed — sign-in may be required"),
    ] {
        sites = sites.push(
            checkbox(!form.skipped_sites.lines().any(|s| s == domain))
                .label(label)
                .on_toggle_maybe((!state.busy).then_some(move |on| Msg::SearchSite(domain, on)))
                .style(style::check_box),
        );
    }
    sites = sites
        .push(
            button(text("Sign in to job sites").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press_maybe((!state.busy).then_some(Msg::SignIn)),
        )
        .push(widgets::caption("The scan pauses if LinkedIn or Indeed asks you to sign in or verify that you're human. If OpenAI is selected for job evaluation, the scan also uses its web search."));
    for status in &step.source_status {
        sites = sites.push(widgets::caption(status.as_str()));
    }

    let mut body = column![
        search,
        widgets::divider(),
        sites,
        widgets::divider(),
        widgets::field(
            "Minimum match score for automatic writing",
            input("0–101", &form.tailor_from_score, Msg::SearchThreshold),
            Some("Matches at or above this score (0–100) get a tailored résumé and application. Enter 101 to write them manually."),
        ),
    ]
    .spacing(14);
    if let Some(profile) = &step.profile_text {
        let toggle_label = if state.search_profile_open {
            "▾ Profile used for matching"
        } else {
            "▸ Profile used for matching"
        };
        body = body.push(widgets::divider()).push(
            button(text(toggle_label).size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press(Msg::ToggleSearchProfile),
        );
        if state.search_profile_open {
            body = body
                .push(widgets::caption("To correct your profile, go to step 3 (Your CV) and import your updated CV with the replacement option selected."))
                .push(widgets::caption(profile.as_str()));
        }
    }
    if !state.search_errors.is_empty() {
        body = body.push(widgets::errors(&state.search_errors));
    }
    body.into()
}

fn first_scan<'a>(step: &'a setup_steps::SearchStep, state: &'a State) -> Element<'a, Msg> {
    let mut body = column![widgets::note("Try one saved search: up to three posts matched, with a five-minute limit. Models on cloud providers use your API account. Open Task manager for live stages and cancellation; Job searches shows results and failures.")].spacing(8);
    let models_ready = local_model::ready(&state.models);
    if !models_ready && step.scan_problem.is_none() {
        body = body.push(widgets::note(
            "Your selected models need to be ready before the trial scan. You can skip it and continue setup.",
        ));
    }
    if let Some(summary) = &step.scan_summary {
        body = body.push(widgets::caption(summary.clone()));
    }
    if step.progress.skipped_scan {
        body = body.push(widgets::caption(
            "First scan deliberately skipped. You can scan from Job searches later.",
        ));
    }
    if let Some(problem) = &step.scan_problem {
        body = body.push(widgets::caption(problem.clone()));
    }
    let mut actions = row![
        button(text("Try first scan").size(style::BODY_SIZE))
            .style(style::primary_button)
            .on_press_maybe(
                (!state.busy && models_ready && !state.search_dirty && step.id > 0 && step.scan_problem.is_none())
                    .then_some(Msg::FirstScan)
            ),
        button(text("Skip for now").size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press_maybe((!state.busy).then_some(Msg::SkipScan)),
        button(text("Review search results").size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press(Msg::ShowSearches),
    ]
    .spacing(8);
    if state.scanning {
        actions = actions.push(
            button(text("Stop trial").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press(Msg::StopScan),
        );
    }
    body.push(actions).into()
}

fn space_fill<'a>() -> Element<'a, Msg> {
    iced::widget::space().width(Fill).into()
}

/// A step's card: its title with a done mark, and its body.
fn step<'a>(title: &'a str, done: bool, body: Element<'a, Msg>) -> Element<'a, Msg> {
    let heading = row![
        style::mark(if done { Health::Ready } else { Health::Neutral }),
        widgets::section_title(title),
    ]
    .spacing(10)
    .align_y(Center);
    column![
        heading,
        widgets::card(container(body).padding(CARD_PADDING).width(Fill).into())
    ]
    .spacing(10)
    .into()
}

fn database_step(database: &DatabaseStep) -> Element<'_, Msg> {
    let kept = if database.made_now {
        "A new database was made for you at"
    } else {
        "Your data is kept in"
    };
    column![
        widgets::section_title("Database"),
        text(database.system.clone()).size(style::BODY_SIZE),
        widgets::caption(database.storage.clone()),
        text(format!("{kept} {}.", database.path)).size(style::BODY_SIZE),
        widgets::caption(
            "Your matches, profile, settings and API keys are in this one file. The keys are sealed with a key file kept beside it, so a copy of the database alone doesn't give them away."
        ),
    ]
    .spacing(6)
    .into()
}

fn cv_step<'a>(cv: &'a CvStep, state: &'a State) -> Element<'a, Msg> {
    let mut body = Column::new().spacing(8).push(widgets::message_bar(
        Health::Attention,
        crate::dashboard::PRIVACY_GUIDANCE,
    ));
    if let Some(filled) = &cv.filled {
        body = body.push(
            text(format!(
                "Your profile: {}. The match page matches with it, and applications and résumés are written from it.",
                setup_steps::filled_line(filled)
            ))
            .size(style::BODY_SIZE),
        );
    }
    if let Some(years) = &cv.years {
        body = body.push(years_row(years, state));
    }
    if !cv.can_read {
        if cv.filled.is_none() {
            body = body.push(widgets::note("Set up an AI model first: reading the CV runs on it."));
        }
        return body.into();
    }
    let models_ready = local_model::ready(&state.models);
    let waiting = |msg: Msg| (!state.busy).then_some(msg);
    let picked = if state.cv_name.is_empty() {
        "A PDF or text file, or paste it below.".to_string()
    } else {
        format!("Read from {}.", state.cv_name)
    };
    body = body
        .push(
            text(if cv.filled.is_some() {
                "Read another CV into your profile"
            } else {
                "Read your CV into your profile"
            })
            .font(style::semibold())
            .size(style::BODY_SIZE),
        )
        .push(
            row![
                button(text("Choose a file").size(style::BODY_SIZE))
                    .style(style::standard_button)
                    .on_press_maybe(waiting(Msg::ChooseCv)),
                widgets::caption(picked),
            ]
            .spacing(10)
            .align_y(Center),
        )
        .push(
            text_editor(&state.cv_text)
                .placeholder("Paste your CV here")
                .height(CV_BOX_HEIGHT)
                .size(style::BODY_SIZE)
                .style(style::text_area)
                .on_action(Msg::CvEdit),
        )
        .push(widgets::caption(
            "Read by your AI model, which takes up to a minute. Leave out your CPR number, health, union membership and photos.",
        ));
    if let Some(why) = &state.cv_error {
        body = body.push(widgets::errors_line(why.clone()));
    }
    if cv.filled.is_some() {
        body = body.push(
            checkbox(state.replace)
                .label("Replace my profile with what this CV says")
                .size(16)
                .text_size(style::BODY_SIZE)
                .style(style::check_box)
                .on_toggle(Msg::Replace),
        );
    }
    if !models_ready {
        body = body.push(widgets::note(
            "You can add your CV text now and read it once the models are ready.",
        ));
    }
    let can_read = models_ready && !state.cv_text.text().trim().is_empty() && (cv.filled.is_none() || state.replace);
    body.push(
        button(text("Read the CV into my profile").size(style::BODY_SIZE))
            .style(style::primary_button)
            .on_press_maybe(waiting(Msg::ReadCv).filter(|_| can_read)),
    )
    .into()
}

/// The years of professional work: a box with the saved years or the suggestion counted from
/// the jobs, Save, and what the number is.
fn years_row<'a>(years: &'a setup_steps::YearsOfWork, state: &'a State) -> Element<'a, Msg> {
    let mut body = Column::new().spacing(6).push(
        row![
            text("Years of professional work").size(style::BODY_SIZE),
            text_input("", &state.years_text)
                .on_input(Msg::YearsEdit)
                .on_submit(Msg::SaveYears)
                .size(style::BODY_SIZE)
                .padding(8)
                .width(YEARS_BOX_WIDTH)
                .style(style::text_field),
            button(text("Save").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press_maybe((!state.busy).then_some(Msg::SaveYears)),
        ]
        .spacing(10)
        .align_y(Center),
    );
    body = body.push(widgets::caption(setup_steps::years_line(years)));
    if let Some(why) = &state.years_error {
        body = body.push(widgets::errors_line(why.clone()));
    }
    body.into()
}

fn schedule_step<'a>(schedule: &'a ScheduleStep, state: &'a State) -> Element<'a, Msg> {
    let waiting = |msg: Msg| (!state.busy).then_some(msg);
    let mut body = Column::new().spacing(8);
    body = body.push(match schedule.interval_hours {
        Some(hours) => text(format!(
            "The job scan runs by itself: {}, while you're logged in to this computer.",
            setup_steps::interval_label(hours).to_lowercase()
        ))
        .size(style::BODY_SIZE),
        None => text(
            "The system's scheduler (Task Scheduler on Windows, a systemd timer on Linux) can run the job scan by itself, so new posts are waiting when you open the app.",
        )
        .size(style::BODY_SIZE)
        .style(style::secondary),
    });
    if let Some(why) = &schedule.error {
        body = body.push(widgets::errors_line(format!("Task Scheduler couldn't be read: {why}")));
    }
    let scheduled = schedule.interval_hours.is_some();
    let mut buttons = row![
        pick_list(intervals(), state.interval, Msg::Interval)
            .width(PICK_WIDTH)
            .text_size(style::BODY_SIZE)
            .style(style::pick_field)
            .menu_style(style::pick_menu),
        button(text(if scheduled { "Change" } else { "Check for new jobs" }).size(style::BODY_SIZE))
            .style(if scheduled {
                style::standard_button
            } else {
                style::primary_button
            })
            .on_press_maybe(waiting(Msg::Schedule)),
    ]
    .spacing(8)
    .align_y(Center);
    if scheduled {
        buttons = buttons.push(
            button(text("Stop checking by itself").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press_maybe(waiting(Msg::Unschedule)),
        );
    }
    body = body.push(buttons);
    if !schedule.task_run.is_empty() {
        body = body.push(widgets::caption(format!("The task runs {}", schedule.task_run)));
    }
    body.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_starts_on_the_tasks_interval_or_the_default() {
        assert_eq!(interval_of(Some(8)).map(|interval| interval.hours), Some(8));
        assert_eq!(
            interval_of(None).map(|interval| interval.hours),
            Some(DEFAULT_INTERVAL_HOURS)
        );
        assert_eq!(interval_of(Some(3)), None);
    }
}
