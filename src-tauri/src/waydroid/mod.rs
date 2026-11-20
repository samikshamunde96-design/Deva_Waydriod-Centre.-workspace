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
/// A cold container often refuses the first connection and can sit at `offline`
/// for tens of seconds after the session reports RUNNING, so the transport gets
/// a window separate from the IP lookup.
const ADB_TRANSPORT_TIMEOUT: Duration = Duration::from_secs(30);
/// `adb connect` is idempotent, so it is re-issued while the transport is not
/// yet `device` instead of failing the whole bring-up on one refused attempt.
const ADB_RECONNECT_INTERVAL: Duration = Duration::from_secs(5);
/// A 4K screenshot is well below this; the cap only rejects runaway frames.
const MAX_SCREENSHOT_BYTES: usize = 32 * 1024 * 1024;
/// 64 megapixels is above every supported Android display size.
const MAX_SCREENSHOT_PIXELS: u64 = 64_000_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DeviceHealth {
    Healthy,
    Starting,
    Stopped,
    Frozen,
    Unresponsive,
    AdbDisconnected,
    PermissionDenied,
    NotInstalled,
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
            DeviceHealth::NotInstalled => "Waydroid not installed",
            DeviceHealth::Unknown => "Unknown",
        }
    }
}

/// Health classification as a pure decision, so every state is reachable and
/// testable without a live device.
pub struct HealthInputs<'a> {
    pub installed: bool,
    pub session_running: bool,
    pub container_status: &'a str,
    pub selected_is_waydroid: bool,
    pub adb_connected: bool,
    pub adb_shell_healthy: bool,
    pub package_manager_healthy: bool,
}

pub fn classify_health(inputs: &HealthInputs<'_>) -> DeviceHealth {
    if !inputs.installed {
        return DeviceHealth::NotInstalled;
    }
    if inputs.container_status == "PERMISSION UNAVAILABLE" {
        return DeviceHealth::PermissionDenied;
    }
    if !inputs.session_running {
        return DeviceHealth::Stopped;
    }
    if inputs.container_status == "FROZEN" {
        return DeviceHealth::Frozen;
    }
    if inputs.container_status != "RUNNING" {
        // The session reports RUNNING before the container finishes booting, or
        // the container state could not be read reliably.
        return DeviceHealth::Starting;
    }
    if !inputs.selected_is_waydroid {
        return DeviceHealth::Unknown;
    }
    if !inputs.adb_connected {
        return DeviceHealth::AdbDisconnected;
    }
    if inputs.adb_shell_healthy && inputs.package_manager_healthy {
        DeviceHealth::Healthy
    } else {
        DeviceHealth::Unresponsive
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

fn session_process() -> &'static Mutex<Option<Child>> {
    SESSION_PROCESS.get_or_init(|| Mutex::new(None))
}

/// What a start request actually achieved. Only `SessionStart::Started` and
/// `AlreadyRunning` claim that the session is up; a start that is still booting
/// or blocked by permissions is never reported as success.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStart {
    AlreadyRunning,
    Started,
    StillStarting,
    PermissionDenied,
    Failed(String),
}

#[derive(Clone, Debug)]
pub struct StartOutcome {
    pub state: SessionStart,
    pub message: String,
    /// `None` means no device is selected, so readiness could not be checked.
    pub android_ready: Option<bool>,
}

impl StartOutcome {
    pub fn session_active(&self) -> bool {
        matches!(
            self.state,
            SessionStart::AlreadyRunning | SessionStart::Started
        )
    }
}

fn classify_start_failure(container_state: &str) -> SessionStart {
    if container_state == "PERMISSION UNAVAILABLE" {
        SessionStart::PermissionDenied
    } else {
        SessionStart::Failed(
            "Waydroid session process exited before the session became active.".to_string(),
        )
    }
}

/// Read-only readiness note for the selected device, used to keep the startup
/// report honest. It never resumes a frozen container by itself.
fn readiness_note() -> (Option<bool>, String) {
    let Ok(device) = crate::android::get_selected_device() else {
        return (
            None,
            " No device is selected, so Android readiness was not verified.".to_string(),
        );
    };
    match WaydroidPowerManager::new().ensure_android_ready_readonly(&device) {
        Ok(_) => (
            Some(true),
            " Android readiness is verified on the selected device.".to_string(),
        ),
        Err(error) => (
            Some(false),
            format!(" Android readiness is not verified yet: {error}"),
        ),
    }
}

pub fn start_session() -> Result<StartOutcome, String> {
    if waydroid_session_running()? {
        let (ready, note) = readiness_note();
        return Ok(StartOutcome {
            state: SessionStart::AlreadyRunning,
            message: format!("Waydroid session is already running.{note}"),
            android_ready: ready,
        });
    }

    spawn_session_process()?;
    let deadline = Instant::now() + Duration::from_secs(25);
    while Instant::now() < deadline {
        if waydroid_session_running()? {
            let (ready, note) = readiness_note();
            return Ok(StartOutcome {
                state: SessionStart::Started,
                message: format!("Waydroid session started.{note}"),
                android_ready: ready,
            });
        }
        if let Some(state) = reap_session_process()? {
            let container = container_state().unwrap_or_else(|_| "Unknown".to_string());
            let state = match state {
                SessionStart::Started => classify_start_failure(&container),
                other => other,
            };
            return Ok(StartOutcome {
                state: state.clone(),
                message: match &state {
                    SessionStart::PermissionDenied => {
                        "Waydroid session could not start: the container manager denied the request. Check that waydroid-container.service is running and that your user is allowed to use Waydroid.".to_string()
                    }
                    SessionStart::Failed(error) => error.clone(),
                    _ => "Waydroid session start finished.".to_string(),
                },
                android_ready: Some(false),
            });
        }
        thread::sleep(Duration::from_millis(500));
    }
    Ok(StartOutcome {
        state: SessionStart::StillStarting,
        message: "Waydroid is still starting after 25s; the session process was left running. Retry in a few seconds.".to_string(),
        android_ready: Some(false),
    })
}

/// Spawns `waydroid session start` unless an earlier child is still running.
fn spawn_session_process() -> Result<(), String> {
    let mut process = session_process()
        .lock()
        .map_err(|_| "Waydroid session process state is unavailable.".to_string())?;
    if let Some(child) = process.as_mut() {
        match child.try_wait() {
            Ok(None) => {
                return Err(
                    "A Waydroid session start is already in progress; wait for it to finish."
                        .to_string(),
                );
            }
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
    Ok(())
}

/// Returns the terminal state of the session child, if it has exited.
fn reap_session_process() -> Result<Option<SessionStart>, String> {
    let mut process = session_process()
        .lock()
        .map_err(|_| "Waydroid session process state is unavailable.".to_string())?;
    let Some(child) = process.as_mut() else {
        return Ok(None);
    };
    match child.try_wait() {
        Ok(Some(status)) => {
            *process = None;
            Ok(Some(if status.success() {
                SessionStart::Started
            } else {
                SessionStart::Failed(
                    "Waydroid session process exited before the session became active.".to_string(),
                )
            }))
        }
        Ok(None) => Ok(None),
        Err(error) => Err(format!(
            "could not inspect Waydroid session process: {error}"
        )),
    }
}

pub fn require_stop_approval(approved: bool) -> Result<(), String> {
    if approved {
        Ok(())
    } else {
        Err("Stopping Waydroid requires explicit confirmation because it closes the Android session."
            .to_string())
    }
}

pub fn stop_session() -> Result<String, String> {
    run_command("waydroid", &["session", "stop"], 20)?;
    if waydroid_session_running()? {
        return Err(
            "Waydroid session stop request returned, but the session is still running.".to_string(),
        );
    }

    let mut process = session_process()
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
    if find_adb_device(&target)?.is_some_and(|device| device.state == "device") {
        return Ok(format!("ADB is already connected to {target}."));
    }
    let mut wait = TransportWait::new(
        Instant::now(),
        ADB_TRANSPORT_TIMEOUT,
        ADB_RECONNECT_INTERVAL,
    );
    // A refused connect is common while the container is still booting, so the
    // error is kept for diagnostics instead of aborting the attempt.
    wait.record_connect(run_command("adb", &["connect", &target], 10).err());
    loop {
        match find_adb_device(&target)? {
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
            Some(device) => wait.observe(Some(&device.state)),
            None => wait.observe(None),
        }
        let now = Instant::now();
        if wait.timed_out(now) {
            return Err(wait.timeout_message(&target));
        }
        if wait.should_reconnect(now) {
            wait.record_connect(run_command("adb", &["connect", &target], 5).err());
        }
        thread::sleep(Duration::from_millis(500));
    }
}

/// One `adb devices -l` lookup; `None` means the serial is not listed yet.
fn find_adb_device(target: &str) -> Result<Option<DeviceSelection>, String> {
    Ok(detect_adb_devices()?
        .into_iter()
        .find(|device| device.serial == target))
}

/// Tracks the transport wait so the retry policy is testable without a device:
/// distinct observed states in first-seen order, the reconnect cadence, and the
/// last `adb connect` error.
struct TransportWait {
    deadline: Instant,
    next_reconnect: Instant,
    reconnect_interval: Duration,
    states: Vec<String>,
    last_connect_error: Option<String>,
}

impl TransportWait {
    fn new(now: Instant, timeout: Duration, reconnect_interval: Duration) -> Self {
        Self {
            deadline: now + timeout,
            next_reconnect: now + reconnect_interval,
            reconnect_interval,
            states: Vec::new(),
            last_connect_error: None,
        }
    }

    fn observe(&mut self, state: Option<&str>) {
        let label = state.unwrap_or("not listed");
        if !self.states.iter().any(|seen| seen == label) {
            self.states.push(label.to_string());
        }
    }

    /// A later successful `adb connect` clears the error. A failed one replaces
    /// it, so the timeout message always describes the most recent attempt.
    fn record_connect(&mut self, result: Option<String>) {
        self.last_connect_error = result;
        self.next_reconnect = Instant::now() + self.reconnect_interval;
    }

    fn should_reconnect(&self, now: Instant) -> bool {
        now >= self.next_reconnect
    }

    fn timed_out(&self, now: Instant) -> bool {
        now >= self.deadline
    }

    fn timeout_message(&self, target: &str) -> String {
        let connect_error = self
            .last_connect_error
            .as_deref()
            .map(|error| format!(" Last `adb connect` error: {error}."))
            .unwrap_or_default();
        let observed = if self.states.is_empty() {
            "no transport state observed".to_string()
        } else {
            self.states.join(" -> ")
        };
        format!(
            "ADB target {target} did not reach the `device` state within {}s (observed: {observed}).{connect_error} Retry Connect ADB once Waydroid has finished booting.",
            ADB_TRANSPORT_TIMEOUT.as_secs(),
        )
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
    decode_screenshot_png_limited(bytes, MAX_SCREENSHOT_BYTES)
}

/// Screenshots are decoded fully to prove they are real PNGs; a byte cap keeps
/// a malformed or oversized frame from allocating unbounded memory.
pub(crate) fn decode_screenshot_png_limited(
    bytes: &[u8],
    max_bytes: usize,
) -> Result<(u32, u32), String> {
    if bytes.len() > max_bytes {
        return Err(format!(
            "Screenshot is larger than the supported {} MiB limit.",
            max_bytes / (1024 * 1024)
        ));
    }
    let decoder = png::Decoder::new(Cursor::new(bytes));
    let mut reader = decoder
        .read_info()
        .map_err(|error| format!("Screenshot is not a valid PNG: {error}"))?;
    let info = reader.info();
    let pixels = u64::from(info.width) * u64::from(info.height);
    if info.width == 0 || info.height == 0 || pixels > MAX_SCREENSHOT_PIXELS {
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

/// Whether an ordinary (read-only) readiness check must take the exclusive
/// power lock because the container has to be resumed first.
fn readiness_requires_power(
    session_is_running: bool,
    state: &str,
    suspend_action: &str,
) -> Result<bool, String> {
    resume_required(session_is_running, state, suspend_action)
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

/// Read-only operations must not observe a cancellation flag left over from an
/// earlier power operation.
fn ensure_not_cancelled_if_active() -> Result<(), String> {
    if POWER_ACTIVE.load(Ordering::Acquire) {
        ensure_not_cancelled()
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
    POWER_CANCELLED.store(false, Ordering::Release);
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
        let result = (|| {
            let resumed = self.resume_if_needed(device)?;
            self.probe_android_ready(device, resumed)
        })();
        end_power_operation(&result);
        result
    }

    /// Read-only readiness check used by ordinary Android operations. It takes
    /// the exclusive power lock only when a frozen container really needs a
    /// resume, so two read-only commands never fail with "another power
    /// operation is active".
    pub fn ensure_android_ready_readonly(
        &self,
        device: &DeviceSelection,
    ) -> Result<String, String> {
        verify_device_connected(device)?;
        if self.needs_resume(device)? {
            return self.ensure_android_ready(device);
        }
        self.probe_android_ready(device, false)
    }

    fn needs_resume(&self, device: &DeviceSelection) -> Result<bool, String> {
        if !device.is_waydroid {
            return Ok(false);
        }
        let running = waydroid_session_running()?;
        let state = container_state()?;
        let suspend_action = read_suspend_action()?;
        readiness_requires_power(running, &state, &suspend_action)
    }

    fn resume_if_needed(&self, device: &DeviceSelection) -> Result<bool, String> {
        if !self.needs_resume(device)? {
            set_power_progress("Waydroid is running; checking Android responsiveness…")?;
            return Ok(false);
        }
        set_power_progress("Resuming frozen Waydroid container through ContainerManager…")?;
        ensure_not_cancelled()?;
        let mut args = CONTAINER_BUS.to_vec();
        args.push("Unfreeze");
        run_command("busctl", &args, 8)?;
        set_power_progress("Waiting for Waydroid container and Android to respond…")?;
        Ok(true)
    }

    fn probe_android_ready(
        &self,
        device: &DeviceSelection,
        resumed: bool,
    ) -> Result<String, String> {
        let deadline = Instant::now() + ANDROID_READY_TIMEOUT;
        let mut last_error = "Android shell has not responded yet.".to_string();
        while Instant::now() < deadline {
            ensure_not_cancelled_if_active()?;
            verify_device_connected(device)?;
            if device.is_waydroid {
                let state = container_state()?;
                container_state_result(&state)?;
            }
            match adb_command_timeout(&device.serial, &["shell", "echo", "wcc-ready"], 4) {
                Ok(output) if output.trim() == "wcc-ready" => {
                    set_power_progress("Android shell responded; checking package manager…")?;
                    ensure_not_cancelled_if_active()?;
                    if let Err(error) =
                        adb_command_timeout(&device.serial, &["shell", "pm", "path", "android"], 6)
                    {
                        last_error =
                            format!("Android shell responds, but package manager failed: {error}");
                        break;
                    }
                    return Ok(format!(
                        "Android shell and package manager are responsive on selected device {}{}.",
                        device.serial,
                        if resumed { " (after resume)" } else { "" }
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
    // One invocation decides both installation and version: probing
    // `waydroid --version` twice doubled the dashboard cost.
    let version_output = run_command("waydroid", &["--version"], 8);
    let installed = version_output.is_ok();
    let mut diagnostics = WaydroidDiagnostics {
        installed,
        service_running: false,
        version: version_output.unwrap_or_default(),

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

    diagnostics.health = classify_health(&HealthInputs {
        installed: diagnostics.installed,
        session_running: diagnostics.session_running,
        container_status: &diagnostics.container_status,
        selected_is_waydroid: selected.is_some_and(|device| device.is_waydroid),
        adb_connected: diagnostics.adb_connected,
        adb_shell_healthy: diagnostics.adb_shell_healthy,
        package_manager_healthy: diagnostics.android_package_manager_healthy,
    })
    .as_str()
    .to_string();

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

    /// Tests below mutate process-wide power flags, so they must not interleave.
    fn test_state_lock() -> &'static Mutex<()> {
        static LOCK: Mutex<()> = Mutex::new(());
        &LOCK
    }

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
        let _serial = test_state_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
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
    fn transport_state_history_keeps_distinct_states_in_first_seen_order() {
        let mut wait = TransportWait::new(
            Instant::now(),
            Duration::from_secs(30),
            Duration::from_secs(5),
        );
        wait.observe(None);
        wait.observe(Some("offline"));
        wait.observe(Some("offline"));
        wait.observe(Some("device"));
        wait.observe(None);
        assert_eq!(wait.states, vec!["not listed", "offline", "device"]);
    }

    #[test]
    fn transport_reconnects_on_the_interval_and_stops_at_the_deadline() {
        let start = Instant::now();
        let wait = TransportWait::new(start, Duration::from_secs(30), Duration::from_secs(5));
        assert!(!wait.should_reconnect(start + Duration::from_millis(4999)));
        assert!(wait.should_reconnect(start + Duration::from_secs(5)));
        assert!(!wait.timed_out(start + Duration::from_millis(29_999)));
        assert!(wait.timed_out(start + Duration::from_secs(30)));
    }

    #[test]
    fn transport_timeout_message_reports_the_path_and_the_last_connect_error() {
        let start = Instant::now();
        let mut wait = TransportWait::new(start, Duration::from_secs(30), Duration::from_secs(5));
        wait.observe(None);
        wait.observe(Some("offline"));
        wait.record_connect(Some("connection refused".to_string()));
        let message = wait.timeout_message("192.168.240.2:5555");
        assert!(message.contains("not listed -> offline"));
        assert!(message.contains("connection refused"));
        assert!(message.contains("30s"));

        wait.record_connect(None);
        assert!(
            !wait
                .timeout_message("192.168.240.2:5555")
                .contains("adb connect")
        );
    }

    #[test]
    fn transport_state_becoming_device_is_an_immediate_success_path() {
        let mut wait = TransportWait::new(
            Instant::now(),
            Duration::from_secs(30),
            Duration::from_secs(5),
        );
        wait.observe(Some("offline"));
        wait.observe(Some("device"));
        assert_eq!(wait.states.last().map(String::as_str), Some("device"));
    }

    #[test]
    fn every_health_state_is_reachable() {
        let base = HealthInputs {
            installed: true,
            session_running: true,
            container_status: "RUNNING",
            selected_is_waydroid: true,
            adb_connected: true,
            adb_shell_healthy: true,
            package_manager_healthy: true,
        };
        assert_eq!(
            classify_health(&HealthInputs {
                installed: false,
                ..base
            }),
            DeviceHealth::NotInstalled
        );

        let mut inputs = HealthInputs { ..base };
        inputs.container_status = "PERMISSION UNAVAILABLE";
        assert_eq!(classify_health(&inputs), DeviceHealth::PermissionDenied);

        let mut inputs = HealthInputs { ..base };
        inputs.session_running = false;
        assert_eq!(classify_health(&inputs), DeviceHealth::Stopped);

        let mut inputs = HealthInputs { ..base };
        inputs.container_status = "FROZEN";
        assert_eq!(classify_health(&inputs), DeviceHealth::Frozen);

        let mut inputs = HealthInputs { ..base };
        inputs.container_status = "UNKNOWN";
        assert_eq!(classify_health(&inputs), DeviceHealth::Starting);

        let mut inputs = HealthInputs { ..base };
        inputs.selected_is_waydroid = false;
        assert_eq!(classify_health(&inputs), DeviceHealth::Unknown);

        let mut inputs = HealthInputs { ..base };
        inputs.adb_connected = false;
        assert_eq!(classify_health(&inputs), DeviceHealth::AdbDisconnected);

        let mut inputs = HealthInputs { ..base };
        inputs.adb_shell_healthy = false;
        assert_eq!(classify_health(&inputs), DeviceHealth::Unresponsive);

        assert_eq!(classify_health(&base), DeviceHealth::Healthy);
    }

    #[test]
    fn start_failures_are_classified_as_permission_denied_only_when_the_container_reports_it() {
        assert_eq!(
            classify_start_failure("PERMISSION UNAVAILABLE"),
            SessionStart::PermissionDenied
        );
        assert!(matches!(
            classify_start_failure("STOPPED"),
            SessionStart::Failed(_)
        ));
    }

    #[test]
    fn read_only_readiness_never_waits_for_the_power_lock() {
        let _serial = test_state_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        begin_power_operation().expect("power operation");
        // A running, unfrozen session means the read-only path has nothing to
        // mutate, so it must not attempt the exclusive lock.
        assert_eq!(
            readiness_requires_power(true, "RUNNING", "freeze"),
            Ok(false)
        );
        assert!(begin_power_operation().is_err());
        end_power_operation(&Ok("finished".to_string()));
        assert!(!POWER_ACTIVE.load(Ordering::Acquire));
        assert!(!POWER_CANCELLED.load(Ordering::Acquire));
    }

    #[test]
    fn cancellation_flags_do_not_leak_into_later_read_only_operations() {
        let _serial = test_state_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        begin_power_operation().unwrap();
        POWER_CANCELLED.store(true, Ordering::Release);
        end_power_operation(&Err("cancelled".to_string()));
        assert!(ensure_not_cancelled_if_active().is_ok());
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
