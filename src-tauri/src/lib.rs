pub mod activity;
pub mod agent;
pub mod android;
pub mod http;
pub mod providers;
pub mod settings;
pub mod waydroid;
pub mod workflow;

use serde::{Deserialize, Serialize};

use crate::activity::LogEntry;
use crate::android::{
    AndroidFileEntry, ApkFileInfo, AppEntry, AppInfo, DeviceSelection, DisplayInfo, apply_display,
    begin_selected_device_operation, choose_apk_file, choose_upload_file, display_info,
    get_app_info, get_foreground_app, get_selected_device, inspect_apk, install_apk, launch_app,
    list_adb_devices, list_android_files, list_installed_apps_selected, press_key,
    pull_file_from_android, push_file_to_android, reset_display, screenshot_base64,
    set_selected_device, swipe, tap, uninstall_app,
};
use crate::providers::{ChatMessage, ChatReply};
use crate::waydroid::{
    WaydroidPowerManager, detect_waydroid, power_operation_progress, require_restart_approval,
    require_stop_approval, run_command, start_session, stop_session,
};

/// Runs blocking provider/ADB work off the Tauri runtime so long operations do
/// not stall unrelated commands.
async fn blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tauri::async_runtime::spawn_blocking(work)
        .await
        .map_err(|error| format!("The background task failed: {error}"))?
}

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
    pub ip_address: String,
    pub session_user: String,
    pub wayland_display: String,
    pub vendor_type: String,
    pub last_operation: String,
    pub power_progress: String,
    pub probe_errors: Vec<String>,
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
        ip_address: diagnostics.ip_address,
        session_user: diagnostics.session_user,
        wayland_display: diagnostics.wayland_display,
        vendor_type: diagnostics.vendor_type,
        last_operation: activity::last_summary()
            .unwrap_or_else(|| format!("Health: {}", diagnostics.health)),
        power_progress: power_operation_progress()?,
        probe_errors: diagnostics.probe_errors,
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

/// A start that is only "still booting" or was denied is reported as an error,
/// so the UI never shows a successful start for a session that is not up.
#[tauri::command]
async fn start_waydroid() -> Result<String, String> {
    let result = blocking(|| {
        start_session().map(|outcome| {
            if outcome.session_active() {
                Ok(outcome.message)
            } else {
                Err(outcome.message)
            }
        })?
    })
    .await;
    activity::record_outcome("Start Waydroid session", &result);
    result
}

#[tauri::command]
async fn stop_waydroid(approved: bool) -> Result<String, String> {
    let result = blocking(move || {
        require_stop_approval(approved)?;
        stop_session()
    })
    .await;
    activity::record_message("Stop Waydroid session", &result);
    result
}

#[tauri::command]
async fn restart_waydroid(approved: bool) -> Result<String, String> {
    require_restart_approval(approved)?;
    let result = blocking(|| {
        stop_session()?;
        start_session().and_then(|outcome| {
            if outcome.session_active() {
                Ok(outcome.message)
            } else {
                Err(outcome.message)
            }
        })
    })
    .await;
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

// ---------------------------------------------------------------------------
// Settings
// ---------------------------------------------------------------------------

#[tauri::command]
async fn load_settings() -> Result<settings::SettingsLoad, String> {
    Ok(settings::load())
}

#[tauri::command]
async fn save_settings(next: settings::Settings) -> Result<settings::Settings, String> {
    let saved = settings::save(&next)?;
    activity::record("info", "Save settings", "preferences updated");
    Ok(saved)
}

// ---------------------------------------------------------------------------
// AI providers
// ---------------------------------------------------------------------------

#[tauri::command]
async fn provider_health(
    provider: String,
    base_url: String,
) -> Result<providers::ProviderHealth, String> {
    blocking(move || Ok(providers::health(&provider, &base_url))).await
}

#[tauri::command]
async fn list_provider_models(
    provider: String,
    base_url: String,
) -> Result<Vec<providers::ModelMetadata>, String> {
    blocking(move || providers::list_models(&provider, &base_url)).await
}

#[tauri::command]
async fn probe_provider_model(
    provider: String,
    base_url: String,
    model: String,
) -> Result<providers::ModelCapabilities, String> {
    blocking(move || providers::probe_model(&provider, &base_url, &model)).await
}

/// The key is stored in process memory only; it is never persisted or returned.
#[tauri::command]
async fn set_provider_credential(provider: String, secret: String) -> Result<bool, String> {
    providers::set_credential(&provider, &secret)?;
    activity::record(
        "info",
        "AI credential",
        format!("configured for {provider} (session only)"),
    );
    Ok(true)
}

#[tauri::command]
async fn clear_provider_credential(provider: String) -> Result<bool, String> {
    providers::clear_credential(&provider)?;
    activity::record("info", "AI credential", format!("cleared for {provider}"));
    Ok(false)
}

#[tauri::command]
async fn ask_ai_chat(
    provider: String,
    base_url: String,
    model: String,
    messages: Vec<ChatMessage>,
) -> Result<ChatReply, String> {
    let reply = blocking(move || providers::chat(&provider, &base_url, &model, &messages)).await;
    match &reply {
        Ok(reply) => activity::record(
            "info",
            "AI chat",
            format!("{} via {}", reply.model, reply.provider),
        ),
        Err(error) => activity::record("error", "AI chat", error.clone()),
    }
    reply
}

// ---------------------------------------------------------------------------
// AI Action engine
// ---------------------------------------------------------------------------

#[tauri::command]
async fn action_start(
    prompt: String,
    provider: String,
    base_url: String,
    model: String,
    max_steps: Option<u32>,
    safety: Option<String>,
) -> Result<agent::ActionTaskView, String> {
    let view = blocking(move || {
        agent::start(
            &prompt,
            &provider,
            &base_url,
            &model,
            max_steps,
            safety.as_deref(),
        )
    })
    .await;
    match &view {
        Ok(view) => activity::record("info", "AI action started", view.prompt.clone()),
        Err(error) => activity::record("error", "AI action", error.clone()),
    }
    view
}

#[tauri::command]
async fn action_approve(
    run_id: String,
    approval_id: String,
    approved: bool,
) -> Result<agent::ActionTaskView, String> {
    blocking(move || agent::approve(&run_id, &approval_id, approved)).await
}

#[tauri::command]
async fn action_cancel(run_id: String) -> Result<agent::ActionTaskView, String> {
    blocking(move || agent::cancel(&run_id)).await
}

#[tauri::command]
async fn action_get(run_id: String) -> Result<agent::ActionTaskView, String> {
    blocking(move || agent::get(&run_id)).await
}

#[tauri::command]
async fn action_list() -> Result<Vec<agent::ActionTaskView>, String> {
    blocking(agent::list).await
}

#[tauri::command]
async fn action_tools() -> Result<Vec<serde_json::Value>, String> {
    Ok(agent::describe_tools())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_dashboard_status,
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
            load_settings,
            save_settings,
            provider_health,
            list_provider_models,
            probe_provider_model,
            set_provider_credential,
            clear_provider_credential,
            ask_ai_chat,
            action_start,
            action_approve,
            action_cancel,
            action_get,
            action_list,
            action_tools
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
