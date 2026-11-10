//! The desktop window shows the splash until the app's services are ready.

use std::cell::Cell;
use std::process::ExitCode;
use std::rc::Rc;
use std::sync::mpsc::Sender;

use iced::theme::Mode;
use iced::widget::{column, container, row, text};
use iced::{Center, Element, Fill, Size, Subscription, Task};
use tokio::task::JoinHandle;

use crate::app_update::Staged;
use crate::dashboard::Health;

use super::{GAP, Launch, PADDING, Shell, TITLE, back_button, boot, drawings, style, window_settings, with_fonts};

enum Phase {
    Starting,
    Ready(Box<Shell>),
    Failed(String),
}

struct Window {
    phase: Phase,
    mode: Mode,
}

enum Message {
    Started(Result<Launch, String>),
    ThemeMode(Mode),
    Shell(super::Message),
}

/// Opens the splash and switches to the app in the same window once startup finishes.
pub fn run(startup: JoinHandle<Result<Launch, String>>, staged: Sender<Staged>) -> Result<ExitCode, iced::Error> {
    let startup = Cell::new(Some(startup));
    let failed = Rc::new(Cell::new(false));
    let startup_failed = Rc::clone(&failed);
    let app = iced::application(
        move || {
            let startup = startup.take().expect("the window boots once");
            let window = Window {
                phase: Phase::Starting,
                mode: Mode::Light,
            };
            let load = Task::perform(
                async move {
                    startup
                        .await
                        .map_err(|e| format!("JobFinder's startup task stopped: {e}"))?
                },
                Message::Started,
            );
            (
                window,
                Task::batch([load, iced::system::theme().map(Message::ThemeMode)]),
            )
        },
        move |window: &mut Window, message| update(window, message, &startup_failed, &staged),
        view,
    )
    .title(TITLE)
    .window(window_settings());
    with_fonts(app)
        .default_font(style::font())
        .theme(|window: &Window| style::theme(window.mode))
        .subscription(|_: &Window| {
            Subscription::batch([
                iced::system::theme_changes().map(Message::ThemeMode),
                iced::event::listen_with(back_button).map(Message::Shell),
            ])
        })
        .run()?;
    Ok(if failed.get() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn update(window: &mut Window, message: Message, failed: &Cell<bool>, staged: &Sender<Staged>) -> Task<Message> {
    match message {
        Message::Started(Ok(launch)) => {
            let (shell, tasks) = boot(launch, staged.clone());
            window.phase = Phase::Ready(Box::new(shell));
            return tasks.map(Message::Shell);
        }
        Message::Started(Err(why)) => {
            crate::log_line!("{why}");
            failed.set(true);
            window.phase = Phase::Failed(why);
        }
        Message::ThemeMode(mode) => window.mode = mode,
        Message::Shell(message) => {
            if let Phase::Ready(shell) = &mut window.phase {
                return super::update(shell, message).map(Message::Shell);
            }
        }
    }
    Task::none()
}

fn view(window: &Window) -> Element<'_, Message> {
    match &window.phase {
        Phase::Starting => {
            let content = column![
                drawings::drawing(&drawings::HERO),
                text(TITLE).font(style::title_font()).size(style::TITLE_SIZE),
                text("Getting things ready…")
                    .size(style::BODY_SIZE)
                    .style(style::secondary),
            ]
            .spacing(GAP)
            .align_x(Center);
            container(content).center(Fill).style(style::page).into()
        }
        Phase::Ready(shell) => super::view(shell).map(Message::Shell),
        Phase::Failed(why) => failure_view(why),
    }
}

/// Shows an error found before the splash opens, including when there is no console.
pub fn show_failure(why: String) -> iced::Result {
    struct Failure(String);
    fn view(failure: &Failure) -> Element<'_, ()> {
        failure_view(&failure.0)
    }

    let app = iced::application(move || Failure(why.clone()), |_: &mut Failure, _: ()| {}, view)
        .title(TITLE)
        .window_size(Size::new(560.0, 240.0));
    with_fonts(app)
        .default_font(style::font())
        .theme(style::theme(Mode::Light))
        .run()
}

fn failure_view<Message: 'static>(why: &str) -> Element<'_, Message> {
    let message = column![
        row![
            style::mark(Health::Attention),
            text("JobFinder couldn't start")
                .font(style::semibold())
                .size(style::SECTION_SIZE),
        ]
        .spacing(GAP)
        .align_y(Center),
        text(why).size(style::BODY_SIZE),
        text("This is in the log too.")
            .size(style::CAPTION_SIZE)
            .style(style::secondary),
    ]
    .spacing(GAP);
    container(container(message).padding(PADDING * 1.5).width(Fill).style(style::card))
        .padding(PADDING)
        .width(Fill)
        .height(Fill)
        .style(style::page)
        .into()
}
