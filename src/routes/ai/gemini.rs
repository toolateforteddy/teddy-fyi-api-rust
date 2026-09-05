use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use schemars::{gen::SchemaSettings, JsonSchema};
use std::time::Duration;
use crate::routes::sync::types::AppError;

/// Where Gemini lives. A constant rather than an env var: the production call
/// site never varies, and tests reach [`call_gemini_at`] directly instead, which
/// keeps a redirectable base URL out of the deployed configuration surface.
pub const GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";

/// Header Google reads the API key from.
///
/// The key used to travel as `?key=...` in the URL. That is the one place a
/// secret must never sit: URLs are logged by proxies and load balancers, and —
/// verified against reqwest 0.12.28's `error.rs` — both `Display` and `Debug` for
/// `reqwest::Error` print the request URL verbatim (`" for url ({url})"`), so
/// every `tracing::error!("{:?}", e)` on a failed call wrote the key into our own
/// logs. Header instead, and the URL below carries nothing secret.
const API_KEY_HEADER: &str = "x-goog-api-key";

/// Deadline for one Gemini round trip, connect included.
///
/// Deliberately well **under** the 30s server-side request deadline in
/// [`crate::guardrails`]: the upstream call has to fail first, or the whole
/// request is cut at 30s and the client gets a bare 408 with nothing said about
/// *why*. Failing at 12s leaves us the chance to answer 503 "AI service error",
/// record the `timeout` outcome, and release the handler task and its socket.
/// Sized above a healthy flash-lite completion (single-digit seconds at worst)
/// and below anything a user would still be waiting for.
pub const GEMINI_TIMEOUT_SECS: u64 = 12;

/// Ceiling on how long the TCP+TLS handshake alone may take, inside the budget
/// above. A Gemini that is unreachable rather than slow should be found out fast.
const GEMINI_CONNECT_TIMEOUT_SECS: u64 = 5;

/// Builds the single, process-wide HTTP client for outbound Gemini calls.
///
/// One client, held in [`crate::state::AppState`] and cloned (which is cheap and
/// shares the inner pool) rather than a `reqwest::Client::new()` per call: a
/// fresh client per request builds a fresh connection pool and a fresh TLS
/// configuration, throws away the connection at the end, and makes every single
/// call pay a full handshake. It also removes any bound on how many sockets the
/// two AI endpoints can open at once.
pub fn build_http_client() -> reqwest::Client {
    build_http_client_with_timeout(Duration::from_secs(GEMINI_TIMEOUT_SECS))
}

/// [`build_http_client`] with an explicit deadline. Exists so tests can assert
/// timeout behaviour in milliseconds rather than in twelve seconds.
pub fn build_http_client_with_timeout(timeout: Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(GEMINI_CONNECT_TIMEOUT_SECS))
        .build()
        // The builder only fails on a broken TLS backend, which is a startup-time
        // environment fault; there is no useful degraded mode for it.
        .expect("failed to build the Gemini HTTP client")
}

/// Renders a `reqwest::Error` for logs and error messages with any URL stripped.
///
/// Belt and braces. With the key in a header the URL is no longer secret, but
/// `Display`/`Debug` on `reqwest::Error` embed whatever URL the error carries,
/// and that is precisely the mechanism that leaked the key before. Stripping it
/// here means a future edit that puts a credential back in a query string cannot
/// re-open the hole through this path. The `Kind` prefix — "error sending
/// request", "error decoding response body" — survives, which is the diagnostic
/// half that was actually useful.
pub(crate) fn redact(err: reqwest::Error) -> String {
    err.without_url().to_string()
}

#[derive(Serialize)]
pub struct GeminiRequest {
    pub contents: Vec<Content>,
    #[serde(rename = "generationConfig")]
    pub generation_config: GenerationConfig,
    #[serde(rename = "systemInstruction", skip_serializing_if = "Option::is_none")]
    pub system_instruction: Option<Content>,
}

#[derive(Serialize, Deserialize)]
pub struct Content {
    pub parts: Vec<Part>,
}

#[derive(Serialize, Deserialize)]
pub struct Part {
    pub text: String,
}

#[derive(Serialize)]
pub struct GenerationConfig {
    #[serde(rename = "responseMimeType")]
    pub response_mime_type: String,
    #[serde(rename = "responseSchema")]
    pub response_schema: Option<Value>,
}

#[derive(Deserialize)]
pub struct GeminiResponse {
    pub candidates: Vec<Candidate>,
}

#[derive(Deserialize)]
pub struct Candidate {
    pub content: Content,
}

pub fn get_schema<T: JsonSchema>() -> Value {
    let settings = SchemaSettings::draft07();
    let gen = settings.into_generator();
    let schema = gen.into_root_schema_for::<T>();
    let mut schema_val = serde_json::to_value(schema).unwrap();
    if let Some(obj) = schema_val.as_object_mut() {
        obj.remove("$schema");
        obj.remove("title");
    }
    schema_val
}

/// Calls Gemini and deserializes its JSON answer into `T`.
///
/// `client` is the shared client from [`crate::state::AppState`]; callers must
/// not build one of their own. Spend limiting is *not* done here — it belongs at
/// the handler, before any database work — see [`super::budget`].
pub async fn call_gemini<T: DeserializeOwned + JsonSchema>(
    client: &reqwest::Client,
    api_key: &str,
    system_prompt: Option<&str>,
    user_prompt: &str,
    model: &str,
) -> Result<T, AppError> {
    call_gemini_at(GEMINI_API_BASE, client, api_key, system_prompt, user_prompt, model).await
}

/// [`call_gemini`] against an explicit base URL, so tests can point it at a local
/// stub and inspect what actually went on the wire.
pub(crate) async fn call_gemini_at<T: DeserializeOwned + JsonSchema>(
    base_url: &str,
    client: &reqwest::Client,
    api_key: &str,
    system_prompt: Option<&str>,
    user_prompt: &str,
    model: &str,
) -> Result<T, AppError> {
    // No credential in this URL, by construction — the key rides in a header. See
    // `API_KEY_HEADER`.
    let url = format!("{}/models/{}:generateContent", base_url, model);

    let schema = get_schema::<T>();

    let request = GeminiRequest {
        contents: vec![Content {
            parts: vec![Part {
                text: user_prompt.to_string(),
            }],
        }],
        system_instruction: system_prompt.map(|s| Content {
            parts: vec![Part {
                text: s.to_string(),
            }],
        }),
        generation_config: GenerationConfig {
            response_mime_type: "application/json".to_string(),
            response_schema: Some(schema),
        },
    };

    // Timed around the network call only. Gemini is the one dependency here that
    // is billed per request, so its rate is a spend signal as much as a health one.
    let started = std::time::Instant::now();
    let response = client
        .post(&url)
        .header(API_KEY_HEADER, api_key)
        .json(&request)
        .send()
        .await
        .map_err(|e| {
            // A timeout is its own outcome on the dashboard: "Gemini is slow" and
            // "Gemini is refusing us" want different responses from an operator.
            let timed_out = e.is_timeout();
            crate::observability::metrics::record_gemini_call(
                model,
                if timed_out { "timeout" } else { "transport_error" },
                started.elapsed().as_secs_f64(),
            );
            let detail = redact(e);
            tracing::error!(model = %model, timed_out, "Failed to call Gemini: {}", detail);
            if timed_out {
                AppError::Gemini(format!(
                    "Gemini API call timed out after {}s",
                    GEMINI_TIMEOUT_SECS
                ))
            } else {
                AppError::Gemini(format!("Gemini API call failed: {}", detail))
            }
        })?;

    let outcome = if response.status().is_success() {
        "ok"
    } else {
        "http_error"
    };
    crate::observability::metrics::record_gemini_call(
        model,
        outcome,
        started.elapsed().as_secs_f64(),
    );

    let gemini_resp: GeminiResponse = response.json().await.map_err(|e| {
        let detail = redact(e);
        tracing::error!("Failed to parse Gemini response: {}", detail);
        AppError::Gemini(format!("Failed to parse Gemini response: {}", detail))
    })?;

    let text = gemini_resp
        .candidates
        .first()
        .and_then(|c| c.content.parts.first())
        .map(|p| p.text.as_str())
        .ok_or_else(|| {
            tracing::error!("Empty Gemini response");
            AppError::Gemini("Empty Gemini response".to_string())
        })?;

    let result: T = serde_json::from_str(text).map_err(|e| {
        tracing::error!("Failed to deserialize Gemini output into target type: {}. Text: {}", e, text);
        AppError::Serialization(e)
    })?;

    Ok(result)
}
