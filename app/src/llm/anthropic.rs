//! llm/anthropic: Claude through Anthropic's
//! Messages API over HTTPS (Rust has no official SDK). Structured output via
//! output_config.format, thinking depth via output_config.effort, and server-side refusal
//! fallbacks enabled.

use serde_json::{Value, json};

use super::billing::{Billing, Provider};
use super::{Effort, LlmException, LlmResponse};

/// Anthropic's API.
pub const BASE_URL: &str = "https://api.anthropic.com";

/// The API version the requests are written for.
const ANTHROPIC_VERSION: &str = "2026-10-21";

/// The beta that lets a declined request be answered by Anthropic's recommended fallback model
/// (fallbacks: "default").
const FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// Most tokens one answer may take: room for a tailored résumé, and under the HTTP timeout of a
/// request that isn't streamed.
const MAX_TOKENS: u32 = 16_000;

/// What messages call the provider.
const NAME: &str = "Claude";

#[derive(Debug, Clone)]
pub struct Anthropic {
    api_key: String,
    pub model: String,
    pub base_url: String,
    client: reqwest::Client,
    pub(super) billing: Option<Billing>,
}

impl Anthropic {
    /// Anthropic's API (or a stand-in for it at base_url, in tests).
    pub fn new(api_key: &str, model: &str, base_url: &str) -> Result<Self, LlmException> {
        Ok(Anthropic {
            api_key: api_key.to_string(),
            model: model.to_string(),
            base_url: base_url.to_string(),
            client: super::http_client(base_url)?,
            billing: None,
        })
    }

    /// _max_tokens is for a local model; Claude's thinking counts against max_tokens, so it keeps
    /// MAX_TOKENS.
    pub async fn structured(
        &self,
        system: &str,
        user: &str,
        schema: &Value,
        effort: Effort,
        _max_tokens: u32,
    ) -> Result<Value, LlmException> {
        if self.api_key.is_empty() {
            return Err(LlmException::new(
                "Claude needs an API key. Add yours on the API key page.",
            ));
        }
        let body = json!({
            "model": self.model,
            "max_tokens": MAX_TOKENS,
            "system": system,
            "messages": [{ "role": "user", "content": user }],
            "output_config": {
                "effort": effort.as_str(),
                "format": { "type": "json_schema", "schema": schema },
            },
            "fallbacks": "default",
        });
        let answer = self.post(&body).await?;

        let message = answer.json();
        match message["stop_reason"].as_str() {
            Some("refusal") => {
                let why = message["stop_details"]["explanation"]
                    .as_str()
                    .unwrap_or("no reason given");
                return Err(LlmException::answered(format!("Claude declined: {why}"), false, answer));
            }
            Some("max_tokens") => {
                let text = "Claude ran out of tokens before finishing.";
                return Err(LlmException::answered(text, false, answer));
            }
            _ => {}
        }
        let text = message["content"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|block| block["type"] == "text")
            .and_then(|block| block["text"].as_str());
        match text {
            Some(text) => super::decoded(NAME, text, &answer),
            None => Err(LlmException::answered("Claude returned no answer.", false, answer)),
        }
    }

    /// POSTs body to the Messages API and returns Claude's answer.
    async fn post(&self, body: &Value) -> Result<LlmResponse, LlmException> {
        let url = format!("{}/v1/messages", self.base_url.trim_end_matches('/'));
        let headers = [
            ("x-api-key", self.api_key.as_str()),
            ("anthropic-version", ANTHROPIC_VERSION),
            ("anthropic-beta", FALLBACK_BETA),
        ];
        let answer = super::post(&self.client, &url, &headers, body)
            .await
            .map_err(|e| super::not_answered(NAME, &self.base_url, &e))?;
        let accepted = (200..300).contains(&answer.status) && answer.is_json();
        let blocked = Provider::Anthropic.blocked(&answer);
        if let Some(billing) = &self.billing {
            billing.observe(Provider::Anthropic, blocked, accepted);
        }
        if accepted {
            return Ok(answer);
        }
        if blocked {
            return Err(super::failed(NAME, answer));
        }
        Err(match answer.status {
            401 => super::rejected_key(NAME, answer),
            429 => rate_limited(answer),
            _ => super::failed(NAME, answer),
        })
    }
}

fn rate_limited(answer: LlmResponse) -> LlmException {
    let detail = answer.error_message();
    let message = if detail.is_empty() {
        "Claude rate limited the request. Try again in a minute.".to_string()
    } else {
        format!("Claude rate limited the request: {detail}")
    };
    LlmException::answered(message, true, answer)
}
