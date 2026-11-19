use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock, RwLock};

use crate::waydroid::{decode_screenshot_png, run_command, run_output};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppEntry {
    pub name: Option<String>,
    pub package: String,
    pub category: String,
    pub source: String,
    pub version_name: Option<String>,
    pub version_code: Option<String>,
    pub install_path: Option<String>,
    pub user_app: bool,
    pub system_app: bool,
    pub is_installed: bool,
    pub launcher_activity: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppInfo {
    pub name: Option<String>,
    pub package: String,
    pub category: String,
    pub source: String,
    pub version_name: Option<String>,
    pub version_code: Option<String>,
    pub install_path: Option<String>,
    pub is_user_app: bool,
    pub is_system_app: bool,
    pub launcher_activity: Option<String>,
    pub launch_action: String,
    pub install_available: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApkFileInfo {
    pub path: String,
    pub filename: String,
    pub size_bytes: u64,
    pub package_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceSelection {
    pub serial: String,
    pub product: String,
    pub model: String,
    pub device_type: String,
    pub state: String,
    pub is_waydroid: bool,
}

static SELECTED_DEVICE: OnceLock<RwLock<Option<DeviceSelection>>> = OnceLock::new();
static ANDROID_OPERATION_LOCK: Mutex<()> = Mutex::new(());

pub fn validate_device_serial(serial: &str) -> Result<(), String> {
    if serial.trim().is_empty() {
        return Err("Device serial is required.".to_string());
    }
    if serial
        .chars()
        .any(|ch| !ch.is_ascii_alphanumeric() && !matches!(ch, ':' | '.' | '_' | '-' | '/' | '@'))
    {
        return Err("Device serial contains unsupported characters.".to_string());
    }
    Ok(())
}

pub fn validate_package_name(package_name: &str) -> Result<(), String> {
    if package_name.trim().is_empty() {
        return Err("Package name is required.".to_string());
    }
    if !package_name.split('.').all(|segment| {
        let mut chars = segment.chars();
        chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
    }) {
        return Err(
            "Package name must contain dot-separated Android identifier segments.".to_string(),
        );
    }
    Ok(())
}

pub fn validate_apk_path(path: &str) -> Result<std::path::PathBuf, String> {
    let candidate = Path::new(path);
    let metadata = std::fs::symlink_metadata(candidate)
        .map_err(|error| format!("APK file does not exist or cannot be read: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("APK path must point to a file.".to_string());
    }
    let extension = candidate
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("");
    if !extension.eq_ignore_ascii_case("apk") {
        return Err("Only APK files are supported for installation.".to_string());
    }
    if metadata.len() == 0 {
        return Err("APK file is empty.".to_string());
    }
    if metadata.len() > 500 * 1024 * 1024 {
        return Err("APK exceeds the supported installation size limit (500 MB).".to_string());
    }
    Ok(candidate.to_path_buf())
}

pub fn inspect_apk(path: &str) -> Result<ApkFileInfo, String> {
    let path = validate_apk_path(path)?;
    let path_string = path
        .to_str()
        .ok_or_else(|| "APK path is not valid UTF-8.".to_string())?
        .to_string();
    let output = run_output(
        "unzip",
        &["-Z", "-1", &path_string, "AndroidManifest.xml"],
        8,
    )?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != "AndroidManifest.xml"
    {
        return Err(
            "Selected file is not a valid APK: its ZIP archive has no AndroidManifest.xml."
                .to_string(),
        );
    }
    let manifest = read_apk_manifest(&path_string)?;
    let package_name = parse_manifest_package(&manifest).ok_or_else(|| {
        "APK AndroidManifest.xml does not contain a valid package identifier.".to_string()
    })?;
    let metadata = std::fs::metadata(&path)
        .map_err(|error| format!("Unable to inspect APK metadata: {error}"))?;
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "APK filename is not valid UTF-8.".to_string())?
        .to_string();
    Ok(ApkFileInfo {
        path: path_string,
        filename,
        size_bytes: metadata.len(),
        package_name,
    })
}

const APK_MANIFEST_LIMIT: u64 = 2 * 1024 * 1024;

fn read_apk_manifest(path: &str) -> Result<Vec<u8>, String> {
    let mut child = Command::new("timeout")
        .args([
            "--kill-after=2s",
            "8",
            "unzip",
            "-p",
            path,
            "AndroidManifest.xml",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Unable to read APK manifest: {error}"))?;
    let mut manifest = Vec::new();
    child
        .stdout
        .take()
        .ok_or_else(|| "Unable to read APK manifest output.".to_string())?
        .take(APK_MANIFEST_LIMIT + 1)
        .read_to_end(&mut manifest)
        .map_err(|error| format!("Unable to read APK manifest: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("Unable to finish reading APK manifest: {error}"))?;
    if manifest.len() as u64 > APK_MANIFEST_LIMIT {
        return Err("APK manifest exceeds the supported 2 MiB limit.".to_string());
    }
    if !output.status.success() {
        return Err(format!(
            "Unable to extract APK AndroidManifest.xml (status {}).",
            output.status
        ));
    }
    Ok(manifest)
}

fn read_u16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_le_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    ))
}

fn read_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_le_bytes(
        bytes.get(offset..offset + 4)?.try_into().ok()?,
    ))
}

fn string_pool(bytes: &[u8], chunk: usize, chunk_end: usize) -> Option<Vec<String>> {
    let header_size = read_u16(bytes, chunk + 2)? as usize;
    let count = read_u32(bytes, chunk + 8)? as usize;
    let flags = read_u32(bytes, chunk + 16)?;
    let strings_start = read_u32(bytes, chunk + 20)? as usize;
    let utf8 = flags & 0x100 != 0;
    let mut strings = Vec::with_capacity(count);
    for index in 0..count {
        let relative = read_u32(bytes, chunk + header_size + index * 4)? as usize;
        let mut cursor = chunk + strings_start + relative;
        let value = if utf8 {
            let (_, next) = string_length8(bytes, cursor)?;
            cursor = next;
            let (length, next) = string_length8(bytes, cursor)?;
            cursor = next;
            let end = cursor.checked_add(length)?;
            if end >= chunk_end || bytes.get(end).copied()? != 0 {
                return None;
            }
            String::from_utf8_lossy(bytes.get(cursor..end)?).into_owned()
        } else {
            let (length, next) = string_length16(bytes, cursor)?;
            cursor = next;
            let byte_len = length.checked_mul(2)?;
            let end = cursor.checked_add(byte_len)?;
            if end > chunk_end {
                return None;
            }
            let units = bytes
                .get(cursor..end)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair));
            char::decode_utf16(units)
                .map(|result| result.unwrap_or(char::REPLACEMENT_CHARACTER))
                .collect()
        };
        strings.push(value);
    }
    Some(strings)
}

fn string_length8(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    let first = *bytes.get(offset)?;
    if first & 0x80 == 0 {
        Some((first as usize, offset + 1))
    } else {
        Some((
            (((first & 0x7f) as usize) << 8) | *bytes.get(offset + 1)? as usize,
            offset + 2,
        ))
    }
}

fn string_length16(bytes: &[u8], offset: usize) -> Option<(usize, usize)> {
    let first = read_u16(bytes, offset)?;
    if first & 0x8000 == 0 {
        Some((first as usize, offset + 2))
    } else {
        Some((
            (((first & 0x7fff) as usize) << 16) | read_u16(bytes, offset + 2)? as usize,
            offset + 4,
        ))
    }
}

fn parse_manifest_package(bytes: &[u8]) -> Option<String> {
    let mut strings = None;
    let mut offset = 8;
    while offset + 8 <= bytes.len() {
        let chunk_type = read_u16(bytes, offset)?;
        let header_size = read_u16(bytes, offset + 2)? as usize;
        let chunk_size = read_u32(bytes, offset + 4)? as usize;
        let chunk_end = offset.checked_add(chunk_size)?;
        if header_size < 8 || chunk_end > bytes.len() || chunk_size < header_size {
            return None;
        }
        match chunk_type {
            0x0001 => strings = Some(string_pool(bytes, offset, chunk_end)?),
            0x0102 => {
                let pool = strings.as_ref()?;
                let element_name = pool.get(read_u32(bytes, offset + 20)? as usize)?;
                if element_name != "manifest" {
                    offset = chunk_end;
                    continue;
                }
                let attributes_offset = read_u16(bytes, offset + 24)? as usize;
                let attribute_size = read_u16(bytes, offset + 26)? as usize;
                let attribute_count = read_u16(bytes, offset + 28)? as usize;
                if attribute_size < 20 {
                    return None;
                }
                let attributes = offset.checked_add(16 + attributes_offset)?;
                for index in 0..attribute_count {
                    let attribute = attributes.checked_add(index.checked_mul(attribute_size)?)?;
                    let name_index = read_u32(bytes, attribute + 4)? as usize;
                    if pool.get(name_index)?.as_str() != "package" {
                        continue;
                    }
                    let raw_value = read_u32(bytes, attribute + 8)?;
                    let value_index = if raw_value != u32::MAX {
                        raw_value
                    } else if bytes.get(attribute + 15).copied()? == 0x03 {
                        read_u32(bytes, attribute + 16)?
                    } else {
                        return None;
                    };
                    let package_name = pool.get(value_index as usize)?.clone();
                    return validate_package_name(&package_name)
                        .is_ok()
                        .then_some(package_name);
                }
                return None;
            }
            _ => {}
        }
        offset = chunk_end;
    }
    None
}

pub fn detect_adb_devices() -> Result<Vec<DeviceSelection>, String> {
    // A single `adb devices` call also proves the binary exists; probing
    // `adb version` first doubled the subprocess cost of every ADB check.
    let raw = run_command("adb", &["devices", "-l"], 8)?;
    Ok(parse_adb_devices(&raw))
}

fn parse_adb_devices(raw: &str) -> Vec<DeviceSelection> {
    let mut devices = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("List of devices") {
            continue;
        }
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let serial = parts[0].to_string();
        let state = parts[1].to_string();
        let mut product = String::new();
        let mut model = String::new();
        let mut device_type = String::new();
        for token in &parts[2..] {
            if let Some(value) = token.strip_prefix("product:") {
                product = value.to_string();
            } else if let Some(value) = token.strip_prefix("model:") {
                model = value.to_string();
            } else if let Some(value) = token.strip_prefix("device:") {
                device_type = value.to_string();
            }
        }
        devices.push(DeviceSelection {
            serial: serial.clone(),
            product: product.clone(),
            model: model.clone(),
            device_type: device_type.clone(),
            state: state.clone(),
            is_waydroid: serial.contains("waydroid")
                || product.contains("waydroid")
                || model.contains("WayDroid"),
        });
    }

    devices
}

pub fn list_adb_devices() -> Result<Vec<DeviceSelection>, String> {
    detect_adb_devices()
}

pub fn get_selected_device() -> Result<DeviceSelection, String> {
    let lock = SELECTED_DEVICE.get_or_init(|| RwLock::new(None));
    let selected = lock
        .read()
        .map_err(|_| "Selected-device state is unavailable.".to_string())?
        .clone();
    selected.ok_or_else(|| {
        "No ADB device is currently selected. Choose a device before running Android operations."
            .to_string()
    })
}

pub fn set_selected_device(serial: &str) -> Result<DeviceSelection, String> {
    let _operation = ANDROID_OPERATION_LOCK
        .lock()
        .map_err(|_| "Android operation lock is unavailable.".to_string())?;
    validate_device_serial(serial)?;
    let devices = detect_adb_devices()?;
    let device = devices
        .iter()
        .find(|entry| entry.serial == serial)
        .cloned()
        .ok_or_else(|| format!("ADB device '{}' is not currently connected.", serial))?;

    if device.state != "device" {
        return Err(format!(
            "ADB device '{}' is not ready for Android operations. State: {}.",
            device.serial, device.state
        ));
    }

    let lock = SELECTED_DEVICE.get_or_init(|| RwLock::new(None));
    let mut selected = lock
        .write()
        .map_err(|_| "Selected-device state is unavailable.".to_string())?;
    *selected = Some(device.clone());
    Ok(device)
}

pub fn begin_selected_device_operation()
-> Result<(MutexGuard<'static, ()>, DeviceSelection), String> {
    let guard = ANDROID_OPERATION_LOCK
        .lock()
        .map_err(|_| "Android operation lock is unavailable.".to_string())?;
    let selected = get_selected_device()?;
    verify_device_connected(&selected)?;
    Ok((guard, selected))
}

pub fn verify_device_connected(device: &DeviceSelection) -> Result<(), String> {
    let current = get_selected_device()?;
    if current.serial != device.serial {
        return Err("Selected ADB device changed during the operation.".to_string());
    }
    let devices = detect_adb_devices()?;
    verify_device_in_list(&devices, &device.serial)
}

fn verify_device_in_list(devices: &[DeviceSelection], serial: &str) -> Result<(), String> {
    match devices.iter().find(|candidate| candidate.serial == serial) {
        Some(candidate) if candidate.state == "device" => Ok(()),
        Some(candidate) => Err(format!(
            "Selected ADB device '{}' is no longer ready (state: {}).",
            serial, candidate.state
        )),
        None => Err(format!(
            "Selected ADB device '{}' disconnected; operation stopped.",
            serial
        )),
    }
}

pub fn adb_command(device_serial: &str, args: &[&str]) -> Result<String, String> {
    adb_command_timeout(device_serial, args, 8)
}

pub fn adb_command_timeout(
    device_serial: &str,
    args: &[&str],
    timeout_seconds: u64,
) -> Result<String, String> {
    let output = adb_output(device_serial, args, timeout_seconds)?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if stderr.is_empty() { stdout } else { stderr };
        let failure = if matches!(output.status.code(), Some(124 | 137)) {
            format!("ADB command timed out after {timeout_seconds}s")
        } else {
            format!("ADB command failed with exit status {}", output.status)
        };
        if detail.is_empty() {
            Err(failure)
        } else {
            Err(format!("{failure}: {detail}"))
        }
    }
}

pub fn adb_output(
    device_serial: &str,
    args: &[&str],
    timeout_seconds: u64,
) -> Result<Output, String> {
    validate_device_serial(device_serial)?;
    let selected = get_selected_device()?;
    if selected.serial != device_serial {
        return Err(
            "ADB operation target does not match the explicitly selected device.".to_string(),
        );
    }
    verify_device_connected(&selected)?;

    let mut command_args = vec!["-s", device_serial];
    command_args.extend_from_slice(args);
    run_output("adb", &command_args, timeout_seconds)
}

fn classify_package(path: &str) -> (String, bool, bool) {
    let lower = path.to_ascii_lowercase();
    let is_system = lower.contains("/system/")
        || lower.contains("/product/")
        || lower.contains("/apex/")
        || lower.contains("/vendor/")
        || lower.starts_with("/system")
        || lower.starts_with("/product")
        || lower.starts_with("/apex");
    let is_user = lower.contains("/data/app/")
        || lower.contains("/data/user/")
        || lower.contains("/user_de/0")
        || lower.contains("/user/0");
    let category = if is_user {
        "User".to_string()
    } else if is_system {
        "System".to_string()
    } else {
        "Android".to_string()
    };
    (category, is_user, is_system)
}

fn parse_installed_packages(raw: &str) -> Vec<AppEntry> {
    let mut apps = Vec::new();
    let mut seen = HashSet::new();
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || !trimmed.starts_with("package:") {
            continue;
        }
        let pair = trimmed.strip_prefix("package:").unwrap_or(trimmed);
        let mut package_name = pair.to_string();
        let mut install_path = None;
        if let Some((path, pkg)) = pair.split_once('=') {
            install_path = Some(path.to_string());
            package_name = pkg.to_string();
        }
        if validate_package_name(&package_name).is_err() || !seen.insert(package_name.clone()) {
            continue;
        }
        let (category, is_user, is_system) = if let Some(path) = install_path.as_deref() {
            classify_package(path)
        } else {
            ("Android".to_string(), false, false)
        };
        apps.push(AppEntry {
            name: None,
            package: package_name,
            category,
            source: "ADB".to_string(),
            version_name: None,
            version_code: None,
            install_path,
            user_app: is_user,
            system_app: is_system,
            is_installed: true,
            launcher_activity: None,
        });
    }
    apps
}

fn package_list_contains(output: &str, package_name: &str) -> bool {
    output
        .lines()
        .any(|line| line.trim() == format!("package:{package_name}"))
}

pub fn list_installed_apps(device_serial: &str) -> Result<Vec<AppEntry>, String> {
    validate_device_serial(device_serial)?;
    let selected = get_selected_device()?;
    if selected.serial != device_serial {
        return Err(
            "ADB operation target does not match the explicitly selected device.".to_string(),
        );
    }
    verify_device_connected(&selected)?;
    crate::waydroid::WaydroidPowerManager::new().ensure_android_ready(&selected)?;
    let raw = adb_command(device_serial, &["shell", "pm", "list", "packages", "-f"])?;
    Ok(parse_installed_packages(&raw))
}

pub fn list_installed_apps_selected() -> Result<Vec<AppEntry>, String> {
    let selected = get_selected_device()?;
    list_installed_apps(&selected.serial)
}

pub fn is_app_installed(device_serial: &str, package_name: &str) -> Result<bool, String> {
    validate_package_name(package_name)?;
    validate_device_serial(device_serial)?;
    let selected = get_selected_device()?;
    if selected.serial != device_serial {
        return Err(
            "ADB operation target does not match the explicitly selected device.".to_string(),
        );
    }
    verify_device_connected(&selected)?;
    crate::waydroid::WaydroidPowerManager::new().ensure_android_ready(&selected)?;
    let output = adb_command(
        device_serial,
        &["shell", "pm", "list", "packages", package_name],
    )?;
    Ok(package_list_contains(&output, package_name))
}

fn foreground_component(raw: &str) -> Option<String> {
    raw.lines().find_map(|line| {
        let trimmed = line.trim();
        if !trimmed.to_ascii_lowercase().contains("mcurrentfocus")
            && !trimmed.to_ascii_lowercase().contains("mfocusedapp")
        {
            return None;
        }
        let component = trimmed.split_whitespace().last()?.trim_end_matches('}');
        component.contains('/').then(|| component.to_string())
    })
}

pub fn get_app_info(device_serial: &str, package_name: &str) -> Result<AppInfo, String> {
    validate_package_name(package_name)?;
    validate_device_serial(device_serial)?;
    let selected = get_selected_device()?;
    if selected.serial != device_serial {
        return Err(
            "ADB operation target does not match the explicitly selected device.".to_string(),
        );
    }
    verify_device_connected(&selected)?;
    crate::waydroid::WaydroidPowerManager::new().ensure_android_ready(&selected)?;
    if !is_app_installed(device_serial, package_name)? {
        return Err(format!(
            "Application {package_name} is not installed on the selected device."
        ));
    }
    let metadata = adb_command(
        device_serial,
        &["shell", "dumpsys", "package", package_name],
    )?;
    let install_path = adb_command(device_serial, &["shell", "pm", "path", package_name])?
        .lines()
        .find_map(|line| line.strip_prefix("package:").map(str::to_string));
    let version_name = metadata.lines().find_map(|line| {
        line.split_once("versionName=")
            .map(|(_, value)| value.trim().to_string())
    });
    let version_code = metadata.lines().find_map(|line| {
        line.split_once("versionCode=")
            .map(|(_, value)| value.split_whitespace().next().unwrap_or("").to_string())
    });
    let launcher_activity = adb_command(
        device_serial,
        &[
            "shell",
            "cmd",
            "package",
            "resolve-activity",
            "--brief",
            package_name,
        ],
    )
    .ok()
    .and_then(|line| {
        let value = line.trim();
        if value.is_empty() || value.starts_with("No activity found") || value.starts_with("Error:")
        {
            None
        } else if let Some((_, activity)) = value.split_once('/') {
            Some(activity.to_string())
        } else {
            Some(value.to_string())
        }
    });
    let has_launcher = launcher_activity.is_some();
    let is_user = install_path
        .as_deref()
        .map(|path| path.contains("/data/") || path.contains("/user/"))
        .unwrap_or(false);
    let is_system = install_path
        .as_deref()
        .map(|path| {
            path.contains("/system/")
                || path.contains("/product/")
                || path.contains("/apex/")
                || path.contains("/vendor/")
        })
        .unwrap_or(false);
    Ok(AppInfo {
        name: None,
        package: package_name.to_string(),
        category: if is_user {
            "User".to_string()
        } else if is_system {
            "System".to_string()
        } else {
            "Android".to_string()
        },
        source: "ADB".to_string(),
        version_name,
        version_code,
        install_path,
        is_user_app: is_user,
        is_system_app: is_system,
        launcher_activity: launcher_activity.clone(),
        launch_action: if has_launcher {
            "MAIN".to_string()
        } else {
            "Unavailable".to_string()
        },
        install_available: true,
    })
}

pub fn launch_app(device_serial: &str, package_name: &str) -> Result<String, String> {
    validate_package_name(package_name)?;
    validate_device_serial(device_serial)?;
    let selected = get_selected_device()?;
    if selected.serial != device_serial {
        return Err(
            "ADB operation target does not match the explicitly selected device.".to_string(),
        );
    }
    verify_device_connected(&selected)?;
    crate::waydroid::WaydroidPowerManager::new().ensure_android_ready(&selected)?;
    if !is_app_installed(device_serial, package_name)? {
        return Err(format!(
            "Application {package_name} is not installed on the selected device."
        ));
    }
    let resolved = adb_command(
        device_serial,
        &[
            "shell",
            "cmd",
            "package",
            "resolve-activity",
            "--brief",
            package_name,
        ],
    );
    let intent = match resolved {
        Ok(raw) if !raw.trim().is_empty() && !raw.contains("No activity found") => {
            raw.trim().to_string()
        }
        Ok(_) => {
            return Err(format!(
                "{} has no launcher activity available on the selected device.",
                package_name
            ));
        }
        Err(error) => {
            return Err(format!(
                "Unable to resolve a launcher for {}: {error}",
                package_name
            ));
        }
    };
    let activity = intent.split_whitespace().last().unwrap_or(&intent).trim();
    let launch_target = if activity.contains('/') {
        activity.to_string()
    } else {
        format!("{package_name}/{activity}")
    };
    let output = adb_command(
        device_serial,
        &["shell", "am", "start", "-W", "-n", &launch_target],
    );
    let output = output.map_err(|error| format!("Unable to launch {package_name}: {error}"))?;
    if !output.contains("Status: ok") {
        return Err(format!(
            "Android did not confirm that {package_name} launched: {output}"
        ));
    }
    let foreground = get_foreground_app(device_serial)?;
    if !foreground.starts_with(&format!("{package_name}/")) && foreground != package_name {
        return Err(format!(
            "Android accepted the launch request, but the foreground app is {foreground}, not {package_name}."
        ));
    }
    Ok(format!(
        "{package_name} is in the foreground ({foreground})."
    ))
}

pub fn get_foreground_app(device_serial: &str) -> Result<String, String> {
    validate_device_serial(device_serial)?;
    let selected = get_selected_device()?;
    if selected.serial != device_serial {
        return Err(
            "ADB operation target does not match the explicitly selected device.".to_string(),
        );
    }
    verify_device_connected(&selected)?;
    crate::waydroid::WaydroidPowerManager::new().ensure_android_ready(&selected)?;
    let raw = adb_command(device_serial, &["shell", "dumpsys", "window", "windows"])?;
    foreground_component(&raw)
        .ok_or_else(|| "Android did not report a foreground application.".to_string())
}

fn selected_ready_device() -> Result<DeviceSelection, String> {
    let selected = get_selected_device()?;
    verify_device_connected(&selected)?;
    Ok(selected)
}

/// Screen capture bytes, validated as a decodable PNG.
pub fn capture_screenshot() -> Result<Vec<u8>, String> {
    let selected = selected_ready_device()?;
    let output = adb_output(&selected.serial, &["exec-out", "screencap", "-p"], 8)?;
    if !output.status.success() {
        return Err("Screenshot command failed on the selected device.".to_string());
    }
    decode_screenshot_png(&output.stdout)?;
    Ok(output.stdout)
}

pub fn screenshot_base64() -> Result<String, String> {
    Ok(base64_encode(&capture_screenshot()?))
}

fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let block = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let value = (u32::from(block[0]) << 16) | (u32::from(block[1]) << 8) | u32::from(block[2]);
        encoded.push(TABLE[(value >> 18) as usize & 63] as char);
        encoded.push(TABLE[(value >> 12) as usize & 63] as char);
        encoded.push(if chunk.len() > 1 {
            TABLE[(value >> 6) as usize & 63] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            TABLE[value as usize & 63] as char
        } else {
            '='
        });
    }
    encoded
}

const MAX_INPUT_COORDINATE: u32 = 8_192;

fn validate_point(x: u32, y: u32) -> Result<(), String> {
    if x > MAX_INPUT_COORDINATE || y > MAX_INPUT_COORDINATE {
        return Err(format!(
            "Screen coordinates must be between 0 and {MAX_INPUT_COORDINATE}."
        ));
    }
    Ok(())
}

pub fn tap(x: u32, y: u32) -> Result<String, String> {
    validate_point(x, y)?;
    let selected = selected_ready_device()?;
    adb_command_timeout(
        &selected.serial,
        &["shell", "input", "tap", &x.to_string(), &y.to_string()],
        6,
    )?;
    Ok(format!("Tap sent at {x},{y}."))
}

pub fn swipe(x1: u32, y1: u32, x2: u32, y2: u32, duration_ms: u32) -> Result<String, String> {
    validate_point(x1, y1)?;
    validate_point(x2, y2)?;
    if !(50..=3_000).contains(&duration_ms) {
        return Err("Swipe duration must be between 50 and 3000 ms.".to_string());
    }
    let selected = selected_ready_device()?;
    adb_command_timeout(
        &selected.serial,
        &[
            "shell",
            "input",
            "swipe",
            &x1.to_string(),
            &y1.to_string(),
            &x2.to_string(),
            &y2.to_string(),
            &duration_ms.to_string(),
        ],
        8,
    )?;
    Ok(format!("Swipe sent from {x1},{y1} to {x2},{y2}."))
}

pub const ANDROID_KEYS: [(&str, &str); 12] = [
    ("home", "KEYCODE_HOME"),
    ("back", "KEYCODE_BACK"),
    ("recents", "KEYCODE_APP_SWITCH"),
    ("power", "KEYCODE_POWER"),
    ("wakeup", "KEYCODE_WAKEUP"),
    ("sleep", "KEYCODE_SLEEP"),
    ("enter", "KEYCODE_ENTER"),
    ("delete", "KEYCODE_DEL"),
    ("volume_up", "KEYCODE_VOLUME_UP"),
    ("volume_down", "KEYCODE_VOLUME_DOWN"),
    ("menu", "KEYCODE_MENU"),
    ("search", "KEYCODE_SEARCH"),
];

pub fn press_key(key: &str) -> Result<String, String> {
    let code = ANDROID_KEYS
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, code)| *code)
        .ok_or_else(|| format!("Unsupported Android key '{key}'."))?;
    let selected = selected_ready_device()?;
    adb_command_timeout(&selected.serial, &["shell", "input", "keyevent", code], 6)?;
    Ok(format!("Sent key event {key}."))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AndroidFileEntry {
    pub name: String,
    pub is_dir: bool,
    pub is_link: bool,
    pub size_bytes: u64,
    pub modified: String,
}

/// Remote paths are passed to the device shell, so only characters that cannot
/// be split or re-interpreted are accepted.
pub fn validate_android_path(path: &str) -> Result<(), String> {
    if !path.starts_with('/') {
        return Err("Android path must be absolute, for example /sdcard/Download.".to_string());
    }
    if path.len() > 512 {
        return Err("Android path is longer than the supported 512 characters.".to_string());
    }
    if path.split('/').any(|segment| segment == "..") {
        return Err("Android path cannot contain '..' segments.".to_string());
    }
    if path
        .chars()
        .any(|ch| !(ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-')))
    {
        return Err(
            "Android path may only contain letters, digits, '/', '.', '_' and '-'.".to_string(),
        );
    }
    Ok(())
}

fn parse_ls_long(raw: &str) -> Vec<AndroidFileEntry> {
    raw.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with("total ") {
                return None;
            }
            let parts: Vec<&str> = trimmed.split_whitespace().collect();
            if parts.len() < 8 || parts[0].len() < 10 {
                return None;
            }
            let is_link = parts[0].starts_with('l');
            let mut name = parts[7..].join(" ");
            if is_link && let Some((link, _)) = name.split_once(" -> ") {
                name = link.trim().to_string();
            }
            if name == "." || name == ".." {
                return None;
            }
            Some(AndroidFileEntry {
                is_dir: parts[0].starts_with('d'),
                is_link,
                size_bytes: parts[4].parse().unwrap_or(0),
                modified: format!("{} {}", parts[5], parts[6]),
                name,
            })
        })
        .collect()
}

/// `ls -l /sdcard` reports the symlink entry rather than its contents; a
/// trailing slash makes the device shell follow it.
fn listing_target(path: &str) -> String {
    format!("{}/", path.trim_end_matches('/'))
}

pub fn list_android_files(path: &str) -> Result<Vec<AndroidFileEntry>, String> {
    validate_android_path(path)?;
    let selected = selected_ready_device()?;
    let raw = adb_command_timeout(
        &selected.serial,
        &["shell", "ls", "-l", &listing_target(path)],
        8,
    )?;
    let mut entries = parse_ls_long(&raw);
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(entries)
}

pub fn push_file_to_android(local_path: &str, remote_dir: &str) -> Result<String, String> {
    let local = Path::new(local_path);
    let metadata = std::fs::symlink_metadata(local)
        .map_err(|error| format!("Local file cannot be read: {error}"))?;
    if !metadata.file_type().is_file() {
        return Err("Choose a regular file to upload.".to_string());
    }
    if metadata.len() > 500 * 1024 * 1024 {
        return Err("File exceeds the supported 500 MB transfer limit.".to_string());
    }
    validate_android_path(remote_dir)?;
    let name = local
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "Local filename is not valid UTF-8.".to_string())?;
    let selected = selected_ready_device()?;
    adb_command_timeout(&selected.serial, &["push", local_path, remote_dir], 300)?;
    let listing = adb_command_timeout(
        &selected.serial,
        &["shell", "ls", "-l", &listing_target(remote_dir)],
        10,
    )?;
    if !parse_ls_long(&listing)
        .iter()
        .any(|entry| entry.name == name && !entry.is_dir)
    {
        return Err(format!(
            "ADB reported success, but {name} is not listed in {remote_dir}."
        ));
    }
    Ok(format!("Uploaded {name} to {remote_dir} and verified it."))
}

pub fn choose_upload_file() -> Result<Option<String>, String> {
    native_picker(
        &[
            "--title=Select a file to upload",
            "--getopenfilename",
            "",
            "All files (*)",
        ],
        &["--file-selection", "--title=Select a file to upload"],
    )
}

pub fn pull_file_from_android(remote_path: &str) -> Result<Option<String>, String> {
    validate_android_path(remote_path)?;
    let name = remote_path.rsplit('/').next().unwrap_or("android-file");
    if name.is_empty() || name == "." || name == ".." {
        return Err("Selected Android path does not name a file.".to_string());
    }
    let Some(save_path) = native_picker(
        &[
            "--title=Save Android file",
            "--getsavefilename",
            "",
            "All files (*)",
        ],
        &[
            "--file-selection",
            "--save",
            "--confirm-overwrite",
            "--title=Save Android file",
            &format!("--filename={name}"),
        ],
    )?
    else {
        return Ok(None);
    };
    let selected = selected_ready_device()?;
    adb_command_timeout(&selected.serial, &["pull", remote_path, &save_path], 300)?;
    std::fs::metadata(&save_path)
        .map_err(|error| format!("ADB reported success, but {save_path} is unreadable: {error}"))?;
    Ok(Some(save_path))
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DisplayInfo {
    pub size: String,
    pub density: String,
}

pub fn display_info() -> Result<DisplayInfo, String> {
    let selected = selected_ready_device()?;
    Ok(DisplayInfo {
        size: adb_command_timeout(&selected.serial, &["shell", "wm", "size"], 6)?,
        density: adb_command_timeout(&selected.serial, &["shell", "wm", "density"], 6)?,
    })
}

pub fn apply_display(
    width: Option<u32>,
    height: Option<u32>,
    density: Option<u32>,
) -> Result<DisplayInfo, String> {
    if width.is_none() && height.is_none() && density.is_none() {
        return Err("Choose a size or density to change.".to_string());
    }
    let selected = selected_ready_device()?;
    if let (Some(width), Some(height)) = (width, height) {
        if !(320..=3_840).contains(&width) || !(240..=2_160).contains(&height) {
            return Err("Display size must be between 320x240 and 3840x2160.".to_string());
        }
        adb_command_timeout(
            &selected.serial,
            &["shell", "wm", "size", &format!("{width}x{height}")],
            8,
        )?;
    } else if width.is_some() || height.is_some() {
        return Err("Display width and height must be provided together.".to_string());
    }
    if let Some(density) = density {
        if !(120..=640).contains(&density) {
            return Err("Display density must be between 120 and 640 dpi.".to_string());
        }
        adb_command_timeout(
            &selected.serial,
            &["shell", "wm", "density", &density.to_string()],
            8,
        )?;
    }
    display_info()
}

pub fn reset_display() -> Result<DisplayInfo, String> {
    let selected = selected_ready_device()?;
    adb_command_timeout(&selected.serial, &["shell", "wm", "size", "reset"], 8)?;
    adb_command_timeout(&selected.serial, &["shell", "wm", "density", "reset"], 8)?;
    display_info()
}

pub fn install_apk(device_serial: &str, apk_path: &str, approved: bool) -> Result<String, String> {
    if !approved {
        return Err("APK installation requires explicit approval.".to_string());
    }
    let apk = inspect_apk(apk_path)?;
    validate_device_serial(device_serial)?;
    let selected = get_selected_device()?;
    if selected.serial != device_serial {
        return Err(
            "ADB operation target does not match the explicitly selected device.".to_string(),
        );
    }
    verify_device_connected(&selected)?;
    crate::waydroid::WaydroidPowerManager::new().ensure_android_ready(&selected)?;
    let output = adb_command(device_serial, &["install", "-r", &apk.path])?;
    if !output.contains("Success") {
        return Err(format!("APK installation failed: {output}"));
    }
    if !is_app_installed(device_serial, &apk.package_name)? {
        return Err(format!(
            "ADB reported installation success, but package {} is not installed.",
            apk.package_name
        ));
    }
    Ok(format!(
        "Installed and verified {package_name} on {}.",
        device_serial,
        package_name = apk.package_name
    ))
}

pub fn uninstall_app(
    device_serial: &str,
    package_name: &str,
    approved: bool,
) -> Result<String, String> {
    if !approved {
        return Err("Application removal requires explicit confirmation.".to_string());
    }
    validate_package_name(package_name)?;
    validate_device_serial(device_serial)?;
    let selected = get_selected_device()?;
    if selected.serial != device_serial {
        return Err(
            "ADB operation target does not match the explicitly selected device.".to_string(),
        );
    }
    verify_device_connected(&selected)?;
    crate::waydroid::WaydroidPowerManager::new().ensure_android_ready(&selected)?;
    let install_path = adb_command(device_serial, &["shell", "pm", "path", package_name])?
        .lines()
        .find_map(|line| line.strip_prefix("package:").map(str::to_string))
        .ok_or_else(|| format!("Application {package_name} is not installed."))?;
    let (_, is_user_app, is_system_app) = classify_package(&install_path);
    if is_system_app || !is_user_app {
        return Err(format!(
            "Refusing to uninstall {package_name}: its installation path is not confirmed as a user-app path."
        ));
    }
    let output = adb_command(device_serial, &["uninstall", package_name])?;
    if !output.contains("Success") {
        return Err(format!("Uninstall failed: {output}"));
    }
    if is_app_installed(device_serial, package_name)? {
        return Err(format!(
            "ADB reported uninstall success, but {package_name} is still installed."
        ));
    }
    Ok(format!(
        "{package_name} was uninstalled and verified absent from {}.",
        device_serial
    ))
}

/// Runs kdialog when available, otherwise zenity, and returns the chosen path.
/// Exit code 1 is the native "user cancelled" result.
fn native_picker(kdialog_args: &[&str], zenity_args: &[&str]) -> Result<Option<String>, String> {
    let (program, args) = if Command::new("kdialog").arg("--version").output().is_ok() {
        ("kdialog", kdialog_args)
    } else {
        ("zenity", zenity_args)
    };
    let output = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| format!("Unable to open the native file picker: {error}"))?;
    if output.status.success() {
        let path = String::from_utf8_lossy(&output.stdout)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        if path.is_empty() {
            return Err("The native file picker returned an empty path.".to_string());
        }
        Ok(Some(path))
    } else if output.status.code() == Some(1) {
        Ok(None)
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(if detail.is_empty() {
            format!("Native file picker failed with status {}.", output.status)
        } else {
            format!("Native file picker failed: {detail}")
        })
    }
}

pub fn choose_apk_file() -> Result<Option<String>, String> {
    native_picker(
        &[
            "--title=Select Android APK",
            "--getopenfilename",
            "",
            "Android APKs (*.apk)",
        ],
        &[
            "--file-selection",
            "--title=Select Android APK",
            "--file-filter=Android APKs | *.apk",
        ],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_validation_rejects_invalid_values() {
        let invalid = validate_package_name("com.example app");
        assert!(invalid.is_err());
        assert!(validate_package_name("com.example.app").is_ok());
        assert!(validate_package_name("Com.Example_2.app").is_ok());
        assert!(validate_package_name("com..example").is_err());
    }

    #[test]
    fn package_listing_parses_paths_and_deduplicates_without_fabricating_labels() {
        let apps = parse_installed_packages(
            "package:/system/priv-app/Settings/Settings.apk=com.android.settings\n\
             package:/data/app/~~abc/org.example.app-random/base.apk=org.example.app\n\
             package:/system/priv-app/Settings/Settings.apk=com.android.settings\n\
             package:not valid\n",
        );
        assert_eq!(apps.len(), 2);
        assert_eq!(apps[0].package, "com.android.settings");
        assert_eq!(apps[0].name, None);
        assert!(apps[0].system_app);
        assert_eq!(apps[1].category, "User");
        assert!(apps[1].user_app);
    }

    #[test]
    fn package_list_membership_is_exact() {
        assert!(package_list_contains(
            "package:org.example.app\n",
            "org.example.app"
        ));
        assert!(!package_list_contains(
            "package:org.example.application\n",
            "org.example.app"
        ));
    }

    #[test]
    fn foreground_component_is_parsed_without_changing_package_case() {
        assert_eq!(
            foreground_component("mCurrentFocus=Window{f00 u0 com.example.App/.MainActivity}\n"),
            Some("com.example.App/.MainActivity".to_string())
        );
        assert_eq!(foreground_component("mCurrentFocus=null\n"), None);
    }

    #[test]
    fn binary_manifest_package_identifier_is_read_from_string_pool() {
        fn push_u16(bytes: &mut Vec<u8>, value: u16) {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        fn push_u32(bytes: &mut Vec<u8>, value: u32) {
            bytes.extend_from_slice(&value.to_le_bytes());
        }

        let values = ["manifest", "package", "org.example.real"];
        let mut pool_data = Vec::new();
        let mut offsets = Vec::new();
        for value in values {
            offsets.push(pool_data.len() as u32);
            pool_data.push(value.len() as u8);
            pool_data.push(value.len() as u8);
            pool_data.extend_from_slice(value.as_bytes());
            pool_data.push(0);
        }
        let pool_size = 40 + pool_data.len();
        let mut pool = Vec::new();
        push_u16(&mut pool, 0x0001);
        push_u16(&mut pool, 28);
        push_u32(&mut pool, pool_size as u32);
        push_u32(&mut pool, values.len() as u32);
        push_u32(&mut pool, 0);
        push_u32(&mut pool, 0x100);
        push_u32(&mut pool, 40);
        push_u32(&mut pool, 0);
        for offset in offsets {
            push_u32(&mut pool, offset);
        }
        pool.extend_from_slice(&pool_data);

        let mut element = Vec::new();
        push_u16(&mut element, 0x0102);
        push_u16(&mut element, 16);
        push_u32(&mut element, 56);
        push_u32(&mut element, 1);
        push_u32(&mut element, u32::MAX);
        push_u32(&mut element, u32::MAX);
        push_u32(&mut element, 0);
        push_u16(&mut element, 20);
        push_u16(&mut element, 20);
        push_u16(&mut element, 1);
        push_u16(&mut element, 0);
        push_u16(&mut element, 0);
        push_u16(&mut element, 0);
        push_u32(&mut element, u32::MAX);
        push_u32(&mut element, 1);
        push_u32(&mut element, 2);
        push_u16(&mut element, 8);
        element.push(0);
        element.push(0x03);
        push_u32(&mut element, 2);

        let mut xml = Vec::new();
        let doc_size = 8 + pool.len() + element.len();
        push_u16(&mut xml, 0x0003);
        push_u16(&mut xml, 8);
        push_u32(&mut xml, doc_size as u32);
        xml.extend_from_slice(&pool);
        xml.extend_from_slice(&element);
        assert_eq!(
            parse_manifest_package(&xml),
            Some("org.example.real".to_string())
        );
    }

    #[test]
    fn apk_path_validation_requires_apk_extension() {
        let temp = std::env::temp_dir().join("waydroid_test.apk");
        std::fs::write(&temp, b"fake apk").unwrap();
        let result = validate_apk_path(temp.to_str().unwrap());
        assert!(result.is_ok());
        let _ = std::fs::remove_file(temp);
    }

    #[test]
    fn device_serial_validation_rejects_unsafe_values() {
        assert!(validate_device_serial("192.168.240.112:5555").is_ok());
        assert!(validate_device_serial("bad;serial").is_err());
    }

    #[test]
    fn install_and_uninstall_require_explicit_approval() {
        assert!(
            install_apk("", "", false)
                .unwrap_err()
                .contains("explicit approval")
        );
        assert!(
            uninstall_app("", "", false)
                .unwrap_err()
                .contains("explicit confirmation")
        );
    }

    #[test]
    fn adb_discovery_keeps_multiple_devices_and_selects_none_implicitly() {
        let devices = parse_adb_devices(
            "List of devices attached\nphone-1 device product:phone model:Pixel_8 device:husky\n192.168.240.112:5555 device product:lineage_waydroid_x86_64 model:WayDroid_x86_64_Device device:waydroid_x86_64\n",
        );
        assert_eq!(devices.len(), 2);
        assert!(!devices[0].is_waydroid);
        assert!(devices[1].is_waydroid);
        assert!(verify_device_in_list(&devices, "phone-1").is_ok());
        assert!(verify_device_in_list(&devices, "missing").is_err());
    }

    #[test]
    fn disconnected_or_offline_selected_device_does_not_fall_back() {
        let devices = parse_adb_devices(
            "List of devices attached\nphone-1 offline product:phone\nphone-2 device product:phone\n",
        );
        assert!(verify_device_in_list(&devices, "phone-1").is_err());
    }

    #[test]
    fn base64_encoding_matches_the_rfc_4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn android_directory_listing_parses_toybox_output() {
        let entries = parse_ls_long(
            "total 12\n\
             drwxrws--- 1 u0_a121 media_rw 0 2026-11-16 08:43 Download\n\
             -rw-r--r-- 1 root root 4096 2026-11-16 08:44 note.txt\n\
             lrwxrwxrwx 1 root root 21 2026-11-16 08:44 shortcut -> /sdcard/Download\n\
             -rw-r--r-- 1 root root 12 2026-11-16 08:44 two words.txt\n",
        );
        assert_eq!(entries.len(), 4);
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].name, "Download");
        assert_eq!(entries[1].size_bytes, 4096);
        assert!(entries[2].is_link);
        assert_eq!(entries[2].name, "shortcut");
        assert_eq!(entries[3].name, "two words.txt");
    }

    #[test]
    fn android_paths_are_restricted_to_shell_safe_absolute_values() {
        assert!(validate_android_path("/sdcard/Download").is_ok());
        assert!(validate_android_path("sdcard/Download").is_err());
        assert!(validate_android_path("/sdcard/../data").is_err());
        assert!(validate_android_path("/sdcard/Download;reboot").is_err());
        assert!(validate_android_path("/sdcard/My Files").is_err());
    }

    #[test]
    fn listing_target_follows_symlinked_directories() {
        assert_eq!(listing_target("/sdcard"), "/sdcard/");
        assert_eq!(listing_target("/sdcard/"), "/sdcard/");
        assert_eq!(listing_target("/"), "/");
    }

    #[test]
    fn android_key_whitelist_rejects_arbitrary_key_codes() {
        assert!(ANDROID_KEYS.iter().any(|(name, _)| *name == "home"));
        assert!(!ANDROID_KEYS.iter().any(|(name, _)| *name == "reboot"));
    }
}
