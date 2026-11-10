//! jobfinder: the desktop app's command line.
//!
//!   jobfinder [--port 8765] [--database path\to\jobfinder.sqlite] [--laya path\to\laya]
//!             [--llama path\to\llama]
//!       opens the app's window (desktop/) and serves what the Jobnet extension asks for on
//!       127.0.0.1 beside it (server); with Laya's model from the laya folder next to the
//!       database unless --laya names another; with the app's own llama.cpp model (llama-server
//!       and models\*.gguf) from the llama folder next to the database unless --llama names
//!       another
//!
//!   jobfinder scan [--database path\to\jobfinder.sqlite]
//!       the job scan, which the setup wizard's Task Scheduler task runs (scan_schedule): every
//!       active job search is searched for new posts, which are matched with the profile
//!       (job_scan::scan_service, the same scan as the dashboard's Scan now)
//!
//!   jobfinder cleanup [--database path\to\jobfinder.sqlite]
//!       removes what the app keeps outside its own file, after saying what it found and asking:
//!       the job scan's schedule, the API keys, and the downloaded and written folders next to the
//!       database; then, asked again, the database itself (cleanup.rs)
//!
//! Nothing is installed: double-click jobfinder.exe (or run the AppImage on Linux) wherever it is
//! kept.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use tokio::runtime::Handle;

use jobfinder::account::former_keys::FormerKeys;
use jobfinder::account::key_vault::KeyVault;
use jobfinder::app_log::{self, Opened};
use jobfinder::context::{Context, DatabaseFile};
use jobfinder::devices::{device_model, device_server, tailscale};
use jobfinder::from_recruiter::{self, Moved};
use jobfinder::instance::{self, Running};
use jobfinder::job_scan::scan_service;
use jobfinder::job_sites::signed_in::{self, SignedInBrowser};
use jobfinder::laya::laya_client::{self, LayaClient};
use jobfinder::llama::{self, hardware::Inventory, llama_server::LlamaServer};
use jobfinder::notifications::{Toasts, notification_model};
use jobfinder::recordings;
use jobfinder::scan_schedule::{self, ScanSchedule};
use jobfinder::server::{self, AppState};
use jobfinder::taxonomy::Taxonomy;
use jobfinder::taxonomy_store;
use jobfinder::whisper::{self, Whisper};
use jobfinder::{cleanup, db, desktop, extension};

/// Options of the web pages, which are gone: a shortcut or login task that still gives one starts
/// the window anyway, and the log says the option is ignored.
const GONE_FLAGS: [&str; 3] = ["--browser", "--no-browser", "--dev"];
/// The same, with a value after it.
const GONE_FLAG_WITH_VALUE: &str = "--tailscale-name";

const USAGE: &str = "usage: jobfinder [--port <port>] [--database <file>] [--laya <folder>] [--llama <folder>]
       jobfinder scan [--database <file>]
       jobfinder cleanup [--database <file>]
       jobfinder read-pdf   (a PDF on stdin, its text on stdout; the app runs it)

JobFinder uses one local SQLite database. --database overrides the saved location for this run.";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some("scan") => run_scan(&args[1..]),
        Some("cleanup") => run_cleanup(&args[1..]),
        Some(jobfinder::pdf_text::child::SUBCOMMAND) => jobfinder::pdf_text::child::run(),
        _ => run_server(&args),
    }
}

/// The database a subcommand's `[--database <file>]` names, or the saved location; None (with the
/// usage said) for anything else.
fn database_arg(args: &[String]) -> Option<PathBuf> {
    match args {
        [] => match saved_database() {
            Ok(saved) => Some(saved.unwrap_or_else(db::default_path)),
            Err(why) => {
                jobfinder::log_line!("{why}");
                None
            }
        },
        [flag, value] if flag == "--database" => Some(PathBuf::from(value)),
        _ => {
            jobfinder::log_line!("{USAGE}");
            None
        }
    }
}

fn database_setting() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(db::users_data_folder)
        .join(db::FOLDER)
        .join("database-location.json")
}

fn saved_database() -> Result<Option<PathBuf>, String> {
    let setting = database_setting();
    match std::fs::read(&setting) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("Couldn't read the database location in {}: {e}", setting.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Couldn't read {}: {e}", setting.display())),
    }
}

fn remember_database(database: &Path) -> Result<(), String> {
    let setting = database_setting();
    let bytes = serde_json::to_vec(database).map_err(|e| e.to_string())?;
    let folder = setting.parent().expect("the setting is in the JobFinder config folder");
    let temporary = setting.with_extension(format!("{}.tmp", std::process::id()));
    let result = std::fs::create_dir_all(folder)
        .and_then(|()| std::fs::write(&temporary, bytes))
        .and_then(|()| std::fs::rename(&temporary, &setting));
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(|e| format!("Couldn't save the database location in {}: {e}", setting.display()))
}

fn choose_database(default: &Path) -> Option<PathBuf> {
    use rfd::{MessageButtons, MessageDialog, MessageDialogResult};

    loop {
        let answer = MessageDialog::new()
            .set_title("JobFinder setup")
            .set_description(format!(
                "JobFinder will keep your database and app data in {}.\n\nWould you like to choose a different folder?",
                default.parent().unwrap_or(default).display(),
            ))
            .set_buttons(MessageButtons::YesNoCancel)
            .show();
        match answer {
            MessageDialogResult::Yes => {
                if let Some(folder) = rfd::FileDialog::new()
                    .set_title("Choose a folder for your JobFinder database and app data")
                    .pick_folder()
                {
                    return Some(folder.join(db::FILE));
                }
            }
            MessageDialogResult::No => return Some(default.to_path_buf()),
            _ => return None,
        }
    }
}

/// code, after waiting for Enter when this program has its console to itself: double-clicked in
/// Explorer, whose window would otherwise close before what it said could be read.
fn ended_by_hand(code: ExitCode) -> ExitCode {
    if own_console() {
        println!("Press Enter to close this window.");
        let _ = std::io::stdin().read_line(&mut String::new());
    }
    code
}

/// Whether this program is the only one on its console, so the console closes when it ends.
#[cfg(windows)]
fn own_console() -> bool {
    let mut processes = [0_u32; 2];
    // SAFETY: the list is a valid buffer of the length passed with it.
    let count = unsafe { windows::Win32::System::Console::GetConsoleProcessList(&mut processes) };
    count == 1
}

#[cfg(not(windows))]
fn own_console() -> bool {
    false
}

/// Closes the console when this program has it to itself (double-clicked in Explorer), and
/// points stdout and stderr nowhere, so nothing writes to a console that is gone.
#[cfg(windows)]
fn hide_own_console() {
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::Console::{FreeConsole, STD_ERROR_HANDLE, STD_OUTPUT_HANDLE, SetStdHandle};
    if !own_console() {
        return;
    }
    // SAFETY: plain calls with no pointers; a null handle is how Windows says "no stdout".
    unsafe {
        let _ = FreeConsole();
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, HANDLE::default());
        let _ = SetStdHandle(STD_ERROR_HANDLE, HANDLE::default());
    }
}

/// Only Windows opens a console for a double-clicked program.
#[cfg(not(windows))]
fn hide_own_console() {}

/// `jobfinder scan`: the job scan (job_scan::scan_service::scan), the same scan as the
/// dashboard's Scan now. It fails (which Task Scheduler shows as the last run result) when it
/// couldn't scan at all; a search or post that failed is said and kept with it.
fn run_scan(args: &[String]) -> ExitCode {
    let Some(database) = database_arg(args) else {
        return ExitCode::FAILURE;
    };
    let moved = from_recruiter::move_data_folder(&database);
    open_log(database.parent().unwrap_or(std::path::Path::new(".")));
    if let Err(why) = said_move(moved) {
        jobfinder::log_line!("{why}");
        return ExitCode::FAILURE;
    }
    // Held while it runs, so `jobfinder cleanup` won't remove the database under it.
    let Ok(_held) = cleanup::hold(&database, cleanup::Hold::Running) else {
        jobfinder::log_line!(
            "JobFinder's cleanup is running on {}, so nothing was scanned.",
            database.display()
        );
        return ExitCode::FAILURE;
    };
    let Some(runtime) = runtime() else {
        return ExitCode::FAILURE;
    };
    let context = match app_context(runtime.handle(), &database, None, None) {
        Ok(context) => context,
        Err(why) => {
            jobfinder::log_line!("{why}");
            return ExitCode::FAILURE;
        }
    };
    let started = runtime.block_on(scheduled_or_by_hand(&context));
    match runtime.block_on(scan_service::scan(Arc::new(context), started)) {
        Ok(report) => {
            jobfinder::say!("{}", report.summary());
            ExitCode::SUCCESS
        }
        Err(e) => {
            jobfinder::log_line!("The job scan couldn't run: {}", e.message);
            ExitCode::FAILURE
        }
    }
}

/// Who `jobfinder scan` runs for: the schedule while its task is registered (the task runs it),
/// so a search scanned by hand since the schedule's last scan waits for its next; else by hand.
async fn scheduled_or_by_hand(context: &Context) -> scan_service::Started {
    match context.scan_schedule.interval_hours().await {
        Ok(Some(every_hours)) => scan_service::Started::BySchedule { every_hours },
        Ok(None) => scan_service::Started::ByHand,
        Err(why) => {
            jobfinder::log_line!(
                "The job scan's schedule couldn't be read ({why}), so searches scanned within the hour are skipped."
            );
            scan_service::Started::ByHand
        }
    }
}

/// `jobfinder cleanup`: says what the app left outside its file, and removes it once the user
/// says yes; the database only after a second yes.
fn run_cleanup(args: &[String]) -> ExitCode {
    let Some(database) = database_arg(args) else {
        return ExitCode::FAILURE;
    };
    if let Err(why) = said_move(from_recruiter::move_data_folder(&database)) {
        jobfinder::log_line!("{why}");
        return ended_by_hand(ExitCode::FAILURE);
    }
    // Held until the database's files are removed, so no JobFinder starts on it meanwhile.
    let Ok(held) = cleanup::hold(&database, cleanup::Hold::Cleanup) else {
        jobfinder::log_line!(
            "JobFinder is running on {}. Close its window, then run the cleanup again.",
            database.display()
        );
        return ended_by_hand(ExitCode::FAILURE);
    };
    let Some(runtime) = runtime() else {
        return ExitCode::FAILURE;
    };
    let former_user_id = runtime.block_on(former_user_id_in(&database));
    let schedule = scan_schedule(&database);
    runtime.block_on(take_over_former_schedule(&database, &schedule));
    let credentials = cleanup::Credentials::LookIn { former_user_id };
    let found = runtime.block_on(cleanup::find(&database, &schedule, credentials));

    println!("JobFinder's cleanup, for {}:", database.display());
    match (found.schedule, &found.schedule_error) {
        (Some(hours), _) => println!("- the job scan's schedule (every {hours} hours)"),
        (None, Some(why)) => println!("  (the job scan's schedule couldn't be read: {why})"),
        (None, None) => {}
    }
    for credential in &found.left_in_credentials {
        println!(
            "- {}, left in {} from before",
            cleanup::left_name(&credential.secret, &found.devices),
            SYSTEM_STORE
        );
    }
    if let Some(why) = &found.credentials_error {
        println!("  ({SYSTEM_STORE} couldn't be read: {why})");
    }
    if let Some(why) = &found.devices_error {
        println!("  (the paired devices couldn't be read: {why})");
    }
    for folder in &found.folders {
        println!("- {}", folder.display());
    }
    if found.notification_name {
        println!("- JobFinder's name for Windows notifications, in your registry");
    }
    let mut failed = Vec::new();
    if !found.anything_besides_the_database() {
        println!("No schedule, leftover keys or downloads to remove.");
    } else if ask("Remove these?") {
        failed.extend(runtime.block_on(cleanup::remove_besides_the_database(&found, &schedule)));
    } else {
        println!("Nothing was removed.");
        return ended_by_hand(ExitCode::SUCCESS);
    }
    if !database.exists() {
        // Only the lock this cleanup took is left.
        // Removed while still held, so an app that starts now locks a new file, never one that
        // is gone.
        let _ = std::fs::remove_file(&cleanup::lock_files(&database)[0]);
        drop(held);
    } else {
        println!(
            "Your database, {}, holds your profile, matches, applications, API keys and paired devices, and can't be brought back once deleted.",
            database.display()
        );
        if ask("Delete your database too?") {
            // The locks are held until the database's files are gone, so no app opens them
            // meanwhile, and their files are removed while still held, so an app that starts after
            // locks new files rather than ones that are gone (Windows allows this: std opens files
            // shared for deletion).
            let lock_files = cleanup::lock_files(&database);
            failed.extend(cleanup::remove_database(&found));
            for lock_file in lock_files {
                let _ = std::fs::remove_file(lock_file);
            }
            drop(held);
            let default_path = db::default_path();
            let default_folder = default_path.parent().unwrap_or(std::path::Path::new("."));
            if failed.is_empty() && cleanup::remove_default_folder(&database, default_folder) {
                println!("Removed the empty data folder too.");
            }
        } else {
            println!("Your database stays.");
        }
    }
    println!("The Jobnet extension, if you loaded it, is removed in Edge's extensions page (edge://extensions).");
    if failed.is_empty() {
        println!("Done. To remove JobFinder itself, delete its program file.");
        ended_by_hand(ExitCode::SUCCESS)
    } else {
        jobfinder::log_line!("These couldn't be removed:");
        for why in &failed {
            jobfinder::log_line!("- {why}");
        }
        ended_by_hand(ExitCode::FAILURE)
    }
}

/// Whether the user answers yes to question; no unless they type y or yes.
fn ask(question: &str) -> bool {
    use std::io::Write;
    print!("{question} [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// The local user's trongate_users id in database, read without changing the file, while the
/// database still has that table (before migration 0010_one_local_user): the API keys' former
/// credential names have it in them. None once the table is gone, or when it can't be read.
///
/// Not checked at compile time: the database the checks run against has no trongate_users.
async fn former_user_id_in(database: &std::path::Path) -> Option<i64> {
    if !database.exists() {
        return None;
    }
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(database)
        .read_only(true);
    let pool = sqlx::SqlitePool::connect_with(options).await.ok()?;
    let id: Option<i64> = sqlx::query_scalar("SELECT MIN(id) FROM trongate_users")
        .fetch_one(&pool)
        .await
        .ok()
        .flatten();
    pool.close().await;
    id
}

fn run_server(args: &[String]) -> ExitCode {
    let mut port = server::DEFAULT_PORT;
    let mut database = None;
    let mut laya_folder = None;
    let mut llama_folder = None;
    let mut args = args.iter();
    let mut ignored = Vec::new();
    while let Some(flag) = args.next() {
        if GONE_FLAGS.contains(&flag.as_str()) {
            ignored.push(flag.clone());
            continue;
        }
        let Some(value) = args.next() else {
            jobfinder::log_line!("{flag} needs a value\n{USAGE}");
            return ExitCode::FAILURE;
        };
        match flag.as_str() {
            "--port" => match value.parse() {
                Ok(value) => port = value,
                Err(_) => {
                    jobfinder::log_line!("--port needs a number\n{USAGE}");
                    return ExitCode::FAILURE;
                }
            },
            "--database" => database = Some(PathBuf::from(value)),
            "--laya" => laya_folder = Some(PathBuf::from(value)),
            "--llama" => llama_folder = Some(PathBuf::from(value)),
            GONE_FLAG_WITH_VALUE => ignored.push(flag.clone()),
            _ => {
                jobfinder::log_line!("unknown option {flag}\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    // Double-clicked, the console would stand open behind the window; what the app says goes to
    // the log instead. Started from a terminal, it keeps printing there.
    hide_own_console();
    let (database, remember) = if let Some(database) = database {
        (database, false)
    } else {
        match saved_database() {
            Err(why) => return failed(why),
            Ok(Some(database)) => (database, false),
            Ok(None) => {
                let default = db::default_path();
                if default.exists() || from_recruiter::former_default_path().exists() {
                    (default, false)
                } else {
                    let Some(database) = choose_database(&default) else {
                        return ExitCode::SUCCESS;
                    };
                    let remember = database != default;
                    (database, remember)
                }
            }
        }
    };
    let moved = from_recruiter::move_data_folder(&database);
    let data_folder = database.parent().unwrap_or(std::path::Path::new(".")).to_path_buf();
    open_log(&data_folder);
    if !ignored.is_empty() {
        jobfinder::log_line!(
            "Ignored {}: JobFinder has no web pages any more, only its window. Take them out of the shortcut or task that starts it.",
            ignored.join(", ")
        );
    }
    if let Err(why) = said_move(moved) {
        return failed(why);
    }
    let Some(runtime) = runtime() else {
        return failed("The app's runtime couldn't start (see the log).".to_string());
    };
    match runtime.block_on(instance::running_on(port, &db::shown_path(&database))) {
        Running::None => {}
        Running::Same => {
            if runtime.block_on(instance::bring_to_front(port)) {
                jobfinder::say!("JobFinder is already running; its window is brought to the front.");
                return ExitCode::SUCCESS;
            }
            return failed(
                "JobFinder is already running, but its window couldn't be brought to the front. Find it on the taskbar, or close it and start JobFinder again.".to_string(),
            );
        }
        Running::Other { database: running } => {
            return failed(format!(
                "Port {port} is taken by a JobFinder on another database ({running}). Close it, or start this one with --port and another number."
            ));
        }
    }
    // Held while the app runs, so `jobfinder cleanup` won't remove the database under it.
    let Ok(_held) = cleanup::hold(&database, cleanup::Hold::Running) else {
        return failed(format!(
            "JobFinder's cleanup is running on {}. Let it finish, then start JobFinder again.",
            database.display()
        ));
    };
    let handle = runtime.handle().clone();
    let startup =
        runtime.spawn_blocking(move || start_services(handle, port, &database, laya_folder, llama_folder, remember));
    let shown = desktop::run(startup);
    runtime.shutdown_timeout(SHUTDOWN_WAIT);
    match shown {
        Ok(code) => code,
        Err(e) => {
            jobfinder::log_line!("The window couldn't open: {e}.");
            ExitCode::FAILURE
        }
    }
}

/// Prepares the app and its servers on a worker while the desktop window shows the splash.
fn start_services(
    runtime: Handle,
    port: u16,
    database: &Path,
    laya_folder: Option<PathBuf>,
    llama_folder: Option<PathBuf>,
    remember: bool,
) -> Result<desktop::Launch, String> {
    let mut context = app_context(&runtime, database, laya_folder, llama_folder)?;
    let data_folder = database.parent().unwrap_or(Path::new("."));
    let folder = data_folder.join(extension::FOLDER);
    context.extension_folder = extension::write(&folder).map_err(|e| {
        jobfinder::log_line!("Couldn't write the Jobnet extension to {}: {e}", folder.display());
        format!("It couldn't be written to {}: {e}", folder.display())
    });
    let listener = runtime.block_on(server::bind(port)).map_err(|e| {
        format!(
            "Couldn't serve on port {port}: {e}. Is another program using it? Then start JobFinder with --port and another number."
        )
    })?;
    if remember {
        remember_database(database)?;
    }
    let context = Arc::new(context);
    let show_setup = runtime.block_on(jobfinder::setup::first_run_or_logged(&context));
    let mut state = AppState::new(Arc::clone(&context), port);
    let (front_sender, front) = tokio::sync::mpsc::unbounded_channel();
    state.front = Some(front_sender);
    let state = Arc::new(state);
    runtime.spawn(jobfinder::idle::unload_when_idle(Arc::clone(&context)));
    let serving = runtime.spawn(server::serve_on(listener, state));
    // A server that stops while the window is open is logged; the window goes on without it.
    runtime.spawn(async move {
        match serving.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => jobfinder::log_line!("The server stopped: {e}"),
            Err(e) => jobfinder::log_line!("The server stopped: {e}"),
        }
    });
    serve_devices(&runtime, &context);
    jobfinder::say!(
        "JobFinder is running, with the Jobnet extension's address at http://127.0.0.1:{port}/ (data in {}).",
        database.display()
    );
    Ok(desktop::Launch {
        context,
        runtime,
        show_setup,
        front,
    })
}

/// Answers paired devices (a phone, a tablet) on their own port, and reads the computer's
/// tailnet name for them. A port that can't be taken is logged and said on the Devices screen;
/// the rest of the app goes on without it.
fn serve_devices(runtime: &Handle, context: &Arc<Context>) {
    let port = device_protocol::PORT;
    match runtime.block_on(device_server::bind()) {
        Ok(listener) => {
            let serving = runtime.spawn(device_server::serve_on(listener, Arc::clone(context)));
            runtime.spawn(async move {
                match serving.await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => jobfinder::log_line!("The device server stopped: {e}"),
                    Err(e) => jobfinder::log_line!("The device server stopped: {e}"),
                }
            });
        }
        Err(e) => {
            let why = format!("Port {port} for paired devices couldn't be taken: {e}. Is another program using it?");
            jobfinder::log_line!("{why}");
            *context
                .devices
                .not_listening
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(why);
        }
    }
    let reading = Arc::clone(context);
    runtime.spawn(async move {
        tailscale::state(&reading, port, chrono::Utc::now().timestamp()).await;
    });
}

/// How long closing the window waits for the app's work to end (a match being written, Laya
/// loading) before the program ends anyway.
const SHUTDOWN_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

/// Says why the app couldn't start (and logs it), in a small window.
fn failed(why: String) -> ExitCode {
    jobfinder::log_line!("{why}");
    if let Err(e) = desktop::show_failure(why) {
        jobfinder::log_line!("The window couldn't open to say so: {e}");
    }
    ExitCode::FAILURE
}

/// What the app's work runs on, for the server and `jobfinder scan`: the database (made when
/// there is none), the local user, the key store, the job scan's schedule, Laya's model and the
/// llama folder (next to the database unless laya_folder or llama_folder names another). Err
/// says why when the database can't be opened.
fn app_context(
    runtime: &Handle,
    database: &Path,
    laya_folder: Option<PathBuf>,
    llama_folder: Option<PathBuf>,
) -> Result<Context, String> {
    let made_now = !database.exists();
    // Read before db::open's migrations drop trongate_users.
    let former_user_id = runtime.block_on(former_user_id_in(database));
    let db = runtime
        .block_on(db::open(database))
        .map_err(|e| format!("Couldn't open {}: {e}", database.display()))?;
    let taxonomy = runtime.block_on(taxonomy_store::open(&db)).unwrap_or_else(|why| {
        // Matching still works on the shipped index alone; only the learned labels are missing.
        jobfinder::log_line!("{why} Matching uses the shipped taxonomy without the learned labels.");
        Taxonomy::load()
    });
    let mut context = Context::new(db, Arc::new(taxonomy), KeyVault::of_database(database));
    context.database = DatabaseFile {
        path: db::shown_path(database),
        made_now,
    };
    context.scan_schedule = scan_schedule(database);
    context.toasts = Toasts::system();
    forget_old_notifications(runtime, context.db());
    runtime.block_on(move_former_keys(database, &context, former_user_id));
    runtime.block_on(take_over_former_schedule(database, &context.scan_schedule));
    let data_folder = database.parent().unwrap_or(std::path::Path::new("."));
    let laya_folder = laya_folder.unwrap_or_else(|| data_folder.join(laya_client::FOLDER));
    context.laya = LayaClient::from_folder(&laya_folder);
    if context.laya.is_none() {
        jobfinder::say!(
            "No Laya model in {} (no {}), so matches go on without Laya.",
            laya_folder.display(),
            laya_client::ONNX_FILE
        );
    }
    let llama_folder = llama_folder.unwrap_or_else(|| data_folder.join(llama::FOLDER));
    let inventory = Inventory::detect();
    jobfinder::say!(
        "This computer, for the app's own model: {inventory}. Its llama folder is {}.",
        llama_folder.display()
    );
    context.whisper = Some(Whisper::new(&data_folder.join(whisper::FOLDER), &inventory));
    context.recordings_folder = Some(
        runtime
            .block_on(recordings::prepare_folder(context.db(), database))
            .map_err(|why| format!("Couldn't prepare the recordings folder: {why}"))?,
    );
    context.llama = Some(LlamaServer::new(&llama_folder, inventory));
    context.signed_in = Some(SignedInBrowser::new(data_folder.join(signed_in::FOLDER)));
    Ok(context)
}

/// Deletes the bell's notifications older than notification_model::KEEP_DAYS; a failure is
/// logged, and they stay till the next start.
fn forget_old_notifications(runtime: &Handle, db: &sqlx::SqlitePool) {
    let now = chrono::Utc::now().timestamp();
    if let Err(e) = runtime.block_on(notification_model::forget_old(db, now)) {
        jobfinder::log_line!("Old notifications couldn't be deleted: {e}");
    }
}

/// Says (and logs) what the data folder's move from the former name did; Err when it failed.
fn said_move(moved: Result<Moved, String>) -> Result<(), String> {
    match moved? {
        Moved::Nothing => {}
        Moved::Moved { from, to } => jobfinder::say!(
            "Recruiter is now called JobFinder: its data moved from {} to {}. If you loaded the Jobnet extension in Edge, remove it there and load it again (Load unpacked) from {}: Edge still looks for it in the former folder.",
            from.display(),
            to.display(),
            to.join(extension::FOLDER).display()
        ),
        Moved::BothThere { former } => jobfinder::say!(
            "Recruiter's former data folder, {}, is left as it is: JobFinder already has its own database. Delete the former folder once you don't need it.",
            former.display()
        ),
    }
    Ok(())
}

/// Where the app kept its secrets before the secrets table, as the cleanup names it.
const SYSTEM_STORE: &str = if cfg!(windows) {
    "Windows Credential Manager"
} else {
    "the system keyring"
};

/// Moves the API keys and device token hashes the app kept in Credential Manager (the keyring)
/// into the secrets table (account::former_keys): under the database's own names, the names with
/// the local user's id (former_user_id, while the database had trongate_users), and Recruiter's.
/// What failed is logged; the app goes on, and tries again at the next start.
async fn move_former_keys(database: &std::path::Path, context: &Context, former_user_id: Option<i64>) {
    let device_ids = match device_model::all(context.db()).await {
        Ok(devices) => devices.iter().map(|device| device.id).collect(),
        Err(e) => {
            jobfinder::log_line!("The paired devices couldn't be read to move their tokens: {e}");
            Vec::new()
        }
    };
    let former = FormerKeys::of(database, former_user_id, device_ids);
    match former.move_into(&context.key_vault, context.db()).await {
        Ok(0) => {}
        Ok(moved) => jobfinder::say!(
            "Moved {moved} API key(s) and device token(s) from {SYSTEM_STORE} into the database's secrets, sealed with its key file."
        ),
        Err(why) => jobfinder::log_line!("Nothing more was moved from {SYSTEM_STORE} into the secrets table: {why}"),
    }
}

/// Moves the job scan's schedule from Recruiter's name, on the default database, unless the former
/// database was left in place beside it. What failed is logged; the app goes on.
async fn take_over_former_schedule(database: &std::path::Path, schedule: &ScanSchedule) {
    if from_recruiter::former_left_in_place(database) || !from_recruiter::is_default(database) {
        return;
    }
    match schedule
        .take_over_former(&from_recruiter::former_scheduled_database())
        .await
    {
        Ok(None) => {}
        Ok(Some(hours)) => {
            jobfinder::say!("The job scan's schedule (every {hours} hours) moved from Recruiter's name to JobFinder's.")
        }
        Err(why) => jobfinder::log_line!("Recruiter's job scan schedule couldn't be moved to JobFinder: {why}"),
    }
}

/// The job scan's Task Scheduler task, running this jobfinder.exe on database.
fn scan_schedule(database: &std::path::Path) -> ScanSchedule {
    // The AppImage on Linux: the program's own path is inside it, and gone once it ends.
    let exe = scan_schedule::running_program().unwrap_or_else(|e| {
        jobfinder::log_line!(
            "This program's own path couldn't be read ({e}), so the job scan's task runs jobfinder.exe from PATH."
        );
        PathBuf::from("jobfinder.exe")
    });
    ScanSchedule::system(
        &exe,
        std::path::Path::new(&db::shown_path(database)),
        scan_schedule::extracted_appimage(),
    )
}

/// Opens the log (app_log) and says where it goes, or why it couldn't be opened.
fn open_log(folder: &std::path::Path) {
    match app_log::open(folder) {
        Ok(Opened::File(path)) => jobfinder::say!("Logging to {}.", path.display()),
        Ok(Opened::Journal) => jobfinder::say!("Logging to the systemd journal: journalctl --user -t jobfinder"),
        Ok(Opened::Console) => jobfinder::say!("Logging to the systemd journal, as this program's output."),
        Err(e) => jobfinder::log_line!(
            "The log file couldn't be opened in {}: {e}. What the app says goes only to this window.",
            folder.display()
        ),
    }
}

/// The async runtime the database and the server run on, or None (said why) when it can't start.
fn runtime() -> Option<tokio::runtime::Runtime> {
    tokio::runtime::Runtime::new()
        .map_err(|e| jobfinder::log_line!("Couldn't start: {e}"))
        .ok()
}
