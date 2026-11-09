//! llama: a GGUF model run by llama.cpp's llama-server, which the app starts itself (the
//! "llama_cpp" provider on the API key page). Its folder is `llama` next to the database unless
//! `--llama` names another; it holds llama-server.exe with its DLLs, unzipped from a llama.cpp
//! release, and models\*.gguf.
//!
//! The setup wizard (account::llama_setup) downloads both: the llama.cpp build for this
//! computer's card (download), and a model from the catalog that fits it.
//!
//! hardware reads the computer's memory and graphics card, model_choice weighs each model
//! against them, and llama_server starts the server with the chosen model, switching to another
//! model only once the calls under way have finished (model_gate). Its API is then the
//! openai adapter's local server, so every AI feature runs on it through the one code path.

pub mod catalog;
pub mod download;
pub mod gguf;
pub mod hardware;
#[cfg(not(windows))]
mod linux_hardware;
pub mod llama_server;
pub mod model_choice;
pub mod model_gate;
pub mod models_folder;

/// The llama folder's name next to the database.
pub const FOLDER: &str = "llama";

/// The file names of the models in the models folder; none without a llama folder.
pub fn model_files(server: Option<&llama_server::LlamaServer>) -> Vec<String> {
    server.map_or_else(Vec::new, |server| {
        server.models().into_iter().map(|model| model.name).collect()
    })
}
