//! User preferences persisted as one validated JSON document.
//!
//! Settings never contain credentials: provider secrets are session-only and
//! live in `providers`. Writes are atomic (temporary file + rename) so a crash
//! cannot leave a half-written configuration behind.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const CURRENT_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub version: u32,
    pub theme: String,
    pub chat_provider: String,
    pub chat_model: String,
    pub action_provider: String,
    pub action_model: String,
    pub openai_base_url: String,
    pub ollama_base_url: String,
    pub auto_refresh: bool,
    pub last_android_directory: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            theme: "dark".to_string(),
            chat_provider: "ollama".to_string(),
            chat_model: String::new(),
            action_provider: "ollama".to_string(),
            action_model: String::new(),
            openai_base_url: crate::providers::DEFAULT_OPENAI_URL.to_string(),
            ollama_base_url: crate::providers::DEFAULT_OLLAMA_URL.to_string(),
            auto_refresh: false,
            last_android_directory: "/sdcard".to_string(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SettingsLoad {
    pub settings: Settings,
    pub warning: Option<String>,
}

impl Settings {
    /// Rejects anything that would produce a broken provider request or a
    /// path that the file browser cannot open.
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.theme.as_str(), "dark" | "light") {
            return Err("Theme must be 'dark' or 'light'.".to_string());
        }
        crate::providers::validate_provider(&self.chat_provider)?;
        crate::providers::validate_provider(&self.action_provider)?;
        crate::providers::validate_model_name(&self.chat_model)?;
        crate::providers::validate_model_name(&self.action_model)?;
        crate::providers::validate_base_url(&self.openai_base_url)?;
        crate::providers::validate_base_url(&self.ollama_base_url)?;
        if self.last_android_directory.trim().is_empty()
            || !self.last_android_directory.starts_with('/')
        {
            return Err("The last Android directory must be an absolute path.".to_string());
        }
        if self.last_android_directory.chars().any(char::is_control) {
            return Err(
                "The last Android directory cannot contain control characters.".to_string(),
            );
        }
        Ok(())
    }
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| Path::new(&home).join(".config")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("waydroid-control-center").join("settings.json")
}

pub fn load() -> SettingsLoad {
    load_from(&config_path())
}

fn load_from(path: &Path) -> SettingsLoad {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return SettingsLoad {
                settings: Settings::default(),
                warning: None,
            };
        }
        Err(error) => {
            return SettingsLoad {
                settings: Settings::default(),
                warning: Some(format!("Settings could not be read: {error}")),
            };
        }
    };
    match parse(&raw) {
        Ok(settings) => SettingsLoad {
            settings,
            warning: None,
        },
        Err(error) => {
            // Keep the broken document for inspection instead of silently
            // overwriting it with defaults.
            let backup = path.with_extension(format!(
                "corrupt-{}",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|duration| duration.as_secs())
                    .unwrap_or_default()
            ));
            let _ = std::fs::rename(path, &backup);
            SettingsLoad {
                settings: Settings::default(),
                warning: Some(format!(
                    "Settings were invalid ({error}); defaults were used and the previous file was kept at {}.",
                    backup.display()
                )),
            }
        }
    }
}

fn parse(raw: &str) -> Result<Settings, String> {
    let mut settings: Settings =
        serde_json::from_str(raw).map_err(|error| format!("invalid JSON: {error}"))?;
    settings.version = CURRENT_VERSION;
    settings.validate()?;
    Ok(settings)
}

pub fn save(settings: &Settings) -> Result<Settings, String> {
    settings.validate()?;
    save_to(&config_path(), settings)?;
    Ok(settings.clone())
}

fn save_to(path: &Path, settings: &Settings) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "Settings path has no parent directory.".to_string())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("Settings directory could not be created: {error}"))?;
    let temporary = parent.join(format!(".settings-{}.tmp", std::process::id()));
    let body = serde_json::to_vec_pretty(settings)
        .map_err(|error| format!("Settings could not be serialized: {error}"))?;
    std::fs::write(&temporary, body)
        .map_err(|error| format!("Settings could not be written: {error}"))?;
    std::fs::rename(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("Settings could not be replaced atomically: {error}")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temporary_directory() -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "wcc-settings-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn defaults_round_trip_and_carry_a_version() {
        let directory = temporary_directory();
        let path = directory.join("settings.json");
        let defaults = Settings::default();
        save_to(&path, &defaults).unwrap();
        let loaded = load_from(&path);
        assert!(loaded.warning.is_none());
        assert_eq!(loaded.settings, defaults);
        assert_eq!(loaded.settings.version, CURRENT_VERSION);
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn missing_keys_are_filled_from_defaults_and_unknown_keys_are_ignored() {
        let settings = parse(r#"{"theme":"light","future_field":true}"#).unwrap();
        assert_eq!(settings.theme, "light");
        assert_eq!(settings.chat_provider, "ollama");
        assert_eq!(settings.version, CURRENT_VERSION);
    }

    #[test]
    fn invalid_values_are_rejected_instead_of_persisted() {
        assert!(
            Settings {
                theme: "purple".to_string(),
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                openai_base_url: "file:///etc/passwd".to_string(),
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            Settings {
                last_android_directory: "relative/path".to_string(),
                ..Settings::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn corrupt_json_is_backed_up_and_replaced_by_defaults() {
        let directory = temporary_directory();
        let path = directory.join("settings.json");
        std::fs::write(&path, b"{ not json").unwrap();
        let loaded = load_from(&path);
        assert!(loaded.warning.is_some());
        assert_eq!(loaded.settings, Settings::default());
        assert!(!path.exists(), "the broken file should be moved aside");
        let kept = std::fs::read_dir(&directory)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.file_name().to_string_lossy().contains("corrupt-"));
        assert!(kept, "the broken document should be preserved");
        let _ = std::fs::remove_dir_all(directory);
    }

    #[test]
    fn settings_contain_no_credential_field() {
        let json = serde_json::to_string(&Settings::default()).unwrap();
        for forbidden in ["api_key", "secret", "token", "authorization"] {
            assert!(!json.contains(forbidden));
        }
    }
}
