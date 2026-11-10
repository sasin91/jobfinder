//! llama-server, run by the app for the "llama_cpp" provider: started on the first call with the
//! model model_choice picks, on a free port on 127.0.0.1, and kept running for the next calls
//! until it has sat unused for a while (stop_if_idle, which crate::idle calls), so a window left
//! open holds no model in the graphics card's memory. Its OpenAI-compatible API is called through
//! the openai adapter as any local server's is.
//!
//! It runs light, as work in the background should: at below-normal priority, on half the
//! processor's cores, one request at a time, and with as many layers on the card as fit in the
//! card's memory that is free when it starts, which llama.cpp itself works out. Forcing every
//! layer onto a card that Windows' desktop or another program already uses spills the model
//! into shared system memory, where it answers several times slower and keeps the card at full
//! load for that much longer.
//!
//! It stops with the app: on Windows it runs in a job object that closes, and so ends it, when
//! the app ends, however the app ends (closing its window included). Its output goes to
//! llama-server.log in the llama folder, whose last lines are shown when it fails to start.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use super::download::Downloads;
use super::gguf::{self, GgufFile};
use super::hardware::Inventory;
use super::model_choice::{self, Fit};
use super::model_gate::{Entry, ModelGate, Ticket};
use super::models_folder;

/// The llama.cpp program in the llama folder.
#[cfg(windows)]
pub const SERVER_FILE: &str = "llama-server.exe";
#[cfg(not(windows))]
pub const SERVER_FILE: &str = "llama-server";

/// The folder of GGUF models inside the llama folder, unless another was picked
/// (models_folder).
pub const MODELS_FOLDER: &str = "models";

/// llama-server's output, in the llama folder.
pub const LOG_FILE: &str = "llama-server.log";

/// The tokens a model may think for before it answers: none. Qwen3 thinks by default, which can
/// add a thousand tokens or more to each answer; on a mid-range graphics card (about 20 tokens a
/// second) that is a minute per call, and the answers follow a JSON schema anyway.
const THINKING_TOKENS: u32 = 0;

/// The context the server keeps, in tokens: a job post, a CV and a tailored résumé's answer.
pub const CONTEXT_TOKENS: u32 = 16_384;

/// How long a model may take to load before its start counts as failed, in seconds: a 20 GB file
/// from a hard disk.
const LOAD_SECONDS: u64 = 300;

/// How often the health check asks while the model loads, in milliseconds.
const HEALTH_POLL_MILLISECONDS: u64 = 250;

/// How long one health check may take, in seconds.
const HEALTH_CHECK_SECONDS: u64 = 2;

/// Requests the server answers at once: one, as the app asks one thing at a time. llama-server's
/// default of four keeps room on the card for four.
const PARALLEL_REQUESTS: u32 = 1;

/// The share of the processor's cores the server computes on (one in this many), so the
/// computer stays responsive while a model runs on the CPU.
const CORES_SHARE: usize = 2;

/// The log lines shown when the server fails to start.
const LOG_TAIL_LINES: usize = 12;

/// program's full path on PATH, with links followed, when it is there.
fn on_path(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|folder| folder.join(program))
        .find(|candidate| candidate.is_file())
        .map(|found| std::fs::canonicalize(&found).unwrap_or(found))
}

/// Where the running server answers, and the model it answers as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub base_url: String,
    pub model: String,
}

/// The server process and what it runs.
struct Running {
    child: Child,
    /// Ends the child with the app; dropped (closed) after the child is told to stop.
    _job: job::KillOnClose,
    model: PathBuf,
    endpoint: Endpoint,
    /// When it was started, in unix seconds.
    started_at: i64,
}

/// The running server, kept from stopping for being idle, and from switching to another model,
/// until this is dropped: what start() gives each caller, held for as long as its calls go on
/// (crate::llm::Llm keeps it).
pub struct InUse {
    pub endpoint: Endpoint,
    _ticket: Ticket,
}

impl std::fmt::Debug for InUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InUse").field("endpoint", &self.endpoint).finish()
    }
}

/// What the API key page shows of the llama folder: the program, the computer, and each model
/// with how it fits.
pub struct Overview {
    pub folder: PathBuf,
    pub models_folder: PathBuf,
    /// The llama-server that runs, when there is one.
    pub server: Option<PathBuf>,
    pub inventory: Inventory,
    /// Why the graphics card can't be used, when it can't.
    pub card_problem: Option<String>,
    pub models: Vec<(GgufFile, Fit)>,
    /// The model a blank model field runs, or why none can.
    pub chosen: Result<String, String>,
}

/// The llama folder (llama-server and its libraries, and models\*.gguf), and the server once
/// started.
pub struct LlamaServer {
    folder: PathBuf,
    #[cfg(test)]
    find_on_path: fn(&str) -> Option<PathBuf>,
    /// Where the models are (models_folder::read), changed by move_models.
    models_folder: std::sync::RwLock<PathBuf>,
    inventory: std::sync::RwLock<Inventory>,
    running: Mutex<Option<Running>>,
    /// The calls on the running server and the model they run on (model_gate), which start waits
    /// on before switching models and stop_if_idle reads.
    calls: Arc<ModelGate>,
    /// The setup wizard's download.
    pub downloads: Downloads,
    /// Held while a task's model is downloaded because the task needs it, so two tasks that need
    /// the same file download it once (llama_setup_service::download_if_missing).
    pub fetching: tokio::sync::Mutex<()>,
}

impl LlamaServer {
    /// The server for folder, made absolute: llama-server runs inside the folder, where a relative
    /// path to itself or its model would point somewhere else.
    pub fn new(folder: &Path, inventory: Inventory) -> Self {
        let folder = std::path::absolute(folder).unwrap_or_else(|_| folder.to_path_buf());
        LlamaServer {
            #[cfg(test)]
            find_on_path: on_path,
            models_folder: std::sync::RwLock::new(models_folder::read(&folder)),
            folder,
            inventory: std::sync::RwLock::new(inventory),
            running: Mutex::new(None),
            calls: Arc::default(),
            downloads: Downloads::default(),
            fetching: tokio::sync::Mutex::new(()),
        }
    }

    pub fn folder(&self) -> &Path {
        &self.folder
    }

    pub fn models_folder(&self) -> PathBuf {
        self.models_folder
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Moves the models to the folder to and runs them from there from now on; says how many
    /// moved. Not while a model downloads; the server is stopped first, as a running model's
    /// file can't be moved on Windows (the next call starts it again).
    pub async fn move_models(
        &self,
        to: PathBuf,
        report: impl FnMut(&std::ffi::OsStr, usize, usize) + Send + 'static,
    ) -> Result<usize, String> {
        let _no_task_download = self.fetching.lock().await;
        let _no_download = self.downloads.begin_move()?;
        let from = self.models_folder();
        if from == to {
            return Ok(0);
        }
        self.stop_now().await;
        let llama_folder = self.folder.clone();
        let target = to.clone();
        let moved = tokio::task::spawn_blocking(move || models_folder::move_to(&llama_folder, &from, &target, report))
            .await
            .map_err(|e| format!("Moving the models stopped: {e}"))??;
        *self
            .models_folder
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = to;
        Ok(moved)
    }

    pub fn inventory(&self) -> Inventory {
        self.inventory.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Explicitly requested from Models; never changes saved task assignments.
    pub fn refresh_inventory(&self, inventory: Inventory) {
        *self.inventory.write().unwrap_or_else(|p| p.into_inner()) = inventory;
    }

    /// The llama-server to run: the llama folder's, else one already on this computer's PATH
    /// (winget's llama.cpp, say), followed through its link to the folder its DLLs are in.
    pub fn server_path(&self) -> Option<PathBuf> {
        let own = self.folder.join(SERVER_FILE);
        if own.is_file() {
            return Some(own);
        }
        #[cfg(test)]
        return (self.find_on_path)(SERVER_FILE);
        #[cfg(not(test))]
        on_path(SERVER_FILE)
    }

    /// The models in the models folder.
    pub fn models(&self) -> Vec<GgufFile> {
        gguf::list(&self.models_folder())
    }

    /// The folder as the API key page shows it.
    /// Whether the card can run a model, as model_choice::card_usable says for the llama-server
    /// here. Before there is one, the card counts as usable: the setup wizard downloads the build
    /// for it, so there is nothing missing to say yet.
    pub fn card_usable(&self) -> Result<bool, String> {
        let Some(server) = self.server_path() else {
            return Ok(self.inventory().gpu.is_some());
        };
        let server_folder = server.parent().unwrap_or(&self.folder);
        model_choice::card_usable(&self.inventory(), &model_choice::compute_libraries(server_folder))
    }

    pub fn overview(&self) -> Overview {
        let card = self.card_usable();
        let card_usable = card.as_ref().is_ok_and(|usable| *usable);
        let models = self.models();
        let chosen =
            model_choice::choose(&models, "", &self.inventory(), card_usable).map(|(model, _)| model.name.clone());
        Overview {
            folder: self.folder.clone(),
            models_folder: self.models_folder(),
            server: self.server_path(),
            inventory: self.inventory(),
            card_problem: card.err(),
            models: models
                .iter()
                .map(|model| (model.clone(), model_choice::fit(model, &self.inventory(), card_usable)))
                .collect(),
            chosen,
        }
    }

    /// The model file the server runs now, or None when it isn't running (or is starting or
    /// stopping, which holds its lock).
    pub fn running_model(&self) -> Option<String> {
        let running = self.running.try_lock().ok()?;
        Some(file_name_of(&running.as_ref()?.model))
    }

    /// The server process running now, for the task manager; None as running_model says.
    pub fn running_process(&self) -> Option<ServerProcess> {
        let running = self.running.try_lock().ok()?;
        let running = running.as_ref()?;
        Some(ServerProcess {
            model: file_name_of(&running.model),
            process_id: running.child.id(),
            started_at: running.started_at,
        })
    }

    /// Stops the server now, calls under way or not (they fail, and the next call starts it
    /// again), and says the model it ran; None when it isn't running. Waits for a start under way.
    pub async fn stop_now(&self) -> Option<String> {
        let mut stopped = self.running.lock().await.take()?;
        stop(&mut stopped).await;
        Some(stopped.endpoint.model)
    }

    /// The server running named (a file in the models folder, or "" for the one that fits best),
    /// started or restarted with it when needed, and kept running while the answer is held. Err
    /// says what's missing or why it didn't start.
    ///
    /// Another model than the running one waits its turn (model_gate): until the calls under way
    /// have finished, so switching never cuts one off. The wait holds neither the server's lock
    /// nor anything a call under way needs.
    pub async fn start(&self, named: &str) -> Result<InUse, String> {
        let Some(server) = self.server_path() else {
            return Err(format!(
                "There's no {SERVER_FILE} in {}. Download it on the API key page's \"Set it up\" (/llama_setup).",
                self.folder.display()
            ));
        };
        let card = self.card_usable()?;
        let models = self.models();
        let (model, fit) = model_choice::choose(&models, named, &self.inventory(), card)?;
        // Counted as under way from here, so stop_if_idle leaves the server running for it.
        let turn = self.calls.enter(&model.path).await;
        let mut running = self.running.lock().await;
        if let Some(current) = running.as_mut()
            && current.model == model.path
            && matches!(current.child.try_wait(), Ok(None))
        {
            return Ok(in_use(current.endpoint.clone(), turn, &model.path));
        }
        // Not running, ended, or (on a switch's turn) another model. A failed start drops the
        // turn, which says nothing runs.
        if let Some(mut previous) = running.take() {
            stop(&mut previous).await;
        }
        let started = self.spawn(&server, model, fit).await?;
        let endpoint = started.endpoint.clone();
        *running = Some(started);
        Ok(in_use(endpoint, turn, &model.path))
    }

    /// Stops the server when it runs, no call is under way and none ended in the last idle_for,
    /// and says the model it ran; None when it was left as it is. A server starting or stopping
    /// (which holds its lock) is left alone; the next check sees it.
    pub async fn stop_if_idle(&self, idle_for: Duration) -> Option<String> {
        let mut running = self.running.try_lock().ok()?;
        let idle = self.calls.idle_for(idle_for);
        if !idle {
            return None;
        }
        let mut stopped = running.take()?;
        stop(&mut stopped).await;
        Some(stopped.endpoint.model)
    }

    async fn spawn(&self, server: &Path, model: &GgufFile, fit: Fit) -> Result<Running, String> {
        let port = free_port().map_err(|e| format!("No free port for llama-server: {e}"))?;
        let log_path = self.folder.join(LOG_FILE);
        let log = File::create(&log_path).map_err(|e| format!("Couldn't write {}: {e}", log_path.display()))?;
        let log_too = log
            .try_clone()
            .map_err(|e| format!("Couldn't write {}: {e}", log_path.display()))?;
        let alias = model_alias(model);
        let mut command = Command::new(server);
        #[cfg(target_os = "linux")]
        {
            let mut paths = vec![server.parent().unwrap_or(&self.folder).to_path_buf()];
            if let Some(existing) = std::env::var_os("LD_LIBRARY_PATH") {
                paths.extend(std::env::split_paths(&existing));
            }
            if let Ok(path) = std::env::join_paths(paths) {
                command.env("LD_LIBRARY_PATH", path);
            }
        }
        let mut child = crate::no_window::in_background(&mut command)
            .current_dir(&self.folder)
            .args(server_arguments(model, &alias, port, fit, background_threads()))
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(log_too)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("{} didn't start: {e}", server.display()))?;
        let job = job::KillOnClose::assign(&child).map_err(|e| {
            crate::log_line!("llama: llama-server won't stop with the app: {e}");
            format!("llama-server couldn't be tied to the app: {e}")
        })?;
        let base = format!("http://127.0.0.1:{port}");
        println!(
            "llama: starting {} with {} ({}), log in {}",
            SERVER_FILE,
            model.name,
            fit.label(),
            log_path.display()
        );
        let started = Instant::now();
        if let Err(why) = wait_until_healthy(&base, &mut child).await {
            let _ = child.start_kill();
            return Err(format!("{why}\n{}", log_tail(&log_path)));
        }
        println!(
            "llama: {} is loaded ({:.1} s).",
            model.name,
            started.elapsed().as_secs_f64()
        );
        Ok(Running {
            child,
            _job: job,
            started_at: chrono::Utc::now().timestamp(),
            model: model.path.clone(),
            endpoint: Endpoint {
                base_url: format!("{base}/v1"),
                model: alias,
            },
        })
    }
}

/// The server at endpoint, running model, in use for the call whose turn it is: a call that
/// joined has its ticket; a switch's turn is over now that model runs.
fn in_use(endpoint: Endpoint, turn: Entry<'_>, model: &Path) -> InUse {
    let ticket = match turn {
        Entry::Join(ticket) => ticket,
        Entry::Switch(turn) => turn.started(model),
    };
    InUse {
        endpoint,
        _ticket: ticket,
    }
}

/// The name the server answers as: the file name without .gguf.
fn model_alias(model: &GgufFile) -> String {
    model
        .path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_string())
        .unwrap_or_else(|| model.name.clone())
}

/// The threads the server computes on: CORES_SHARE of the processor's, at least one.
fn background_threads() -> usize {
    let cores = std::thread::available_parallelism().map_or(1, |cores| cores.get());
    (cores / CORES_SHARE).max(1)
}

/// llama-server's --n-gpu-layers for fit: none on the card for a model run in memory, else not
/// given, so llama.cpp fits the layers to the card's memory that is free when it starts.
fn gpu_layers_argument(fit: Fit) -> Option<u32> {
    match fit {
        Fit::Memory | Fit::TooBig => Some(0),
        Fit::Gpu | Fit::Split { .. } => None,
    }
}

/// llama-server's command line: the model, on 127.0.0.1 only, its context, one request at a time
/// on threads threads, its layers on the card (gpu_layers_argument), no thinking before answering
/// (--reasoning-budget 0), and the model's own chat template (--jinja).
fn server_arguments(model: &GgufFile, alias: &str, port: u16, fit: Fit, threads: usize) -> Vec<String> {
    let mut arguments = vec![
        "--model".to_string(),
        model.path.display().to_string(),
        "--alias".to_string(),
        alias.to_string(),
        "--host".to_string(),
        "127.0.0.1".to_string(),
        "--port".to_string(),
        port.to_string(),
        "--ctx-size".to_string(),
        CONTEXT_TOKENS.to_string(),
        "--parallel".to_string(),
        PARALLEL_REQUESTS.to_string(),
        "--threads".to_string(),
        threads.to_string(),
        "--reasoning-budget".to_string(),
        THINKING_TOKENS.to_string(),
    ];
    if let Some(layers) = gpu_layers_argument(fit) {
        arguments.extend(["--n-gpu-layers".to_string(), layers.to_string()]);
    }
    arguments.push("--jinja".to_string());
    arguments
}

/// A port nothing listens on now. Another program could take it before llama-server does; then
/// the start fails and says so.
fn free_port() -> std::io::Result<u16> {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map(|address| address.port())
}

/// Waits for GET /health to answer 200 (it answers 503 while the model loads), failing when the
/// server ends or LOAD_SECONDS pass.
async fn wait_until_healthy(base: &str, child: &mut Child) -> Result<(), String> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(HEALTH_CHECK_SECONDS))
        .build()
        .map_err(|e| format!("Couldn't make the health check: {e}"))?;
    let deadline = Instant::now() + Duration::from_secs(LOAD_SECONDS);
    let health = format!("{base}/health");
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!("llama-server stopped while loading the model ({status})."));
        }
        let healthy = client
            .get(&health)
            .send()
            .await
            .is_ok_and(|response| response.status().is_success());
        if healthy {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("llama-server didn't load the model in {LOAD_SECONDS} seconds."));
        }
        tokio::time::sleep(Duration::from_millis(HEALTH_POLL_MILLISECONDS)).await;
    }
}

/// The file name of a model's path, as the window shows it.
fn file_name_of(model: &Path) -> String {
    model
        .file_name()
        .unwrap_or(model.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The running llama-server, as the task manager lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerProcess {
    /// The model file it runs.
    pub model: String,
    pub process_id: Option<u32>,
    /// When it was started, in unix seconds.
    pub started_at: i64,
}

/// Stops a server and waits for it to end.
async fn stop(running: &mut Running) {
    if let Err(e) = running.child.kill().await {
        crate::log_line!("llama: the previous llama-server couldn't be stopped: {e}");
    }
}

/// The log's last LOG_TAIL_LINES lines, or why it can't be read.
fn log_tail(path: &Path) -> String {
    match std::fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let lines: Vec<&str> = text.lines().collect();
            let tail = &lines[lines.len().saturating_sub(LOG_TAIL_LINES)..];
            format!("The end of {}:\n{}", path.display(), tail.join("\n"))
        }
        Err(e) => format!("{} couldn't be read: {e}", path.display()),
    }
}

#[cfg(windows)]
mod job {
    use tokio::process::Child;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation, SetInformationJobObject,
    };

    /// A job object that ends its processes when it is closed, by drop or by the app ending.
    pub struct KillOnClose(HANDLE);

    // SAFETY: the handle is only closed, once, in drop; a job handle may be used from any thread.
    unsafe impl Send for KillOnClose {}
    // SAFETY: as above; no method reads or changes the job through a shared reference.
    unsafe impl Sync for KillOnClose {}

    impl KillOnClose {
        /// A new job with child in it.
        pub fn assign(child: &Child) -> Result<Self, String> {
            let process = child.raw_handle().ok_or("the process has already ended")?;
            // SAFETY: no name or security attributes; the handle is owned by the returned value.
            let job = KillOnClose(unsafe { CreateJobObjectW(None, None) }.map_err(|e| e.to_string())?);
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            // SAFETY: limits is the JOBOBJECT_EXTENDED_LIMIT_INFORMATION the class names, with its size.
            unsafe {
                SetInformationJobObject(
                    job.0,
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            }
            .map_err(|e| e.to_string())?;
            // SAFETY: both handles are open: the job above, and the child's process, which tokio keeps.
            unsafe { AssignProcessToJobObject(job.0, HANDLE(process)) }.map_err(|e| e.to_string())?;
            Ok(job)
        }
    }

    impl Drop for KillOnClose {
        fn drop(&mut self) {
            // SAFETY: the handle is open and closed only here.
            if let Err(e) = unsafe { CloseHandle(self.0) } {
                crate::log_line!("llama: the job object couldn't be closed: {e}");
            }
        }
    }
}

#[cfg(not(windows))]
mod job {
    use tokio::process::Child;

    /// Elsewhere the server only stops with the app through kill_on_drop.
    pub struct KillOnClose;

    impl KillOnClose {
        pub fn assign(_child: &Child) -> Result<Self, String> {
            Ok(KillOnClose)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llama::hardware::GIB;

    fn model() -> GgufFile {
        GgufFile {
            name: "Qwen3-8B-Q4_K_M.gguf".to_string(),
            path: PathBuf::from("models").join("Qwen3-8B-Q4_K_M.gguf"),
            bytes: 5 * GIB,
            layers: Some(36),
        }
    }

    fn pairs(arguments: &[String]) -> Vec<(&str, &str)> {
        arguments
            .chunks(2)
            .filter(|pair| pair.len() == 2)
            .map(|pair| (pair[0].as_str(), pair[1].as_str()))
            .collect()
    }

    #[test]
    fn the_server_listens_on_this_computer_only_and_runs_light() {
        let arguments = server_arguments(&model(), "Qwen3-8B-Q4_K_M", 50123, Fit::Gpu, 4);
        let pairs = pairs(&arguments);
        assert!(pairs.contains(&("--host", "127.0.0.1")));
        assert!(pairs.contains(&("--port", "50123")));
        assert!(pairs.contains(&("--alias", "Qwen3-8B-Q4_K_M")));
        assert!(pairs.contains(&("--reasoning-budget", "0")));
        assert!(pairs.contains(&("--parallel", "1")));
        assert!(pairs.contains(&("--threads", "4")));
        assert_eq!(arguments.last().map(String::as_str), Some("--jinja"));
        assert_eq!(model_alias(&model()), "Qwen3-8B-Q4_K_M");
    }

    #[test]
    fn llama_cpp_fits_the_layers_to_the_free_memory_on_the_card() {
        for fit in [Fit::Gpu, Fit::Split { gpu_layers: 20 }] {
            let arguments = server_arguments(&model(), "m", 50123, fit, 4);
            assert!(
                !arguments.contains(&"--n-gpu-layers".to_string()),
                "{fit:?}: {arguments:?}"
            );
        }
        let in_memory = server_arguments(&model(), "m", 50123, Fit::Memory, 4);
        assert!(pairs(&in_memory).contains(&("--n-gpu-layers", "0")));
        assert!(background_threads() >= 1);
    }

    #[tokio::test]
    async fn the_server_is_idle_only_with_no_call_under_way_for_long_enough() {
        let calls = Arc::new(ModelGate::default());
        let idle = Duration::from_secs(300);
        assert!(calls.idle_for(idle));
        let model = model().path;
        let held = InUse {
            endpoint: Endpoint {
                base_url: "http://127.0.0.1:1/v1".to_string(),
                model: "m".to_string(),
            },
            _ticket: match calls.enter(&model).await {
                Entry::Join(ticket) => ticket,
                Entry::Switch(turn) => turn.started(&model),
            },
        };
        assert!(!calls.idle_for(Duration::ZERO));
        drop(held);
        assert!(calls.idle_for(Duration::ZERO));
        assert!(!calls.idle_for(idle), "a call just ended");
    }

    #[tokio::test]
    async fn a_server_that_isnt_running_isnt_stopped() {
        let server = LlamaServer::new(
            &std::env::temp_dir().join("jobfinder-llama-test-idle"),
            Inventory::default(),
        );
        assert_eq!(server.stop_if_idle(Duration::ZERO).await, None);
    }

    #[tokio::test]
    async fn without_the_program_the_folder_is_named() {
        let folder = std::env::temp_dir().join("jobfinder-llama-test-no-server");
        let mut server = LlamaServer::new(&folder, Inventory::default());
        server.find_on_path = |_| None;
        let why = server.start("").await.unwrap_err();
        assert!(why.starts_with(&format!("There's no {SERVER_FILE} in ")), "{why}");
        assert!(server.folder().is_absolute());
        let relative = LlamaServer::new(Path::new("llama"), Inventory::default());
        assert!(relative.folder().is_absolute() && relative.folder().ends_with("llama"));
        let overview = server.overview();
        assert!(overview.server.is_none());
        assert_eq!(
            overview.chosen,
            Err("There's no .gguf model in the models folder yet.".to_string())
        );
    }

    #[test]
    fn an_amd_card_is_not_said_to_lack_vulkan_before_llama_server_is_there() {
        let folder = std::env::temp_dir().join("jobfinder-llama-test-amd-no-server");
        let amd = Inventory {
            memory_bytes: 32 * GIB,
            gpu: Some(crate::llama::hardware::Gpu {
                name: "AMD Radeon RX 7600".to_string(),
                vendor: crate::llama::hardware::GpuVendor::Amd,
                vram_bytes: 8 * GIB,
            }),
        };
        let server = LlamaServer::new(&folder, amd);
        assert_eq!(server.card_usable(), Ok(true));
        assert_eq!(server.overview().card_problem, None);
    }
}
