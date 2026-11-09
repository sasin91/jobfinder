//! The Local model screen: setting up the app's own llama.cpp model in the window. This computer,
//! llama-server, the download running with its progress, and the models to download or use,
//! all through llama_setup_service.

use std::sync::Arc;
use std::time::Duration;

use iced::widget::{Column, button, column, container, progress_bar, row, text};
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
}

#[derive(Clone)]
pub enum Msg {
    Load,
    Loaded(Result<Arc<LlamaSetupView>, String>),
    Evaluate,
    Prepare,
    CheckTask(&'static str),
    ApplyTask(&'static str),
    ShowLanguageModels,
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
        Msg::Prepare => {
            state.busy = true;
            state.said = Some((
                Health::Neutral,
                "Preparing the local task plan… Progress and cancellation are available in Task manager.".into(),
            ));
            return Task::batch([
                runner.run(
                    |context| async move {
                        let work = Arc::clone(&context.running);
                        work.run(
                            crate::task_manager::running_work::WorkKind::Download,
                            "Prepare task models",
                            model_plan::prepare(context),
                        )
                        .await
                        .unwrap_or_else(|_| {
                            Err(crate::service_error::ServiceError::failed(
                                "models",
                                "Preparation stopped. Downloaded files are kept; try again to resume.",
                            ))
                        })
                    },
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
        Msg::ApplyTask(task) => {
            state.busy = true;
            return runner.run(
                move |context| async move { model_plan::apply(&context, task).await },
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
            return runner.run(
                move |context| async move { llama_setup_service::use_model(&context, file).await },
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
        Msg::ModelInUse { .. } | Msg::ShowLanguageModels | Msg::BackToSetup => {}
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
        screen = screen.push(plan_card(plan, busy));
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

fn plan_card(plan: &model_plan::Overview, busy: bool) -> Element<'_, Msg> {
    let evaluated = plan
        .assessment
        .as_ref()
        .and_then(|a| chrono::DateTime::from_timestamp(a.at, 0))
        .map(|at| format!("Last evaluated: {}", at.format("%e %B %Y, %H:%M UTC")))
        .unwrap_or_else(|| "Not evaluated on this computer yet.".to_string());
    let mut body = Column::new().spacing(12)
        .push(widgets::caption(evaluated))
        .push(widgets::note("Changed your hardware or want to compare models? Evaluate again. This refreshes hardware and fit estimates; it keeps your choices and downloads nothing."))
        .push(row![
            button(text("Evaluate again").size(style::BODY_SIZE)).style(style::primary_button).on_press_maybe((!busy).then_some(Msg::Evaluate)),
            button(text("Providers and advanced choices").size(style::BODY_SIZE)).style(style::standard_button).on_press(Msg::ShowLanguageModels),
        ].spacing(8));
    body = body.push(widgets::caption(format!("Remaining catalog downloads in the local plan: {} total, shared models counted once. Resumable partial files are credited; changed remote files may need restarting. Runtime and temporary extraction space are additional. Existing providers and manual task choices are preserved.", crate::llama::hardware::gigabytes(plan.download_bytes))))
        .push(button(text("Download and prepare local plan").size(style::BODY_SIZE)).style(style::primary_button).on_press_maybe((!busy).then_some(Msg::Prepare)));
    for task in &plan.rows {
        let current = if task.provider.is_empty() {
            "Not configured".to_string()
        } else {
            format!(
                "{} / {}{}",
                crate::account::account_model::provider_label(&task.provider),
                task.current,
                if task.manual {
                    " (chosen explicitly)"
                } else {
                    " (default)"
                }
            )
        };
        let mut card = column![
            text(task.task.label).font(style::semibold()).size(style::BODY_SIZE),
            text(current).size(style::BODY_SIZE),
            widgets::caption(task.why),
        ]
        .spacing(6);
        if let Some(file) = task.suggested {
            let size = crate::llama::catalog::find(file)
                .map(|m| crate::llama::hardware::gigabytes(m.bytes))
                .unwrap_or_default();
            card = card.push(widgets::caption(format!(
                "Local alternative: {file} · {size} · Estimated: {}",
                task.fit
            )));
        }
        if task.provider == crate::llm::LLAMA_CPP {
            card = card.push(widgets::caption(if task.downloaded {
                "Assigned file is downloaded."
            } else {
                "Assigned file needs downloading. Prepare the plan above."
            }));
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
            } else {
                card = card.push(widgets::caption(
                    "Not checked in the current assessment. Fit estimates do not establish speed or quality.",
                ));
            }
            let mut actions = row![
                button(text("Run local check").size(style::BODY_SIZE))
                    .style(style::standard_button)
                    .on_press_maybe((!busy && task.downloaded).then_some(Msg::CheckTask(task.task.name)))
            ]
            .spacing(8);
            if task.suggested.is_some_and(|f| f != task.current) {
                actions = actions.push(
                    button(text("Use local alternative").size(style::BODY_SIZE))
                        .style(style::standard_button)
                        .on_press_maybe((!busy).then_some(Msg::ApplyTask(task.task.name))),
                );
            }
            card = card.push(actions);
        }
        body = body.push(widgets::card(container(card).padding(CARD_PADDING).width(Fill).into()));
    }

    widgets::section(
        "Your task models",
        container(body).padding(CARD_PADDING).width(Fill).into(),
    )
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
