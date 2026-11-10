//! The setup wizard's downloads: the platform's llama.cpp build for this computer's graphics
//! card, from its GitHub releases, installed into the llama folder; and a catalog model into the
//! models folder. Downloads tracks one operation at a time, including batches of model files.
//!
//! A file is written as name.part and renamed when it is whole, so a broken download never looks
//! like a model or a server.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{StreamExt, stream};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

use super::catalog::CatalogModel;
use super::hardware::{GpuVendor, Inventory};
use super::llama_server::SERVER_FILE;
use crate::task_manager::running_work::{self, Stopped};

/// llama.cpp's releases, newest first, as GitHub's API lists them.
pub const RELEASES: &str = "https://api.github.com/repos/ggml-org/llama.cpp/releases";

/// How many of the newest releases are looked through for this computer's build. llama.cpp
/// publishes a release before its builds are uploaded, so the newest can lack it for a while.
const RELEASES_SEARCHED: usize = 10;

/// The most files GitHub's API lists on one page.
const ASSETS_PER_PAGE: usize = 100;

/// What the app calls itself to GitHub, whose API refuses requests without a User-Agent.
const USER_AGENT: &str = "jobfinder-desktop";

/// How long connecting may take, in seconds.
const CONNECT_SECONDS: u64 = 15;

/// How long the download may stall between two pieces before it counts as failed, in seconds.
const STALL_SECONDS: u64 = 60;

/// The folder inside the llama folder that llama.cpp's zips are unzipped into before they are
/// all there.
const STAGING_FOLDER: &str = ".llama-cpp-download";

/// The suffix of a file being downloaded.
const PART_SUFFIX: &str = ".part";

const PARALLEL_MODEL_DOWNLOADS: usize = 3;

/// A release file.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
}

/// A GitHub release, as its API lists it.
#[derive(Debug, Deserialize)]
pub(crate) struct Release {
    pub id: u64,
    pub tag_name: String,
}

/// A llama.cpp build for Windows, by the name part its zip carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Build {
    /// NVIDIA cards, with CUDA's runtime in a second zip.
    Cuda,
    /// AMD, Intel and other cards, through the graphics driver alone.
    Vulkan,
    /// No graphics card.
    Cpu,
}

impl Build {
    /// The build for this computer's card. An AMD card gets Vulkan rather than HIP: HIP needs
    /// AMD's HIP SDK installed, Vulkan only the graphics driver.
    pub fn for_computer(inventory: &Inventory) -> Self {
        match inventory.gpu.as_ref().map(|gpu| gpu.vendor) {
            Some(GpuVendor::Nvidia) => Build::Cuda,
            Some(_) => Build::Vulkan,
            None => Build::Cpu,
        }
    }

    /// What the wizard calls it.
    pub fn label(self) -> &'static str {
        match self {
            Build::Cuda => "the CUDA build, for NVIDIA cards",
            Build::Vulkan => "the Vulkan build, which runs on the graphics driver",
            Build::Cpu => "the CPU build, as there is no graphics card",
        }
    }

    /// Its name, as a sentence names it ("the Vulkan build").
    fn name(self) -> &'static str {
        match self {
            Build::Cuda => "CUDA",
            Build::Vulkan => "Vulkan",
            Build::Cpu => "CPU",
        }
    }

    /// The text in the release zip's name, after "-bin-".
    fn marker(self) -> &'static str {
        match self {
            Build::Cuda => "-bin-win-cuda-",
            Build::Vulkan => "-bin-win-vulkan-x64.zip",
            Build::Cpu => "-bin-win-cpu-x64.zip",
        }
    }
}

/// The zips to download for build: llama.cpp's own, and for CUDA the matching CUDA runtime
/// (cudart-llama-bin-win-cuda-12.4-x64.zip beside llama-b1234-bin-win-cuda-12.4-x64.zip), taking
/// the newest CUDA version when there are several. Err when the release has none.
pub fn assets_for(build: Build, assets: &[Asset]) -> Result<Vec<Asset>, String> {
    let is_build = |asset: &&Asset| {
        asset.name.starts_with("llama-") && asset.name.contains(build.marker()) && asset.name.ends_with("-x64.zip")
    };
    let mut builds: Vec<&Asset> = assets.iter().filter(is_build).collect();
    builds.sort_by(|a, b| a.name.cmp(&b.name));
    let Some(chosen) = builds.last() else {
        return Err(format!(
            "There's no Windows {} build of llama.cpp in the release.",
            build.name()
        ));
    };
    let mut wanted = vec![(*chosen).clone()];
    if build == Build::Cuda {
        let version = chosen.name.split(build.marker()).nth(1).unwrap_or_default();
        let runtime_name = format!("cudart-llama-bin-win-cuda-{version}");
        let runtime = assets
            .iter()
            .find(|asset| asset.name == runtime_name)
            .ok_or_else(|| format!("There's no {runtime_name} in the release."))?;
        wanted.push(runtime.clone());
    }
    Ok(wanted)
}

/// Select explicitly by target; a Linux host must never install Windows executables.
pub fn assets_for_target(build: Build, assets: &[Asset], os: &str, arch: &str) -> Result<Vec<Asset>, String> {
    if os == "windows" && arch == "x86_64" {
        return assets_for(build, assets);
    }
    if os != "linux" || !matches!(arch, "x86_64" | "aarch64") {
        return Err(format!(
            "Automatic llama.cpp installation is not available for {os}/{arch}. Install llama-server on PATH instead."
        ));
    }
    let arch = if arch == "x86_64" { "x64" } else { "arm64" };
    let suffix = match build {
        Build::Cpu => format!("-bin-ubuntu-{arch}.tar.gz"),
        Build::Vulkan => format!("-bin-ubuntu-vulkan-{arch}.tar.gz"),
        Build::Cuda => format!("-bin-ubuntu-cuda-12.8-{arch}.tar.gz"),
    };
    // CUDA 12.8 has wider driver compatibility than newer releases. A missing compatible
    // build is actionable; do not silently download a build requiring a newer driver.
    let chosen = assets.iter().find(|a| a.name.starts_with("llama-") && a.name.ends_with(&suffix))
        .ok_or_else(|| format!("No Linux {} {arch} runtime in this release. Install a compatible llama-server on PATH or use another provider.", build.name()))?;
    let mut wanted = vec![chosen.clone()];
    if build == Build::Cuda {
        let runtime = format!("cudart-{}", chosen.name);
        wanted.push(
            assets
                .iter()
                .find(|a| a.name == runtime)
                .ok_or_else(|| format!("The CUDA runtime {runtime} is missing."))?
                .clone(),
        );
    }
    Ok(wanted)
}

/// How the download is going.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Progress {
    #[default]
    Idle,
    Running {
        what: String,
        done_bytes: u64,
        total_bytes: Option<u64>,
    },
    Finished {
        what: String,
    },
    Failed {
        what: String,
        why: String,
    },
}

/// The one download, and how it is going.
#[derive(Debug, Default)]
pub struct Downloads {
    progress: Mutex<Progress>,
    /// Unzips running on their own thread. One goes on when its download is stopped in the task
    /// manager (a blocking thread can't be stopped), so no download starts until it is done:
    /// the next would clear the staging folder it writes into.
    unzipping: Arc<AtomicUsize>,
    /// The models are being moved to another folder (llama_server::move_models), so no download
    /// starts: it would write into the folder being left.
    moving: AtomicBool,
}

/// Holds the downloads off while the models are moved; dropping it lets them start again.
pub struct MoveTurn<'a>(&'a AtomicBool);

impl Drop for MoveTurn<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Ends a download begun by the work that holds it, when that work is dropped before it ends
/// the download itself (a task stopped, or a scan's time limit): the download is then said to
/// have stopped, so the next one can begin.
pub struct DownloadTurn<'a> {
    downloads: &'a Downloads,
    ended: bool,
}

impl DownloadTurn<'_> {
    /// Ends the download with its outcome.
    pub fn end(mut self, outcome: Result<(), String>) {
        self.ended = true;
        self.downloads.end(outcome);
    }
}

impl Drop for DownloadTurn<'_> {
    fn drop(&mut self) {
        if !self.ended {
            self.downloads.end(Err("It stopped before it finished.".to_string()));
        }
    }
}

/// Counts an unzip as running until it is dropped, at its thread's end.
struct Unzipping(Arc<AtomicUsize>);

impl Unzipping {
    fn start(count: &Arc<AtomicUsize>) -> Self {
        count.fetch_add(1, Ordering::AcqRel);
        Unzipping(Arc::clone(count))
    }
}

impl Drop for Unzipping {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Downloads {
    pub fn progress(&self) -> Progress {
        self.lock().clone()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Progress> {
        self.progress.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Marks a download of what as started, or Err when one is already running.
    pub fn begin(&self, what: &str) -> Result<(), String> {
        let mut progress = self.lock();
        if let Progress::Running { what, .. } = &*progress {
            return Err(format!("{what} is still downloading. Wait for it to finish."));
        }
        if self.moving.load(Ordering::Acquire) {
            return Err("The models are being moved to another folder. Try again once that is done.".to_string());
        }
        if self.unzipping.load(Ordering::Acquire) > 0 {
            return Err("The stopped llama.cpp download is still being unzipped. Try again in a minute.".to_string());
        }
        *progress = Progress::Running {
            what: what.to_string(),
            done_bytes: 0,
            total_bytes: None,
        };
        Ok(())
    }

    /// Marks a download of what as started, as begin does, ended when the turn is dropped
    /// unless it was ended first: for a download whose caller may be dropped while it waits.
    pub fn begin_turn(&self, what: &str) -> Result<DownloadTurn<'_>, String> {
        self.begin(what)?;
        Ok(DownloadTurn {
            downloads: self,
            ended: false,
        })
    }

    /// Holds downloads off while the models are moved; Err while one is running or another move
    /// is. Checked under the same lock begin checks under, so neither slips past the other.
    pub fn begin_move(&self) -> Result<MoveTurn<'_>, String> {
        let progress = self.lock();
        if let Progress::Running { what, .. } = &*progress {
            return Err(format!("{what} is still downloading. Move the models once it is done."));
        }
        if self.moving.swap(true, Ordering::AcqRel) {
            return Err("The models are being moved already.".to_string());
        }
        Ok(MoveTurn(&self.moving))
    }

    /// Whether the models are being moved now.
    pub fn moving(&self) -> bool {
        self.moving.load(Ordering::Acquire)
    }

    fn set_bytes(&self, done_bytes: u64, total_bytes: Option<u64>) {
        if let Progress::Running {
            done_bytes: done,
            total_bytes: total,
            ..
        } = &mut *self.lock()
        {
            *done = done_bytes;
            *total = total_bytes;
        }
    }

    /// Unzips zip into folder on a thread of its own, counted as running until it is done (see
    /// unzipping), and deletes the zip after.
    pub(crate) async fn unzip(&self, zip: &Path, folder: &Path) -> Result<(), String> {
        let name = zip.file_name().unwrap_or_default().to_string_lossy().to_string();
        let (zip_path, into) = (zip.to_path_buf(), folder.to_path_buf());
        let unzipping = Unzipping::start(&self.unzipping);
        tokio::task::spawn_blocking(move || {
            let _unzipping = unzipping;
            if zip_path.to_string_lossy().ends_with(".tar.gz") {
                untar(&zip_path, &into)
            } else {
                unzip(&zip_path, &into)
            }
        })
        .await
        .map_err(|e| format!("Unzipping {name} stopped: {e}"))?
        .map_err(|e| format!("Couldn't unzip {name}: {e}"))?;
        if let Err(e) = std::fs::remove_file(zip) {
            crate::log_line!("{} couldn't be deleted after unzipping: {e}", zip.display());
        }
        Ok(())
    }

    /// Ends the running download with its outcome; a failure is logged.
    pub fn end(&self, outcome: Result<(), String>) {
        let mut progress = self.lock();
        let what = match &*progress {
            Progress::Running { what, .. } => what.clone(),
            _ => return,
        };
        *progress = match outcome {
            Ok(()) => Progress::Finished { what },
            Err(why) => {
                crate::log_line!("llama: downloading {what} failed: {why}");
                Progress::Failed { what, why }
            }
        };
    }
}

pub(crate) fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(CONNECT_SECONDS))
        .read_timeout(Duration::from_secs(STALL_SECONDS))
        .build()
        .map_err(|e| format!("The app couldn't make its HTTP client: {e}"))
}

/// Downloads llama.cpp's build for this computer into folder and unzips it there.
pub async fn download_server(folder: &Path, inventory: &Inventory, downloads: &Downloads) -> Result<(), String> {
    let client = client()?;
    let build = Build::for_computer(inventory);
    let (release, assets) = newest_release_with(&client, build).await?;
    // Every zip is unzipped into the staging folder first, and moved into the llama folder only
    // when all of them are, so a half-finished download never looks like an installed llama.cpp.
    let staging = folder.join(STAGING_FOLDER);
    if staging.exists() {
        std::fs::remove_dir_all(&staging).map_err(|e| format!("Couldn't clear {}: {e}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging).map_err(|e| format!("Couldn't make {}: {e}", staging.display()))?;
    for asset in &assets {
        let zip_path = folder.join(&asset.name);
        download_file(&client, &asset.browser_download_url, &zip_path, downloads).await?;
        downloads.unzip(&zip_path, &staging).await?;
    }
    if !staging.join(SERVER_FILE).is_file() {
        return Err(format!(
            "llama.cpp {} was unzipped, but there's no {SERVER_FILE} in it.",
            release.tag_name
        ));
    }
    move_into(&staging, folder).map_err(|e| format!("Couldn't move llama.cpp into {}: {e}", folder.display()))?;
    std::fs::write(folder.join("runtime-release.txt"), &release.tag_name)
        .map_err(|e| format!("Couldn't record the installed runtime version: {e}"))?;
    println!(
        "llama: llama.cpp {} ({}) is in {}.",
        release.tag_name,
        build.label(),
        folder.display()
    );
    Ok(())
}

/// The newest of the last RELEASES_SEARCHED releases with build's zips, and those zips.
async fn newest_release_with(client: &reqwest::Client, build: Build) -> Result<(Release, Vec<Asset>), String> {
    let program = format!("llama.cpp's {} {} build", std::env::consts::OS, build.name());
    newest_release_having(client, RELEASES, &program, |assets| {
        assets_for_target(build, assets, std::env::consts::OS, std::env::consts::ARCH)
    })
    .await
}

/// The newest of the last RELEASES_SEARCHED releases at releases (a GitHub API releases URL) whose
/// files pick finds, and the files it picked. program names what is looked for, for the messages.
pub(crate) async fn newest_release_having(
    client: &reqwest::Client,
    releases: &str,
    program: &str,
    pick: impl Fn(&[Asset]) -> Result<Vec<Asset>, String>,
) -> Result<(Release, Vec<Asset>), String> {
    let listed: Vec<Release> = get_json(client, &format!("{releases}?per_page={RELEASES_SEARCHED}")).await?;
    let mut skipped = Vec::new();
    for release in listed {
        match pick(&all_assets(client, releases, release.id).await?) {
            Ok(assets) => {
                if !skipped.is_empty() {
                    println!(
                        "{} has no {program} yet, so {} is used.",
                        skipped.join(", "),
                        release.tag_name
                    );
                }
                return Ok((release, assets));
            }
            Err(_) => skipped.push(release.tag_name),
        }
    }
    Err(format!("None of the last {RELEASES_SEARCHED} releases has {program}."))
}

/// Every file of a release, page by page.
async fn all_assets(client: &reqwest::Client, releases: &str, release_id: u64) -> Result<Vec<Asset>, String> {
    let mut assets = Vec::new();
    for page in 1.. {
        let url = format!("{releases}/{release_id}/assets?per_page={ASSETS_PER_PAGE}&page={page}");
        let listed: Vec<Asset> = get_json(client, &url).await?;
        let last_page = listed.len() < ASSETS_PER_PAGE;
        assets.extend(listed);
        if last_page {
            break;
        }
    }
    Ok(assets)
}

/// url's JSON answer from GitHub's API.
async fn get_json<T: serde::de::DeserializeOwned>(client: &reqwest::Client, url: &str) -> Result<T, String> {
    client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| format!("GitHub didn't answer {url}: {e}"))?
        .json()
        .await
        .map_err(|e| format!("GitHub's answer to {url} couldn't be read: {e}"))
}

/// Downloads model into models_folder.
pub async fn download_model(model: &CatalogModel, models_folder: &Path, downloads: &Downloads) -> Result<(), String> {
    std::fs::create_dir_all(models_folder).map_err(|e| format!("Couldn't make {}: {e}", models_folder.display()))?;
    download_file(&client()?, &model.url(), &models_folder.join(model.file), downloads).await?;
    println!("llama: {} is in {}.", model.file, models_folder.display());
    Ok(())
}

/// Downloads the distinct missing models within a single preparation turn.
pub(crate) async fn download_models(
    models: &[&CatalogModel],
    folder: &Path,
    downloads: &Downloads,
) -> Result<(), String> {
    if models.is_empty() {
        return Ok(());
    }
    let files: Vec<_> = models
        .iter()
        .map(|model| (model.url(), folder.join(model.file)))
        .collect();
    download_model_files(&client()?, &files, downloads).await
}

async fn download_model_files(
    client: &reqwest::Client,
    files: &[(String, PathBuf)],
    downloads: &Downloads,
) -> Result<(), String> {
    let progress = Mutex::new(vec![(0, None); files.len()]);
    downloads.set_bytes(0, None);
    let transfers = stream::iter((0..files.len()).map(|index| {
        let progress = &progress;
        async move {
            let (url, to) = &files[index];
            download_file_reporting(client, url, to, |done, total| {
                let mut progress = progress.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                progress[index] = (done, total);
                downloads.set_bytes(
                    progress.iter().map(|(done, _)| done).sum(),
                    progress.iter().map(|(_, total)| *total).sum(),
                );
            })
            .await
            .map_err(|why| format!("{}: {why}", to.display()))
        }
    }))
    .buffer_unordered(PARALLEL_MODEL_DOWNLOADS);
    // A failed transfer leaves its partial file for retry while the other models can finish.
    let failures: Vec<_> = transfers.filter_map(|result| async { result.err() }).collect().await;
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}

/// url's body into to, through to.part, counting bytes into downloads. Paused in the task
/// manager, it lets go of the connection (a server ends one that waits long) between two chunks,
/// and on resume asks for the rest (an HTTP range), or for all of it again when the server can't
/// send a part.
pub(crate) async fn download_file(
    client: &reqwest::Client,
    url: &str,
    to: &Path,
    downloads: &Downloads,
) -> Result<(), String> {
    download_file_reporting(client, url, to, |done, total| downloads.set_bytes(done, total)).await
}

async fn download_file_reporting(
    client: &reqwest::Client,
    url: &str,
    to: &Path,
    report: impl Fn(u64, Option<u64>),
) -> Result<(), String> {
    // Paused before this file (between two of a build's zips): it waits to be resumed first.
    running_work::until_resumed().await;
    if running_work::stop_asked() {
        return Err(Stopped::SAID.to_string());
    }
    let part = part_path(to);
    let metadata = part.with_extension("resume.json");
    let (mut done, mut version) = resume_from(url, to);
    let mut answer = get_from(client, url, done, version.as_ref()).await?;
    if answer.is_none() {
        done = 0;
        answer = get_from(client, url, 0, None).await?;
    }
    let Some(mut response) = answer else {
        return Err(format!("{url} couldn't be downloaded: the server sent nothing."));
    };
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        done = 0;
    }
    validate_range(&response, done)?;
    let mut total = response.content_length().map(|length| length + done);
    if let Some(total) = total {
        ensure_space(to.parent().unwrap_or(Path::new(".")), total.saturating_sub(done))?;
    }
    // What the server says this version of the file is, so a resume gets the rest of this one.
    version = version_of(&response);
    write_resume(&metadata, url, version.as_ref())?;
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(done == 0)
        .open(&part)
        .await
        .map_err(|e| format!("Couldn't write {}: {e}", part.display()))?;
    file.seek(io::SeekFrom::Start(done)).await.map_err(|e| e.to_string())?;
    report(done, total);
    loop {
        // All of it came already: the end is read from this answer, paused or not.
        let all_in = total.is_some_and(|total| done >= total);
        if running_work::paused() && !all_in {
            drop(response);
            running_work::until_resumed().await;
            if running_work::stop_asked() {
                return Err(Stopped::SAID.to_string());
            }
            let Some(rest) = get_from(client, url, done, version.as_ref()).await? else {
                // Nothing after done: it all came before the pause.
                break;
            };
            response = rest;
            validate_range(
                &response,
                if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                    done
                } else {
                    0
                },
            )?;
            if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
                // The whole file again: start over.
                file.set_len(0)
                    .await
                    .and(file.seek(io::SeekFrom::Start(0)).await.map(|_| ()))
                    .map_err(|e| format!("Couldn't write {}: {e}", part.display()))?;
                done = 0;
                total = response.content_length();
                version = version_of(&response);
                write_resume(&metadata, url, version.as_ref())?;
                report(done, total);
            }
        }
        let Some(piece) = response
            .chunk()
            .await
            .map_err(|e| format!("The download of {url} broke off: {e}"))?
        else {
            break;
        };
        file.write_all(&piece)
            .await
            .map_err(|e| format!("Couldn't write {}: {e}", part.display()))?;
        done += piece.len() as u64;
        report(done, total);
    }
    file.flush()
        .await
        .map_err(|e| format!("Couldn't write {}: {e}", part.display()))?;
    drop(file);
    if total.is_some_and(|total| total != done) {
        return Err(format!("The download of {url} ended early ({done} bytes)."));
    }
    if to.extension().is_some_and(|extension| extension == "gguf") {
        use std::io::Read;
        let mut magic = [0u8; 4];
        std::fs::File::open(&part)
            .and_then(|mut f| f.read_exact(&mut magic))
            .map_err(|e| e.to_string())?;
        if &magic != b"GGUF" {
            return Err("The download is not a GGUF model. Retry or choose another source.".to_string());
        }
    }
    tokio::fs::rename(&part, to)
        .await
        .map_err(|e| format!("Couldn't move {} to {}: {e}", part.display(), to.display()))?;
    let _ = std::fs::remove_file(metadata);
    report(done, Some(done));
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Resume {
    url: String,
    version: String,
}

/// Use the same recovery eligibility for the plan's storage estimate and the request.
fn resume_from(url: &str, to: &Path) -> (u64, Option<reqwest::header::HeaderValue>) {
    let part = part_path(to);
    let version = std::fs::read(part.with_extension("resume.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Resume>(&bytes).ok())
        .filter(|r| r.url == url && !r.version.is_empty() && !r.version.starts_with("W/"))
        .and_then(|r| reqwest::header::HeaderValue::from_str(&r.version).ok());
    let done = if version.is_some() {
        std::fs::metadata(part).map_or(0, |m| m.len())
    } else {
        0
    };
    (done, version)
}

/// A remote version change can still require a full transfer. download_file checks the
/// actual response's remaining size before writing, even after this optimistic estimate.
pub(crate) fn remaining_bytes(url: &str, to: &Path, full_bytes: u64) -> u64 {
    full_bytes.saturating_sub(resume_from(url, to).0)
}

fn write_resume(path: &Path, url: &str, version: Option<&reqwest::header::HeaderValue>) -> Result<(), String> {
    if let Some(version) = version.and_then(|v| v.to_str().ok()) {
        let value = serde_json::to_vec(&Resume {
            url: url.to_string(),
            version: version.to_string(),
        })
        .map_err(|e| e.to_string())?;
        std::fs::write(path, value).map_err(|e| format!("Couldn't save download recovery information: {e}"))?;
    } else {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

fn validate_range(response: &reqwest::Response, offset: u64) -> Result<(), String> {
    if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
        let range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if !range.starts_with(&format!("bytes {offset}-")) {
            return Err(
                "The server returned an unexpected download range. Remove the .part file and retry.".to_string(),
            );
        }
    }
    Ok(())
}

pub(crate) fn ensure_space(folder: &Path, bytes: u64) -> Result<(), String> {
    let available =
        fs2::available_space(folder).map_err(|e| format!("Couldn't check storage at {}: {e}", folder.display()))?;
    let required = bytes.saturating_add(512 * 1024 * 1024);
    if available < required {
        return Err(format!(
            "Not enough space in {}: need {} including a 0.5 GB reserve; {} available. Choose another models folder or free space.",
            folder.display(),
            super::hardware::gigabytes(required),
            super::hardware::gigabytes(available)
        ));
    }
    Ok(())
}

/// What a response says its file's version is, for If-Range: its ETag when that is a strong one,
/// else when it was last modified. None when it says neither.
fn version_of(response: &reqwest::Response) -> Option<reqwest::header::HeaderValue> {
    let headers = response.headers();
    let strong_etag = headers
        .get(reqwest::header::ETAG)
        .filter(|etag| !etag.as_bytes().starts_with(b"W/"));
    strong_etag
        .or_else(|| headers.get(reqwest::header::LAST_MODIFIED))
        .cloned()
}

/// The answer to a GET of url, from byte from on (the whole of it from 0); None when there is
/// nothing from there on (416, the file ends at from). From a byte on only while the file is
/// still version (If-Range): a file changed meanwhile (a model updated on Hugging Face) comes
/// whole (200), so its new end isn't added to the old start. Without a version it comes whole.
async fn get_from(
    client: &reqwest::Client,
    url: &str,
    from: u64,
    version: Option<&reqwest::header::HeaderValue>,
) -> Result<Option<reqwest::Response>, String> {
    let mut request = client.get(url);
    if from > 0
        && let Some(version) = version
    {
        request = request
            .header(reqwest::header::RANGE, format!("bytes={from}-"))
            .header(reqwest::header::IF_RANGE, version.clone());
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("{url} couldn't be downloaded: {e}"))?;
    if from > 0 && response.status() == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        return Ok(None);
    }
    response
        .error_for_status()
        .map(Some)
        .map_err(|e| format!("{url} couldn't be downloaded: {e}"))
}

fn part_path(to: &Path) -> PathBuf {
    let mut name = to.file_name().unwrap_or_default().to_os_string();
    name.push(PART_SUFFIX);
    to.with_file_name(name)
}

/// Moves everything in staging into folder, replacing what is there by the same name, and
/// removes staging.
pub(crate) fn move_into(staging: &Path, folder: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(staging)? {
        let entry = entry?;
        let target = folder.join(entry.file_name());
        if target.is_dir() {
            std::fs::remove_dir_all(&target)?;
        } else if target.exists() {
            std::fs::remove_file(&target)?;
        }
        std::fs::rename(entry.path(), &target)?;
    }
    std::fs::remove_dir(staging)
}

/// Unzips zip into folder, refusing entries that would land outside it.
fn unzip(zip: &Path, folder: &Path) -> io::Result<()> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip)?).map_err(io::Error::other)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(io::Error::other)?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{} would unzip outside the folder", entry.name().unwrap_or_default()),
            ));
        };
        let target = folder.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        ensure_space(folder, entry.size()).map_err(io::Error::other)?;
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        io::copy(&mut entry, &mut std::fs::File::create(&target)?)?;
    }
    Ok(())
}

/// Upstream tarballs have a release directory, bin/ and lib/. Flatten regular files into
/// the private runtime folder, with safe relative symlinks for versioned shared libraries.
fn untar(path: &Path, folder: &Path) -> io::Result<()> {
    let stream = flate2::read::GzDecoder::new(std::fs::File::open(path)?);
    let mut archive = tar::Archive::new(stream);
    let mut links = Vec::new();
    for entry in archive.entries()? {
        let mut entry = entry?;
        let relative = entry.path()?.into_owned();
        if relative.is_absolute()
            || relative
                .components()
                .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(io::Error::other("Runtime archive contains a path outside its folder."));
        }
        let Some(name) = relative.file_name() else { continue };
        let kind = entry.header().entry_type();
        let target = folder.join(name);
        if kind.is_file() {
            if target.exists() {
                return Err(io::Error::other("Duplicate runtime archive filename."));
            }
            ensure_space(folder, entry.size()).map_err(io::Error::other)?;
            io::copy(&mut entry, &mut std::fs::File::create(&target)?)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(entry.header().mode()? & 0o777))?;
            }
        } else if kind.is_symlink() {
            let link = entry
                .link_name()?
                .ok_or_else(|| io::Error::other("Missing archive link target"))?
                .into_owned();
            if link.components().count() != 1
                || !matches!(link.components().next(), Some(std::path::Component::Normal(_)))
            {
                return Err(io::Error::other("Runtime archive link must stay in its folder."));
            }
            links.push((target, link));
        } else if !kind.is_dir() {
            return Err(io::Error::other("Unsupported entry in runtime archive."));
        }
    }
    #[cfg(unix)]
    for (target, link) in links {
        std::os::unix::fs::symlink(link, target)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires JOBFINDER_TEST_RUNTIME_ARCHIVE pointing to an official downloaded Linux CPU archive"]
    fn official_linux_runtime_installs_and_starts() {
        let archive = std::env::var_os("JOBFINDER_TEST_RUNTIME_ARCHIVE").expect("archive path");
        let folder = std::env::temp_dir().join(format!("jobfinder-official-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        untar(Path::new(&archive), &folder).unwrap();
        let result = std::process::Command::new(folder.join(SERVER_FILE))
            .arg("--version")
            .env("LD_LIBRARY_PATH", &folder)
            .output()
            .unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn linux_assets_never_select_windows_and_require_matching_cuda_runtime() {
        let assets = [
            asset("llama-b12211-bin-win-vulkan-x64.zip"),
            asset("llama-b12211-bin-ubuntu-vulkan-x64.tar.gz"),
            asset("llama-b12211-bin-ubuntu-x64.tar.gz"),
            asset("llama-b12211-bin-ubuntu-cuda-12.8-x64.tar.gz"),
            asset("cudart-llama-b12211-bin-ubuntu-cuda-12.8-x64.tar.gz"),
        ];
        assert_eq!(
            names(&assets_for_target(Build::Vulkan, &assets, "linux", "x86_64").unwrap()),
            ["llama-b12211-bin-ubuntu-vulkan-x64.tar.gz"]
        );
        assert_eq!(
            assets_for_target(Build::Cuda, &assets, "linux", "x86_64")
                .unwrap()
                .len(),
            2
        );
        assert!(assets_for_target(Build::Cuda, &assets[..4], "linux", "x86_64").is_err());
        assert!(assets_for_target(Build::Cpu, &assets, "linux", "riscv64").is_err());
        assert!(assets_for_target(Build::Vulkan, &release(), "linux", "x86_64").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn linux_archive_flattens_the_release_and_preserves_executable_and_library_links() {
        use std::os::unix::fs::PermissionsExt;
        let folder = std::env::temp_dir().join(format!("jobfinder-tar-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("build.tar.gz");
        let gz = flate2::write::GzEncoder::new(std::fs::File::create(&path).unwrap(), flate2::Compression::default());
        let mut tar = tar::Builder::new(gz);
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o755);
        header.set_size(4);
        header.set_cksum();
        tar.append_data(&mut header, "llama-release/llama-server", &b"ELF!"[..])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(3);
        header.set_cksum();
        tar.append_data(&mut header, "llama-release/libggml.so.1", &b"lib"[..])
            .unwrap();
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        tar.append_link(&mut header, "llama-release/libggml.so", "libggml.so.1")
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        let into = folder.join("out");
        std::fs::create_dir_all(&into).unwrap();
        untar(&path, &into).unwrap();
        assert_eq!(std::fs::read(into.join("libggml.so")).unwrap(), b"lib");
        assert_ne!(
            std::fs::metadata(into.join("llama-server"))
                .unwrap()
                .permissions()
                .mode()
                & 0o111,
            0
        );
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[tokio::test]
    async fn model_downloads_overlap_and_a_failed_file_does_not_discard_a_success() {
        use axum::{Router, extract::Path as RoutePath, routing::get};

        let both_requested = Arc::new(tokio::sync::Barrier::new(2));
        let app = Router::new().route(
            "/{name}",
            get(move |RoutePath(name): RoutePath<String>| {
                let both_requested = Arc::clone(&both_requested);
                async move {
                    both_requested.wait().await;
                    if name == "good" { "GGUF model" } else { "bad file" }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let folder = std::env::temp_dir().join(format!("jobfinder-parallel-models-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let files = [
            (format!("http://{address}/good"), folder.join("good.gguf")),
            (format!("http://{address}/bad"), folder.join("bad.gguf")),
        ];
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let downloads = Downloads::default();
        let turn = downloads.begin_turn("Preparing task models").unwrap();
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            download_model_files(&client, &files, &downloads),
        )
        .await
        .expect("the server answers only once both requests arrive");
        assert!(result.as_ref().unwrap_err().contains("bad.gguf"));
        assert_eq!(std::fs::read_to_string(&files[0].1).unwrap(), "GGUF model");
        assert!(!files[1].1.exists());
        assert!(part_path(&files[1].1).exists());
        assert!(matches!(
            downloads.progress(),
            Progress::Running {
                done_bytes: 18,
                total_bytes: Some(18),
                ..
            }
        ));
        turn.end(result);
        task.abort();
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[tokio::test]
    async fn interrupted_download_resumes_after_restart_and_changed_versions_restart_cleanly() {
        use axum::{
            Router,
            http::{HeaderMap, StatusCode},
            routing::get,
        };
        let app = Router::new()
            .route(
                "/same",
                get(|headers: HeaderMap| async move {
                    assert_eq!(headers.get("range").unwrap(), "bytes=4-");
                    assert_eq!(headers.get("if-range").unwrap(), "\"v1\"");
                    (
                        StatusCode::PARTIAL_CONTENT,
                        [("etag", "\"v1\""), ("content-range", "bytes 4-7/8")],
                        "efgh",
                    )
                }),
            )
            .route("/changed", get(|| async { ([("etag", "\"v2\"")], "new file") }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let folder = std::env::temp_dir().join(format!("jobfinder-resume-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        for (route, expected) in [("same", "abcdefgh"), ("changed", "new file")] {
            let to = folder.join(route);
            let part = part_path(&to);
            let url = format!("http://{address}/{route}");
            std::fs::write(&part, "abcd").unwrap();
            write_resume(
                &part.with_extension("resume.json"),
                &url,
                Some(&reqwest::header::HeaderValue::from_static("\"v1\"")),
            )
            .unwrap();
            assert_eq!(
                remaining_bytes(&url, &to, 8),
                4,
                "credit the same prefix the request resumes"
            );
            download_file(&client, &url, &to, &Downloads::default()).await.unwrap();
            assert_eq!(std::fs::read_to_string(&to).unwrap(), expected);
            assert!(!part.exists());
        }
        task.abort();
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn partial_credit_requires_matching_usable_recovery_metadata() {
        let folder = std::env::temp_dir().join(format!("jobfinder-partial-credit-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let to = folder.join("model.gguf");
        let part = part_path(&to);
        let metadata = part.with_extension("resume.json");
        let url = "https://example.com/model.gguf";
        std::fs::write(&part, "GGUF").unwrap();
        assert_eq!(remaining_bytes(url, &to, 8), 8);
        for value in [
            r#"{"url":"https://example.com/other.gguf","version":"\"v1\""}"#,
            r#"{"url":"https://example.com/model.gguf","version":""}"#,
            r#"{"url":"https://example.com/model.gguf","version":"W/\"v1\""}"#,
            "broken json",
        ] {
            std::fs::write(&metadata, value).unwrap();
            assert_eq!(remaining_bytes(url, &to, 8), 8);
        }
        write_resume(
            &metadata,
            url,
            Some(&reqwest::header::HeaderValue::from_static("\"v1\"")),
        )
        .unwrap();
        assert_eq!(remaining_bytes(url, &to, 8), 4);
        std::fs::remove_file(part).unwrap();
        assert_eq!(remaining_bytes(url, &to, 8), 8);
        std::fs::remove_dir_all(folder).unwrap();
    }

    #[test]
    fn no_download_starts_while_a_stopped_ones_unzip_runs() {
        let downloads = Downloads::default();
        let unzipping = Unzipping::start(&downloads.unzipping);
        assert!(downloads.begin("llama.cpp").is_err());
        drop(unzipping);
        assert!(downloads.begin("llama.cpp").is_ok());
    }

    #[test]
    fn a_download_whose_caller_is_dropped_ends_so_the_next_can_begin() {
        let downloads = Downloads::default();
        let turn = downloads.begin_turn("gemma.gguf").unwrap();
        assert!(downloads.begin("qwen.gguf").is_err());
        drop(turn);
        assert!(matches!(downloads.progress(), Progress::Failed { .. }));
        let turn = downloads.begin_turn("gemma.gguf").unwrap();
        turn.end(Ok(()));
        assert!(matches!(downloads.progress(), Progress::Finished { .. }));
    }

    #[test]
    fn no_download_starts_while_the_models_move_and_no_move_while_one_runs() {
        let downloads = Downloads::default();
        let moving = downloads.begin_move().unwrap();
        assert!(downloads.moving());
        assert!(downloads.begin("gemma.gguf").is_err());
        assert!(downloads.begin_move().is_err(), "one move at a time");
        drop(moving);
        assert!(!downloads.moving());
        downloads.begin("gemma.gguf").unwrap();
        assert!(downloads.begin_move().is_err());
    }
    use crate::llama::hardware::{GIB, Gpu};

    fn asset(name: &str) -> Asset {
        Asset {
            name: name.to_string(),
            browser_download_url: format!("https://github.com/ggml-org/llama.cpp/releases/download/b7000/{name}"),
        }
    }

    fn release() -> Vec<Asset> {
        [
            "cudart-llama-bin-win-cuda-12.4-x64.zip",
            "cudart-llama-bin-win-cuda-13.1-x64.zip",
            "llama-b7000-bin-macos-arm64.zip",
            "llama-b7000-bin-win-cpu-arm64.zip",
            "llama-b7000-bin-win-cpu-x64.zip",
            "llama-b7000-bin-win-cuda-12.4-x64.zip",
            "llama-b7000-bin-win-cuda-13.1-x64.zip",
            "llama-b7000-bin-win-hip-radeon-x64.zip",
            "llama-b7000-bin-win-vulkan-x64.zip",
        ]
        .into_iter()
        .map(asset)
        .collect()
    }

    fn names(assets: &[Asset]) -> Vec<&str> {
        assets.iter().map(|asset| asset.name.as_str()).collect()
    }

    #[test]
    fn each_card_gets_its_build() {
        let card = |vendor| Inventory {
            memory_bytes: 32 * GIB,
            gpu: Some(Gpu {
                name: "a card".to_string(),
                vendor,
                vram_bytes: 8 * GIB,
            }),
        };
        assert_eq!(Build::for_computer(&card(GpuVendor::Amd)), Build::Vulkan);
        assert_eq!(Build::for_computer(&card(GpuVendor::Nvidia)), Build::Cuda);
        assert_eq!(Build::for_computer(&Inventory::default()), Build::Cpu);
        assert_eq!(
            names(&assets_for(Build::Vulkan, &release()).unwrap()),
            ["llama-b7000-bin-win-vulkan-x64.zip"]
        );
        assert_eq!(
            names(&assets_for(Build::Cpu, &release()).unwrap()),
            ["llama-b7000-bin-win-cpu-x64.zip"]
        );
        assert_eq!(
            names(&assets_for(Build::Cuda, &release()).unwrap()),
            [
                "llama-b7000-bin-win-cuda-13.1-x64.zip",
                "cudart-llama-bin-win-cuda-13.1-x64.zip"
            ]
        );
        assert_eq!(
            assets_for(Build::Vulkan, &[]).unwrap_err(),
            "There's no Windows Vulkan build of llama.cpp in the release."
        );
        let b12020: Vec<Asset> = [
            "llama-b12020-bin-win-cpu-x64.zip",
            "llama-b12020-bin-win-rocm-10.0-x64.zip",
            "llama-b12020-bin-win-vulkan-x64.zip",
        ]
        .into_iter()
        .map(asset)
        .collect();
        assert_eq!(
            names(&assets_for(Build::Vulkan, &b12020).unwrap()),
            ["llama-b12020-bin-win-vulkan-x64.zip"]
        );
    }

    #[test]
    fn one_download_runs_at_a_time() {
        let downloads = Downloads::default();
        downloads.begin("llama.cpp").unwrap();
        assert!(downloads.begin("Qwen3-8B-Q4_K_M.gguf").is_err());
        downloads.end(Err("offline".to_string()));
        assert_eq!(
            downloads.progress(),
            Progress::Failed {
                what: "llama.cpp".to_string(),
                why: "offline".to_string()
            }
        );
        downloads.begin("Qwen3-8B-Q4_K_M.gguf").unwrap();
    }

    #[test]
    fn a_staged_install_replaces_the_old_one() {
        let folder = std::env::temp_dir().join(format!("jobfinder-staging-{}", std::process::id()));
        let staging = folder.join(STAGING_FOLDER);
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(folder.join(SERVER_FILE), b"old").unwrap();
        std::fs::write(staging.join(SERVER_FILE), b"new").unwrap();
        std::fs::write(staging.join("ggml-vulkan.dll"), b"dll").unwrap();
        move_into(&staging, &folder).unwrap();
        assert_eq!(std::fs::read(folder.join(SERVER_FILE)).unwrap(), b"new");
        assert!(folder.join("ggml-vulkan.dll").is_file());
        assert!(!staging.exists());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn a_zip_unzips_into_the_folder_only() {
        let folder = std::env::temp_dir().join(format!("jobfinder-unzip-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let zip_path = folder.join("build.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        writer.start_file("llama-server.exe", options).unwrap();
        writer.write_all(b"MZ").unwrap();
        writer.finish().unwrap();
        unzip(&zip_path, &folder).unwrap();
        assert_eq!(std::fs::read(folder.join("llama-server.exe")).unwrap(), b"MZ");

        let evil_path = folder.join("evil.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&evil_path).unwrap());
        writer.start_file("../outside.txt", options).unwrap();
        writer.finish().unwrap();
        assert!(unzip(&evil_path, &folder).is_err());
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
