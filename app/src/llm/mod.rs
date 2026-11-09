//! llm: language models behind one call, whatever the provider.
//! Each provider is an adapter (openai, anthropic) that answers structured(); a local
//! OpenAI-compatible server (Ollama, LM Studio) is the openai adapter at its own base_url. The
//! desktop app has no server key: every call runs on the user's own key (Llm::use_key), which
//! the account page keeps.
//!
//! ```ignore
//! let llm = Llm::use_key("openai", &api_key, "", "")?;
//! let answer = llm.structured(system, text, &schema, Effort::Low, 4096).await?;
//! ```
//!
//! A new provider is one more adapter and one more arm in Adapter.

pub mod anthropic;
pub mod llm_exception;
pub mod llm_response;
pub mod openai;
pub mod transcript;

use std::error::Error as _;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

pub use self::anthropic::Anthropic;
pub use self::llm_exception::LlmException;
pub use self::llm_response::LlmResponse;
pub use self::openai::{Openai, WebSearchAnswer};
use crate::task_manager::running_work::{self, Stopped, WorkKind};

/// The provider name of OpenAI's API.
pub const OPENAI: &str = "openai";
/// The provider name of Anthropic's API.
pub const ANTHROPIC: &str = "anthropic";
/// The provider name of an OpenAI-compatible server on this computer or network, at base_url.
pub const LOCAL: &str = "local";
/// The provider name of a GGUF model the app runs itself with llama.cpp's llama-server (the llama
/// module), which then answers as a LOCAL server.
pub const LLAMA_CPP: &str = "llama_cpp";

/// The model a key runs on when the user doesn't name one: the extraction bake-off's OpenAI
/// pick, and a Claude model that takes the effort setting the anthropic adapter sends. A local
/// server has no default: its models are whatever the user pulled.
pub const USER_DEFAULT_MODELS: [(&str, &str); 2] = [(OPENAI, "gpt-5.4-mini"), (ANTHROPIC, "claude-sonnet-5-5")];

/// How long one call may take, in seconds. A long structured answer (a tailored résumé) takes
/// well over a minute.
pub const CALL_SECONDS: u64 = 180;

/// How long one call to a model on this computer may take, in seconds (15 minutes): it costs
/// nothing, and a mid-range graphics card writes a job post's English text and requirements at
/// about 20 tokens a second, so a long post takes minutes.
pub const LOCAL_CALL_SECONDS: u64 = 900;

/// How long connecting to the provider may take, in seconds.
const CONNECT_SECONDS: u64 = 15;

/// How hard the model thinks before answering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effort {
    Low,
    Medium,
    High,
}

impl Effort {
    /// As the providers' APIs name it.
    pub fn as_str(self) -> &'static str {
        match self {
            Effort::Low => "low",
            Effort::Medium => "medium",
            Effort::High => "high",
        }
    }
}

/// One provider's adapter, configured with a key and a model.
#[derive(Debug, Clone)]
pub enum Adapter {
    Openai(Openai),
    Anthropic(Anthropic),
}

/// The model a key runs on: its provider's adapter, and for the app's own llama-server what keeps
/// it from stopping for being idle while this (or a clone) is held.
#[derive(Debug, Clone)]
pub struct Llm {
    pub adapter: Adapter,
    pub server_in_use: Option<Arc<crate::llama::llama_server::InUse>>,
}

impl Llm {
    /// Calls on the user's own key. model "" is the provider's USER_DEFAULT_MODELS entry;
    /// base_url is only read for a local server, so an OpenAI or Anthropic key only goes to its
    /// own provider.
    pub fn use_key(provider: &str, api_key: &str, model: &str, base_url: &str) -> Result<Llm, LlmException> {
        let model = if model.is_empty() {
            default_model(provider).unwrap_or("")
        } else {
            model
        };
        let adapter = match provider {
            OPENAI => Adapter::Openai(Openai::new(api_key, model, openai::BASE_URL)?),
            ANTHROPIC => Adapter::Anthropic(Anthropic::new(api_key, model, anthropic::BASE_URL)?),
            LOCAL if model.is_empty() => {
                return Err(LlmException::new(
                    "A local model needs its model name. Add it on the API key page.",
                ));
            }
            LOCAL => Adapter::Openai(Openai::local(api_key, model, base_url)?),
            LLAMA_CPP => {
                return Err(LlmException::new(
                    "The app's own model runs through its llama-server; start it with model_runtime::load_key.",
                ));
            }
            _ => return Err(LlmException::new(format!("Unknown LLM provider \"{provider}\"."))),
        };
        Ok(Llm {
            adapter,
            server_in_use: None,
        })
    }

    /// One JSON answer that follows `schema` (objects closed with additionalProperties: false,
    /// every property required), from the key's provider. max_tokens: the longest answer the call
    /// may write; a local model is stopped there (see Openai::structured).
    pub async fn structured(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
        effort: Effort,
        max_tokens: u32,
    ) -> Result<Value, LlmException> {
        let call = transcript::call(system, user);
        let asking = async {
            match &self.adapter {
                Adapter::Openai(openai) => openai.structured(system, user, schema, effort, max_tokens).await,
                Adapter::Anthropic(anthropic) => anthropic.structured(system, user, schema, effort, max_tokens).await,
            }
        };
        let answer = self.listed(asking).await;
        call.answered(|| answer_text(answer.as_ref()));
        answer
    }

    /// structured(), searching the web while answering, on a provider that can (OpenAI's
    /// Responses API). domains: sites to search only, [] for any; country: the two-letter
    /// country the search is made from ("DK"), or "".
    pub async fn web_search(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
        domains: &[&str],
        country: &str,
        effort: Effort,
    ) -> Result<WebSearchAnswer, LlmException> {
        let call = transcript::call(system, user);
        let asking = async {
            match &self.adapter {
                Adapter::Openai(openai) => openai.web_search(system, user, schema, domains, country, effort).await,
                Adapter::Anthropic(_) => Err(LlmException::new(
                    "Searching the web needs an OpenAI key; this key is for another provider.",
                )),
            }
        };
        let answer = self.listed(asking).await;
        call.answered(|| answer_text(answer.as_ref().map(|found| &found.answer)));
        answer
    }

    /// A call, listed in the task manager while it waits for its answer; stopped there, it fails
    /// saying so.
    async fn listed<T>(&self, asking: impl Future<Output = Result<T, LlmException>>) -> Result<T, LlmException> {
        running_work::part(WorkKind::ModelCall, self.model(), asking)
            .await
            .unwrap_or_else(|Stopped| Err(LlmException::new(Stopped::SAID)))
    }

    /// The model the calls run on.
    pub fn model(&self) -> &str {
        match &self.adapter {
            Adapter::Openai(openai) => &openai.model,
            Adapter::Anthropic(anthropic) => &anthropic.model,
        }
    }
}

/// An answer as the transcript keeps it: its JSON, pretty, or the error.
fn answer_text(answer: Result<&Value, &LlmException>) -> String {
    match answer {
        Ok(json) => serde_json::to_string_pretty(json).unwrap_or_default(),
        Err(e) => format!("Failed: {}", e.message),
    }
}

/// The model a provider's key runs on when the user names none, if it has one.
pub fn default_model(provider: &str) -> Option<&'static str> {
    USER_DEFAULT_MODELS
        .iter()
        .find(|(name, _)| *name == provider)
        .map(|(_, model)| *model)
}

/// The HTTP client a provider at base_url is called with. A server on this computer is called
/// directly, never through the system's proxy.
fn http_client(base_url: &str) -> Result<reqwest::Client, LlmException> {
    let builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(CONNECT_SECONDS))
        .timeout(Duration::from_secs(call_seconds(base_url)));
    let builder = if is_this_computer(base_url) {
        builder.no_proxy()
    } else {
        builder
    };
    builder.build().map_err(|e| {
        crate::log_line!("llm: couldn't make the HTTP client: {}", error_chain(&e));
        LlmException::new(format!("The app couldn't make its HTTP client: {}", error_chain(&e)))
    })
}

/// How long a call to base_url may take: longer on this computer.
pub fn call_seconds(base_url: &str) -> u64 {
    if is_this_computer(base_url) {
        LOCAL_CALL_SECONDS
    } else {
        CALL_SECONDS
    }
}

/// Whether base_url names this computer (localhost, 127.x.x.x or ::1).
fn is_this_computer(base_url: &str) -> bool {
    let host = reqwest::Url::parse(base_url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_ascii_lowercase))
        .unwrap_or_default();
    host == "localhost" || host == "[::1]" || host.starts_with("127.")
}

/// POSTs body as JSON to url with headers, and reads the whole answer, whatever its status.
async fn post(
    client: &reqwest::Client,
    url: &str,
    headers: &[(&str, &str)],
    body: &Value,
) -> Result<LlmResponse, reqwest::Error> {
    let mut request = client.post(url).json(body);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let response = request.send().await?;
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_ascii_lowercase(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            )
        })
        .collect();
    let body = response.text().await?;
    Ok(LlmResponse::new(status, headers, &body))
}

/// The request never got an answer: it timed out, or the provider couldn't be reached.
fn not_answered(provider: &str, base_url: &str, error: &reqwest::Error) -> LlmException {
    if error.is_timeout() {
        return LlmException::retryable(format!(
            "{provider} didn't answer within {} seconds. Try again; if it keeps happening, try with less text.",
            call_seconds(base_url)
        ));
    }
    LlmException::retryable(format!("{provider} could not be reached: {}", error_chain(error)))
}

/// An error and what caused it, as one line: reqwest's own message only says which request
/// failed.
pub(crate) fn error_chain(error: &reqwest::Error) -> String {
    let mut parts = vec![error.to_string()];
    let mut source = error.source();
    while let Some(cause) = source {
        let text = cause.to_string();
        if !parts.contains(&text) {
            parts.push(text);
        }
        source = cause.source();
    }
    parts.join(": ")
}

/// The model's JSON answer, or an LlmException carrying the response when it isn't JSON.
fn decoded(provider: &str, text: &str, answer: &LlmResponse) -> Result<Value, LlmException> {
    match serde_json::from_str::<Value>(text) {
        Ok(decoded @ (Value::Object(_) | Value::Array(_))) => Ok(decoded),
        _ => Err(LlmException::answered(
            format!("{provider} answered with something other than the JSON asked for."),
            true,
            answer.clone(),
        )),
    }
}

/// "{provider} failed: what it said", or its HTTP status when it said nothing; a server error
/// can be tried again.
fn failed(provider: &str, answer: LlmResponse) -> LlmException {
    let detail = answer.error_message();
    let message = if detail.is_empty() {
        format!("{provider} failed: HTTP {}", answer.status)
    } else {
        format!("{provider} failed: {detail}")
    };
    let retryable = answer.status >= 500;
    LlmException::answered(message, retryable, answer)
}

/// "{provider} rejected the API key", with what it said.
fn rejected_key(provider: &str, answer: LlmResponse) -> LlmException {
    let detail = answer.error_message();
    let message = if detail.is_empty() {
        format!("{provider} rejected the API key.")
    } else {
        format!("{provider} rejected the API key: {detail}")
    };
    LlmException::answered(message, false, answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_runs_on_its_providers_default_model() {
        let llm = Llm::use_key(OPENAI, "sk-test-0123456789abcdef", "", "http://evil.example").unwrap();
        assert_eq!(llm.model(), "gpt-5.4-mini");
        let Adapter::Openai(openai) = &llm.adapter else {
            panic!("openai")
        };
        assert_eq!(openai.base_url, openai::BASE_URL, "an OpenAI key only goes to OpenAI");
        let llm = Llm::use_key(ANTHROPIC, "sk-ant-test-0123456789", "", "").unwrap();
        assert_eq!(llm.model(), "claude-sonnet-5-5");
        let llm = Llm::use_key(ANTHROPIC, "sk-ant-test-0123456789", "claude-opus-5-5", "").unwrap();
        assert_eq!(llm.model(), "claude-opus-5-5");
    }

    #[test]
    fn a_local_model_needs_its_name_and_address() {
        assert!(Llm::use_key(LOCAL, "", "", "http://localhost:11434/v1").is_err());
        let llm = Llm::use_key(LOCAL, "", "llama3.1:8b", "http://localhost:11434/v1").unwrap();
        let Adapter::Openai(openai) = &llm.adapter else {
            panic!("openai")
        };
        assert_eq!(openai.base_url, "http://localhost:11434/v1");
        assert!(Llm::use_key("gemini", "x", "", "").is_err());
    }

    #[test]
    fn only_this_computer_skips_the_proxy() {
        assert!(is_this_computer("http://localhost:11434/v1"));
        assert!(is_this_computer("http://127.0.0.1:1234/v1"));
        assert!(is_this_computer("http://[::1]:8080/v1"));
        assert!(!is_this_computer("https://api.openai.com/v1"));
        assert!(!is_this_computer("http://192.168.1.20:11434/v1"));
        assert_eq!(call_seconds("http://127.0.0.1:1234/v1"), LOCAL_CALL_SECONDS);
        assert_eq!(call_seconds("https://api.openai.com/v1"), CALL_SECONDS);
    }
}
