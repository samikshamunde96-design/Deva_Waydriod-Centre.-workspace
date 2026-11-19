use serde::Serialize;
use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const LOG_LIMIT: usize = 200;

#[derive(Clone, Debug, Serialize)]
pub struct LogEntry {
    /// Unix epoch seconds; the UI formats it in local time.
    pub timestamp: u64,
    pub level: String,
    pub action: String,
    pub detail: String,
}

static LOG: OnceLock<Mutex<VecDeque<LogEntry>>> = OnceLock::new();

fn log() -> &'static Mutex<VecDeque<LogEntry>> {
    LOG.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub fn record(level: &str, action: &str, detail: impl Into<String>) {
    let Ok(mut entries) = log().lock() else {
        return;
    };
    if entries.len() >= LOG_LIMIT {
        entries.pop_front();
    }
    entries.push_back(LogEntry {
        timestamp: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_secs())
            .unwrap_or_default(),
        level: level.to_string(),
        action: action.to_string(),
        detail: detail.into(),
    });
}

pub fn record_outcome<T>(action: &str, result: &Result<T, String>) {
    match result {
        Ok(_) => record("info", action, "completed"),
        Err(error) => record("error", action, error.clone()),
    }
}

/// For commands whose success message is already user-facing.
pub fn record_message(action: &str, result: &Result<String, String>) {
    match result {
        Ok(message) => record("info", action, message.clone()),
        Err(error) => record("error", action, error.clone()),
    }
}

/// Newest entries first.
pub fn entries() -> Vec<LogEntry> {
    log()
        .lock()
        .map(|entries| entries.iter().rev().cloned().collect())
        .unwrap_or_default()
}

pub fn last_summary() -> Option<String> {
    log().lock().ok().and_then(|entries| {
        entries
            .back()
            .map(|entry| format!("{}: {}", entry.action, entry.detail))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_entry_is_reported_first() {
        record("info", "log-test", "detail-value");
        let entries = entries();
        assert_eq!(entries[0].action, "log-test");
        assert_eq!(entries[0].detail, "detail-value");
        assert!(last_summary().is_some_and(|summary| summary.contains("log-test")));
    }
}
