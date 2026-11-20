//! Chat and Action provider adapters.
//!
//! Two providers are implemented and tested:
//! - `ollama` for a local Ollama service (fixed loopback default, no credential)
//! - `openai_compatible` for any `/chat/completions` endpoint
//!
//! Capabilities are reported only when the provider actually reports them.
//! `None` means "unknown", never "assumed supported".
//!
//! Credentials are held in backend memory for the current process only. They are
//! never written to settings, never returned to the frontend, and never placed
//! in a process argument list.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::http::{self, Response};

pub const OLLAMA: &str = "ollama";
pub const OPENAI_COMPATIBLE: &str = "openai_compatible";
pub const DEFAULT_OLLAMA_URL: &str = "http://127.0.0.1:11434";
pub const DEFAULT_OPENAI_URL: &str = "https://api.openai.com/v1";

const MAX_MESSAGES: usize = 40;
const MAX_MESSAGE_CHARS: usize = 16_000;
const MAX_MODEL_NAME: usize = 120;
const MAX_CREDENTIAL_CHARS: usize = 512;
const DISCOVERY_TIMEOUT: u64 = 10;
const CHAT_TIMEOUT: u64 = 180;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// Base64 PNG images attached to a user message (vision models only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelMetadata {
    pub name: String,
    pub size_bytes: Option<u64>,
    pub modified_at: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ModelCapabilities {
    pub provider: String,
    pub model: String,
    /// `None` when the provider does not report the capability.
    pub tools: Option<bool>,
    pub vision: Option<bool>,
    pub family: Option<String>,
    pub parameter_size: Option<String>,
    pub note: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProviderHealth {
    pub provider: String,
    pub base_url: String,
    pub reachable: bool,
    pub credential_configured: bool,
    pub detail: String,
    pub models: Vec<ModelMetadata>,
    pub last_successful_verification: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ChatReply {
    pub provider: String,
    pub model: String,
    pub content: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ToolResult {
    pub tool_call_id: String,
    pub name: String,
    pub content: String,
}

/// Provider-neutral conversation item, including tool turns.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub enum AiMessage {
    System(String),
    User(String),
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCall>,
    },
    Tool(ToolResult),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Completion {
    pub content: Option<String>,
    pub tool_calls: Vec<ToolCall>,
}

static CREDENTIALS: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
static PROVIDER_SUCCESS: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

fn credentials() -> &'static Mutex<HashMap<String, String>> {
    CREDENTIALS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn validate_provider(provider: &str) -> Result<(), String> {
    if matches!(provider, OLLAMA | OPENAI_COMPATIBLE) {
        Ok(())
    } else {
        Err(format!(
            "Unknown AI provider '{provider}'. Supported providers: {OLLAMA}, {OPENAI_COMPATIBLE}."
        ))
    }
}

pub fn default_base_url(provider: &str) -> &'static str {
    match provider {
        OPENAI_COMPATIBLE => DEFAULT_OPENAI_URL,
        _ => DEFAULT_OLLAMA_URL,
    }
}

/// Model names go into request bodies, not shells, but they still must be sane.
/// An empty value is allowed here so a fresh settings file can omit a choice.
pub fn validate_model_name(model: &str) -> Result<(), String> {
    if model.is_empty() {
        return Ok(());
    }
    if model.len() > MAX_MODEL_NAME
        || model
            .chars()
            .any(|ch| !ch.is_ascii_alphanumeric() && !matches!(ch, '.' | ':' | '-' | '_' | '/'))
    {
        return Err("Model name contains unsupported characters.".to_string());
    }
    Ok(())
}

fn ensure_model_selected(model: &str) -> Result<(), String> {
    validate_model_name(model)?;
    if model.trim().is_empty() {
        return Err("Choose a model for this provider first.".to_string());
    }
    Ok(())
}

fn is_loopback(host: &str) -> bool {
    matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

/// Plain HTTP is accepted only for loopback services (Ollama, local
/// OpenAI-compatible servers). Everything else must be HTTPS, which keeps API
/// keys off the wire in clear text.
pub fn validate_base_url(raw: &str) -> Result<String, String> {
    let url = raw.trim().trim_end_matches('/');
    if url.is_empty() {
        return Err("A provider base URL is required.".to_string());
    }
    if url.len() > 512 || url.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return Err("The provider base URL contains unsupported characters.".to_string());
    }
    if url.contains('#') || url.contains('?') {
        return Err("The provider base URL cannot contain a query or fragment.".to_string());
    }
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| "The provider base URL must start with http:// or https://.".to_string())?;
    if !matches!(scheme, "http" | "https") {
        return Err("Only http:// and https:// provider URLs are supported.".to_string());
    }
    let authority = rest.split('/').next().unwrap_or("");
    if authority.is_empty() {
        return Err("The provider base URL has no host.".to_string());
    }
    if authority.contains('@') {
        return Err("Credentials must not be embedded in the provider base URL.".to_string());
    }
    let host = authority
        .strip_prefix('[')
        .and_then(|value| value.split_once(']').map(|(host, _)| format!("[{host}]")))
        .unwrap_or_else(|| {
            authority
                .split_once(':')
                .map(|(host, _)| host.to_string())
                .unwrap_or_else(|| authority.to_string())
        });
    if host.is_empty() {
        return Err("The provider base URL has no host.".to_string());
    }
    if scheme == "http" && !is_loopback(&host) {
        return Err(
            "Plain HTTP is only allowed for loopback providers. Use HTTPS for remote endpoints."
                .to_string(),
        );
    }
    Ok(url.to_string())
}

pub fn set_credential(provider: &str, secret: &str) -> Result<bool, String> {
    validate_provider(provider)?;
    let secret = secret.trim();
    if secret.is_empty() {
        return Err("The API key cannot be empty.".to_string());
    }
    if secret.chars().count() > MAX_CREDENTIAL_CHARS || secret.chars().any(|ch| ch.is_control()) {
        return Err("The API key contains unsupported characters.".to_string());
    }
    credentials()
        .lock()
        .map_err(|_| "Credential storage is unavailable.".to_string())?
        .insert(provider.to_string(), secret.to_string());
    Ok(true)
}

pub fn clear_credential(provider: &str) -> Result<bool, String> {
    validate_provider(provider)?;
    credentials()
        .lock()
        .map_err(|_| "Credential storage is unavailable.".to_string())?
        .remove(provider);
    Ok(false)
}

pub fn has_credential(provider: &str) -> bool {
    credentials()
        .lock()
        .map(|stored| stored.contains_key(provider))
        .unwrap_or(false)
}

fn credential(provider: &str) -> Option<String> {
    credentials()
        .lock()
        .ok()
        .and_then(|stored| stored.get(provider).cloned())
}

/// Removes any stored secret from text that may reach the UI or the logs.
pub fn redact_secrets(text: &str) -> String {
    let Ok(stored) = credentials().lock() else {
        return text.to_string();
    };
    stored
        .values()
        .filter(|secret| secret.len() >= 8)
        .fold(text.to_string(), |result, secret| {
            result.replace(secret.as_str(), "[redacted]")
        })
}

/// A loopback OpenAI-compatible server (llama.cpp, LM Studio, vLLM) usually has
/// no key. Remote endpoints must have one, so a missing key can never send an
/// unauthenticated request off the machine.
fn auth_headers(provider: &str, base_url: &str) -> Result<Vec<(String, String)>, String> {
    let mut headers = vec![("Content-Type".to_string(), "application/json".to_string())];
    match provider {
        OLLAMA => {}
        OPENAI_COMPATIBLE => match credential(provider) {
            Some(key) => headers.push(("Authorization".to_string(), format!("Bearer {key}"))),
            None if is_loopback_url(base_url) => {}
            None => {
                return Err(
                        "This remote provider requires an API key for the current session. Set it in the AI panel; it is kept in memory only."
                            .to_string(),
                    );
            }
        },
        other => return Err(format!("Unknown AI provider '{other}'.")),
    }
    Ok(headers)
}

fn is_loopback_url(base_url: &str) -> bool {
    validate_base_url(base_url)
        .ok()
        .and_then(|url| url.split_once("://").map(|(_, rest)| rest.to_string()))
        .map(|rest| {
            let authority = rest.split('/').next().unwrap_or_default();
            let host = authority
                .strip_prefix('[')
                .and_then(|value| value.split_once(']').map(|(host, _)| format!("[{host}]")))
                .unwrap_or_else(|| {
                    authority
                        .split_once(':')
                        .map(|(host, _)| host.to_string())
                        .unwrap_or_else(|| authority.to_string())
                });
            is_loopback(&host)
        })
        .unwrap_or(false)
}

fn endpoint(provider: &str, base_url: &str, path: &str) -> Result<String, String> {
    validate_provider(provider)?;
    let base = validate_base_url(base_url)?;
    Ok(format!("{base}{path}"))
}

fn ensure_success(provider: &str, response: Response) -> Result<Value, String> {
    if !(200..300).contains(&response.status) {
        let detail = http::error_detail(&response.body);
        return Err(redact_secrets(&format!(
            "{provider} returned HTTP {}: {}",
            response.status,
            if detail.is_empty() {
                "no error detail".to_string()
            } else {
                detail
            }
        )));
    }
    serde_json::from_str(&response.body)
        .map_err(|error| format!("{provider} returned a response that is not valid JSON: {error}"))
}

pub fn validate_messages(messages: &[ChatMessage]) -> Result<(), String> {
    if messages.is_empty() {
        return Err("A prompt is required.".to_string());
    }
    if messages.len() > MAX_MESSAGES {
        return Err(format!(
            "This chat exceeds the supported conversation length of {MAX_MESSAGES} messages."
        ));
    }
    for message in messages {
        if !matches!(message.role.as_str(), "user" | "assistant" | "system") {
            return Err("Chat messages must use the user, assistant or system role.".to_string());
        }
        if message.content.trim().is_empty() {
            return Err("Chat messages cannot be empty.".to_string());
        }
        if message.content.chars().count() > MAX_MESSAGE_CHARS {
            return Err(format!(
                "A chat message exceeds the supported {MAX_MESSAGE_CHARS}-character limit."
            ));
        }
        if message.images.as_ref().is_some_and(|images| {
            message.role != "user"
                || images.len() > 4
                || images.iter().any(|image| image.len() > 8 * 1024 * 1024)
        }) {
            return Err(
                "Images are limited to four PNG attachments on a user message.".to_string(),
            );
        }
    }
    Ok(())
}

fn ollama_messages(messages: &[ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            let mut value = json!({ "role": message.role, "content": message.content });
            if let Some(images) = &message.images {
                value["images"] = json!(images);
            }
            value
        })
        .collect()
}

fn openai_messages(messages: &[ChatMessage]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| match &message.images {
            Some(images) if !images.is_empty() => {
                let mut parts = vec![json!({ "type": "text", "text": message.content })];
                for image in images {
                    parts.push(json!({
                        "type": "image_url",
                        "image_url": { "url": format!("data:image/png;base64,{image}") }
                    }));
                }
                json!({ "role": message.role, "content": parts })
            }
            _ => json!({ "role": message.role, "content": message.content }),
        })
        .collect()
}

fn ai_message_to_ollama(message: &AiMessage) -> Value {
    match message {
        AiMessage::System(content) => json!({ "role": "system", "content": content }),
        AiMessage::User(content) => json!({ "role": "user", "content": content }),
        AiMessage::Assistant {
            content,
            tool_calls,
        } => {
            if tool_calls.is_empty() {
                json!({ "role": "assistant", "content": content.clone().unwrap_or_default() })
            } else {
                json!({
                    "role": "assistant",
                    "content": content.clone().unwrap_or_default(),
                    "tool_calls": tool_calls.iter().map(|call| json!({
                        "function": { "name": call.name, "arguments": call.arguments }
                    })).collect::<Vec<_>>(),
                })
            }
        }
        AiMessage::Tool(result) => json!({
            "role": "tool",
            "content": result.content,
            "tool_name": result.name,
        }),
    }
}

fn ai_message_to_openai(message: &AiMessage) -> Value {
    match message {
        AiMessage::System(content) => json!({ "role": "system", "content": content }),
        AiMessage::User(content) => json!({ "role": "user", "content": content }),
        AiMessage::Assistant {
            content,
            tool_calls,
        } => {
            let mut value = json!({ "role": "assistant", "content": content });
            if !tool_calls.is_empty() {
                value["tool_calls"] = json!(tool_calls
                    .iter()
                    .map(|call| json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": serde_json::to_string(&call.arguments).unwrap_or_else(|_| "{}".to_string()),
                        }
                    }))
                    .collect::<Vec<_>>());
            }
            value
        }
        AiMessage::Tool(result) => json!({
            "role": "tool",
            "tool_call_id": result.tool_call_id,
            "content": result.content,
        }),
    }
}

fn parse_tool_arguments(value: &Value) -> Value {
    match value {
        Value::String(raw) => serde_json::from_str(raw).unwrap_or_else(|_| json!({})),
        other => other.clone(),
    }
}

fn parse_ollama_completion(value: &Value) -> Result<Completion, String> {
    let message = value
        .get("message")
        .ok_or_else(|| "Ollama returned no message.".to_string())?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let tool_calls: Vec<ToolCall> = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .enumerate()
                .filter_map(|(index, call)| {
                    let function = call.get("function")?;
                    let name = function.get("name")?.as_str()?.to_string();
                    Some(ToolCall {
                        id: format!("ollama-{index}"),
                        name,
                        arguments: function
                            .get("arguments")
                            .map(parse_tool_arguments)
                            .unwrap_or_else(|| json!({})),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if content.is_none() && tool_calls.is_empty() {
        return Err("Ollama returned an empty reply.".to_string());
    }
    Ok(Completion {
        content,
        tool_calls,
    })
}

fn parse_openai_completion(value: &Value) -> Result<Completion, String> {
    let message = value
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
        .ok_or_else(|| "The provider returned no assistant message.".to_string())?;
    let content = message
        .get("content")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let tool_calls: Vec<ToolCall> = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .map(|calls| {
            calls
                .iter()
                .enumerate()
                .filter_map(|(index, call)| {
                    let function = call.get("function")?;
                    Some(ToolCall {
                        id: call
                            .get("id")
                            .and_then(Value::as_str)
                            .map(str::to_string)
                            .unwrap_or_else(|| format!("openai-{index}")),
                        name: function.get("name")?.as_str()?.to_string(),
                        arguments: function
                            .get("arguments")
                            .map(parse_tool_arguments)
                            .unwrap_or_else(|| json!({})),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    if content.is_none() && tool_calls.is_empty() {
        return Err("The provider returned an empty reply.".to_string());
    }
    Ok(Completion {
        content,
        tool_calls,
    })
}

pub fn list_models(provider: &str, base_url: &str) -> Result<Vec<ModelMetadata>, String> {
    validate_provider(provider)?;
    let (url, headers) = match provider {
        OLLAMA => (
            endpoint(provider, base_url, "/api/tags")?,
            auth_headers(provider, base_url)?,
        ),
        _ => (
            endpoint(provider, base_url, "/models")?,
            auth_headers(provider, base_url)?,
        ),
    };
    let response = http::request(&url, "GET", &headers, None, DISCOVERY_TIMEOUT)?;
    let value = ensure_success(provider, response)?;
    match provider {
        OLLAMA => parse_ollama_models(&value),
        _ => parse_openai_models(&value),
    }
}

fn parse_ollama_models(value: &Value) -> Result<Vec<ModelMetadata>, String> {
    let models = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| "Ollama returned no model list.".to_string())?;
    Ok(models
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name").or_else(|| entry.get("model"))?.as_str()?;
            Some(ModelMetadata {
                name: name.to_string(),
                size_bytes: entry.get("size").and_then(Value::as_u64),
                modified_at: entry
                    .get("modified_at")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            })
        })
        .collect())
}

fn parse_openai_models(value: &Value) -> Result<Vec<ModelMetadata>, String> {
    let models = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| "The provider returned no model list.".to_string())?;
    Ok(models
        .iter()
        .filter_map(|entry| {
            Some(ModelMetadata {
                name: entry.get("id")?.as_str()?.to_string(),
                size_bytes: None,
                modified_at: entry
                    .get("created")
                    .and_then(Value::as_u64)
                    .map(|created| created.to_string()),
            })
        })
        .collect())
}

fn parse_ollama_capabilities(model: &str, value: &Value) -> ModelCapabilities {
    let capabilities = value.get("capabilities").and_then(Value::as_array);
    let has = |wanted: &str| {
        capabilities.map(|items| {
            items.iter().any(|item| {
                item.as_str()
                    .is_some_and(|text| text.eq_ignore_ascii_case(wanted))
            })
        })
    };
    let details = value.get("details");
    ModelCapabilities {
        provider: OLLAMA.to_string(),
        model: model.to_string(),
        tools: has("tools"),
        vision: has("vision"),
        family: details
            .and_then(|details| details.get("family"))
            .and_then(Value::as_str)
            .map(str::to_string),
        parameter_size: details
            .and_then(|details| details.get("parameter_size"))
            .and_then(Value::as_str)
            .map(str::to_string),
        note: if capabilities.is_some() {
            "Capabilities reported by Ollama.".to_string()
        } else {
            "This Ollama version does not report model capabilities; tool calling and vision are unverified."
                .to_string()
        },
    }
}

pub fn probe_model(
    provider: &str,
    base_url: &str,
    model: &str,
) -> Result<ModelCapabilities, String> {
    ensure_model_selected(model)?;
    match provider {
        OLLAMA => {
            let url = endpoint(provider, base_url, "/api/show")?;
            let headers = auth_headers(provider, base_url)?;
            let body = json!({ "name": model }).to_string();
            let response = http::request(&url, "POST", &headers, Some(&body), DISCOVERY_TIMEOUT)?;
            let value = ensure_success(provider, response)?;
            Ok(parse_ollama_capabilities(model, &value))
        }
        OPENAI_COMPATIBLE => Ok(ModelCapabilities {
            provider: OPENAI_COMPATIBLE.to_string(),
            model: model.to_string(),
            tools: None,
            vision: None,
            family: None,
            parameter_size: None,
            note: "This provider does not report model capabilities. Vision and tool support must be confirmed with a real request."
                .to_string(),
        }),
        other => Err(format!("Unknown AI provider '{other}'.")),
    }
}

pub fn health(provider: &str, base_url: &str) -> ProviderHealth {
    let base_url = validate_base_url(base_url).unwrap_or_else(|_| base_url.to_string());
    let key = format!("{provider}|{base_url}");
    let successful_verifications = PROVIDER_SUCCESS.get_or_init(|| Mutex::new(HashMap::new()));
    let last_successful_verification = successful_verifications
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).copied());
    match list_models(provider, &base_url) {
        Ok(models) => {
            let verified_at = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_secs())
                .unwrap_or_default();
            if let Ok(mut cache) = successful_verifications.lock() {
                cache.insert(key, verified_at);
            }
            ProviderHealth {
                provider: provider.to_string(),
                base_url,
                reachable: true,
                credential_configured: has_credential(provider),
                detail: format!("Connected. {} model(s) available.", models.len()),
                models,
                last_successful_verification: Some(verified_at),
            }
        }
        Err(error) => ProviderHealth {
            provider: provider.to_string(),
            base_url,
            reachable: false,
            credential_configured: has_credential(provider),
            detail: redact_secrets(&error),
            models: Vec::new(),
            last_successful_verification,
        },
    }
}

fn completion_body(
    provider: &str,
    model: &str,
    messages: &[ChatMessage],
    tools: Option<&[Value]>,
) -> Value {
    match provider {
        OLLAMA => {
            let mut body = json!({
                "model": model,
                "messages": ollama_messages(messages),
                "stream": false,
            });
            if let Some(tools) = tools {
                body["tools"] = json!(tools);
            }
            body
        }
        _ => {
            let mut body = json!({
                "model": model,
                "messages": openai_messages(messages),
                "stream": false,
            });
            if let Some(tools) = tools {
                body["tools"] = json!(tools);
                body["tool_choice"] = json!("auto");
            }
            body
        }
    }
}

pub fn chat(
    provider: &str,
    base_url: &str,
    model: &str,
    messages: &[ChatMessage],
) -> Result<ChatReply, String> {
    validate_provider(provider)?;
    ensure_model_selected(model)?;
    validate_messages(messages)?;
    let path = match provider {
        OLLAMA => "/api/chat",
        _ => "/chat/completions",
    };
    let url = endpoint(provider, base_url, path)?;
    let headers = auth_headers(provider, base_url)?;
    let body = completion_body(provider, model, messages, None).to_string();
    let response = http::request(&url, "POST", &headers, Some(&body), CHAT_TIMEOUT)?;
    let value = ensure_success(provider, response)?;
    let completion = match provider {
        OLLAMA => parse_ollama_completion(&value),
        _ => parse_openai_completion(&value),
    }?;
    let content = completion
        .content
        .ok_or_else(|| "The model returned a tool call instead of a chat answer.".to_string())?;
    Ok(ChatReply {
        provider: provider.to_string(),
        model: model.to_string(),
        content,
    })
}

/// One tool-capable completion round for the Action engine.
pub fn complete_with_tools(
    provider: &str,
    base_url: &str,
    model: &str,
    messages: &[AiMessage],
    tools: &[Value],
    timeout_seconds: u64,
) -> Result<Completion, String> {
    validate_provider(provider)?;
    ensure_model_selected(model)?;
    if tools.is_empty() {
        return Err("The Action engine has no registered tools.".to_string());
    }
    let path = match provider {
        OLLAMA => "/api/chat",
        _ => "/chat/completions",
    };
    let url = endpoint(provider, base_url, path)?;
    let headers = auth_headers(provider, base_url)?;
    let body = match provider {
        OLLAMA => json!({
            "model": model,
            "messages": messages.iter().map(ai_message_to_ollama).collect::<Vec<_>>(),
            "stream": false,
            "tools": tools,
        }),
        _ => json!({
            "model": model,
            "messages": messages.iter().map(ai_message_to_openai).collect::<Vec<_>>(),
            "stream": false,
            "tools": tools,
            "tool_choice": "auto",
        }),
    }
    .to_string();
    let response = http::request(&url, "POST", &headers, Some(&body), timeout_seconds)?;
    let value = ensure_success(provider, response)?;
    match provider {
        OLLAMA => parse_ollama_completion(&value),
        _ => parse_openai_completion(&value),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
            images: None,
        }
    }

    #[test]
    fn model_and_message_validation_rejects_unsafe_input() {
        assert!(validate_model_name("qwen2.5-coder:1.5b").is_ok());
        assert!(validate_model_name("").is_ok());
        assert!(validate_model_name("model; rm -rf /").is_err());
        assert!(validate_messages(&[message("user", "hi")]).is_ok());
        assert!(validate_messages(&[message("tool", "hi")]).is_err());
        assert!(validate_messages(&[message("user", " ")]).is_err());
        assert!(ensure_model_selected("").is_err());
    }

    #[test]
    fn base_urls_reject_plain_http_off_loopback_and_embedded_credentials() {
        assert_eq!(
            validate_base_url("http://127.0.0.1:11434"),
            Ok("http://127.0.0.1:11434".to_string())
        );
        assert!(validate_base_url("http://localhost:1234/v1").is_ok());
        assert!(validate_base_url("https://api.openai.com/v1/").is_ok());
        assert!(validate_base_url("http://192.168.1.10:1234/v1").is_err());
        assert!(validate_base_url("ftp://example.com").is_err());
        assert!(validate_base_url("https://user:pass@example.com").is_err());
        assert!(validate_base_url("https://example.com/v1?x=1").is_err());
        assert!(validate_base_url("nonsense").is_err());
    }

    #[test]
    fn ollama_and_openai_completions_are_parsed_into_the_same_shape() {
        let ollama = json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": { "name": "tap", "arguments": { "x": 10, "y": 20 } }
                }]
            }
        });
        let parsed = parse_ollama_completion(&ollama).unwrap();
        assert_eq!(parsed.tool_calls[0].name, "tap");
        assert_eq!(parsed.tool_calls[0].arguments["x"], 10);

        let openai = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "swipe", "arguments": "{\"x1\":1}" }
                    }]
                }
            }]
        });
        let parsed = parse_openai_completion(&openai).unwrap();
        assert_eq!(parsed.tool_calls[0].id, "call_1");
        assert_eq!(parsed.tool_calls[0].arguments["x1"], 1);

        assert!(parse_ollama_completion(&json!({ "message": { "content": "" } })).is_err());
        assert!(parse_openai_completion(&json!({ "choices": [] })).is_err());
    }

    #[test]
    fn discovery_parsers_read_both_provider_shapes() {
        let ollama = parse_ollama_models(&json!({
            "models": [{ "name": "a:1b", "size": 12, "modified_at": "2026-01-01" }]
        }))
        .unwrap();
        assert_eq!(ollama[0].name, "a:1b");
        assert_eq!(ollama[0].size_bytes, Some(12));

        let openai =
            parse_openai_models(&json!({ "data": [{ "id": "gpt-x", "created": 7 }] })).unwrap();
        assert_eq!(openai[0].name, "gpt-x");
        assert!(parse_ollama_models(&json!({})).is_err());
        assert!(parse_openai_models(&json!({})).is_err());
    }

    #[test]
    fn capabilities_are_only_claimed_when_the_provider_reports_them() {
        let reported = parse_ollama_capabilities(
            "m",
            &json!({ "capabilities": ["completion", "tools", "vision"], "details": { "family": "qwen2", "parameter_size": "2B" } }),
        );
        assert_eq!(reported.tools, Some(true));
        assert_eq!(reported.vision, Some(true));
        assert_eq!(reported.family.as_deref(), Some("qwen2"));

        let unknown = parse_ollama_capabilities("m", &json!({}));
        assert_eq!(unknown.tools, None);
        assert_eq!(unknown.vision, None);
        assert!(unknown.note.contains("does not report"));
    }

    #[test]
    fn tool_turns_are_rendered_per_provider() {
        let assistant = AiMessage::Assistant {
            content: None,
            tool_calls: vec![ToolCall {
                id: "call_1".to_string(),
                name: "tap".to_string(),
                arguments: json!({ "x": 1, "y": 2 }),
            }],
        };
        let ollama = ai_message_to_ollama(&assistant);
        assert_eq!(ollama["tool_calls"][0]["function"]["name"], "tap");
        let openai = ai_message_to_openai(&assistant);
        assert_eq!(openai["tool_calls"][0]["id"], "call_1");
        assert_eq!(
            openai["tool_calls"][0]["function"]["arguments"],
            "{\"x\":1,\"y\":2}"
        );

        let tool = AiMessage::Tool(ToolResult {
            tool_call_id: "call_1".to_string(),
            name: "tap".to_string(),
            content: "ok".to_string(),
        });
        assert_eq!(ai_message_to_ollama(&tool)["role"], "tool");
        assert_eq!(ai_message_to_openai(&tool)["tool_call_id"], "call_1");
    }

    #[test]
    fn credentials_stay_out_of_errors_and_are_required_for_remote_providers() {
        set_credential(OPENAI_COMPATIBLE, "sk-test-secret-value").unwrap();
        assert!(has_credential(OPENAI_COMPATIBLE));
        let redacted = redact_secrets("provider rejected sk-test-secret-value");
        assert_eq!(redacted, "provider rejected [redacted]");
        clear_credential(OPENAI_COMPATIBLE).unwrap();
        assert!(!has_credential(OPENAI_COMPATIBLE));

        let error = chat(
            OPENAI_COMPATIBLE,
            DEFAULT_OPENAI_URL,
            "gpt-x",
            &[message("user", "hi")],
        )
        .unwrap_err();
        assert!(error.contains("requires an API key"));
    }

    #[test]
    fn image_messages_are_rendered_for_vision_models() {
        let mut user = message("user", "what is on screen?");
        user.images = Some(vec!["AAAA".to_string()]);
        assert_eq!(ollama_messages(&[user.clone()])[0]["images"][0], "AAAA");
        let openai = openai_messages(&[user]);
        assert_eq!(openai[0]["content"][1]["type"], "image_url");
        assert!(
            openai[0]["content"][1]["image_url"]["url"]
                .as_str()
                .unwrap()
                .starts_with("data:image/png;base64,")
        );
    }
}
