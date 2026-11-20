//! Minimal HTTP client for AI providers.
//!
//! It shells out to the system `curl` (already required for the previous Ollama
//! bridge) instead of pulling a full async HTTP/TLS stack into a desktop app
//! that already spends its memory budget on Waydroid. Requests are passed to
//! curl through its config-file stdin, so URLs, bodies and `Authorization`
//! headers never appear in the process argument list.

use std::io::Write;
use std::process::{Command, Stdio};

const STATUS_MARKER: &str = "__WCC_HTTP_STATUS__";
const MAX_RESPONSE_BYTES: &str = "8388608";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// curl config strings use double quotes with backslash escapes.
fn config_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\r', "")
        .replace('\n', "\\n")
}

pub fn build_config(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
) -> String {
    let mut config = format!(
        "url = \"{}\"\nrequest = \"{}\"\n",
        config_escape(url),
        config_escape(method)
    );
    for (name, value) in headers {
        config.push_str(&format!(
            "header = \"{}: {}\"\n",
            config_escape(name),
            config_escape(value)
        ));
    }
    if let Some(body) = body {
        config.push_str(&format!("data = \"{}\"\n", config_escape(body)));
    }
    config
}

pub fn request(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: Option<&str>,
    timeout_seconds: u64,
) -> Result<Response, String> {
    let config = build_config(url, method, headers, body);
    let transfer_timeout = timeout_seconds.saturating_sub(2).max(1).to_string();
    let timeout = timeout_seconds.max(3).to_string();

    let mut child = Command::new("timeout")
        .args(["--kill-after=2s", &timeout, "curl", "-sS"])
        .args([
            "--proto",
            "=http,https",
            "--max-filesize",
            MAX_RESPONSE_BYTES,
        ])
        .args(["--connect-timeout", "10", "-m", &transfer_timeout])
        .args(["-K", "-"])
        .arg("-w")
        .arg(format!("\n{STATUS_MARKER}%{{http_code}}"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            format!("`curl` is required to reach AI providers but could not be started: {error}")
        })?;

    child
        .stdin
        .take()
        .ok_or_else(|| "The provider request could not be prepared.".to_string())?
        .write_all(config.as_bytes())
        .map_err(|error| format!("The provider request could not be sent: {error}"))?;

    let output = child
        .wait_with_output()
        .map_err(|error| format!("The provider request could not be completed: {error}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if matches!(output.status.code(), Some(124 | 137)) {
        return Err(format!(
            "The provider request timed out after {timeout_seconds}s."
        ));
    }
    if !output.status.success() {
        return Err(match stderr.is_empty() {
            true => format!("The provider request failed with status {}.", output.status),
            false => format!("The provider endpoint could not be reached: {stderr}"),
        });
    }

    let Some((body, status)) = stdout.rsplit_once(STATUS_MARKER) else {
        return Err("The provider returned an unreadable HTTP response.".to_string());
    };
    let status = status
        .trim()
        .parse::<u16>()
        .map_err(|_| "The provider returned an invalid HTTP status.".to_string())?;
    Ok(Response {
        status,
        body: body.trim_end_matches(['\r', '\n']).to_string(),
    })
}

/// Trims provider error bodies so they stay useful in logs without dumping
/// megabytes of HTML into the UI.
pub fn error_detail(body: &str) -> String {
    let compact = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() > 240 {
        let trimmed: String = compact.chars().take(240).collect();
        format!("{trimmed}…")
    } else {
        compact
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn config_escapes_quotes_backslashes_and_newlines() {
        let config = build_config(
            "http://127.0.0.1:11434/api/chat",
            "POST",
            &[("Authorization".to_string(), "Bearer a\"b\\c".to_string())],
            Some("{\"a\":\"line\\n\"}"),
        );
        assert!(config.contains("url = \"http://127.0.0.1:11434/api/chat\""));
        assert!(config.contains("header = \"Authorization: Bearer a\\\"b\\\\c\""));
        assert!(config.contains("data = \"{\\\"a\\\":\\\"line\\\\n\\\"}\""));
    }

    #[test]
    fn error_detail_is_compact_and_bounded() {
        let detail = error_detail(&format!("line one\nline two {}", "x".repeat(400)));
        assert!(!detail.contains('\n'));
        assert!(detail.chars().count() <= 241);
    }

    fn curl_available() -> bool {
        Command::new("curl")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    }

    #[test]
    fn request_sends_headers_and_reads_the_status() {
        if !curl_available() {
            eprintln!("curl is not installed; skipping HTTP transport test");
            return;
        }
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let read = stream.read(&mut request).unwrap();
            sender
                .send(String::from_utf8_lossy(&request[..read]).into_owned())
                .unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nContent-Type: application/json\r\n\r\n{\"ok\":true}")
                .unwrap();
        });

        let response = request(
            &format!("http://{address}/v1/chat/completions"),
            "POST",
            &[
                ("Content-Type".to_string(), "application/json".to_string()),
                ("Authorization".to_string(), "Bearer test-token".to_string()),
            ],
            Some("{\"model\":\"m\"}"),
            10,
        )
        .unwrap();
        server.join().unwrap();

        assert_eq!(response.status, 200);
        assert_eq!(response.body, "{\"ok\":true}");
        let received = receiver.recv().unwrap();
        assert!(received.starts_with("POST /v1/chat/completions"));
        assert!(received.contains("Authorization: Bearer test-token"));
        assert!(received.contains("\"model\":\"m\""));
    }
}
