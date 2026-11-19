use crate::android::{
    DeviceSelection, adb_command_timeout, adb_output, begin_selected_device_operation,
    detect_adb_devices, verify_device_connected,
};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Cursor;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock, RwLock};
use std::thread;
use std::time::{Duration, Instant};

const WAYDROID_CONFIG: &str = "/var/lib/waydroid/waydroid.cfg";
const CONTAINER_PATH: &str = "/var/lib/waydroid/lxc";
const CONTAINER_BUS: [&str; 5] = [
    "--system",
    "call",
    "id.waydro.Container",
    "/ContainerManager",
    "id.waydro.ContainerManager",
];
const ANDROID_READY_TIMEOUT: Duration = Duration::from_secs(20);
/// A fresh session reports `Session: RUNNING` before its network is up, so the
/// ADB target only becomes available some seconds later.
const ADB_TARGET_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeviceHealth {
    Healthy,
    Starting,
    Stopped,
    Frozen,
    Unresponsive,
    AdbDisconnected,
    PermissionDenied,
    Unknown,
}

impl DeviceHealth {
    pub fn as_str(&self) -> &'static str {
        match self {
            DeviceHealth::Healthy => "Healthy",
            DeviceHealth::Starting => "Starting",
            DeviceHealth::Stopped => "Stopped",
            DeviceHealth::Frozen => "Frozen",
            DeviceHealth::Unresponsive => "Unresponsive",
            DeviceHealth::AdbDisconnected => "ADB disconnected",
            DeviceHealth::PermissionDenied => "Permission denied",
            DeviceHealth::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WaydroidDiagnostics {
    pub installed: bool,
    pub service_running: bool,
    pub version: String,
    pub session_running: bool,
    pub container_status: String,
    pub suspend_action: String,
    pub suspension_active: bool,
    pub adb_connected: bool,
    pub adb_shell_healthy: bool,
    pub android_package_manager_healthy: bool,
    pub screenshot_healthy: bool,
    pub health: String,
    pub device_name: String,
    pub selected_device_state: String,
    pub android_version: String,
    pub resolution: String,
    pub ip_address: String,
    pub session_user: String,
    pub wayland_display: String,
    pub vendor_type: String,
    pub probe_errors: Vec<String>,
}

static POWER_ACTIVE: AtomicBool = AtomicBool::new(false);
static POWER_CANCELLED: AtomicBool = AtomicBool::new(false);
static POWER_PROGRESS: OnceLock<RwLock<String>> = OnceLock::new();

pub fn run_output(program: &str, args: &[&str], timeout_seconds: u64) -> Result<Output, String> {
    Command::new("timeout")
        .args(["--kill-after=2s", &timeout_seconds.to_string(), program])
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|err| format!("failed to execute `{program}`: {err}"))
}

pub fn command_ok(program: &str, args: &[&str]) -> bool {
    run_output(program, args, 5).is_ok_and(|output| output.status.success())
}

pub fn run_command(program: &str, args: &[&str], timeout_seconds: u64) -> Result<String, String> {
    let output = run_output(program, args, timeout_seconds)?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    let timeout_note = if matches!(output.status.code(), Some(124 | 137)) {
        format!("timed out after {timeout_seconds}s")
    } else {
        "command failed".to_string()
    };
    if detail.is_empty() {
        Err(timeout_note)
    } else {
        Err(format!("{detail} ({timeout_note})"))
    }
}

fn read_suspend_action() -> Result<String, String> {
    let config = fs::read_to_string(WAYDROID_CONFIG)
        .map_err(|err| format!("could not read Waydroid config: {err}"))?;
    config
        .lines()
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "suspend_action").then(|| value.trim().to_string())
        })
        .ok_or_else(|| "suspend_action is not present in the Waydroid config.".to_string())
}

pub fn waydroid_session_running() -> Result<bool, String> {
    let status = run_command("waydroid", &["status"], 8)?;
    Ok(status.lines().any(|line| {
        line.strip_prefix("Session:")
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("RUNNING"))
    }))
}

static SESSION_PROCESS: OnceLock<Mutex<Option<Child>>> = OnceLock::new();

pub fn start_session() -> Result<String, String> {
    if waydroid_session_running()? {
        return Ok("Waydroid session is already running.".to_string());
    }

    let process_lock = SESSION_PROCESS.get_or_init(|| Mutex::new(None));
    let mut process = process_lock
        .lock()
        .map_err(|_| "Waydroid session process state is unavailable.".to_string())?;
    if let Some(child) = process.as_mut() {
        match child.try_wait() {
            Ok(None) => return Ok("Waydroid session startup is still in progress.".to_string()),
            Ok(Some(_)) => *process = None,
            Err(error) => {
                return Err(format!(
                    "could not inspect Waydroid session process: {error}"
                ));
            }
        }
    }
    *process = Some(
        Command::new("waydroid")
            .args(["session", "start"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|err| format!("failed to start Waydroid session: {err}"))?,
    );

    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        if waydroid_session_running()? {
            return Ok("Waydroid session started.".to_string());
        }
        if let Some(child) = process.as_mut() {
            match child.try_wait() {
                Ok(Some(_)) => {
                    *process = None;
                    return Err(
                        "Waydroid session process exited before the session became active."
                            .to_string(),
                    );
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(format!(
                        "could not inspect Waydroid session process: {error}"
                    ));
                }
            }
        }
        thread::sleep(Duration::from_millis(500));
    }
    Ok("Waydroid startup is still in progress; the session process was left running.".to_string())
}

pub fn stop_session() -> Result<String, String> {
    run_command("waydroid", &["session", "stop"], 20)?;
    if waydroid_session_running()? {
        return Err(
            "Waydroid session stop request returned, but the session is still running.".to_string(),
        );
    }

    let process_lock = SESSION_PROCESS.get_or_init(|| Mutex::new(None));
    let mut process = process_lock
        .lock()
        .map_err(|_| "Waydroid session process state is unavailable.".to_string())?;
    if let Some(child) = process.as_mut() {
        for _ in 0..20 {
            match child.try_wait() {
                Ok(Some(_)) => {
                    *process = None;
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(250)),
                Err(error) => {
                    return Err(format!(
                        "could not inspect Waydroid session process: {error}"
                    ));
                }
            }
        }
        if process.is_some() {
            return Err(
                "Waydroid session stopped, but its CLI process did not exit within five seconds."
                    .to_string(),
            );
        }
    }
    Ok("Waydroid session stopped.".to_string())
}

/// Explicit user action: connect ADB to the running Waydroid session. The
/// device still has to be selected separately, so the target never changes
/// without an explicit choice.
pub fn connect_waydroid_adb() -> Result<String, String> {
    if !waydroid_session_running()? {
        return Err(
            "Waydroid session is stopped; start the session before connecting ADB.".to_string(),
        );
    }
    if container_state()? == "FROZEN" {
        // ADB stays offline while the container is frozen; resuming needs the
        // explicit power action, so report the real cause instead of a timeout.
        return Err(
            "Waydroid container is frozen; resume it in the Device panel, then connect ADB."
                .to_string(),
        );
    }
    let target = wait_for_adb_target()?;
    if let Ok(devices) = detect_adb_devices()
        && devices
            .iter()
            .any(|device| device.serial == target && device.state == "device")
    {
        return Ok(format!("ADB is already connected to {target}."));
    }
    run_command("adb", &["connect", &target], 10)?;
    // `adb connect` returns before the transport is registered, so the device
    // appears in `adb devices` a moment later.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_state;
    loop {
        match detect_adb_devices()?
            .into_iter()
            .find(|device| device.serial == target)
        {
            Some(device) if device.state == "device" => {
                return Ok(format!(
                    "ADB connected to {} ({target}).",
                    if device.model.is_empty() {
                        "the Waydroid device"
                    } else {
                        device.model.as_str()
                    }
                ));
            }
            Some(device) => last_state = Some(device.state),
            None => last_state = None,
        }
        if Instant::now() >= deadline {
            let state = last_state.as_deref().unwrap_or("not listed");
            return Err(format!(
                "ADB reached {target}, but its state is {state} after 10s."
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

fn container_state() -> Result<String, String> {
    let state = normalize_container_state(run_command(
        "lxc-info",
        &["-P", CONTAINER_PATH, "-n", "waydroid", "-sH"],
        5,
    ))?;
    if state != "PERMISSION UNAVAILABLE" {
        return Ok(state);
    }
    // `lxc-info` needs privileges to read the container; `waydroid status`
    // reports the same RUNNING/FROZEN state for the session user, so a frozen
    // container can still be detected and unfrozen.
    let status = run_command("waydroid", &["status"], 8)?;
    Ok(container_state_from_status(&status).unwrap_or(state))
}

fn container_state_from_status(status: &str) -> Option<String> {
    status
        .lines()
        .find_map(|line| line.strip_prefix("Container:"))
        .map(|value| value.trim().to_ascii_uppercase())
}

fn normalize_container_state(result: Result<String, String>) -> Result<String, String> {
    match result {
        Ok(state) => Ok(state.to_ascii_uppercase()),
        Err(error)
            if error.to_ascii_lowercase().contains("doesn't exist")
                || error.to_ascii_lowercase().contains("does not exist") =>
        {
            Ok("STOPPED".to_string())
        }
        Err(error)
            if error.to_ascii_lowercase().contains("privileges")
                || error.to_ascii_lowercase().contains("permission denied") =>
        {
            Ok("PERMISSION UNAVAILABLE".to_string())
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn decode_screenshot_png(bytes: &[u8]) -> Result<(u32, u32), String> {
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("Screenshot is not a valid PNG: {error}"))?;
    let info = reader.info();
    let pixels = u64::from(info.width) * u64::from(info.height);
    if info.width == 0 || info.height == 0 || pixels > 64_000_000 {
        return Err(format!(
            "Screenshot dimensions are unsupported: {}x{}.",
            info.width, info.height
        ));
    }
    let mut decoded = vec![
        0;
        reader.output_buffer_size().ok_or_else(|| {
            "Screenshot dimensions overflow the decoder buffer.".to_string()
        })?
    ];
    let frame = reader
        .next_frame(&mut decoded)
        .map_err(|error| format!("Screenshot PNG could not be fully decoded: {error}"))?;
    Ok((frame.width, frame.height))
}

fn resume_required(
    session_is_running: bool,
    state: &str,
    suspend_action: &str,
) -> Result<bool, String> {
    if !session_is_running {
        return Err(
            "Waydroid session is stopped; it will not be started or restarted automatically."
                .to_string(),
        );
    }
    match state.to_ascii_uppercase().as_str() {
        "FROZEN" if suspend_action == "freeze" => Ok(true),
        "FROZEN" => Err(format!(
            "Waydroid is frozen while suspend_action is '{suspend_action}'; refusing an automatic resume."
        )),
        "RUNNING" => Ok(false),
        "STOPPED" => {
            Err("Waydroid container is stopped; no resume operation was attempted.".to_string())
        }
        "PERMISSION UNAVAILABLE" => Ok(false),
        other => Err(format!("Waydroid container state is unknown: {other}.")),
    }
}

fn ensure_not_cancelled() -> Result<(), String> {
    if POWER_CANCELLED.load(Ordering::Acquire) {
        Err("Android operation cancelled before the next step.".to_string())
    } else {
        Ok(())
    }
}

fn set_power_progress(message: impl Into<String>) -> Result<(), String> {
    let progress = POWER_PROGRESS.get_or_init(|| RwLock::new("Idle".to_string()));
    *progress
        .write()
        .map_err(|_| "Power-operation progress state is unavailable.".to_string())? =
        message.into();
    Ok(())
}

pub fn power_operation_progress() -> Result<String, String> {
    let progress = POWER_PROGRESS.get_or_init(|| RwLock::new("Idle".to_string()));
    progress
        .read()
        .map(|message| message.clone())
        .map_err(|_| "Power-operation progress state is unavailable.".to_string())
}

pub fn cancel_power_operation() -> Result<String, String> {
    if !POWER_ACTIVE.load(Ordering::Acquire) {
        return Err("No Android power operation is currently active.".to_string());
    }
    POWER_CANCELLED.store(true, Ordering::Release);
    set_power_progress("Cancellation requested; waiting for the current bounded command.")?;
    Ok("Cancellation requested.".to_string())
}

fn begin_power_operation() -> Result<(), String> {
    POWER_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| "Another Android power operation is already active.".to_string())?;
    POWER_CANCELLED.store(false, Ordering::Release);
    Ok(())
}

fn end_power_operation(result: &Result<String, String>) {
    let message = match result {
        Ok(message) => message.clone(),
        Err(error) => format!("Failed: {error}"),
    };
    let _ = set_power_progress(message);
    POWER_ACTIVE.store(false, Ordering::Release);
}

pub struct WaydroidPowerManager;

impl Default for WaydroidPowerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl WaydroidPowerManager {
    pub fn new() -> Self {
        Self
    }

    pub fn ensure_android_ready(&self, device: &DeviceSelection) -> Result<String, String> {
        begin_power_operation()?;
        let result = self.ensure_android_ready_inner(device);
        end_power_operation(&result);
        result
    }

    fn ensure_android_ready_inner(&self, device: &DeviceSelection) -> Result<String, String> {
        ensure_not_cancelled()?;
        verify_device_connected(device)?;
        if device.is_waydroid {
            let running = waydroid_session_running()?;
            let state = container_state()?;
            let suspend_action = read_suspend_action()?;
            if resume_required(running, &state, &suspend_action)? {
                set_power_progress("Resuming frozen Waydroid container through ContainerManager…")?;
                ensure_not_cancelled()?;
                let mut args = CONTAINER_BUS.to_vec();
                args.push("Unfreeze");
                run_command("busctl", &args, 8)?;
                set_power_progress("Waiting for Waydroid container and Android to respond…")?;
            } else {
                set_power_progress("Waydroid is running; checking Android responsiveness…")?;
            }
        } else {
            set_power_progress(format!(
                "Checking selected Android device {}…",
                device.serial
            ))?;
        }

        let deadline = Instant::now() + ANDROID_READY_TIMEOUT;
        let mut last_error = "Android shell has not responded yet.".to_string();
        while Instant::now() < deadline {
            ensure_not_cancelled()?;
            verify_device_connected(device)?;
            if device.is_waydroid {
                let state = container_state()?;
                container_state_result(&state)?;
            }
            match adb_command_timeout(&device.serial, &["shell", "echo", "wcc-ready"], 4) {
                Ok(output) if output.trim() == "wcc-ready" => {
                    set_power_progress("Android shell responded; checking package manager…")?;
                    ensure_not_cancelled()?;
                    if let Err(error) =
                        adb_command_timeout(&device.serial, &["shell", "pm", "path", "android"], 6)
                    {
                        last_error =
                            format!("Android shell responds, but package manager failed: {error}");
                        break;
                    }
                    return Ok(format!(
                        "Android shell and package manager are responsive on selected device {}.",
                        device.serial
                    ));
                }
                Ok(_) => last_error = "Android shell returned an unexpected response.".to_string(),
                Err(error) => last_error = format!("Android shell is not responding: {error}"),
            }
            thread::sleep(Duration::from_millis(500));
        }
        Err(last_error)
    }

    pub fn wake_selected_device(&self) -> Result<String, String> {
        let (_operation, device) = begin_selected_device_operation()?;
        if !device.is_waydroid {
            return Err(format!(
                "Selected device '{}' is not identified as Waydroid.",
                device.serial
            ));
        }
        self.ensure_android_ready(&device)
    }

    pub fn suspend_selected_device(&self) -> Result<String, String> {
        // Take the device-operation lock before the power lock: the other
        // commands acquire them in this order, so the reverse order could deadlock.
        let (_operation, device) = begin_selected_device_operation()?;
        begin_power_operation()?;
        let result = (|| {
            if !device.is_waydroid {
                return Err(
                    "Suspend is available only for an explicitly selected Waydroid device."
                        .to_string(),
                );
            }
            ensure_not_cancelled()?;
            let running = waydroid_session_running()?;
            let state = container_state()?;
            if !running {
                return Err("Waydroid session is stopped; nothing was suspended.".to_string());
            }
            if state == "FROZEN" {
                return Ok("Waydroid is already suspended.".to_string());
            }
            if state != "RUNNING" {
                return Err(format!(
                    "Cannot suspend Waydroid while its container is {state}."
                ));
            }

            verify_device_connected(&device)?;
            set_power_progress("Suspending Waydroid through ContainerManager…")?;
            let mut args = CONTAINER_BUS.to_vec();
            args.push("Freeze");
            run_command("busctl", &args, 8)?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                ensure_not_cancelled()?;
                if container_state()? == "FROZEN" {
                    return Ok("Waydroid container is suspended.".to_string());
                }
                thread::sleep(Duration::from_millis(200));
            }
            Err(
                "ContainerManager returned, but Waydroid did not enter the frozen state."
                    .to_string(),
            )
        })();
        end_power_operation(&result);
        result
    }
}

pub fn detect_waydroid(selected: Option<&DeviceSelection>) -> WaydroidDiagnostics {
    let installed = command_ok("waydroid", &["--version"]);
    let mut diagnostics = WaydroidDiagnostics {
        installed,
        service_running: false,
        version: String::new(),
        session_running: false,
        container_status: "Unknown".to_string(),
        suspend_action: "Unknown".to_string(),
        suspension_active: false,
        adb_connected: false,
        adb_shell_healthy: false,
        android_package_manager_healthy: false,
        screenshot_healthy: false,
        health: DeviceHealth::Unknown.as_str().to_string(),
        device_name: selected
            .map(|device| device.model.clone())
            .unwrap_or_else(|| "No device selected".to_string()),
        selected_device_state: if selected.is_some() {
            "Disconnected".to_string()
        } else {
            "No selection".to_string()
        },
        android_version: "Unavailable".to_string(),
        resolution: "Unavailable".to_string(),
        ip_address: "Unavailable".to_string(),
        session_user: "Unavailable".to_string(),
        wayland_display: "Unavailable".to_string(),
        vendor_type: "Unavailable".to_string(),
        probe_errors: Vec::new(),
    };

    if diagnostics.installed {
        diagnostics.service_running = command_ok(
            "systemctl",
            &["is-active", "--quiet", "waydroid-container.service"],
        );
        if let Ok(version) = run_command("waydroid", &["--version"], 8) {
            diagnostics.version = version;
        }
        match read_suspend_action() {
            Ok(action) => diagnostics.suspend_action = action,
            Err(error) => diagnostics.probe_errors.push(error),
        }
        match run_command("waydroid", &["status"], 8) {
            Ok(status) => {
                for line in status.lines() {
                    if let Some(value) = line.strip_prefix("Session:") {
                        diagnostics.session_running = value.trim().eq_ignore_ascii_case("RUNNING");
                    } else if let Some(value) = line.strip_prefix("IP address:") {
                        diagnostics.ip_address = value.trim().to_string();
                    } else if let Some(value) = line.strip_prefix("Session user:") {
                        diagnostics.session_user = value
                            .split('(')
                            .next()
                            .unwrap_or(value.trim())
                            .trim()
                            .to_string();
                    } else if let Some(value) = line.strip_prefix("Wayland display:") {
                        diagnostics.wayland_display = value.trim().to_string();
                    } else if let Some(value) = line.strip_prefix("Vendor type:") {
                        diagnostics.vendor_type = value.trim().to_string();
                    }
                }
            }
            Err(error) => diagnostics
                .probe_errors
                .push(format!("Waydroid status probe failed: {error}")),
        }
        match container_state() {
            Ok(state) => diagnostics.container_status = state,
            Err(error) => diagnostics
                .probe_errors
                .push(format!("Container-state probe failed: {error}")),
        }
        diagnostics.suspension_active =
            diagnostics.suspend_action == "freeze" && diagnostics.container_status == "FROZEN";
    }

    if let Some(device) = selected {
        match crate::android::detect_adb_devices() {
            Ok(devices) => {
                if let Some(found) = devices
                    .iter()
                    .find(|candidate| candidate.serial == device.serial)
                {
                    diagnostics.selected_device_state = found.state.clone();
                    diagnostics.adb_connected = found.state == "device";
                } else {
                    diagnostics.probe_errors.push(format!(
                        "Selected ADB device '{}' is disconnected.",
                        device.serial
                    ));
                }
            }
            Err(error) => diagnostics
                .probe_errors
                .push(format!("ADB transport probe failed: {error}")),
        }
        if diagnostics.adb_connected {
            match adb_command_timeout(&device.serial, &["shell", "echo", "wcc-health"], 4) {
                Ok(response) if response.trim() == "wcc-health" => {
                    diagnostics.adb_shell_healthy = true;
                }
                Ok(_) => diagnostics
                    .probe_errors
                    .push("Android shell returned an unexpected response.".to_string()),
                Err(error) => diagnostics
                    .probe_errors
                    .push(format!("Android shell probe failed: {error}")),
            }
            match adb_command_timeout(&device.serial, &["shell", "pm", "path", "android"], 6) {
                Ok(response) if response.starts_with("package:") => {
                    diagnostics.android_package_manager_healthy = true;
                }
                Ok(_) => diagnostics.probe_errors.push(
                    "Android package-manager probe returned no framework package path.".to_string(),
                ),
                Err(error) => diagnostics
                    .probe_errors
                    .push(format!("Android package-manager probe failed: {error}")),
            }
            match adb_command_timeout(
                &device.serial,
                &["shell", "getprop", "ro.build.version.release"],
                4,
            ) {
                Ok(version) if !version.is_empty() => diagnostics.android_version = version,
                Ok(_) => {}
                Err(error) => diagnostics
                    .probe_errors
                    .push(format!("Android property probe failed: {error}")),
            }
            match adb_output(&device.serial, &["exec-out", "screencap", "-p"], 8) {
                Ok(output) => {
                    diagnostics.screenshot_healthy = if output.status.success() {
                        match decode_screenshot_png(&output.stdout) {
                            Ok((width, height)) => {
                                diagnostics.resolution = format!("{width}x{height}");
                                true
                            }
                            Err(error) => {
                                diagnostics.probe_errors.push(error);
                                false
                            }
                        }
                    } else {
                        diagnostics
                            .probe_errors
                            .push("Screenshot command failed on the selected device.".to_string());
                        false
                    }
                }
                Err(error) => diagnostics
                    .probe_errors
                    .push(format!("Screenshot probe failed: {error}")),
            }
        }
    }

    diagnostics.health = if !diagnostics.installed || !diagnostics.session_running {
        DeviceHealth::Stopped.as_str().to_string()
    } else if diagnostics.container_status == "FROZEN" {
        DeviceHealth::Frozen.as_str().to_string()
    } else if !selected.is_some_and(|device| device.is_waydroid) {
        DeviceHealth::Unknown.as_str().to_string()
    } else if !diagnostics.adb_connected {
        DeviceHealth::AdbDisconnected.as_str().to_string()
    } else if diagnostics.adb_shell_healthy && diagnostics.android_package_manager_healthy {
        DeviceHealth::Healthy.as_str().to_string()
    } else {
        DeviceHealth::Unresponsive.as_str().to_string()
    };

    diagnostics
}

fn container_state_result(state: &str) -> Result<(), String> {
    if state == "RUNNING" || state == "PERMISSION UNAVAILABLE" {
        Ok(())
    } else {
        Err(format!(
            "Waydroid container did not resume; current state: {state}."
        ))
    }
}

/// ADB endpoint reported by `waydroid status`, e.g. `192.168.240.112:5555`.
pub fn adb_target_from_status(status: &str) -> Result<String, String> {
    let ip = status
        .lines()
        .find_map(|line| line.strip_prefix("IP address:"))
        .map(str::trim)
        .filter(|value| value.parse::<std::net::Ipv4Addr>().is_ok())
        .ok_or_else(|| "Waydroid did not report a usable IP address.".to_string())?;
    Ok(format!("{ip}:5555"))
}

fn wait_for_adb_target() -> Result<String, String> {
    let deadline = Instant::now() + ADB_TARGET_TIMEOUT;
    let mut last_error;
    loop {
        match run_command("waydroid", &["status"], 8) {
            Ok(status) => match adb_target_from_status(&status) {
                Ok(target) => return Ok(target),
                Err(error) => last_error = Some(error),
            },
            Err(error) => last_error = Some(format!("Waydroid status probe failed: {error}")),
        }
        if Instant::now() >= deadline {
            let error = last_error.unwrap_or_default();
            return Err(format!(
                "{error} Waited {}s for the session network.",
                ADB_TARGET_TIMEOUT.as_secs()
            ));
        }
        thread::sleep(Duration::from_millis(500));
    }
}

pub fn require_restart_approval(approved: bool) -> Result<(), String> {
    if approved {
        Ok(())
    } else {
        Err(
            "Restart requires explicit approval because it interrupts the active Android session."
                .to_string(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_states_are_mapped() {
        assert_eq!(DeviceHealth::Healthy.as_str(), "Healthy");
        assert_eq!(DeviceHealth::Frozen.as_str(), "Frozen");
        assert_eq!(DeviceHealth::AdbDisconnected.as_str(), "ADB disconnected");
    }

    #[test]
    fn frozen_container_requires_resume_but_running_does_not() {
        assert_eq!(resume_required(true, "FROZEN", "freeze"), Ok(true));
        assert_eq!(resume_required(true, "RUNNING", "freeze"), Ok(false));
        assert!(resume_required(true, "FROZEN", "none").is_err());
    }

    #[test]
    fn stopped_session_or_container_is_not_started_implicitly() {
        assert!(resume_required(false, "FROZEN", "freeze").is_err());
        assert!(resume_required(true, "STOPPED", "freeze").is_err());
    }

    #[test]
    fn failed_resume_is_reported_when_container_remains_frozen() {
        assert!(container_state_result("FROZEN").is_err());
        assert!(container_state_result("RUNNING").is_ok());
    }

    #[test]
    fn missing_container_is_reported_as_stopped() {
        assert_eq!(
            normalize_container_state(Err("waydroid doesn't exist".to_string())),
            Ok("STOPPED".to_string())
        );
        assert_eq!(
            normalize_container_state(
                Err("Insufficent privileges to control waydroid".to_string())
            ),
            Ok("PERMISSION UNAVAILABLE".to_string())
        );
        assert!(normalize_container_state(Err("lxc-info timed out".to_string())).is_err());
    }

    #[test]
    fn cancellation_is_checked_before_the_next_operation() {
        POWER_CANCELLED.store(true, Ordering::Release);
        assert!(ensure_not_cancelled().is_err());
        POWER_CANCELLED.store(false, Ordering::Release);
    }

    #[test]
    fn external_command_timeout_is_reported() {
        let result = run_command("sleep", &["5"], 1);
        assert!(result.unwrap_err().contains("timed out"));
    }

    #[test]
    fn restart_requires_explicit_approval() {
        assert!(require_restart_approval(false).is_err());
        assert!(require_restart_approval(true).is_ok());
    }

    #[test]
    fn container_state_falls_back_to_waydroid_status_output() {
        assert_eq!(
            container_state_from_status("Session:\tRUNNING\nContainer:\tFROZEN\n"),
            Some("FROZEN".to_string())
        );
        assert_eq!(
            container_state_from_status("Session:\tRUNNING\nContainer:\tRUNNING\n"),
            Some("RUNNING".to_string())
        );
        assert_eq!(container_state_from_status("Session:\tSTOPPED\n"), None);
    }

    #[test]
    fn adb_target_is_taken_only_from_a_real_reported_ipv4_address() {
        assert_eq!(
            adb_target_from_status("Session:\tRUNNING\nIP address:\t192.168.240.112\n"),
            Ok("192.168.240.112:5555".to_string())
        );
        assert!(adb_target_from_status("Session:\tRUNNING\nIP address:\tUNKNOWN\n").is_err());
        assert!(adb_target_from_status("Session:\tSTOPPED\n").is_err());
    }

    #[test]
    fn screenshot_validation_decodes_the_image_and_rejects_truncated_png() {
        let mut bytes = Vec::new();
        {
            let mut encoder = png::Encoder::new(&mut bytes, 2, 1);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().unwrap();
            writer.write_image_data(&[255, 0, 0, 0, 255, 0]).unwrap();
        }
        assert_eq!(decode_screenshot_png(&bytes), Ok((2, 1)));
        assert!(decode_screenshot_png(&bytes[..bytes.len() / 2]).is_err());
    }
}
