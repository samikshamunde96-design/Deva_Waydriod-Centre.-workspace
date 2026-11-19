pub mod activity;
pub mod android;
pub mod assistant;
pub mod waydroid;
pub mod workflow;

use serde::{Deserialize, Serialize};

use crate::activity::LogEntry;
use crate::android::{
    AndroidFileEntry, ApkFileInfo, AppEntry, AppInfo, DeviceSelection, DisplayInfo, adb_command,
    apply_display, begin_selected_device_operation, choose_apk_file, choose_upload_file,
    display_info, get_app_info, get_foreground_app, get_selected_device, inspect_apk, install_apk,
    is_app_installed, launch_app, list_adb_devices, list_android_files,
    list_installed_apps_selected, press_key, pull_file_from_android, push_file_to_android,
    reset_display, screenshot_base64, set_selected_device, swipe, tap, uninstall_app,
};
use crate::assistant::ChatMessage;
use crate::waydroid::{
    WaydroidPowerManager, detect_waydroid, power_operation_progress, require_restart_approval,
    run_command, start_session, stop_session,
};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DashboardStatus {
    pub installed: bool,
    pub service_running: bool,
    pub version: String,
    pub session_running: bool,
    pub container_status: String,
    pub suspend_action: String,
    pub suspension_active: bool,
    pub health: String,
    pub adb_connected: bool,
    pub adb_shell_healthy: bool,
    pub android_package_manager_healthy: bool,
    pub screenshot_healthy: bool,
    pub selected_device: Option<DeviceSelection>,
    pub selected_device_state: String,
    pub device_name: String,
    pub android_version: String,
    pub resolution: String,
    pub cpu_usage: String,
    pub memory_usage: String,
    pub storage_usage: String,
    pub app_count: Option<usize>,
    pub ip_address: String,
    pub session_user: String,
    pub wayland_display: String,
    pub vendor_type: String,
    pub last_operation: String,
    pub automation_status: String,
    pub power_progress: String,
    pub probe_errors: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DeviceDetails {
    pub health: String,
    pub android_version: String,
    pub device_name: String,
    pub dimensions: String,
    pub density: String,
    pub details: String,
}

fn read_system_value(program: &str, args: &[&str]) -> String {
    run_command(program, args, 5).unwrap_or_else(|_| String::from("Unavailable"))
}

/// 1-minute load average: a real system number, unlike summing per-process
/// `%cpu` from `ps`, which produced meaningless values above 100%.
fn cpu_load_summary() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|raw| {
            raw.split_whitespace()
                .next()
                .map(|load| format!("{load} (1m load)"))
        })
        .unwrap_or_else(|| String::from("Unavailable"))
}

#[tauri::command]
async fn get_dashboard_status() -> Result<DashboardStatus, String> {
    let selected = get_selected_device().ok();
    let diagnostics = detect_waydroid(selected.as_ref());
    Ok(DashboardStatus {
        installed: diagnostics.installed,
        service_running: diagnostics.service_running,
        version: diagnostics.version,
        session_running: diagnostics.session_running,
        container_status: diagnostics.container_status,
        suspend_action: diagnostics.suspend_action,
        suspension_active: diagnostics.suspension_active,
        health: diagnostics.health.clone(),
        adb_connected: diagnostics.adb_connected,
        adb_shell_healthy: diagnostics.adb_shell_healthy,
        android_package_manager_healthy: diagnostics.android_package_manager_healthy,
        screenshot_healthy: diagnostics.screenshot_healthy,
        selected_device: selected,
        selected_device_state: diagnostics.selected_device_state,
        device_name: diagnostics.device_name,
        android_version: diagnostics.android_version,
        resolution: diagnostics.resolution,
        cpu_usage: cpu_load_summary(),
        memory_usage: match read_system_value("free", &["-m"])
            .lines()
            .find(|line| line.starts_with("Mem:"))
        {
            Some(line) => {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 {
                    format!("{}MiB / {}MiB", parts[2], parts[1])
                } else {
                    String::from("Unavailable")
                }
            }
            None => String::from("Unavailable"),
        },
        storage_usage: match read_system_value("df", &["-h", "/"]).lines().nth(1) {
            Some(line) => {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5 {
                    format!("{} / {} ({})", parts[2], parts[1], parts[4])
                } else {
                    String::from("Unavailable")
                }
            }
            None => String::from("Unavailable"),
        },
        app_count: None,
        ip_address: diagnostics.ip_address,
        session_user: diagnostics.session_user,
        wayland_display: diagnostics.wayland_display,
        vendor_type: diagnostics.vendor_type,
        last_operation: activity::last_summary()
            .unwrap_or_else(|| format!("Health: {}", diagnostics.health)),
        automation_status: String::from("Idle"),
        power_progress: power_operation_progress()?,
        probe_errors: diagnostics.probe_errors,
    })
}

#[tauri::command]
async fn get_device_details() -> Result<DeviceDetails, String> {
    let (_operation, selected_device) = begin_selected_device_operation()?;
    WaydroidPowerManager::new().ensure_android_ready(&selected_device)?;
    let diagnostics = detect_waydroid(Some(&selected_device));
    let dimensions = adb_command(&selected_device.serial, &["shell", "wm", "size"])?;
    let density = adb_command(&selected_device.serial, &["shell", "wm", "density"])?;
    let details = adb_command(&selected_device.serial, &["shell", "getprop"])?;

    Ok(DeviceDetails {
        health: diagnostics.health,
        android_version: diagnostics.android_version,
        device_name: diagnostics.device_name,
        dimensions,
        density,
        details,
    })
}

#[tauri::command]
async fn list_connected_devices() -> Result<Vec<DeviceSelection>, String> {
    list_adb_devices()
}

#[tauri::command]
async fn select_android_device(serial: String) -> Result<DeviceSelection, String> {
    let result = set_selected_device(&serial);
    activity::record_outcome("Select ADB device", &result);
    result
}

/// Explicit user action: connect ADB to the running Waydroid session.
#[tauri::command]
async fn connect_waydroid_adb() -> Result<String, String> {
    let result = crate::waydroid::connect_waydroid_adb();
    activity::record_message("Connect ADB", &result);
    result
}

#[tauri::command]
async fn get_selected_android_device() -> Result<DeviceSelection, String> {
    get_selected_device()
}

#[tauri::command]
async fn list_android_apps() -> Result<Vec<AppEntry>, String> {
    list_installed_apps_selected()
}

#[tauri::command]
async fn get_android_app_info(package_name: String) -> Result<AppInfo, String> {
    if package_name.trim().is_empty() {
        return Err("Package name is required.".to_string());
    }
    let selected = get_selected_device()?;
    get_app_info(&selected.serial, &package_name)
}

#[tauri::command]
async fn launch_android_app(package_name: String) -> Result<String, String> {
    if package_name.trim().is_empty() {
        return Err("Package name is required.".to_string());
    }
    let selected = get_selected_device()?;
    let result = launch_app(&selected.serial, &package_name);
    activity::record_message(&format!("Launch {package_name}"), &result);
    result
}

#[tauri::command]
async fn is_android_app_installed(package_name: String) -> Result<bool, String> {
    if package_name.trim().is_empty() {
        return Err("Package name is required.".to_string());
    }
    let selected = get_selected_device()?;
    is_app_installed(&selected.serial, &package_name)
}

#[tauri::command]
async fn get_android_foreground_app() -> Result<String, String> {
    let selected = get_selected_device()?;
    get_foreground_app(&selected.serial)
}

#[tauri::command]
async fn choose_android_apk() -> Result<Option<String>, String> {
    choose_apk_file()
}

#[tauri::command]
async fn inspect_android_apk(apk_path: String) -> Result<ApkFileInfo, String> {
    inspect_apk(&apk_path)
}

#[tauri::command]
async fn install_android_apk(apk_path: String, approved: bool) -> Result<String, String> {
    let selected = get_selected_device()?;
    let result = install_apk(&selected.serial, &apk_path, approved);
    activity::record_message("Install APK", &result);
    result
}

#[tauri::command]
async fn uninstall_android_app(package_name: String, approved: bool) -> Result<String, String> {
    let selected = get_selected_device()?;
    let result = uninstall_app(&selected.serial, &package_name, approved);
    activity::record_message(&format!("Uninstall {package_name}"), &result);
    result
}

#[tauri::command]
async fn start_waydroid() -> Result<String, String> {
    let result = start_session();
    activity::record_message("Start Waydroid session", &result);
    result
}

#[tauri::command]
async fn stop_waydroid() -> Result<String, String> {
    let result = stop_session();
    activity::record_message("Stop Waydroid session", &result);
    result
}

#[tauri::command]
async fn restart_waydroid(approved: bool) -> Result<String, String> {
    require_restart_approval(approved)?;
    let result = (|| {
        stop_session()?;
        start_session()
    })();
    activity::record_message("Restart Waydroid session", &result);
    result
}

#[tauri::command]
async fn open_android_ui() -> Result<String, String> {
    let (_operation, device) = begin_selected_device_operation()?;
    if !device.is_waydroid {
        return Err("Open Android UI requires an explicitly selected Waydroid device.".to_string());
    }
    WaydroidPowerManager::new().ensure_android_ready(&device)?;
    let result = run_command("waydroid", &["show-full-ui"], 8)
        .map(|_| String::from("Android UI foreground request completed."));
    activity::record_message("Open Android UI", &result);
    result
}

#[tauri::command]
async fn wake_android() -> Result<String, String> {
    let result = WaydroidPowerManager::new().wake_selected_device();
    activity::record_message("Wake Android", &result);
    result
}

#[tauri::command]
async fn suspend_android() -> Result<String, String> {
    let result = WaydroidPowerManager::new().suspend_selected_device();
    activity::record_message("Suspend Android", &result);
    result
}

#[tauri::command]
async fn cancel_android_operation() -> Result<String, String> {
    crate::waydroid::cancel_power_operation()
}

#[tauri::command]
async fn get_power_progress() -> Result<String, String> {
    power_operation_progress()
}

#[tauri::command]
async fn get_android_screenshot() -> Result<String, String> {
    screenshot_base64()
}

#[tauri::command]
async fn tap_android(x: u32, y: u32) -> Result<String, String> {
    tap(x, y)
}

#[tauri::command]
async fn swipe_android(
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    duration_ms: u32,
) -> Result<String, String> {
    swipe(x1, y1, x2, y2, duration_ms)
}

#[tauri::command]
async fn press_android_key(key: String) -> Result<String, String> {
    press_key(&key)
}

#[tauri::command]
async fn get_android_display() -> Result<DisplayInfo, String> {
    display_info()
}

#[tauri::command]
async fn apply_android_display(
    width: Option<u32>,
    height: Option<u32>,
    density: Option<u32>,
) -> Result<DisplayInfo, String> {
    let result = apply_display(width, height, density);
    activity::record_outcome("Change Android display", &result);
    result
}

#[tauri::command]
async fn reset_android_display() -> Result<DisplayInfo, String> {
    let result = reset_display();
    activity::record_outcome("Reset Android display", &result);
    result
}

#[tauri::command]
async fn read_android_directory(path: String) -> Result<Vec<AndroidFileEntry>, String> {
    list_android_files(&path)
}

#[tauri::command]
async fn choose_android_upload() -> Result<Option<String>, String> {
    choose_upload_file()
}

#[tauri::command]
async fn upload_android_file(local_path: String, remote_dir: String) -> Result<String, String> {
    let result = push_file_to_android(&local_path, &remote_dir);
    activity::record_message("Upload file to Android", &result);
    result
}

#[tauri::command]
async fn download_android_file(remote_path: String) -> Result<Option<String>, String> {
    let result = pull_file_from_android(&remote_path);
    activity::record_outcome("Download file from Android", &result);
    result
}

#[tauri::command]
async fn run_android_workflow(
    id: String,
    package: Option<String>,
) -> Result<workflow::WorkflowReport, String> {
    let result = workflow::run(&id, package.as_deref());
    match &result {
        Ok(report) => activity::record(
            if report.success { "info" } else { "error" },
            &format!("Workflow: {}", report.title),
            report
                .steps
                .iter()
                .map(|step| format!("{}: {}", step.name, step.detail))
                .collect::<Vec<_>>()
                .join(" | "),
        ),
        Err(error) => activity::record("error", "Workflow", error.clone()),
    }
    result
}

#[tauri::command]
async fn get_activity_log() -> Result<Vec<LogEntry>, String> {
    Ok(activity::entries())
}

#[tauri::command]
async fn list_ollama_models() -> Result<Vec<String>, String> {
    assistant::list_models()
}

#[tauri::command]
async fn ask_ollama(model: String, messages: Vec<ChatMessage>) -> Result<String, String> {
    let result = assistant::chat(&model, &messages);
    match &result {
        Ok(_) => activity::record("info", "Local assistant query", format!("model {model}")),
        Err(error) => activity::record("error", "Local assistant query", error.clone()),
    }
    result
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_dashboard_status,
            get_device_details,
            list_connected_devices,
            select_android_device,
            connect_waydroid_adb,
            get_selected_android_device,
            wake_android,
            suspend_android,
            cancel_android_operation,
            get_power_progress,
            list_android_apps,
            get_android_app_info,
            is_android_app_installed,
            get_android_foreground_app,
            launch_android_app,
            choose_android_apk,
            inspect_android_apk,
            install_android_apk,
            uninstall_android_app,
            start_waydroid,
            stop_waydroid,
            restart_waydroid,
            open_android_ui,
            get_android_screenshot,
            tap_android,
            swipe_android,
            press_android_key,
            get_android_display,
            apply_android_display,
            reset_android_display,
            read_android_directory,
            choose_android_upload,
            upload_android_file,
            download_android_file,
            run_android_workflow,
            get_activity_log,
            list_ollama_models,
            ask_ollama
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
