//! The app update card in Settings.

use std::sync::mpsc::Sender;

use iced::widget::{button, column, container, text};
use iced::{Element, Fill, Task};

use super::{GAP, Runner, style};
use crate::app_update::{self, Staged, Update};

#[derive(Default)]
pub enum State {
    #[default]
    Idle,
    Checking(Option<Update>),
    Current,
    Available(Update, Option<String>),
    Downloading(Update),
    CheckFailed(String),
}

impl State {
    pub fn has_update(&self) -> bool {
        matches!(
            self,
            Self::Available(..) | Self::Checking(Some(_)) | Self::Downloading(_)
        )
    }
}

#[derive(Clone)]
pub enum Msg {
    Check,
    Checked(Result<Option<Update>, String>),
    Install,
    Downloaded(Result<(), String>),
}

pub fn update(state: &mut State, message: Msg, runner: &Runner, staged: &Sender<Staged>) -> Task<Msg> {
    match message {
        Msg::Check if !matches!(state, State::Checking(_) | State::Downloading(_)) && app_update::supported() => {
            let previous = match state {
                State::Available(update, _) => Some(update.clone()),
                _ => None,
            };
            *state = State::Checking(previous);
            return runner.run(
                |_| app_update::check(),
                |result| Msg::Checked(result.and_then(|result| result)),
            );
        }
        Msg::Checked(result) => {
            if let State::Checking(previous) = state {
                *state = match result {
                    Ok(Some(update)) => State::Available(update, None),
                    Ok(None) => State::Current,
                    Err(why) => match previous.take() {
                        Some(update) => State::Available(update, Some(why)),
                        None => State::CheckFailed(why),
                    },
                };
            }
        }
        Msg::Install => {
            if let State::Available(update, _) = state {
                let update = update.clone();
                *state = State::Downloading(update.clone());
                let staged = staged.clone();
                return runner.run(
                    move |_| async move {
                        let download = app_update::download(update).await?;
                        staged.send(download).map_err(|_| "JobFinder is closing.".to_string())
                    },
                    |result| Msg::Downloaded(result.and_then(|result| result)),
                );
            }
        }
        Msg::Downloaded(result) => {
            if let State::Downloading(update) = state {
                match result {
                    Ok(()) => return iced::exit(),
                    Err(why) => *state = State::Available(update.clone(), Some(why)),
                }
            }
        }
        _ => {}
    }
    Task::none()
}

pub fn view(state: &State) -> Element<'_, Msg> {
    let (status, label, action) = match state {
        State::Idle if !app_update::supported() => (
            "In-app updates are available for the Windows release and the Linux AppImage.".to_string(),
            None,
            None,
        ),
        State::Idle => (
            "Check GitHub for a newer version.".to_string(),
            Some("Check for updates"),
            Some(Msg::Check),
        ),
        State::Checking(_) => ("Checking GitHub…".to_string(), Some("Checking…"), None),
        State::Current => ("You're up to date.".to_string(), Some("Check again"), Some(Msg::Check)),
        State::CheckFailed(why) => (why.clone(), Some("Check again"), Some(Msg::Check)),
        State::Available(update, why) => (
            match why {
                Some(why) => format!("JobFinder {} is available. {why}", update.version),
                None => format!(
                    "JobFinder {} is available. The app will restart after downloading.",
                    update.version
                ),
            },
            Some("Update and restart"),
            Some(Msg::Install),
        ),
        State::Downloading(update) => (
            format!("Downloading and verifying JobFinder {}…", update.version),
            Some("Updating…"),
            None,
        ),
    };
    let mut card = column![
        text(format!("JobFinder {}", app_update::VERSION))
            .font(style::semibold())
            .size(style::BODY_SIZE),
        text(status).size(style::CAPTION_SIZE).style(style::secondary),
    ]
    .spacing(GAP / 2.0);
    if let Some(label) = label {
        let appearance = if matches!(state, State::Available(..)) {
            style::primary_button
        } else {
            style::standard_button
        };
        card = card.push(
            button(text(label).size(style::CAPTION_SIZE))
                .style(appearance)
                .on_press_maybe(action),
        );
    }
    container(card)
        .padding([12.0, 16.0])
        .width(Fill)
        .style(style::card)
        .into()
}
