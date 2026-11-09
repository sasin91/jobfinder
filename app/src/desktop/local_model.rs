//! The Local model screen: setting up the app's own llama.cpp model in the window. This computer,
//! llama-server, the download running with its progress, and the models to download or use,
//! all through llama_setup_service.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use iced::widget::{Column, button, column, container, pick_list, progress_bar, row, text};
use iced::{Center, Element, Fill, Task};

use super::style;
use super::widgets::{self, CARD_PADDING, ROW_PADDING};
use super::{GAP, Runner};
use crate::account::llama_setup_service::{self, CatalogRow, LlamaSetupView};
use crate::account::model_plan;
use crate::dashboard::Health;

/// How often the screen reads a running download's progress.
const PROGRESS_EVERY: Duration = Duration::from_secs(1);
/// The progress bar's height.
const BAR_HEIGHT: f32 = 6.0;
/// Percent, the bar's whole.
const PERCENT: f32 = 100.0;

#[derive(Default)]
pub struct State {
    /// None until read; Err says why it couldn't be.
    view: Option<Result<Arc<LlamaSetupView>, String>>,
    /// What the last click came to.
    said: Option<(Health, String)>,
    /// A reload is on its way, so a download's progress isn't asked for twice.
    polling: bool,
    busy: bool,
    choices: BTreeMap<&'static str, String>,
    /// Persisted choices (or fresh recommendations) that the pending edits started from.
    choice_baseline: BTreeMap<&'static str, String>,
    dirty: bool,
}

impl State {
    fn reconcile_choices(&mut self, plan: &model_plan::Overview) {
        let baseline: BTreeMap<_, _> = plan
            .rows
            .iter()
            .filter(|r| plan.fresh || r.runtime.is_some())
            .map(|r| {
                (
                    r.task.name,
                    if plan.fresh {
                        r.suggested.unwrap_or("").to_string()
                    } else {
                        r.current.clone()
                    },
                )
            })
            .collect();
        let mut choices = baseline.clone();
        if self.dirty {
            for (task, file) in &self.choices {
                // Only carry genuine edits forward, and only for tasks still using local models.
                // Unedited tasks follow any saved changes made in the advanced screen.
                if self.choice_baseline.get(task) != Some(file)
                    && let Some(choice) = choices.get_mut(task)
                {
                    choice.clone_from(file);
                }
            }
        }
        self.dirty = choices != baseline;
        self.choices = choices;
        self.choice_baseline = baseline;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelChoice {
    file: String,
    label: String,
}

impl std::fmt::Display for ModelChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Clone)]
pub enum Msg {
    Load,
    Loaded(Result<Arc<LlamaSetupView>, String>),
    Evaluate,
    PickTask(&'static str, ModelChoice),
    SaveChoices,
    ChoicesSaved(Result<(), String>),
    RetryDownloads,
    CheckTask(&'static str),
    ShowLanguageModels,
    ShowFullModels,
    BackToSetup,
    DownloadServer,
    DownloadModel(&'static str),
    UseModel(&'static str),
    /// Asks for a folder and moves the models there.
    MoveModels,
    /// The folder dialog was closed without a folder.
    NoFolder,
    /// What a click came to; a reload follows.
    Said(Result<String, String>),
    /// The next look at a running download.
    Poll,
    /// A downloaded model is saved as this app's own model; the shell goes on to where its uses
    /// are picked (Language models), or to Setup while that is unfinished, saying message there.
    ModelInUse {
        message: String,
        setup_unfinished: bool,
    },
}

pub fn update(state: &mut State, msg: Msg, runner: &Runner) -> Task<Msg> {
    match msg {
        Msg::Load => {
            return runner.run(
                |context| async move { llama_setup_service::llama_setup_view(&context).await.map(Arc::new) },
                |read| Msg::Loaded(read.and_then(|view| view.map_err(|e| e.message))),
            );
        }
        Msg::Loaded(view) => {
            let running = state.busy || view.as_ref().is_ok_and(|view| view.busy);
            if let Ok(view) = &view
                && let Some(plan) = &view.plan
            {
                state.reconcile_choices(plan);
            }
            state.view = Some(view);
            if running && !state.polling {
                state.polling = true;
                return runner.run(|_| tokio::time::sleep(PROGRESS_EVERY), |_| Msg::Poll);
            }
        }
        Msg::Poll => {
            state.polling = false;
            return Task::done(Msg::Load);
        }
        Msg::Evaluate => {
            state.busy = true;
            state.said = Some((Health::Neutral, "Refreshing hardware and comparing task models…".into()));
            return runner.run(
                |context| async move { model_plan::evaluate_again(&context).await },
                said,
            );
        }
        Msg::PickTask(task, choice) => {
            if working(state) {
                return Task::none();
            }
            state.choices.insert(task, choice.file);
            state.dirty = state.choices != state.choice_baseline;
        }
        Msg::SaveChoices => {
            state.busy = true;
            let choices = state
                .choices
                .iter()
                .map(|(name, file)| (name.to_string(), file.clone()))
                .collect::<Vec<_>>();
            return runner.run(
                move |context| async move { model_plan::save_choices(&context, &choices).await },
                |answer| Msg::ChoicesSaved(answer.and_then(|r| r.map_err(|e| e.message))),
            );
        }
        Msg::ChoicesSaved(Err(why)) => {
            state.busy = false;
            state.said = Some((Health::Attention, why));
        }
        Msg::ChoicesSaved(Ok(())) => {
            state.dirty = false;
            return Task::done(Msg::RetryDownloads);
        }
        Msg::RetryDownloads => {
            state.busy = true;
            state.said = Some((
                Health::Neutral,
                "Setting up your selected models… Downloads start automatically. Task manager offers cancellation; saved choices and partial downloads are kept.".into(),
            ));
            return Task::batch([
                runner.run(
                    |context| async move { model_plan::download_selected(context).await },
                    said,
                ),
                Task::done(Msg::Load),
            ]);
        }
        Msg::CheckTask(task) => {
            state.busy = true;
            state.said = Some((
                Health::Neutral,
                "Loading and checking the assigned local model (up to two minutes)…".into(),
            ));
            return runner.run(
                move |context| async move { model_plan::check(&context, task).await },
                said,
            );
        }
        Msg::DownloadServer => {
            return runner.run(
                |context| async move {
                    llama_setup_service::download_server(&context).map(|()| "Downloading llama.cpp…".to_string())
                },
                said,
            );
        }
        Msg::DownloadModel(file) => {
            return runner.run(
                move |context| async move {
                    llama_setup_service::download_model(&context, file).map(|()| format!("Downloading {file}…"))
                },
                said,
            );
        }
        Msg::UseModel(file) => {
            state.busy = true;
            return runner.run(
                move |context| async move {
                    let saved = llama_setup_service::use_model(&context, file).await?;
                    model_plan::download_selected(context).await?;
                    Ok::<_, crate::service_error::ServiceError>(saved)
                },
                |answer| match answer.and_then(|saved| saved.map_err(|e| e.message)) {
                    Ok(saved) => Msg::ModelInUse {
                        message: saved.message,
                        setup_unfinished: saved.setup_unfinished,
                    },
                    Err(why) => Msg::Said(Err(why)),
                },
            );
        }
        Msg::MoveModels => {
            state.said = Some((Health::Neutral, "Pick the folder to keep the models in…".to_string()));
            return runner.run(
                |context| async move {
                    let Some(folder) = rfd::AsyncFileDialog::new()
                        .set_title("Choose the folder to keep the models in")
                        .pick_folder()
                        .await
                    else {
                        return Ok(None);
                    };
                    llama_setup_service::move_models(&context, folder.path().to_path_buf())
                        .await
                        .map(Some)
                },
                |answer| match answer.and_then(|moved| moved.map_err(|e| e.message)) {
                    Ok(None) => Msg::NoFolder,
                    Ok(Some(said)) => Msg::Said(Ok(said)),
                    Err(why) => Msg::Said(Err(why)),
                },
            );
        }
        Msg::NoFolder => state.said = None,
        // The shell goes on.
        Msg::ModelInUse { .. } => state.busy = false,
        Msg::ShowLanguageModels | Msg::ShowFullModels | Msg::BackToSetup => {}
        Msg::Said(said) => {
            state.busy = false;
            state.said = Some(match said {
                Ok(said) => (Health::Ready, said),
                Err(why) => (Health::Attention, why),
            });
            return Task::done(Msg::Load);
        }
    }
    Task::none()
}

/// A service's answer as Msg::Said.
fn said(answer: Result<Result<String, crate::service_error::ServiceError>, String>) -> Msg {
    Msg::Said(answer.and_then(|said| said.map_err(|e| e.message)))
}

pub fn view(state: &State) -> Element<'_, Msg> {
    let title = widgets::page_title(
        "Models",
        widgets::note(
            "Which model handles each task, why it was chosen, and alternatives for this computer. Local models keep your CV here; cloud providers receive the inputs for their assigned tasks.",
        ),
    );
    let mut screen = Column::new().spacing(GAP * 1.5).push(title);
    if let Some((health, said)) = &state.said {
        screen = screen.push(widgets::message_bar(*health, said.as_str()));
    }
    let view = match &state.view {
        None => return screen.push(widgets::note("Looking at this computer…")).into(),
        Some(Err(why)) => {
            return screen
                .push(widgets::message_bar(Health::Attention, why.as_str()))
                .into();
        }
        Some(Ok(view)) => view,
    };
    let busy = state.busy || view.busy;
    if let Some(plan) = &view.plan {
        screen = screen.push(plan_card(state, view, plan, busy));
    }
    if view.setup_unfinished {
        screen = screen.push(
            button(text("Continue setup").size(style::BODY_SIZE))
                .style(style::standard_button)
                .on_press_maybe((!busy).then_some(Msg::BackToSetup)),
        );
    }
    screen = screen.push(widgets::section("This computer", computer(view, busy)));
    if let Some(download) = &view.download {
        screen = screen.push(widgets::section("Download", download_card(download)));
    }
    if !view.downloaded.is_empty() {
        screen = screen.push(widgets::section(
            "Downloaded models",
            widgets::rows(
                view.downloaded
                    .iter()
                    .map(|model| model_row(model, ModelAction::Use, busy)),
            ),
        ));
    }
    if !view.to_download.is_empty() {
        screen = screen.push(widgets::section(
            "Models to download",
            widgets::rows(
                view.to_download
                    .iter()
                    .map(|model| model_row(model, ModelAction::Download, busy)),
            ),
        ));
    }
    let move_models = button(text("Move to another folder…").size(style::BODY_SIZE))
        .style(style::standard_button)
        .on_press_maybe((!busy).then_some(Msg::MoveModels));
    let folder = row![
        column![
            text(view.models_folder.as_str()).size(style::BODY_SIZE),
            widgets::caption("The models you have move with it; new ones download there."),
        ]
        .spacing(2)
        .width(Fill),
        move_models,
    ]
    .spacing(14)
    .align_y(Center)
    .padding(ROW_PADDING);
    screen.push(widgets::section("Models folder", folder.into())).into()
}

/// The same task choices used in Models, without the catalog and folder-management sections.
pub fn setup_view(state: &State) -> Element<'_, Msg> {
    let mut body = Column::new().spacing(12);
    if let Some((health, said)) = &state.said {
        body = body.push(widgets::message_bar(*health, said.as_str()));
    }
    let view = match &state.view {
        None => return body.push(widgets::note("Checking hardware and model choices…")).into(),
        Some(Err(why)) => {
            return body
                .push(widgets::message_bar(Health::Attention, why.as_str()))
                .push(button(text("Providers and advanced choices")).on_press(Msg::ShowLanguageModels))
                .into();
        }
        Some(Ok(view)) => view,
    };
    body = body.push(text(view.computer.as_str()).size(style::BODY_SIZE))
        .push(widgets::note("Choose a model for each task below. Using your choices automatically downloads missing models and the local runtime. Shared models download once. Local tasks keep their inputs on this computer; cloud tasks send inputs to your chosen provider."));
    if let Some(problem) = &view.card_problem {
        body = body.push(widgets::message_bar(Health::Attention, problem.as_str()));
    }
    if let Some(download) = &view.download {
        body = body.push(download_card(download));
    }
    if let Some(plan) = &view.plan {
        body = body.push(plan_card(state, view, plan, working(state)));
    }
    body.push(
        button(text("Open full Models overview").size(style::BODY_SIZE))
            .style(style::standard_button)
            .on_press(Msg::ShowFullModels),
    )
    .into()
}

pub fn working(state: &State) -> bool {
    state.busy || state.view.as_ref().is_some_and(|v| v.as_ref().is_ok_and(|v| v.busy))
}

pub fn ready(state: &State) -> bool {
    !working(state)
        && !state.dirty
        && state.view.as_ref().is_some_and(|v| {
            v.as_ref().is_ok_and(|v| {
                v.plan.as_ref().is_some_and(|p| {
                    p.rows.iter().all(|r| {
                        if r.provider.is_empty() {
                            r.task.use_ == crate::account::account_model::APPLICATION
                        } else {
                            r.runtime.is_none() || (r.downloaded && r.runtime_available)
                        }
                    })
                })
            })
        })
}

fn model_choices(view: &LlamaSetupView, row: &model_plan::Row) -> Vec<ModelChoice> {
    let mut choices: Vec<_> = view
        .downloaded
        .iter()
        .chain(&view.to_download)
        .map(|m| ModelChoice {
            file: m.file.to_string(),
            label: format!(
                "{} · {}{}",
                m.label.split(':').next().unwrap_or(m.label),
                m.size,
                if row.suggested == Some(m.file) {
                    " · Recommended"
                } else {
                    ""
                }
            ),
        })
        .collect();
    if !row.current.is_empty() && !choices.iter().any(|c| c.file == row.current) {
        choices.push(ModelChoice {
            file: row.current.clone(),
            label: row.current.clone(),
        });
    }
    choices
}

fn plan_card<'a>(
    state: &'a State,
    view: &'a LlamaSetupView,
    plan: &'a model_plan::Overview,
    busy: bool,
) -> Element<'a, Msg> {
    let evaluated = plan
        .assessment
        .as_ref()
        .and_then(|a| chrono::DateTime::from_timestamp(a.at, 0))
        .map(|at| format!("Last evaluated: {}", at.format("%e %B %Y, %H:%M UTC")))
        .unwrap_or_else(|| "Recommendations use the detected memory and graphics card.".to_string());
    let mut body = Column::new().spacing(12)
        .push(widgets::caption(evaluated))
        .push(widgets::caption("Changed hardware or want to review the recommendations? Evaluate again refreshes hardware and fit estimates while keeping your choices. Local checks below measure a small correctness fixture and loading time."))
        .push(row![
            button(text("Evaluate again").size(style::BODY_SIZE)).style(style::standard_button).on_press_maybe((!busy).then_some(Msg::Evaluate)),
            button(text("Providers and advanced choices").size(style::BODY_SIZE)).style(style::standard_button).on_press(Msg::ShowLanguageModels),
        ].spacing(8));
    for task in &plan.rows {
        let local = plan.fresh || task.runtime.is_some();
        let mut card = column![text(task.task.label).font(style::semibold()).size(style::BODY_SIZE)].spacing(6);
        if local {
            let choices = model_choices(view, task);
            let selected = state
                .choices
                .get(task.task.name)
                .and_then(|f| choices.iter().find(|c| &c.file == f))
                .cloned();
            let label = selected
                .as_ref()
                .map(|c| c.label.clone())
                .unwrap_or_else(|| "Choose a model".into());
            let picker = pick_list(choices, selected, move |c| Msg::PickTask(task.task.name, c))
                .placeholder("Choose a model")
                .width(Fill)
                .text_size(style::BODY_SIZE);
            card = if busy {
                card.push(text(label).size(style::BODY_SIZE))
            } else {
                card.push(picker)
            };
            let file = state.choices.get(task.task.name).map(String::as_str).unwrap_or("");
            let option = view.downloaded.iter().chain(&view.to_download).find(|m| m.file == file);
            if let Some(option) = option {
                card = card.push(widgets::caption(format!(
                    "{} · {}",
                    option.fit,
                    if option.downloaded {
                        "Downloaded"
                    } else {
                        "Downloads automatically when you use these choices"
                    }
                )));
            }
            if let Some(recommended) = task.suggested {
                card = card.push(widgets::caption(format!(
                    "Recommended: {}. {}",
                    crate::llama::catalog::find(recommended)
                        .map(|m| m.label.split(':').next().unwrap_or(m.label))
                        .unwrap_or(recommended),
                    task.why
                )));
            } else {
                card = card.push(widgets::caption("No recommended model fits the known memory. Choose a smaller model or configure a provider in advanced choices."));
            }
            if !plan.fresh {
                card = card.push(widgets::caption(format!("Currently assigned: {}", task.current)));
                if let Some(check) = &task.check {
                    card = card.push(widgets::caption(format!(
                        "{} · {:.1} s including loading · {}",
                        if check.passed {
                            "Local fixture passed"
                        } else {
                            "Check needs attention"
                        },
                        check.milliseconds as f64 / 1000.0,
                        check.detail
                    )));
                }
                card = card.push(
                    button(text("Run local check").size(style::BODY_SIZE))
                        .style(style::standard_button)
                        .on_press_maybe(
                            (!busy && !state.dirty && task.downloaded && task.runtime_available)
                                .then_some(Msg::CheckTask(task.task.name)),
                        ),
                );
            }
        } else {
            card = card
                .push(
                    text(if task.provider.is_empty() {
                        "Not configured".to_string()
                    } else {
                        format!(
                            "{} / {}",
                            crate::account::account_model::provider_label(&task.provider),
                            task.current
                        )
                    })
                    .size(style::BODY_SIZE),
                )
                .push(widgets::caption(
                    "Manage this task's provider and model in Providers and advanced choices.",
                ));
        }
        body = body.push(widgets::card(container(card).padding(CARD_PADDING).width(Fill).into()));
    }
    if !state.choices.is_empty() {
        let unique: std::collections::BTreeSet<_> = state.choices.values().filter(|f| !f.is_empty()).collect();
        let remaining: u64 = unique
            .iter()
            .filter(|f| !view.downloaded.iter().any(|m| m.file == f.as_str()))
            .filter_map(|f| crate::llama::catalog::find(f))
            .map(|m| {
                crate::llama::download::remaining_bytes(
                    &m.url(),
                    &std::path::Path::new(&view.models_folder).join(m.file),
                    m.bytes,
                )
            })
            .sum();
        body = body.push(widgets::caption(format!("{} unique models selected · {} remaining to download. Shared models count once; resumable partial files are credited. Runtime and extraction space are additional.", unique.len(), crate::llama::hardware::gigabytes(remaining))));
        if plan.fresh || state.dirty {
            body = body
                .push(
                    button(text("Use these models").size(style::BODY_SIZE))
                        .style(style::primary_button)
                        .on_press_maybe(
                            (!busy && state.choices.values().all(|f| !f.is_empty())).then_some(Msg::SaveChoices),
                        ),
                )
                .push(widgets::caption(
                    "Saves these task choices and automatically downloads what they need.",
                ));
        } else if plan
            .rows
            .iter()
            .any(|r| r.runtime.is_some() && (!r.downloaded || !r.runtime_available))
        {
            body = body.push(
                button(text("Retry model downloads").size(style::BODY_SIZE))
                    .style(style::primary_button)
                    .on_press_maybe((!busy).then_some(Msg::RetryDownloads)),
            );
        } else {
            body = body.push(widgets::caption("Your selected local models are ready."));
        }
    }
    widgets::section("Your task models", body.into())
}

/// The computer the models are weighed against, and llama-server.
fn computer(view: &LlamaSetupView, busy: bool) -> Element<'_, Msg> {
    let mut rows: Vec<Element<'_, Msg>> = vec![
        row![
            style::mark(Health::Neutral),
            column![
                text("Memory and graphics card")
                    .font(style::semibold())
                    .size(style::BODY_SIZE),
                widgets::caption(view.computer.as_str()),
            ]
            .spacing(2)
            .width(Fill),
        ]
        .spacing(14)
        .align_y(Center)
        .padding(ROW_PADDING)
        .into(),
    ];
    let server: Element<'_, Msg> = if view.server_found {
        row![
            style::mark(Health::Ready),
            column![
                text("llama.cpp").font(style::semibold()).size(style::BODY_SIZE),
                widgets::caption(view.server_path.as_str()),
            ]
            .spacing(2)
            .width(Fill),
        ]
        .spacing(14)
        .align_y(Center)
        .padding(ROW_PADDING)
        .into()
    } else {
        row![
            style::mark(Health::Attention),
            column![
                text("llama.cpp").font(style::semibold()).size(style::BODY_SIZE),
                widgets::caption(format!("Not downloaded yet. This computer gets {}.", view.build_label)),
            ]
            .spacing(2)
            .width(Fill),
            button(text("Download llama.cpp").size(style::BODY_SIZE))
                .style(style::primary_button)
                .on_press_maybe((!busy).then_some(Msg::DownloadServer)),
        ]
        .spacing(14)
        .align_y(Center)
        .padding(ROW_PADDING)
        .into()
    };
    rows.push(server);
    // llama-server is here but can't use the graphics card: the right build replaces it.
    if let (true, Some(problem)) = (view.server_found, &view.card_problem) {
        rows.push(
            row![
                style::mark(Health::Attention),
                text(format!("{problem} Download it again to get {}.", view.build_label))
                    .size(style::CAPTION_SIZE)
                    .width(Fill),
                button(text("Download llama.cpp again").size(style::BODY_SIZE))
                    .style(style::standard_button)
                    .on_press_maybe((!busy).then_some(Msg::DownloadServer)),
            ]
            .spacing(14)
            .align_y(Center)
            .padding(ROW_PADDING)
            .into(),
        );
    }
    widgets::rows(rows.into_iter())
}

/// The running or last download: what, how far, and how it ended.
fn download_card(download: &llama_setup_service::DownloadView) -> Element<'_, Msg> {
    let mut body = column![
        text(download.what.as_str())
            .font(style::semibold())
            .size(style::BODY_SIZE)
    ]
    .spacing(8);
    if download.running {
        let percent = download.percent.unwrap_or(0) as f32;
        body = body
            .push(
                progress_bar(0.0..=PERCENT, percent)
                    .girth(BAR_HEIGHT)
                    .style(style::progress),
            )
            .push(widgets::caption(download.progress.as_str()));
    } else if let Some(why) = &download.why_failed {
        body = body.push(
            text(format!("It stopped: {why}"))
                .size(style::CAPTION_SIZE)
                .style(style::health_text(Health::Attention)),
        );
    } else if download.finished {
        body = body.push(
            text("Downloaded.")
                .size(style::CAPTION_SIZE)
                .style(style::health_text(Health::Ready)),
        );
    }
    container(body).padding(CARD_PADDING).width(Fill).into()
}

/// What a model row's button does.
#[derive(Clone, Copy, PartialEq)]
enum ModelAction {
    Use,
    Download,
}

/// A catalog model: its name, size and how it fits, and Use or Download.
fn model_row(model: &CatalogRow, action: ModelAction, busy: bool) -> Element<'_, Msg> {
    let mut name = row![text(model.label).font(style::semibold()).size(style::BODY_SIZE)].spacing(8);
    if model.recommended {
        name = name.push(
            container(text("Suggested").size(style::CAPTION_SIZE).style(style::accent_text))
                .padding([1.0, 8.0])
                .style(style::badge),
        );
    }
    let (label, msg, enabled) = match action {
        ModelAction::Use => ("Use as base model", Msg::UseModel(model.file), !busy),
        ModelAction::Download => ("Download", Msg::DownloadModel(model.file), !busy && !model.too_big),
    };
    let style = if model.recommended {
        style::primary_button
    } else {
        style::standard_button
    };
    row![
        column![name, widgets::caption(format!("{} · {}", model.size, model.fit))]
            .spacing(2)
            .width(Fill),
        button(text(label).size(style::BODY_SIZE))
            .style(style)
            .on_press_maybe(enabled.then_some(msg)),
    ]
    .spacing(14)
    .align_y(Center)
    .padding(ROW_PADDING)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::{account_model, key_vault::KeyVault, task_model};
    use crate::context::Context;
    use crate::llama::{
        catalog,
        hardware::{GIB, Inventory},
        llama_server::LlamaServer,
    };
    use crate::llm::{LLAMA_CPP, OPENAI};
    use std::sync::LazyLock;

    async fn runner() -> Runner {
        static TAXONOMY: LazyLock<Arc<crate::taxonomy::Taxonomy>> =
            LazyLock::new(|| Arc::new(crate::taxonomy::Taxonomy::load()));
        let mut context = Context::new(
            crate::db::open_in_memory().await.unwrap(),
            Arc::clone(&TAXONOMY),
            KeyVault::with_key_in_memory(),
        );
        context.llama = Some(LlamaServer::new(
            &std::env::temp_dir().join("jobfinder-routing-reconciliation"),
            Inventory {
                memory_bytes: 16 * GIB,
                gpu: None,
            },
        ));
        Runner {
            context: Arc::new(context),
            runtime: tokio::runtime::Handle::current(),
        }
    }

    async fn reload(state: &mut State, runner: &Runner) {
        let view = llama_setup_service::llama_setup_view(&runner.context).await.unwrap();
        let _ = update(state, Msg::Loaded(Ok(Arc::new(view))), runner);
    }

    fn pick(state: &mut State, runner: &Runner, task: &'static str, file: &str) {
        let _ = update(
            state,
            Msg::PickTask(
                task,
                ModelChoice {
                    file: file.into(),
                    label: file.into(),
                },
            ),
            runner,
        );
    }

    fn chosen(state: &State) -> Vec<(String, String)> {
        state
            .choices
            .iter()
            .map(|(task, file)| (task.to_string(), file.clone()))
            .collect()
    }

    #[tokio::test]
    async fn switching_to_cloud_after_unsaved_first_run_edits_unblocks_setup() {
        let runner = runner().await;
        let mut state = State::default();
        reload(&mut state, &runner).await;
        pick(&mut state, &runner, "read_cv", catalog::GRANITE_4_1_3B);
        assert!(state.dirty);
        assert!(!ready(&state));
        // Reverting a selector should also stop blocking Continue on an otherwise ready plan.
        pick(&mut state, &runner, "read_cv", catalog::QWEN3_4B);
        assert!(!state.dirty);
        pick(&mut state, &runner, "read_cv", catalog::GRANITE_4_1_3B);
        account_model::save(
            runner.context.db(),
            &runner.context.key_vault,
            OPENAI,
            "cloud-model",
            "",
            "sk-test",
        )
        .await
        .unwrap();
        reload(&mut state, &runner).await;
        assert!(state.choices.is_empty());
        assert!(!state.dirty);
        assert!(
            ready(&state),
            "Returning from cloud setup must enable Continue without restarting"
        );
        assert!(
            model_plan::overview(&runner.context)
                .await
                .unwrap()
                .rows
                .iter()
                .all(|r| r.provider == OPENAI && r.current == "cloud-model")
        );
    }

    #[tokio::test]
    async fn mixed_routing_keeps_applicable_edits_and_refreshes_unedited_saved_choices() {
        let runner = runner().await;
        let context = &runner.context;
        let all = task_model::TASKS
            .iter()
            .map(|t| (t.name.into(), catalog::QWEN3_4B.into()))
            .collect::<Vec<_>>();
        model_plan::save_choices(context, &all).await.unwrap();
        let mut state = State::default();
        reload(&mut state, &runner).await;
        pick(&mut state, &runner, "read_job", catalog::GRANITE_4_1_3B);
        pick(&mut state, &runner, "application_form", "Qwen3-1.7B-Q8_0.gguf");
        account_model::save(context.db(), &context.key_vault, OPENAI, "cloud-model", "", "sk-test")
            .await
            .unwrap();
        account_model::save_uses(
            context.db(),
            &account_model::UseChoices {
                resume: Some(LLAMA_CPP.into()),
                application: Some(OPENAI.into()),
                job_evaluation: Some(LLAMA_CPP.into()),
            },
        )
        .await
        .unwrap();
        task_model::save(context.db(), &[(&task_model::READ_CV, catalog::GRANITE_4_1_3B.into())])
            .await
            .unwrap();
        reload(&mut state, &runner).await;
        assert_eq!(state.choices.len(), 4);
        assert!(!state.choices.contains_key("application_form"));
        assert_eq!(
            state.choices["read_job"],
            catalog::GRANITE_4_1_3B,
            "Keep the pending local edit"
        );
        assert_eq!(
            state.choices["read_cv"],
            catalog::GRANITE_4_1_3B,
            "Unedited selectors follow advanced changes"
        );
        assert!(state.dirty);
        model_plan::save_choices(context, &chosen(&state)).await.unwrap();
        // Reloading an externally saved draft also clears dirty state.
        reload(&mut state, &runner).await;
        assert!(!state.dirty);
        let plan = model_plan::overview(context).await.unwrap();
        assert!(
            plan.rows
                .iter()
                .filter(|r| r.task.use_ == account_model::APPLICATION)
                .all(|r| r.provider == OPENAI && r.current == "cloud-model")
        );
    }

    #[tokio::test]
    async fn transient_reload_errors_do_not_discard_pending_local_edits() {
        let runner = runner().await;
        let mut state = State::default();
        reload(&mut state, &runner).await;
        pick(&mut state, &runner, "read_cv", catalog::GRANITE_4_1_3B);
        let _ = update(&mut state, Msg::Loaded(Err("Temporary database error".into())), &runner);
        reload(&mut state, &runner).await;
        assert_eq!(state.choices["read_cv"], catalog::GRANITE_4_1_3B);
        assert!(state.dirty);
    }
}
