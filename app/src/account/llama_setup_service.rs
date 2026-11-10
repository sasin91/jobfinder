//! The setup wizard for this app's own model, outside the web page, so a desktop window can do
//! the same from the same functions: the llama.cpp build for this computer's graphics card, the
//! catalog's models with how each fits (model_choice, penny's weighing), the one download that
//! runs in the background, and saving a downloaded model as the "llama_cpp" key.

use std::sync::Arc;

use super::account_model;
use crate::context::Context;
use crate::llama::catalog::{self, CatalogModel};
use crate::llama::download::{self, Build, Progress};
use crate::llama::hardware;
use crate::llama::llama_server::LlamaServer;
use crate::llama::model_choice::{self, Fit};
use crate::llm::LLAMA_CPP;
use crate::service_error::{ErrorKind, ServiceError};
use crate::setup;
use crate::task_manager::running_work::{Stopped, WorkKind};

/// What the download of llama.cpp is called while it runs.
const SERVER_DOWNLOAD: &str = "llama.cpp";

/// How often the page asks how the download is going, in milliseconds.
pub const POLL_MILLISECONDS: u32 = 1000;

/// Percent, for the download's progress and the card's share of a model.
const PERCENT: u64 = 100;

/// The share of a model's layers that must be on the graphics card for it to be suggested as
/// this app's own model, in percent: with less, most of it runs on the CPU, which is slow.
const MIN_CARD_SHARE_PERCENT: u64 = 50;

/// A catalog model as the wizard lists it.
#[derive(Clone)]
pub struct CatalogRow {
    pub file: &'static str,
    pub label: &'static str,
    /// Its size, as "4.7 GB".
    pub size: String,
    /// How it would fit, as model_choice::Fit::label says.
    pub fit: String,
    /// Too big for this computer: it can't be downloaded here.
    pub too_big: bool,
    /// The biggest of the best fit among the models every task can run on: the one to take.
    pub recommended: bool,
    /// In the models folder already.
    pub downloaded: bool,
    /// Good enough for the AI match (catalog::is_good_enough), a model every task can run on, and
    /// whole or partly on the graphics card, so it answers in reasonable time.
    pub good_enough: bool,
}

/// The running or last download.
pub struct DownloadView {
    pub what: String,
    /// "1.2 GB of 5.0 GB (24%)", or "" when it isn't running.
    pub progress: String,
    /// How much is downloaded, in percent, when the size is known.
    pub percent: Option<u64>,
    pub running: bool,
    pub finished: bool,
    pub why_failed: Option<String>,
}

/// The wizard's data.
pub struct LlamaSetupView {
    pub plan: Option<super::model_plan::Overview>,
    pub computer: String,
    pub models_folder: String,
    pub server_found: bool,
    /// The llama-server that runs: the llama folder's, or one on PATH.
    pub server_path: String,
    /// Which llama.cpp build this computer gets.
    pub build_label: &'static str,
    pub card_problem: Option<String>,
    pub download: Option<DownloadView>,
    /// Whether a download is running, so the buttons wait.
    pub busy: bool,
    /// The model to take, when one fits this computer.
    pub recommended: Option<CatalogRow>,
    /// The models that fit and aren't downloaded, for the download list (the recommended one
    /// selected).
    pub to_download: Vec<CatalogRow>,
    /// The downloaded models, for the list of the one to use (the recommended one selected).
    pub downloaded: Vec<CatalogRow>,
    /// The setup wizard isn't finished, so the page links back to it.
    pub setup_unfinished: bool,
}

/// A model saved as this app's own: what to say, and whether the setup wizard is still to finish.
#[derive(Debug, PartialEq)]
pub struct ModelSaved {
    pub message: String,
    pub setup_unfinished: bool,
}

/// The llama folder's server, or why there is none.
fn server_of(context: &Context) -> Result<&LlamaServer, ServiceError> {
    context
        .llama
        .as_ref()
        .ok_or_else(|| ServiceError::internal("llama_setup", "The app was started without its llama folder."))
}

/// The wizard's data for this computer.
pub async fn llama_setup_view(context: &Context) -> Result<LlamaSetupView, ServiceError> {
    let server = server_of(context)?;
    let mut view = view_of(server);
    view.plan = Some(super::model_plan::overview(context).await?);
    view.setup_unfinished = setup::first_run_or_logged(context).await;
    Ok(view)
}

/// Downloads llama.cpp's build for this card in the background.
pub fn download_server(context: &Arc<Context>) -> Result<(), ServiceError> {
    start_download(context, SERVER_DOWNLOAD, |context: Arc<Context>| async move {
        let server = server_of(&context).map_err(|e| e.message)?;
        download::download_server(server.folder(), &server.inventory(), &server.downloads).await
    })
}

/// Downloads a catalog model in the background.
pub fn download_model(context: &Arc<Context>, file: &str) -> Result<(), ServiceError> {
    let model = catalog::find(file)
        .ok_or_else(|| ServiceError::new(ErrorKind::Invalid, format!("{file} isn't one of the models offered.")))?;
    start_download(context, model.file, move |context: Arc<Context>| async move {
        let server = server_of(&context).map_err(|e| e.message)?;
        download::download_model(model, &server.models_folder(), &server.downloads).await
    })
}

/// Downloads file, a catalog model, when it isn't in the models folder yet, and waits for it: a
/// task's default model comes down the first time the task needs it. A file the catalog doesn't
/// offer is left to llama-server's start to say it is missing.
pub async fn download_if_missing(context: &Context, file: &str) -> Result<(), ServiceError> {
    let Some(server) = &context.llama else {
        return Ok(());
    };
    let Some(model) = catalog::find(file) else {
        return Ok(());
    };
    let _one_at_a_time = server.fetching.lock().await;
    if server
        .models()
        .iter()
        .any(|downloaded| downloaded.name.eq_ignore_ascii_case(file))
    {
        return Ok(());
    }
    crate::log_line!("llama: {file} isn't downloaded yet; downloading it now");
    // Ended here, or on drop when this call is dropped while it waits (a scan's time limit).
    let turn = server
        .downloads
        .begin_turn(model.file)
        .map_err(|why| ServiceError::new(ErrorKind::TooMany, why))?;
    let outcome = context
        .running
        .run(
            WorkKind::Download,
            model.file.to_string(),
            download::download_model(model, &server.models_folder(), &server.downloads),
        )
        .await
        .unwrap_or_else(|Stopped| Err(Stopped::SAID.to_string()));
    turn.end(outcome.clone());
    outcome.map_err(|why| ServiceError::new(ErrorKind::Internal, format!("{file} couldn't be downloaded: {why}")))
}

/// Moves the models to the folder to (a bigger disk, say) and keeps them there from now on;
/// says how it went.
pub async fn move_models(
    context: &Context,
    to: std::path::PathBuf,
    mut report: impl FnMut(&str) + Send + 'static,
) -> Result<String, ServiceError> {
    let server = server_of(context)?;
    let moved = server
        .move_models(to.clone(), move |file, index, total| {
            report(&format!("Moving model {index} of {total} · {}", file.to_string_lossy()));
        })
        .await
        .map_err(|why| ServiceError::internal("llama_setup (move models)", why))?;
    let models = if moved == 1 { "model" } else { "models" };
    Ok(format!(
        "Moved {moved} {models}. Models are kept in {} from now on.",
        to.display()
    ))
}

/// Saves the downloaded model file as this app's own model.
pub async fn use_model(context: &Context, file: &str) -> Result<ModelSaved, ServiceError> {
    let server = server_of(context)?;
    let downloaded = server.models().iter().any(|model| model.name == file);
    if !downloaded {
        return Err(ServiceError::new(
            ErrorKind::Invalid,
            format!("{file} isn't in the models folder yet. Download it first."),
        ));
    }
    account_model::save(context.db(), &context.key_vault, LLAMA_CPP, file, "", "")
        .await
        .map_err(|e| ServiceError::internal("llama_setup", format!("The model couldn't be saved: {e}")))?;
    let setup_unfinished = setup::first_run_or_logged(context).await;
    let message = if setup_unfinished {
        format!("This app's own model runs on {file}. It starts on the first AI call, which reads your CV below.")
    } else {
        format!("This app's own model runs on {file}. Pick below what it is used for, and press Check to load it.")
    };
    Ok(ModelSaved {
        message,
        setup_unfinished,
    })
}

/// Starts work as the one download, called what, unless one is running.
fn start_download<Work, Done>(context: &Arc<Context>, what: &str, work: Work) -> Result<(), ServiceError>
where
    Work: FnOnce(Arc<Context>) -> Done + Send + 'static,
    Done: Future<Output = Result<(), String>> + Send,
{
    let server = server_of(context)?;
    server
        .downloads
        .begin(what)
        .map_err(|why| ServiceError::new(ErrorKind::TooMany, why))?;
    let context = Arc::clone(context);
    let what = what.to_string();
    tokio::spawn(async move {
        let list = Arc::clone(&context.running);
        let outcome = list
            .run(WorkKind::Download, what, work(Arc::clone(&context)))
            .await
            .unwrap_or_else(|Stopped| Err(Stopped::SAID.to_string()));
        if let Some(server) = &context.llama {
            server.downloads.end(outcome);
        }
    });
    Ok(())
}

/// The wizard's data for the computer server runs on; setup_unfinished is left false.
pub fn view_of(server: &LlamaServer) -> LlamaSetupView {
    let overview = server.overview();
    let card_usable = card_usable_for_weighing(server);
    let inventory = server.inventory();
    let models_folder = server.models_folder();
    let download = download_view(&server.downloads.progress());
    let busy = server.downloads.moving() || download.as_ref().is_some_and(|download| download.running);
    let rows = catalog_rows(server, card_usable);
    let good_model_fits_card = rows.iter().any(|row| row.good_enough);
    let ModelLists {
        recommended,
        to_download,
        downloaded,
    } = model_lists(rows);
    LlamaSetupView {
        plan: None,
        computer: overview.inventory.to_string(),
        models_folder: models_folder.display().to_string(),
        server_found: overview.server.is_some(),
        server_path: overview
            .server
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        build_label: Build::for_computer(&inventory).label(),
        card_problem: card_problem(server, overview.card_problem, good_model_fits_card),
        download,
        busy,
        recommended,
        to_download,
        downloaded,
        setup_unfinished: false,
    }
}

/// The catalog, each model weighed against this computer, with the one to take marked: never
/// one picked by hand for a task (catalog::CatalogModel::fits_every_task).
fn catalog_rows(server: &LlamaServer, card_usable: bool) -> Vec<CatalogRow> {
    let models_folder = server.models_folder();
    let inventory = server.inventory();
    let as_files: Vec<_> = catalog::MODELS
        .iter()
        .map(|model| model.as_gguf(&models_folder))
        .collect();
    let for_every_task: Vec<_> = catalog::MODELS
        .iter()
        .zip(&as_files)
        .filter(|(model, _)| model.fits_every_task)
        .map(|(_, file)| file.clone())
        .collect();
    let recommended = model_choice::choose(&for_every_task, "", &inventory, card_usable)
        .map(|(model, _)| model.name.clone())
        .ok();
    let downloaded: Vec<String> = server.models().into_iter().map(|model| model.name).collect();
    let rows = catalog::MODELS
        .iter()
        .zip(&as_files)
        .map(|(model, file): (&CatalogModel, _)| {
            let fit = model_choice::fit(file, &inventory, card_usable);
            CatalogRow {
                file: model.file,
                label: model.label,
                size: hardware::gigabytes(model.bytes),
                fit: fit.label(),
                too_big: fit == Fit::TooBig,
                recommended: recommended.as_deref() == Some(model.file),
                downloaded: downloaded.iter().any(|name| name == model.file),
                good_enough: model.fits_every_task
                    && catalog::is_good_enough(model)
                    && runs_mostly_on_the_card(fit, model.layers),
            }
        })
        .collect();
    recommend_a_good_one(rows)
}

/// Moves the recommendation to the smallest good-enough model (the catalog lists the smallest
/// first), so the wizard downloads the model the API key page and the setup suggest: the model
/// bake-off found the bigger ones answer no better, only slower. Without a good-enough one, the
/// smallest one good enough that runs at all (on the CPU, as the bake-off ran Qwen3 4B at about
/// a minute per post) is recommended; without that either, the best fit stays recommended.
fn recommend_a_good_one(mut rows: Vec<CatalogRow>) -> Vec<CatalogRow> {
    let on_the_card = rows.iter().position(|row| row.good_enough);
    let Some(good) = on_the_card.or_else(|| rows.iter().position(runs_well_enough_anywhere)) else {
        return rows;
    };
    for (index, row) in rows.iter_mut().enumerate() {
        row.recommended = index == good;
    }
    rows
}

/// Whether row is a model every task can run on, good enough for the AI match, that runs on this
/// computer at all.
fn runs_well_enough_anywhere(row: &CatalogRow) -> bool {
    !row.too_big && catalog::find(row.file).is_some_and(|model| model.fits_every_task && catalog::is_good_enough(model))
}

/// Whether a model of layers with this fit has at least MIN_CARD_SHARE_PERCENT of them on the
/// graphics card, so it answers in reasonable time.
fn runs_mostly_on_the_card(fit: Fit, layers: u32) -> bool {
    match fit {
        Fit::Gpu => true,
        Fit::Split { gpu_layers } => u64::from(gpu_layers) * PERCENT >= u64::from(layers) * MIN_CARD_SHARE_PERCENT,
        Fit::Memory | Fit::TooBig => false,
    }
}

/// The model to suggest as this app's own: the recommended one (recommend_a_good_one), when it
/// is good enough; None when this computer has no room for a good model, so an OpenAI
/// or Claude key is the better suggestion.
pub fn good_local_model(server: &LlamaServer) -> Option<CatalogRow> {
    let rows = catalog_rows(server, card_usable_for_weighing(server));
    rows.into_iter().find(|row| row.recommended && row.good_enough)
}

/// Whether the card counts when models are weighed: whenever there is one, since the build the
/// wizard downloads for it (CUDA for NVIDIA, Vulkan for any other card) drives it, even when the
/// llama-server there now lacks its compute library (card_problem then says to download it).
fn card_usable_for_weighing(server: &LlamaServer) -> bool {
    server.inventory().gpu.is_some()
}

/// Why the llama-server there can't drive the card, so the wizard offers its build: llama_server's
/// own complaint for an NVIDIA or AMD card, and for an Intel or other card (which llama_server
/// quietly runs without) when the card is big enough for a good model. None when it can.
fn card_problem(server: &LlamaServer, overview_problem: Option<String>, good_model_fits_card: bool) -> Option<String> {
    if overview_problem.is_some() {
        return overview_problem;
    }
    let inventory = server.inventory();
    let gpu = inventory.gpu.as_ref()?;
    let driven = server.server_path().is_none() || server.card_usable().unwrap_or(false);
    if driven || !good_model_fits_card {
        return None;
    }
    Some(format!(
        "The llama-server here can't use {}, so models would run on the CPU.",
        gpu.name
    ))
}

/// The catalog as the page lists it: the recommended model, the ones still to download, and the
/// downloaded ones. A model too big for this computer is in neither list.
struct ModelLists {
    recommended: Option<CatalogRow>,
    to_download: Vec<CatalogRow>,
    downloaded: Vec<CatalogRow>,
}

fn model_lists(rows: Vec<CatalogRow>) -> ModelLists {
    let recommended = rows.iter().find(|row| row.recommended).cloned();
    let (downloaded, to_download): (Vec<CatalogRow>, Vec<CatalogRow>) = rows
        .into_iter()
        .filter(|row| !row.too_big)
        .partition(|row| row.downloaded);
    ModelLists {
        recommended,
        to_download,
        downloaded,
    }
}

fn download_view(progress: &Progress) -> Option<DownloadView> {
    let view = |what: &str| DownloadView {
        what: what.to_string(),
        progress: String::new(),
        percent: None,
        running: false,
        finished: false,
        why_failed: None,
    };
    match progress {
        Progress::Idle => None,
        Progress::Running {
            what,
            done_bytes,
            total_bytes,
        } => Some(DownloadView {
            progress: progress_text(*done_bytes, *total_bytes),
            percent: percent_of(*done_bytes, *total_bytes),
            running: true,
            ..view(what)
        }),
        Progress::Finished { what } => Some(DownloadView {
            finished: true,
            ..view(what)
        }),
        Progress::Failed { what, why } => Some(DownloadView {
            why_failed: Some(why.clone()),
            ..view(what)
        }),
    }
}

/// "1.2 GB of 5.0 GB (24%)", or "1.2 GB" when the size isn't known.
fn progress_text(done_bytes: u64, total_bytes: Option<u64>) -> String {
    match (total_bytes, percent_of(done_bytes, total_bytes)) {
        (Some(total), Some(percent)) => format!(
            "{} of {} ({percent}%)",
            hardware::gigabytes(done_bytes),
            hardware::gigabytes(total)
        ),
        _ => hardware::gigabytes(done_bytes),
    }
}

/// done_bytes as a percent of total_bytes, when that is known and not 0.
fn percent_of(done_bytes: u64, total_bytes: Option<u64>) -> Option<u64> {
    total_bytes
        .filter(|total| *total > 0)
        .map(|total| done_bytes * PERCENT / total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llama::hardware::{GIB, Gpu, GpuVendor, Inventory};

    #[test]
    fn the_progress_says_how_much_of_how_much() {
        assert_eq!(progress_text(GIB, Some(4 * GIB)), "1.0 GB of 4.0 GB (25%)");
        assert_eq!(progress_text(GIB, None), "1.0 GB");
    }

    #[test]
    fn an_8_gb_card_is_offered_qwen3_4b_which_the_bake_off_found_as_good_as_the_8b() {
        // Jonas's PC: 31.1 GB memory, AMD Radeon RX 7600 with 7.9 GB.
        let inventory = Inventory {
            memory_bytes: 311 * GIB / 10,
            gpu: Some(Gpu {
                name: "AMD Radeon RX 7600".to_string(),
                vendor: GpuVendor::Amd,
                vram_bytes: 79 * GIB / 10,
            }),
        };
        let folder = std::env::temp_dir().join("jobfinder-llama-setup-test");
        let server = LlamaServer::new(&folder, inventory);
        let page = view_of(&server);
        assert_eq!(page.recommended.as_ref().map(|row| row.file), Some(catalog::QWEN3_4B));
        let offered: Vec<&str> = page.to_download.iter().map(|row| row.file).collect();
        assert_eq!(
            offered.len(),
            catalog::MODELS.len(),
            "every model fits a 7.9 GB card one way or another: {offered:?}"
        );
        assert!(page.downloaded.is_empty());
        assert_eq!(page.build_label, Build::Vulkan.label());
        assert_eq!(page.card_problem, None);
    }

    #[test]
    fn a_computer_without_room_on_its_card_for_a_good_model_gets_none() {
        let folder = std::env::temp_dir().join("jobfinder-llama-advice-test");
        let card = |vram_tenths: u64| Inventory {
            memory_bytes: 16 * GIB,
            gpu: Some(Gpu {
                name: "A card".to_string(),
                vendor: GpuVendor::Nvidia,
                vram_bytes: vram_tenths * GIB / 10,
            }),
        };
        let jonas = LlamaServer::new(&folder, card(79));
        assert_eq!(good_local_model(&jonas).map(|row| row.file), Some(catalog::QWEN3_4B));
        // Only the 1.7B fits whole on a 4 GB card; under half of the 4B fits on it.
        let small_card = LlamaServer::new(&folder, card(40));
        assert_eq!(good_local_model(&small_card).map(|row| row.file), None);
        // On 5 GB most of the 4B is on the card, though the 1.7B is the recommended fit.
        let five_gb = LlamaServer::new(&folder, card(50));
        assert_eq!(
            good_local_model(&five_gb).map(|row| row.file),
            Some("Qwen3-4B-Q4_K_M.gguf")
        );
        // The download wizard recommends the same one.
        let page = view_of(&five_gb);
        assert_eq!(page.recommended.map(|row| row.file), Some("Qwen3-4B-Q4_K_M.gguf"));
        let no_card = LlamaServer::new(
            &folder,
            Inventory {
                memory_bytes: 16 * GIB,
                gpu: None,
            },
        );
        assert_eq!(good_local_model(&no_card).map(|row| row.file), None);
    }

    #[test]
    fn an_intel_card_counts_and_a_llama_server_that_cant_drive_it_is_said() {
        let folder = std::env::temp_dir().join(format!("jobfinder-llama-intel-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let arc = Inventory {
            memory_bytes: 32 * GIB,
            gpu: Some(Gpu {
                name: "Intel Arc A770".to_string(),
                vendor: GpuVendor::Intel,
                vram_bytes: 16 * GIB,
            }),
        };
        let server = LlamaServer::new(&folder, arc);
        // Before llama.cpp is downloaded.
        assert!(good_local_model(&server).is_some());
        // A CPU-only llama-server: still weighed for the card, with a note to download its build.
        std::fs::write(folder.join(crate::llama::llama_server::SERVER_FILE), b"cpu build").unwrap();
        assert!(good_local_model(&server).is_some());
        let page = view_of(&server);
        assert_eq!(
            page.card_problem.as_deref(),
            Some("The llama-server here can't use Intel Arc A770, so models would run on the CPU.")
        );
        std::fs::remove_dir_all(&folder).unwrap();
    }
}
