//! The desktop window (iced): Match, the settings screens (Language models, Local model,
//! Taxonomy, Job searches, Setup, the Jobnet extension), the Task manager and Settings, with a status line for
//! Laya and the app's own model under them. The status line's bell opens a drawer from the
//! bottom with what the app told the user (notification_drawer); its system notifications open
//! their place when clicked. Its screens print what the service layer gives them
//! (dashboard, setup_steps). A link the service layer gives as a path ("setup#cv", "match/12")
//! opens the screen for it (place_for); the server runs beside the window for the Jobnet
//! extension. The mouse's back button goes back to the place before (history), as in a browser.
//!
//! The window runs on the main thread, as the OS's windowing wants; the app's work runs on the
//! tokio runtime the server runs on (on_runtime), since the database and HTTP clients need it.

mod apply;
mod batch;
mod devices_screen;
mod documents;
mod drawings;
mod extension_screen;
mod history;
mod job_searches;
mod language_models;
mod local_model;
mod match_screen;
mod matching;
mod notification_drawer;
mod recordings_card;
mod settings_screen;
mod setup_screen;
mod style;
mod task_manager;
mod taxonomy_screen;
mod widgets;

use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use iced::futures::Stream;
use iced::theme::Mode;
use iced::widget::scrollable::{Direction, Scrollbar};
use iced::widget::{Column, Row, button, column, container, row, scrollable, space, stack, text, tooltip};
use iced::{Center, Element, Fill, Size, Task};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

use crate::context::Context;
use crate::dashboard::{self, Dashboard, Findings, Health, Work};
use crate::game_mode::Playing;
use crate::job_scan::scan_service;
use crate::laya::laya_client::LayaState;
use crate::notifications::notification_model;

/// The window's title.
const TITLE: &str = "JobFinder";
/// The window's size when it opens, in logical pixels.
const WINDOW_SIZE: Size = Size::new(980.0, 720.0);
/// The failure window's size.
const FAILURE_WINDOW_SIZE: Size = Size::new(560.0, 240.0);
/// The smallest the window can be made.
const MIN_WINDOW_SIZE: Size = Size::new(560.0, 420.0);
/// How often the Match screen reads its status again, so a scan or a post the Jobnet extension
/// sent shows without a click.
const RELOAD_EVERY: Duration = Duration::from_secs(30);
/// Space between blocks on a screen.
const GAP: f32 = 12.0;
/// The window's padding.
const PADDING: f32 = 16.0;

/// What the window needs from main: the app's work, the runtime it runs on, and what tells it to
/// come to the front.
pub struct Launch {
    pub context: Arc<Context>,
    pub runtime: Handle,
    /// A second start on this database (instance::front).
    pub front: UnboundedReceiver<()>,
}

/// The window's screens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Screen {
    Match,
    LanguageModels,
    LocalModel,
    JobSearches,
    TaskManager,
    Settings,
    Taxonomy,
    Setup,
    JoblogExtension,
    Devices,
}

/// What the Match screen shows, read together.
#[derive(Clone)]
pub struct Overview {
    pub dashboard: Dashboard,
    pub findings: Findings,
}

/// What a reload read.
#[derive(Clone)]
pub struct Read {
    overview: Result<Overview, String>,
    status: Vec<(Health, String)>,
    /// The bell's count: notifications not read yet (0 when it couldn't be read; logged).
    unread: i64,
}

/// The window's state.
pub struct Shell {
    context: Arc<Context>,
    runtime: Handle,
    screen: Screen,
    /// None until first read; Err says why it couldn't be.
    overview: Option<Result<Overview, String>>,
    /// The status line, read with the overview (it looks in the llama folder, too slow for
    /// every redraw).
    status: Vec<(Health, String)>,
    detected_game: Option<Playing>,
    /// The bell's count, read with the overview.
    unread: i64,
    /// The last thing a click said (Scan now's answer, a page that couldn't open).
    said: Option<String>,
    language_models: language_models::State,
    local_model: local_model::State,
    job_searches: job_searches::State,
    matching: matching::State,
    /// Windows' light or dark mode, which the window follows.
    mode: Mode,
    task_manager: task_manager::State,
    taxonomy: taxonomy_screen::State,
    setup: setup_screen::State,
    extension: extension_screen::State,
    devices: devices_screen::State,
    notifications: notification_drawer::State,
    /// The notification drawer is open over the bottom of the screen.
    drawer_open: bool,
    /// The places left, for the mouse's back button.
    history: history::History<Place>,
}

/// Clone, as iced's buttons need; a reload's answer is shared rather than copied.
#[derive(Clone)]
pub enum Message {
    Show(Screen),
    Reload,
    ToggleGameMode,
    GameDetected(Option<Playing>),
    Loaded(Arc<Read>),
    /// Goes to the place a link names by its path, as the work's links give it (place_for).
    OpenPage(String),
    Do(Work),
    Said(String),
    /// A second start asked for the window.
    Front,
    LanguageModels(language_models::Msg),
    LocalModel(local_model::Msg),
    JobSearches(job_searches::Msg),
    Matching(matching::Msg),
    /// Windows' light or dark mode, read at the start and whenever it changes.
    ThemeMode(Mode),
    TaskManager(task_manager::Msg),
    Taxonomy(taxonomy_screen::Msg),
    Setup(setup_screen::Msg),
    Extension(extension_screen::Msg),
    Devices(devices_screen::Msg),
    Notifications(notification_drawer::Msg),
    /// The bell was clicked: the drawer opens, or closes when open.
    ToggleDrawer,
    /// A system notification was clicked: its link, which the window comes up on.
    NotificationClicked(String),
    /// The mouse's back button: the open drawer closes, else the place before shows.
    Back,
}

/// Opens the window and runs until it is closed.
pub fn run(launch: Launch) -> iced::Result {
    // boot may be called more than once by iced's types, but runs once; the channel's end is
    // taken by that run.
    let launch = Mutex::new(Some(launch));
    let app = iced::application(move || boot(&launch), update, view)
        .title(TITLE)
        .window(window_settings());
    with_fonts(app)
        .default_font(style::font())
        .theme(|shell: &Shell| style::theme(shell.mode))
        .subscription(|_: &Shell| {
            iced::Subscription::batch([
                iced::system::theme_changes().map(Message::ThemeMode),
                iced::event::listen_with(back_button),
            ])
        })
        .run()
}

/// A small window saying why the app couldn't start, for a start without a console to say it
/// in (the window is how a double-clicked JobFinder talks).
pub fn show_failure(why: String) -> iced::Result {
    let app = iced::application(move || Failure(why.clone()), |_: &mut Failure, _: ()| {}, failure_view)
        .title(TITLE)
        .window_size(FAILURE_WINDOW_SIZE);
    with_fonts(app)
        .default_font(style::font())
        .theme(style::theme(Mode::Light))
        .run()
}

/// The application with the fonts built into the program loaded (style::FONTS).
fn with_fonts<A: iced::Program>(app: iced::Application<A>) -> iced::Application<A> {
    style::FONTS.into_iter().fold(app, |app, font| app.font(font))
}

/// Why the app couldn't start, as the failure window's state.
struct Failure(String);

fn failure_view(failure: &Failure) -> Element<'_, ()> {
    let message = column![
        row![
            style::mark(Health::Attention),
            text("JobFinder couldn't start")
                .font(style::semibold())
                .size(style::SECTION_SIZE),
        ]
        .spacing(GAP)
        .align_y(Center),
        text(failure.0.as_str()).size(style::BODY_SIZE),
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

fn window_settings() -> iced::window::Settings {
    iced::window::Settings {
        size: WINDOW_SIZE,
        min_size: Some(MIN_WINDOW_SIZE),
        ..iced::window::Settings::default()
    }
}

fn boot(launch: &Mutex<Option<Launch>>) -> (Shell, Task<Message>) {
    let Launch {
        context,
        runtime,
        front,
    } = launch
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take()
        .expect("the window boots once");
    let ticks = ticks(&runtime);
    let clicks = notification_clicks(&context);
    let detected = context.game_mode.detected();
    let detected_game = detected.borrow().clone();
    let shell = Shell {
        context,
        runtime,
        screen: Screen::Match,
        overview: None,
        status: Vec::new(),
        detected_game,
        said: None,
        language_models: language_models::State::default(),
        local_model: local_model::State::default(),
        job_searches: job_searches::State::default(),
        matching: matching::State::default(),
        mode: Mode::Light,
        task_manager: task_manager::State::default(),
        taxonomy: taxonomy_screen::State::default(),
        setup: setup_screen::State::default(),
        extension: extension_screen::State::default(),
        devices: devices_screen::State::default(),
        notifications: notification_drawer::State::default(),
        unread: 0,
        drawer_open: false,
        history: history::History::new(),
    };
    let tasks = Task::batch([
        Task::done(Message::Reload),
        iced::system::theme().map(Message::ThemeMode),
        Task::run(received(front), |()| Message::Front),
        Task::run(received(ticks), |()| Message::Reload),
        Task::run(received(clicks), Message::NotificationClicked),
        Task::run(
            iced::futures::stream::unfold(detected, |mut detected| async {
                detected.changed().await.ok()?;
                let playing = detected.borrow_and_update().clone();
                Some((playing, detected))
            }),
            Message::GameDetected,
        ),
        first_run(&shell),
    ]);
    (shell, tasks)
}

/// A tick every RELOAD_EVERY, from a timer on the runtime (iced's own executor has no timers).
fn ticks(runtime: &Handle) -> UnboundedReceiver<()> {
    let (sender, receiver) = unbounded_channel();
    runtime.spawn(async move {
        loop {
            tokio::time::sleep(RELOAD_EVERY).await;
            if sender.send(()).is_err() {
                break;
            }
        }
    });
    receiver
}

/// The links of the system notifications clicked while the window is open.
fn notification_clicks(context: &Context) -> UnboundedReceiver<String> {
    let (sender, receiver) = unbounded_channel();
    context.toasts.on_click(Arc::new(move |link| {
        // The window was closed: nobody to open it in.
        let _ = sender.send(link);
    }));
    receiver
}

/// What receiver gets, as a stream for a Task.
fn received<T: Send + 'static>(receiver: UnboundedReceiver<T>) -> impl Stream<Item = T> + Send + 'static {
    iced::futures::stream::unfold(receiver, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    })
}

/// Runs work on the app's runtime and gives its answer to done; a panic in it is said.
fn on_runtime<T: Send + 'static>(
    runtime: &Handle,
    work: impl Future<Output = T> + Send + 'static,
    done: impl Fn(T) -> Message + Send + 'static,
) -> Task<Message> {
    let job = runtime.spawn(work);
    Task::perform(job, move |answer| match answer {
        Ok(answer) => done(answer),
        Err(e) => {
            crate::log_line!("The window's work stopped: {e}");
            Message::Said(format!("That stopped with an error: {e}"))
        }
    })
}

impl Shell {
    fn runner(&self) -> Runner {
        Runner {
            context: Arc::clone(&self.context),
            runtime: self.runtime.clone(),
        }
    }
}

/// Runs a screen's work on the app's runtime, with the app's work (Context).
pub struct Runner {
    context: Arc<Context>,
    runtime: Handle,
}

impl Runner {
    /// Runs work(context) on the runtime and gives its answer to done: Err when the work
    /// panicked (logged). work is called on the runtime too, not on the window's thread, which
    /// has no runtime: a timer or a client made there would panic.
    pub fn run<T, F, M>(
        &self,
        work: impl FnOnce(Arc<Context>) -> F + Send + 'static,
        done: impl Fn(Result<T, String>) -> M + Send + 'static,
    ) -> Task<M>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
        M: Send + 'static,
    {
        let job = spawn_work(&self.runtime, Arc::clone(&self.context), work);
        Task::perform(job, move |answer| {
            done(answer.map_err(|e| {
                crate::log_line!("The window's work stopped: {e}");
                format!("That stopped with an error: {e}")
            }))
        })
    }
}

/// A step of work run with Runner::run_saying: a line it says as it goes, or its answer.
pub enum Step<T> {
    Said(String),
    Done(Result<T, String>),
}

/// What work run with Runner::run_saying says its steps with.
pub type Sayer = Arc<dyn Fn(&str) + Send + Sync>;

impl Runner {
    /// As run, for work that says what it is doing as it goes: each line it says comes as
    /// Step::Said, then its answer as Step::Done.
    pub fn run_saying<T, F, M>(
        &self,
        work: impl FnOnce(Arc<Context>, Sayer) -> F + Send + 'static,
        step: impl Fn(Step<T>) -> M + Send + 'static,
    ) -> Task<M>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
        M: Send + 'static,
    {
        let (sender, receiver) = unbounded_channel();
        let says = sender.clone();
        let sayer: Sayer = Arc::new(move |line: &str| {
            // The window was closed: nobody to say it to.
            let _ = says.send(Step::Said(line.to_string()));
        });
        let job = spawn_work(&self.runtime, Arc::clone(&self.context), move |context| {
            work(context, sayer)
        });
        self.runtime.spawn(async move {
            let done = job.await.map_err(|e| {
                crate::log_line!("The window's work stopped: {e}");
                format!("That stopped with an error: {e}")
            });
            let _ = sender.send(Step::Done(done));
        });
        Task::run(received(receiver), step)
    }
}

/// Calls work(context) on the runtime and runs what it gives there, in the task manager's list
/// (its model calls, say).
fn spawn_work<T, F>(
    runtime: &Handle,
    context: Arc<Context>,
    work: impl FnOnce(Arc<Context>) -> F + Send + 'static,
) -> tokio::task::JoinHandle<T>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    runtime.spawn(async move {
        let running = Arc::clone(&context.running);
        running.within(work(context)).await
    })
}

/// The Match screen's status and saved matches, and the status line.
async fn read(context: Arc<Context>) -> Arc<Read> {
    Arc::new(Read {
        overview: overview(&context).await,
        status: vec![laya_status(&context), local_model_status(&context)],
        unread: unread(&context).await,
    })
}

/// The bell's count; 0 when it can't be read (logged).
async fn unread(context: &Context) -> i64 {
    notification_model::unread(context.db()).await.unwrap_or_else(|e| {
        crate::log_line!("The bell's count couldn't be read: {e}");
        0
    })
}

async fn overview(context: &Context) -> Result<Overview, String> {
    let dashboard = dashboard::dashboard(context).await?;
    let findings = dashboard::findings(context).await?;
    Ok(Overview { dashboard, findings })
}

/// The mouse's back button, pressed anywhere in the window.
fn back_button(event: iced::Event, _: iced::event::Status, _: iced::window::Id) -> Option<Message> {
    let iced::Event::Mouse(iced::mouse::Event::ButtonPressed(iced::mouse::Button::Back)) = event else {
        return None;
    };
    Some(Message::Back)
}

/// Handles a message, noting the place left when it goes somewhere else (for Back).
fn update(shell: &mut Shell, message: Message) -> Task<Message> {
    if let Message::Back = message {
        return back(shell);
    }
    let goes_somewhere = goes_somewhere(&message);
    let before = place_shown(shell);
    let task = handle(shell, message);
    if goes_somewhere && place_shown(shell) != before {
        shell.history.left(before);
    }
    task
}

/// A click that goes to another place: the sidebar, a link, a page of Match or Job searches.
/// What a click's work leads to afterwards (a search deleted goes back to the list) isn't a
/// place to go back to, so only these are noted.
fn goes_somewhere(message: &Message) -> bool {
    matches!(
        message,
        Message::Show(_)
            | Message::OpenPage(_)
            | Message::Matching(matching::Msg::Go(_))
            | Message::JobSearches(job_searches::Msg::Go(_))
    )
}

/// Closes the open drawer, else shows the place before; nothing when there is none.
fn back(shell: &mut Shell) -> Task<Message> {
    if shell.drawer_open {
        shell.drawer_open = false;
        return Task::none();
    }
    // A click's work may have led back to a place left before (a search deleted shows the list
    // again): going back to the place on screen would look like nothing happened.
    let shown = place_shown(shell);
    while let Some(place) = shell.history.back() {
        if place != shown {
            return go_to(shell, place);
        }
    }
    Task::none()
}

/// The place on screen.
fn place_shown(shell: &Shell) -> Place {
    match shell.screen {
        Screen::Match => Place::Match(shell.matching.page),
        Screen::JobSearches => Place::JobSearches(shell.job_searches.page()),
        screen => Place::Screen(screen),
    }
}

fn handle(shell: &mut Shell, message: Message) -> Task<Message> {
    match message {
        Message::ToggleGameMode => {
            let mode = &shell.context.game_mode;
            mode.set_manual(!mode.manual());
        }
        Message::GameDetected(playing) => shell.detected_game = playing,
        Message::Show(screen) => {
            shell.screen = screen;
            shell.said = None;
            shell.task_manager.hide();
            // Each visit reads the screen's data again, so it shows what a scan or the extension
            // changed.
            match screen {
                Screen::LanguageModels => return Task::done(Message::LanguageModels(language_models::Msg::Load)),
                Screen::LocalModel => return Task::done(Message::LocalModel(local_model::Msg::Load)),
                Screen::JobSearches => {
                    shell.job_searches.show_list();
                    return Task::done(Message::JobSearches(job_searches::Msg::Load));
                }
                // The sidebar's Match is the overview, read again.
                Screen::Match => {
                    shell.matching.page = matching::Page::Overview;
                    return Task::done(Message::Reload);
                }
                Screen::TaskManager => {
                    shell.task_manager.show();
                    return Task::done(Message::TaskManager(task_manager::Msg::Load));
                }
                Screen::Taxonomy => return Task::done(Message::Taxonomy(taxonomy_screen::Msg::Load)),
                Screen::Setup => return Task::done(Message::Setup(setup_screen::Msg::Load)),
                Screen::JoblogExtension => extension_screen::shown(&mut shell.extension),
                Screen::Devices => {
                    devices_screen::shown(&mut shell.devices);
                    return Task::done(Message::Devices(devices_screen::Msg::Load));
                }
                Screen::Settings => {}
            }
        }
        Message::ToggleDrawer => {
            shell.drawer_open = !shell.drawer_open;
            if shell.drawer_open {
                // Opening it marks them read.
                shell.unread = 0;
                return Task::done(Message::Notifications(notification_drawer::Msg::Load));
            }
        }
        Message::Notifications(notification_drawer::Msg::Close) => shell.drawer_open = false,
        Message::Notifications(notification_drawer::Msg::Open(link)) => {
            shell.drawer_open = false;
            return Task::done(Message::OpenPage(link));
        }
        Message::Notifications(msg) => {
            let runner = shell.runner();
            return notification_drawer::update(&mut shell.notifications, msg, &runner).map(Message::Notifications);
        }
        Message::NotificationClicked(link) => {
            return Task::done(Message::Front).chain(Task::done(Message::OpenPage(link)));
        }
        Message::TaskManager(msg) => {
            let runner = shell.runner();
            return task_manager::update(&mut shell.task_manager, msg, &runner).map(Message::TaskManager);
        }
        Message::Taxonomy(msg) => {
            let runner = shell.runner();
            return taxonomy_screen::update(&mut shell.taxonomy, msg, &runner).map(Message::Taxonomy);
        }
        Message::Matching(matching::Msg::Changed) => return Task::done(Message::Reload),
        Message::Matching(matching::Msg::ShowLanguageModels) => {
            return Task::done(Message::Show(Screen::LanguageModels));
        }
        Message::Matching(msg) => {
            let runner = shell.runner();
            return matching::update(&mut shell.matching, msg, &runner).map(Message::Matching);
        }
        Message::LanguageModels(language_models::Msg::BackToSetup(message)) => {
            return Task::done(Message::Show(Screen::Setup))
                .chain(Task::done(Message::Setup(setup_screen::Msg::Tell(message))));
        }
        Message::LanguageModels(language_models::Msg::ShowLocalModel) => {
            return Task::done(Message::Show(Screen::LocalModel));
        }
        Message::JobSearches(job_searches::Msg::OpenPage(path)) => {
            return Task::done(Message::OpenPage(path));
        }
        Message::JobSearches(msg) => {
            let runner = shell.runner();
            return job_searches::update(&mut shell.job_searches, msg, &runner).map(Message::JobSearches);
        }
        Message::LocalModel(local_model::Msg::ModelInUse {
            message,
            setup_unfinished,
        }) => {
            // To Setup while it is unfinished, else to where the
            // model's uses are picked, since the uses stay on the key they were on.
            if setup_unfinished {
                return Task::done(Message::Show(Screen::Setup))
                    .chain(Task::done(Message::Setup(setup_screen::Msg::Tell(message))));
            }
            return Task::done(Message::Show(Screen::LanguageModels))
                .chain(Task::done(Message::LanguageModels(language_models::Msg::Tell(message))));
        }
        Message::LocalModel(local_model::Msg::ShowLanguageModels) => {
            return Task::done(Message::Show(Screen::LanguageModels));
        }
        Message::LocalModel(local_model::Msg::BackToSetup) => return Task::done(Message::Show(Screen::Setup)),
        Message::LocalModel(msg) => {
            let runner = shell.runner();
            return local_model::update(&mut shell.local_model, msg, &runner).map(Message::LocalModel);
        }
        Message::LanguageModels(msg) => {
            let runner = shell.runner();
            return language_models::update(&mut shell.language_models, msg, &runner).map(Message::LanguageModels);
        }
        Message::Reload => {
            let reload = on_runtime(&shell.runtime, read(Arc::clone(&shell.context)), Message::Loaded);
            // A device that pairs with the code shown is listed without leaving the screen.
            if shell.screen == Screen::Devices && devices_screen::pairing_shown(&shell.devices) {
                return Task::batch([reload, Task::done(Message::Devices(devices_screen::Msg::LoadDevices))]);
            }
            return reload;
        }
        Message::Loaded(read) => {
            let read = Arc::unwrap_or_clone(read);
            if let Err(why) = &read.overview {
                crate::log_line!("The window's status couldn't be read: {why}");
            }
            shell.overview = Some(read.overview);
            shell.status = read.status;
            // In the open drawer new ones are listed (and so read) as they come.
            if !shell.drawer_open {
                shell.unread = read.unread;
            } else if read.unread > 0 {
                return Task::done(Message::Notifications(notification_drawer::Msg::Load));
            }
        }
        Message::Setup(setup_screen::Msg::ShowLanguageModels) => {
            return Task::done(Message::Show(Screen::LanguageModels));
        }
        Message::Setup(setup_screen::Msg::ShowLocalModel) => return Task::done(Message::Show(Screen::LocalModel)),
        Message::Setup(setup_screen::Msg::Finish) => return Task::done(Message::Show(Screen::Match)),
        Message::Setup(setup_screen::Msg::Later) => return Task::done(Message::Show(Screen::Match)),
        Message::Setup(setup_screen::Msg::ShowSearches) => return Task::done(Message::Show(Screen::JobSearches)),
        Message::Setup(msg) => {
            let runner = shell.runner();
            return setup_screen::update(&mut shell.setup, msg, &runner).map(Message::Setup);
        }
        Message::Extension(msg) => return extension_screen::update(&mut shell.extension, msg).map(Message::Extension),
        Message::Devices(msg) => {
            let runner = shell.runner();
            return devices_screen::update(&mut shell.devices, msg, &runner).map(Message::Devices);
        }
        Message::OpenPage(path) => {
            if let Some(place) = place_for(&path) {
                return go_to(shell, place);
            }
            crate::log_line!("The window has no place for the link {path}.");
            shell.said = Some(format!("The window has no place for the link {path}."));
        }
        Message::Do(Work::ScanNow) => {
            let scan = scan_service::scan_now(Arc::clone(&shell.context));
            return on_runtime(&shell.runtime, scan, |said| {
                Message::Said(said.map_or_else(|e| e.message, str::to_string))
            })
            .chain(Task::done(Message::Reload));
        }
        Message::Said(said) => shell.said = Some(said),
        Message::ThemeMode(mode) => shell.mode = mode,
        // Handled before (update), since it goes to a place without noting one.
        Message::Back => {}
        Message::Front => {
            return iced::window::latest().and_then(|window| {
                // Out of the taskbar first, when it was minimised there.
                iced::window::minimize(window, false).chain(iced::window::gain_focus(window))
            });
        }
    }
    Task::none()
}

/// Opens Setup when the app hasn't been set up yet (setup_steps::is_first_run).
fn first_run(shell: &Shell) -> Task<Message> {
    let context = Arc::clone(&shell.context);
    on_runtime(
        &shell.runtime,
        async move { crate::setup::first_run_or_logged(&context).await },
        |first| Message::Show(if first { Screen::Setup } else { Screen::Match }),
    )
}

/// A place in the window: a screen, and on Match and Job searches, which of their pages.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Place {
    /// A screen without pages of its own.
    Screen(Screen),
    Match(matching::Page),
    JobSearches(job_searches::Page),
}

/// The place for a link: the service layer gives its links as paths ("setup#cv", "match/12"),
/// named as the web pages that were. None for a path the window has no place for.
fn place_for(path: &str) -> Option<Place> {
    let path = path.trim_start_matches('/');
    let page = path.split(['#', '?']).next().unwrap_or_default();
    Some(match page {
        "setup" => Place::Screen(Screen::Setup),
        "account" => Place::Screen(Screen::LanguageModels),
        "llama_setup" => Place::Screen(Screen::LocalModel),
        "joblog_extension" => Place::Screen(Screen::JoblogExtension),
        "devices" => Place::Screen(Screen::Devices),
        "job_scan" => Place::JobSearches(job_searches::Page::List),
        "job_scan/create" => Place::JobSearches(job_searches::Page::Form(0)),
        "" | "match" => Place::Match(matching::Page::Overview),
        _ => Place::Match(matching::Page::Saved(page.strip_prefix("match/")?.parse().ok()?)),
    })
}

/// Opens a link (a job post, a mailto:) in the user's default browser or mail app. When that
/// fails it is logged, and the answer says so for the screen to show.
fn open_in_browser(url: &str) -> Result<(), String> {
    webbrowser::open(url).map_err(|e| {
        crate::log_line!("Couldn't open {url} in the browser: {e}");
        format!("Couldn't open the browser ({e}). Open {url} in it yourself.")
    })
}

/// Goes to a place: its screen as the sidebar shows it, then its page when it isn't the one
/// the screen starts on. Both here, so the window lands on the place in one step.
fn go_to(shell: &mut Shell, place: Place) -> Task<Message> {
    let (screen, page) = match place {
        Place::Screen(screen) => (screen, None),
        Place::Match(matching::Page::Overview) => (Screen::Match, None),
        Place::Match(page) => (Screen::Match, Some(Message::Matching(matching::Msg::Go(page)))),
        Place::JobSearches(job_searches::Page::List) => (Screen::JobSearches, None),
        Place::JobSearches(page) => (
            Screen::JobSearches,
            Some(Message::JobSearches(job_searches::Msg::Go(page))),
        ),
    };
    let shown = handle(shell, Message::Show(screen));
    match page {
        Some(page) => Task::batch([shown, handle(shell, page)]),
        None => shown,
    }
}

fn view(shell: &Shell) -> Element<'_, Message> {
    let screen = match shell.screen {
        Screen::Match if shell.matching.page == matching::Page::Overview => match_screen::view(shell.overview.as_ref()),
        Screen::Match => matching::view(&shell.matching).map(Message::Matching),
        Screen::LanguageModels => language_models::view(&shell.language_models).map(Message::LanguageModels),
        Screen::LocalModel => local_model::view(&shell.local_model).map(Message::LocalModel),
        Screen::JobSearches => job_searches::view(&shell.job_searches).map(Message::JobSearches),
        Screen::TaskManager => task_manager::view(&shell.task_manager).map(Message::TaskManager),
        Screen::Settings => settings_screen::view(&shell.context),
        Screen::Taxonomy => taxonomy_screen::view(&shell.taxonomy).map(Message::Taxonomy),
        Screen::Setup => setup_screen::view(&shell.setup).map(Message::Setup),
        Screen::JoblogExtension => extension_screen::view(&shell.extension, &shell.context).map(Message::Extension),
        Screen::Devices => devices_screen::view(&shell.devices, not_listening(&shell.context)).map(Message::Devices),
    };
    let mut page = Column::new().spacing(GAP).max_width(PAGE_MAX_WIDTH);
    if let Some(said) = &shell.said {
        page = page.push(
            container(text(said.as_str()).size(style::BODY_SIZE))
                .padding([10.0, 14.0])
                .width(Fill)
                .style(style::card),
        );
    }
    let page = page.push(screen);
    let page = scrollable(container(page).padding([PADDING, PADDING * 2.0]).center_x(Fill))
        .direction(Direction::Vertical(
            Scrollbar::new()
                .width(SCROLLBAR_WIDTH)
                .scroller_width(SCROLLBAR_WIDTH)
                .margin(2),
        ))
        .height(Fill);
    // A match's details keep their actions in a footer under the page, in view while it scrolls.
    let page: Element<'_, Message> = match matching_footer(shell) {
        Some(footer) => column![page, footer].into(),
        None => page.into(),
    };
    let game_mode = if shell.context.game_mode.manual() {
        Some(Playing::Manual)
    } else {
        shell.detected_game.clone()
    };
    let page: Element<'_, Message> = if let Some(playing) = game_mode {
        let reason = match playing {
            Playing::Manual => "Enabled manually".to_string(),
            detected => crate::game_mode::capitalized(&detected),
        };
        let banner = container(
            row![
                style::dot(Health::Ready),
                text("Game mode active").font(style::semibold()).size(style::BODY_SIZE),
                text(reason).size(style::BODY_SIZE).style(style::secondary),
            ]
            .spacing(10)
            .align_y(Center),
        )
        .padding([10.0, PADDING * 2.0])
        .width(Fill)
        .style(style::card);
        column![banner, page].into()
    } else {
        page
    };
    // The drawer lies over the bottom of the page, which keeps its place under it.
    let page: Element<'_, Message> = if shell.drawer_open {
        let drawer = notification_drawer::view(&shell.notifications).map(Message::Notifications);
        stack![page, column![space().height(Fill), drawer]].into()
    } else {
        page
    };
    let main = column![page, status_line(&shell.status, shell.drawer_open, shell.unread)];
    container(row![sidebar(shell), main])
        .width(Fill)
        .height(Fill)
        .style(style::page)
        .into()
}

/// The match details' footer (matching::footer), across the page under a hairline, its buttons
/// as wide as the page's cards.
fn matching_footer(shell: &Shell) -> Option<Element<'_, Message>> {
    if shell.screen != Screen::Match {
        return None;
    }
    let actions = matching::footer(&shell.matching)?.map(Message::Matching);
    let bar = container(container(actions).max_width(PAGE_MAX_WIDTH))
        .padding([10.0, PADDING * 2.0])
        .center_x(Fill)
        .style(style::footer_bar);
    Some(column![container(space().height(1)).width(Fill).style(style::divider), bar].into())
}

/// The widest a page's cards get, so lines stay readable on a wide window.
const PAGE_MAX_WIDTH: f32 = 920.0;
/// The scrollbar's width: thin, as Windows 11's.
const SCROLLBAR_WIDTH: f32 = 6.0;
/// The sidebar's width.
const SIDEBAR_WIDTH: f32 = 200.0;
/// The accent strip's size beside the selected sidebar item.
const STRIP_SIZE: Size = Size::new(3.0, 16.0);

/// The app's name and its screens, as Windows 11's Settings has them.
fn sidebar(shell: &Shell) -> Element<'static, Message> {
    let current = shell.screen;
    column![
        container(text(TITLE).font(style::title_font()).size(style::SECTION_SIZE)).padding([8.0, 12.0]),
        nav_item("Match", Screen::Match, current),
        nav_item("Language models", Screen::LanguageModels, current),
        nav_item("Models", Screen::LocalModel, current),
        nav_item("Taxonomy", Screen::Taxonomy, current),
        nav_item("Job searches", Screen::JobSearches, current),
        nav_item("Task manager", Screen::TaskManager, current),
        nav_item("Setup", Screen::Setup, current),
        nav_item("Settings", Screen::Settings, current),
        space().height(Fill),
        game_mode_button(shell.context.game_mode.manual(), shell.detected_game.is_some()),
    ]
    .spacing(4)
    .padding(PADDING / 2.0)
    .width(SIDEBAR_WIDTH)
    .into()
}

fn game_mode_button(manual: bool, detected: bool) -> Element<'static, Message> {
    let (label, hint) = match (manual, detected) {
        (true, _) => ("End manual game mode", "Return to automatic game detection."),
        (false, true) => (
            "Keep game mode on",
            "Game mode is on automatically. Click to keep it on after the game closes.",
        ),
        (false, false) => (
            "Enable game mode",
            "Free GPU memory when JobFinder’s local model is idle. Job scans use a saved cloud key or wait until game mode ends.",
        ),
    };
    let control = button(text(label).size(style::BODY_SIZE))
        .width(Fill)
        .padding(8)
        .style(style::game_mode_button(manual || detected))
        .on_press(Message::ToggleGameMode);
    tooltip(
        control,
        text(hint).size(style::CAPTION_SIZE).width(260),
        tooltip::Position::Right,
    )
    .gap(8)
    .padding(10)
    .style(style::card)
    .into()
}

/// The bell, with how many notifications aren't read yet on a badge; it opens the drawer, and
/// shows as pressed while it is open.
fn bell(selected: bool, unread: i64) -> Element<'static, Message> {
    let glyph = text(style::BELL).font(style::icon_font()).size(style::SECTION_SIZE);
    let mut bell = row![glyph].spacing(4).align_y(Center);
    if unread > 0 {
        let count = if unread > MOST_UNREAD_SHOWN {
            format!("{MOST_UNREAD_SHOWN}+")
        } else {
            unread.to_string()
        };
        bell = bell.push(
            container(text(count).font(style::semibold()).size(style::CAPTION_SIZE))
                .padding([0.0, 6.0])
                .style(style::badge),
        );
    }
    button(bell)
        .padding([4.0, 8.0])
        .style(style::subtle_button(selected))
        .on_press(Message::ToggleDrawer)
        .into()
}

/// The highest count the bell's badge shows; past it, "99+".
const MOST_UNREAD_SHOWN: i64 = 99;

fn nav_item(label: &'static str, screen: Screen, current: Screen) -> Element<'static, Message> {
    let selected = screen == current || (screen == Screen::Settings && opened_from_settings(current));
    let strip: Element<'static, Message> = if selected {
        container(space())
            .width(STRIP_SIZE.width)
            .height(STRIP_SIZE.height)
            .style(style::selected_strip)
            .into()
    } else {
        space().width(STRIP_SIZE.width).into()
    };
    let label = text(label)
        .size(style::BODY_SIZE)
        .font(if selected { style::semibold() } else { style::font() });
    button(row![strip, label].spacing(10).align_y(Center))
        .padding([8.0, 4.0])
        .width(Fill)
        .style(style::subtle_button(selected))
        .on_press(Message::Show(screen))
        .into()
}

/// The screens without a sidebar entry, opened from Settings, which stays marked for them.
fn opened_from_settings(screen: Screen) -> bool {
    matches!(screen, Screen::JoblogExtension | Screen::Devices)
}

/// Why the device server isn't listening, when it couldn't start.
fn not_listening(context: &Context) -> Option<String> {
    context
        .devices
        .not_listening
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

/// Laya and the app's own model, as they stood at the last reload: a dot and a word each; the
/// bell at the right end.
fn status_line(status: &[(Health, String)], drawer_open: bool, unread: i64) -> Element<'_, Message> {
    let parts = status.iter().map(|(health, part)| {
        row![
            style::dot(*health),
            text(part.as_str()).size(style::CAPTION_SIZE).style(style::secondary)
        ]
        .spacing(6)
        .align_y(Center)
        .into()
    });
    let line = row![
        Row::with_children(parts).spacing(GAP * 2.0).width(Fill),
        bell(drawer_open, unread)
    ]
    .align_y(Center);
    container(line)
        .padding(iced::Padding::from([4.0, PADDING * 2.0]).right(PADDING))
        .into()
}

fn laya_status(context: &Context) -> (Health, String) {
    match context.laya.as_ref().map(|laya| laya.state()) {
        None => (Health::Neutral, "Laya: no model".to_string()),
        Some(LayaState::Unloaded) => (Health::Ready, "Laya: ready, loads for a match".to_string()),
        Some(LayaState::Loading) => (Health::Neutral, "Laya: loading".to_string()),
        Some(LayaState::Loaded) => (Health::Ready, "Laya: ready".to_string()),
        Some(LayaState::Failed(_)) => (Health::Attention, "Laya: couldn't load (see the log)".to_string()),
    }
}

fn local_model_status(context: &Context) -> (Health, String) {
    let Some(llama) = &context.llama else {
        return (Health::Neutral, "Local model: none".to_string());
    };
    if let Some(model) = llama.running_model() {
        return (Health::Ready, format!("Local model: {model}, running"));
    }
    if llama.server_path().is_none() {
        return (Health::Neutral, "Local model: not set up".to_string());
    }
    match llama.overview().chosen {
        Ok(model) => (Health::Ready, format!("Local model: {model}, starts when used")),
        Err(_) => (Health::Attention, "Local model: no model fits".to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account::key_vault::KeyVault;

    #[test]
    fn a_web_pages_link_opens_its_place_in_the_window() {
        assert_eq!(place_for("setup#cv"), Some(Place::Screen(Screen::Setup)));
        assert_eq!(place_for("/account"), Some(Place::Screen(Screen::LanguageModels)));
        assert_eq!(
            place_for("job_scan/create"),
            Some(Place::JobSearches(job_searches::Page::Form(0)))
        );
        assert_eq!(place_for("match/12"), Some(Place::Match(matching::Page::Saved(12))));
        assert_eq!(place_for("match"), Some(Place::Match(matching::Page::Overview)));
        assert_eq!(place_for("match/history"), None);
        assert_eq!(place_for("elsewhere"), None);
    }

    /// A window's state as boot makes it, on an empty database, and the runtime it runs on.
    fn shell_for_test() -> (Shell, tokio::runtime::Runtime) {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let context = runtime.block_on(async {
            let db = crate::db::open_in_memory().await.unwrap();
            let taxonomy = Arc::new(crate::taxonomy::Taxonomy::load());
            Arc::new(Context::new(db, taxonomy, KeyVault::with_key_in_memory()))
        });
        let (_front, front) = unbounded_channel();
        let launch = Mutex::new(Some(Launch {
            context,
            runtime: runtime.handle().clone(),
            front,
        }));
        let (shell, _) = boot(&launch);
        (shell, runtime)
    }

    #[test]
    fn the_back_button_goes_back_through_the_places_shown() {
        let (mut shell, _runtime) = shell_for_test();
        let _ = update(&mut shell, Message::Show(Screen::LanguageModels));
        let _ = update(&mut shell, Message::OpenPage("match/12".to_string()));
        let _ = update(&mut shell, Message::Show(Screen::Devices));

        let _ = update(&mut shell, Message::Back);
        assert_eq!(place_shown(&shell), Place::Match(matching::Page::Saved(12)));
        let _ = update(&mut shell, Message::Back);
        assert_eq!(place_shown(&shell), Place::Screen(Screen::LanguageModels));
        let _ = update(&mut shell, Message::Back);
        assert_eq!(place_shown(&shell), Place::Match(matching::Page::Overview));
        // Nothing before the first place: it stays.
        let _ = update(&mut shell, Message::Back);
        assert_eq!(place_shown(&shell), Place::Match(matching::Page::Overview));
    }

    #[test]
    fn the_back_button_closes_the_open_drawer_first() {
        let (mut shell, _runtime) = shell_for_test();
        let _ = update(&mut shell, Message::Show(Screen::Settings));
        let _ = update(&mut shell, Message::ToggleDrawer);
        let _ = update(&mut shell, Message::Back);
        assert!(!shell.drawer_open);
        assert_eq!(place_shown(&shell), Place::Screen(Screen::Settings));
    }

    #[test]
    fn a_page_a_clicks_work_leads_to_is_not_a_place_to_go_back_to() {
        let (mut shell, _runtime) = shell_for_test();
        let _ = update(&mut shell, Message::Show(Screen::JobSearches));
        let _ = update(
            &mut shell,
            Message::JobSearches(job_searches::Msg::Go(job_searches::Page::Search(3))),
        );
        // A search deleted: the list again, and Back doesn't go to the search that is gone.
        let _ = update(
            &mut shell,
            Message::JobSearches(job_searches::Msg::Said {
                said: Ok("Deleted.".to_string()),
                then: job_searches::Page::List,
            }),
        );
        let _ = update(&mut shell, Message::Back);
        assert_eq!(place_shown(&shell), Place::Match(matching::Page::Overview));
    }

    #[test]
    fn a_screens_work_may_make_a_timer_from_a_thread_without_the_runtime() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let context = runtime.block_on(async {
            let db = crate::db::open_in_memory().await.unwrap();
            let taxonomy = Arc::new(crate::taxonomy::Taxonomy::load());
            Arc::new(Context::new(db, taxonomy, KeyVault::with_key_in_memory()))
        });
        let handle = runtime.handle().clone();
        // The window's thread: no runtime entered, as in run_window.
        let job =
            std::thread::spawn(move || spawn_work(&handle, context, |_| tokio::time::sleep(Duration::from_millis(1))))
                .join()
                .unwrap();
        runtime.block_on(job).unwrap();
    }
}
