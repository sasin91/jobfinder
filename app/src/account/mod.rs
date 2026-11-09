//! account: the API keys and models the one local user's AI work runs on.
//! The AI features of the CV match run on the user's own OpenAI or Anthropic API keys, or on a
//! local OpenAI-compatible server (Ollama, LM Studio), or on a GGUF model the app runs itself with
//! llama.cpp (the llama module), which needs no key. One key is kept per provider, sealed in the
//! database's secrets table (key_vault), and never shown again; only its last 4 characters are.
//! Each use (tailoring résumés, writing applications, evaluating job posts) runs on the key picked
//! for it, or on none, and each task of a use on the model picked for it (task_model), or on the
//! key's own.
//!
//! The work is in account_service, which the Language models and Local model screens call.
//! There is no account on the desktop, so no profile links, data download or account deletion.

pub mod account_model;
pub mod account_service;
pub mod data_key;
pub mod former_keys;
pub mod key_vault;
pub mod llama_setup_service;
pub mod model_advice;
pub mod model_plan;
pub mod saved_key;
pub mod task_model;
pub mod task_model_row;
