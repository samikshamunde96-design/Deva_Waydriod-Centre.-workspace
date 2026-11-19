//! Local-only AI assistant bridge.
//!
//! The assistant talks to a locally running Ollama instance over loopback. It
//! deliberately has no device tools: the model can describe Android commands
//! but cannot execute anything, so the control surface stays explicit.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::process::{Command, Stdio};

const OLLAMA_URL: &str = "http://localhost:11434";
const MAX_MESSAGES: usize = 40;
const MAX_MESSAGE_CHARS: usize = 8_000;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

#[derive(Deserialize)]
struct TagList {
    models: Vec<TagEntry>,
}

#[derive(Deserialize)]
struct TagEntry {
    name: String,
}

#[derive(Deserialize)]
struct ChatReply {
    message: ChatMessage,
}

fn validate_model(model: &str) -> Result<(), String> {
    if model.is_empty() {
        return Err("Choose a local Ollama model first.".to_string());
    }
    if model.len() > 120
        || model
            .chars()
            .any(|ch| !ch.is_ascii_alphanumeric() && !matches!(ch, '.' | ':' | '-' | '_' | '/'))
    {
        return Err("Ollama model name contains unsupported characters.".to_string());
    }
    Ok(())
}

fn validate_messages(messages: &[ChatMessage]) -> Result<(), String> {
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
    }
    Ok(())
}

fn curl_json(path: &str, body: Option<String>, timeout_seconds: u64) -> Result<String, String> {
    let mut command = Command::new("timeout");
    command.args([
        "--kill-after=2s",
        &timeout_seconds.to_string(),
        "curl",
        "-sS",
        "-m",
        &timeout_seconds.saturating_sub(5).to_string(),
    ]);
    if body.is_some() {
        command.args([
            "-X",
            "POST",
            "-H",
            "Content-Type: application/json",
            "--data-binary",
            "@-",
        ]);
    }
    command
        .arg(format!("{OLLAMA_URL}{path}"))
        .stdin(if body.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|error| {
        format!("`curl` is required to reach the local Ollama service: {error}")
    })?;
    if let Some(body) = body {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Ollama request body could not be written.".to_string())?;
        stdin
            .write_all(body.as_bytes())
            .map_err(|error| format!("Ollama request body could not be written: {error}"))?;
    }
    let output = child
        .wait_with_output()
        .map_err(|error| format!("Ollama request could not be completed: {error}"))?;
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        if stderr.contains("Failed to connect") || stderr.contains("Could not connect") {
            return Err(
                "Local Ollama is not reachable on localhost:11434. Start it with `ollama serve`."
                    .to_string(),
            );
        }
        let note = if matches!(output.status.code(), Some(124 | 137)) {
            "Ollama request timed out"
        } else {
            "Ollama request failed"
        };
        return Err(if stderr.is_empty() {
            note.to_string()
        } else {
            format!("{note}: {stderr}")
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn list_models() -> Result<Vec<String>, String> {
    let raw = curl_json("/api/tags", None, 10)?;
    let tags: TagList = serde_json::from_str(&raw)
        .map_err(|error| format!("Ollama returned an unreadable model list: {error}"))?;
    Ok(tags.models.into_iter().map(|entry| entry.name).collect())
}

pub fn chat(model: &str, messages: &[ChatMessage]) -> Result<String, String> {
    validate_model(model)?;
    validate_messages(messages)?;
    let body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
    })
    .to_string();
    let raw = curl_json("/api/chat", Some(body), 180)?;
    let reply: ChatReply = serde_json::from_str(&raw)
        .map_err(|error| format!("Ollama returned an unreadable reply: {error}"))?;
    if reply.message.content.trim().is_empty() {
        return Err("Ollama returned an empty reply.".to_string());
    }
    Ok(reply.message.content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.to_string(),
            content: content.to_string(),
        }
    }

    #[test]
    fn model_and_message_validation_rejects_unsafe_input() {
        assert!(validate_model("qwen2.5-coder:1.5b").is_ok());
        assert!(validate_model("model; rm -rf /").is_err());
        assert!(validate_messages(&[message("user", "hi")]).is_ok());
        assert!(validate_messages(&[message("tool", "hi")]).is_err());
        assert!(validate_messages(&[message("user", " ")]).is_err());
    }
}
