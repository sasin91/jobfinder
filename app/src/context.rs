//! What the app's work shares, whichever front end asks for it: the database, the taxonomy,
//! where the API keys are kept, Laya, the app's own llama.cpp model, the Jobnet
//! extension's folder, the database file and the job scan's schedule.
//!
//! The web server keeps its own state beside this (server::AppState: the flash message, the
//! port, the Tailscale name, the page reloads), so a desktop window can run the same work
//! (matches::match_service) without a web server's parts.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};

use sqlx::SqlitePool;

use crate::account::key_vault::KeyVault;
use crate::devices::Devices;
use crate::game_mode::GameMode;
use crate::job_sites::signed_in::SignedInBrowser;
use crate::laya::laya_client::LayaClient;
use crate::llama::llama_server::LlamaServer;
use crate::matches::match_model::SortOrder;
use crate::matches::match_service;
use crate::matches::page_reader::Readers;
use crate::notifications::Toasts;
use crate::scan_schedule::ScanSchedule;
use crate::task_manager::running_work::RunningWork;
use crate::taxonomy::Taxonomy;
use crate::whisper::Whisper;

pub struct Context {
    db: SqlitePool,
    /// The taxonomy every lookup uses, swapped whole when a learned label turns active
    /// (taxonomy_store::reload); a lookup keeps the one it took for as long as it runs.
    taxonomy: RwLock<Arc<Taxonomy>>,
    pub key_vault: KeyVault,
    /// When the pages fetch_job read in its window were fetched, oldest first (unix seconds).
    fetches: Mutex<Vec<i64>>,
    /// The order chosen in Saved matches, also used by the browser's application picker.
    saved_match_sort: RwLock<SortOrder>,
    /// Where the Jobnet extension was written for the browser to load, or why it couldn't be.
    pub extension_folder: Result<PathBuf, String>,
    /// Laya's model, when its folder has one.
    pub laya: Option<LayaClient>,
    /// The llama folder, whose llama-server runs a "llama_cpp" key's model.
    pub llama: Option<LlamaServer>,
    /// The database file, for the setup wizard.
    pub database: DatabaseFile,
    /// When the job scan runs by itself (Task Scheduler; in memory unless main sets it).
    pub scan_schedule: ScanSchedule,
    /// The program whose `read-pdf` reads an uploaded PDF (pdf_text::child): this one, unless
    /// a test names the built jobfinder.
    pub pdf_reader: Option<PathBuf>,
    /// A job scan runs, on an in-memory database (job_scan::scan_lock; a database file is
    /// locked by a file beside it instead).
    pub scan_in_memory: AtomicBool,
    /// JobFinder's signed-in browser, which reads LinkedIn and Indeed (None: not here, as in
    /// tests).
    pub signed_in: Option<SignedInBrowser>,
    /// The work running now, for the task manager (task_manager::running_work).
    pub running: Arc<RunningWork>,
    /// Whether a game runs, as last looked (game_mode).
    pub game_mode: GameMode,
    /// How the system's notifications are raised (notifications; none unless main sets it).
    pub toasts: Toasts,
    /// The pairing code shown now and the tailnet name, for paired devices (devices).
    pub devices: Devices,
    /// The whisper folder, whose whisper-cli transcribes the recordings (whisper).
    pub whisper: Option<Whisper>,
    /// The folder the recordings' audio files are kept in, next to the database (recordings).
    pub recordings_folder: Option<PathBuf>,
}

/// The database file the app runs on.
#[derive(Debug, Clone, Default)]
pub struct DatabaseFile {
    /// Its full path, or "" for an in-memory one.
    pub path: String,
    /// Made by this start of the app, rather than found.
    pub made_now: bool,
}

impl Context {
    pub fn new(db: SqlitePool, taxonomy: Arc<Taxonomy>, key_vault: KeyVault) -> Self {
        Context {
            db,
            taxonomy: RwLock::new(taxonomy),
            key_vault,
            fetches: Mutex::new(Vec::new()),
            saved_match_sort: RwLock::default(),
            extension_folder: Err("The extension wasn't written out.".to_string()),
            laya: None,
            llama: None,
            database: DatabaseFile::default(),
            scan_schedule: ScanSchedule::in_memory(),
            pdf_reader: std::env::current_exe().ok(),
            scan_in_memory: AtomicBool::new(false),
            signed_in: None,
            running: Arc::default(),
            game_mode: GameMode::default(),
            toasts: Toasts::default(),
            devices: Devices::default(),
            whisper: None,
            recordings_folder: None,
        }
    }

    /// What job posts are read with besides fetching their page (page_reader::read).
    pub fn readers(&self) -> Readers<'_> {
        Readers {
            pdf_reader: self.pdf_reader.as_deref(),
            signed_in: self.signed_in.as_ref(),
        }
    }

    pub fn db(&self) -> &SqlitePool {
        &self.db
    }

    pub fn saved_match_sort(&self) -> SortOrder {
        *self
            .saved_match_sort
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn set_saved_match_sort(&self, sort: SortOrder) {
        *self
            .saved_match_sort
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = sort;
    }

    /// The taxonomy now.
    pub fn taxonomy(&self) -> Arc<Taxonomy> {
        self.taxonomy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Puts taxonomy in place of the one lookups take from now on.
    pub fn replace_taxonomy(&self, taxonomy: Taxonomy) {
        *self.taxonomy.write().unwrap_or_else(|poisoned| poisoned.into_inner()) = Arc::new(taxonomy);
    }

    /// Counts a page fetch at now, or says in how many minutes the next one may go, when
    /// match_service::MAX_FETCHES were fetched in the last match_service::FETCH_WINDOW_SECONDS.
    pub fn count_fetch(&self, now: i64) -> Result<(), i64> {
        /// Seconds in a minute, which the wait is told in.
        const MINUTE: i64 = 60;
        let mut fetches = self.fetches.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        fetches.retain(|at| *at > now - match_service::FETCH_WINDOW_SECONDS);
        if fetches.len() >= match_service::MAX_FETCHES {
            let wait = fetches[0] + match_service::FETCH_WINDOW_SECONDS - now;
            return Err((wait + MINUTE - 1) / MINUTE);
        }
        fetches.push(now);
        Ok(())
    }
}
