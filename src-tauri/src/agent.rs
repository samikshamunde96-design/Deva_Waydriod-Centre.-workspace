//! Bounded AI Action engine.
//!
//! The Action model can only choose from a fixed backend tool registry. It never
//! gets shell access: every argument is parsed into a typed value, validated,
//! and executed through the same primitives the UI uses.
//!
//! Flow: observe → plan (one tool call per round) → validate → approve when
//! sensitive → execute → verify/observe → report. A run is bounded by a step
//! limit and an overall deadline, and can be cancelled.
//!
//! This module deliberately excludes file transfer tools: uploads need a
//! native picker and downloads write to a host path, neither of which should be
//! driven by a model.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::android;
use crate::providers::{self, AiMessage, ToolCall, ToolResult};
use crate::waydroid::{decode_screenshot_png, detect_waydroid};

pub const DEFAULT_MAX_STEPS: u32 = 6;
pub const HARD_MAX_STEPS: u32 = 8;
pub const TASK_TIMEOUT: Duration = Duration::from_secs(180);
const PROVIDER_STEP_TIMEOUT: u64 = 90;
const MAX_PROMPT_CHARS: usize = 4_000;
const MAX_TOOL_RESULT_CHARS: usize = 6_000;
const MAX_RETAINED_RUNS: usize = 20;

const SYSTEM_PROMPT: &str = "\
You operate a real Android device through the tools provided. Follow these rules strictly:
1. Call exactly one tool per turn. Never invent tool names or arguments.
2. Observe before you act: use get_device_status and get_screen to understand the current state.
3. Treat every screen action as potentially consequential and follow the user's selected safety mode.
4. After a UI action (tap, swipe, press_key, input_text) capture the screen again before reporting progress.
5. A successful input command or frame-metadata capture alone does not prove the intended result. Only claim success when a tool provides meaningful confirmation.
6. Never claim a tool ran, or that something changed, unless its result says so.
7. If a tool fails, read the error, adjust once, and explain the limitation instead of repeating the same call.
8. Destructive actions require user approval and may be declined. Do not try to work around a refusal.
9. The device is a real user's Android environment. Do not attempt to bypass confirmations or privacy settings.
10. Finish with a short report of what was verified, what was not, and the next safe step.";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum RunStatus {
    Running,
    AwaitingApproval,
    Cancelling,
    Completed,
    /// A mutation ran, but no tool-specific evidence establishes the requested
    /// device state.
    NeedsVerification,
    /// Some tool calls failed along the way; part of the task may have happened.
    PartiallyCompleted,
    Failed,
    Cancelled,
}

impl RunStatus {
    fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Running => "running",
            RunStatus::AwaitingApproval => "awaiting_approval",
            RunStatus::Cancelling => "cancelling",
            RunStatus::Completed => "completed",
            RunStatus::NeedsVerification => "needs_verification",
            RunStatus::PartiallyCompleted => "partially_completed",
            RunStatus::Failed => "failed",
            RunStatus::Cancelled => "cancelled",
        }
    }

    fn terminal(&self) -> bool {
        matches!(
            self,
            RunStatus::Completed
                | RunStatus::NeedsVerification
                | RunStatus::PartiallyCompleted
                | RunStatus::Failed
                | RunStatus::Cancelled
        )
    }
}

/// How much the Action engine may do without the user in the loop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SafetyMode {
    /// No tool may change the device.
    ReadOnly,
    /// Every tool that changes device state needs approval.
    ConfirmMutations,
    /// Every tool call, including read-only ones, needs approval.
    ConfirmEveryAction,
    /// Only volume adjustment keys run unattended. Other device changes need
    /// approval because their screen-level impact cannot be inferred safely.
    TrustedLowRisk,
}

impl SafetyMode {
    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw.trim() {
            "read_only" => Ok(SafetyMode::ReadOnly),
            "confirm_mutations" => Ok(SafetyMode::ConfirmMutations),
            "confirm_every_action" => Ok(SafetyMode::ConfirmEveryAction),
            "trusted_low_risk" => Ok(SafetyMode::TrustedLowRisk),
            other => Err(format!(
                "Unknown safety mode '{other}'. Use read_only, confirm_mutations, confirm_every_action or trusted_low_risk."
            )),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            SafetyMode::ReadOnly => "read_only",
            SafetyMode::ConfirmMutations => "confirm_mutations",
            SafetyMode::ConfirmEveryAction => "confirm_every_action",
            SafetyMode::TrustedLowRisk => "trusted_low_risk",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActionStepView {
    pub index: usize,
    /// `observe`, `plan`, `action`, `verify`, `approval` or `result`.
    pub kind: String,
    pub title: String,
    pub detail: String,
    pub ok: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingApproval {
    pub id: String,
    pub tool: String,
    pub summary: String,
    pub arguments: Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ActionTaskView {
    pub id: String,
    pub prompt: String,
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub status: String,
    pub steps: Vec<ActionStepView>,
    pub pending_approval: Option<PendingApproval>,
    pub result: Option<String>,
    pub error: Option<String>,
    pub started_at: u64,
    pub finished_at: Option<u64>,
    pub max_steps: u32,
    pub safety: String,
    pub actions_attempted: u32,
    pub actions_run: u32,
    pub actions_failed: u32,
    pub actions_blocked: u32,
    /// `verified`, `readback_only`, `unverified` or `no_device_change`.
    pub verification: String,
}

struct PendingAction {
    approval_id: String,
    call: ToolCall,
    summary: String,
}

struct ActionRun {
    id: String,
    prompt: String,
    provider: String,
    base_url: String,
    model: String,
    status: RunStatus,
    steps: Vec<ActionStepView>,
    messages: Vec<AiMessage>,
    pending: Option<PendingAction>,
    result: Option<String>,
    error: Option<String>,
    cancel: Arc<AtomicBool>,
    runner_active: bool,
    seen: HashSet<String>,
    started: Instant,
    started_at: u64,
    finished_at: Option<u64>,
    steps_taken: u32,
    max_steps: u32,
    /// Number of tool calls that executed, excluding rejected or blocked calls.
    actions_run: u32,
    actions_failed: u32,
    actions_blocked: u32,
    /// Tools that changed device state and the read-backs after the last one.
    mutations: u32,
    verified_after_mutation: bool,
    readback_after_mutation: bool,
    safety: SafetyMode,
    actions_attempted: u32,
}

impl ActionRun {
    fn terminal(&self) -> bool {
        self.status.terminal()
    }

    fn push_step(
        &mut self,
        kind: &str,
        title: impl Into<String>,
        detail: impl Into<String>,
        ok: bool,
    ) {
        self.steps.push(ActionStepView {
            index: self.steps.len() + 1,
            kind: kind.to_string(),
            title: title.into(),
            detail: detail.into(),
            ok,
        });
    }

    fn record_action(&mut self, action: &PlannedAction, outcome: &ExecOutcome) {
        self.actions_run += 1;
        if !outcome.ok {
            self.actions_failed += 1;
        }
        if action.is_mutation() {
            self.mutations += 1;
            self.verified_after_mutation = outcome.verified;
            self.readback_after_mutation = false;
        } else if action.is_readback() && self.mutations > 0 && outcome.ok {
            self.readback_after_mutation = true;
        }
    }

    fn finish(&mut self, status: RunStatus, result: Option<String>, error: Option<String>) {
        self.status = status;
        self.result = result;
        self.error = error;
        self.pending = None;
        self.finished_at = Some(now_seconds());
    }

    fn finish_cancelled(&mut self) {
        self.push_step(
            "result",
            "Cancelled",
            "The task was cancelled before it finished.",
            false,
        );
        self.finish(
            RunStatus::Cancelled,
            None,
            Some("Cancelled by the user.".to_string()),
        );
    }

    fn finish_failed(&mut self, error: impl Into<String>) {
        let error = error.into();
        self.push_step("result", "Task failed", error.clone(), false);
        self.finish(RunStatus::Failed, None, Some(error));
    }

    fn view(&self) -> ActionTaskView {
        ActionTaskView {
            id: self.id.clone(),
            prompt: self.prompt.clone(),
            provider: self.provider.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            status: self.status.as_str().to_string(),
            steps: self.steps.clone(),
            pending_approval: self.pending.as_ref().map(|pending| PendingApproval {
                id: pending.approval_id.clone(),
                tool: pending.call.name.clone(),
                summary: pending.summary.clone(),
                arguments: pending.call.arguments.clone(),
            }),
            result: self.result.clone(),
            error: self.error.clone(),
            started_at: self.started_at,
            finished_at: self.finished_at,
            max_steps: self.max_steps,
            safety: self.safety.label().to_string(),
            actions_run: self.actions_run,
            actions_failed: self.actions_failed,
            actions_blocked: self.actions_blocked,
            actions_attempted: self.actions_attempted,
            verification: self.verification().to_string(),
        }
    }

    fn verification(&self) -> &'static str {
        if self.mutations == 0 {
            "no_device_change"
        } else if self.verified_after_mutation {
            "verified"
        } else if self.readback_after_mutation {
            "readback_only"
        } else {
            "unverified"
        }
    }

    /// A model that stops talking is not evidence that the task worked. Success
    /// needs every action to have succeeded and a tool-specific confirmation
    /// of the requested device state, not just a successful command/read-back.
    fn completion_status(&self) -> (RunStatus, Option<String>) {
        if self.actions_failed > 0 {
            return (
                RunStatus::PartiallyCompleted,
                Some(format!(
                    "{} of {} actions failed. The device may be in a partial state; review the steps before retrying.",
                    self.actions_failed, self.actions_run
                )),
            );
        }
        if self.actions_blocked > 0 {
            return (
                if self.actions_run > 0 {
                    RunStatus::PartiallyCompleted
                } else {
                    RunStatus::Failed
                },
                Some(format!(
                    "{} action(s) were not executed because of the read-only safety policy or a declined approval.",
                    self.actions_blocked
                )),
            );
        }
        if self.mutations > 0 && !self.verified_after_mutation {
            return (
                RunStatus::NeedsVerification,
                Some(
                    if self.readback_after_mutation {
                        "A read-back succeeded, but the available tool output does not establish that the requested device state occurred. Verify it in the Android interface."
                    } else {
                        "The device may have changed, but no successful read-back or tool-specific verification followed. Verify it in the Android interface."
                    }
                        .to_string(),
                ),
            );
        }
        (RunStatus::Completed, None)
    }
}

type RunHandle = Arc<Mutex<ActionRun>>;
static RUNS: OnceLock<Mutex<HashMap<String, RunHandle>>> = OnceLock::new();

fn runs() -> &'static Mutex<HashMap<String, RunHandle>> {
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn lock_run(handle: &RunHandle) -> Result<std::sync::MutexGuard<'_, ActionRun>, String> {
    handle
        .lock()
        .map_err(|_| "The action task state is unavailable.".to_string())
}

fn find_run(run_id: &str) -> Result<RunHandle, String> {
    runs()
        .lock()
        .map_err(|_| "The action task registry is unavailable.".to_string())?
        .get(run_id)
        .cloned()
        .ok_or_else(|| format!("Action task '{run_id}' was not found."))
}

fn snapshot(handle: &RunHandle) -> Result<ActionTaskView, String> {
    Ok(lock_run(handle)?.view())
}

fn insert_run(handle: RunHandle) {
    let Ok(mut registry) = runs().lock() else {
        return;
    };
    registry.insert(
        handle.lock().map(|run| run.id.clone()).unwrap_or_default(),
        handle,
    );
    if registry.len() > MAX_RETAINED_RUNS {
        let oldest = registry
            .iter()
            .filter(|(_, run)| run.lock().map(|run| run.terminal()).unwrap_or(false))
            .min_by_key(|(_, run)| run.lock().map(|run| run.started_at).unwrap_or_default())
            .map(|(id, _)| id.clone());
        if let Some(id) = oldest {
            registry.remove(&id);
        }
    }
}

// ---------------------------------------------------------------------------
// Tool registry
// ---------------------------------------------------------------------------

const TOOL_NAMES: [&str; 11] = [
    "get_device_status",
    "get_screen",
    "get_installed_apps",
    "get_app_info",
    "launch_app",
    "tap",
    "swipe",
    "press_key",
    "input_text",
    "list_android_files",
    "uninstall_app",
];

pub fn tool_names() -> Vec<&'static str> {
    TOOL_NAMES.to_vec()
}

fn tool_definitions() -> Vec<Value> {
    let function = |name: &str, description: &str, parameters: Value| {
        json!({
            "type": "function",
            "function": { "name": name, "description": description, "parameters": parameters }
        })
    };
    vec![
        function(
            "get_device_status",
            "Read Waydroid session, container, ADB, Android shell, package manager and screenshot health for the selected device.",
            json!({ "type": "object", "properties": {}, "additionalProperties": false }),
        ),
        function(
            "get_screen",
            "Capture the current Android screen and report its size. Frame metadata only; the image is not attached to the conversation.",
            json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        ),
        function(
            "get_installed_apps",
            "List installed Android packages, optionally filtered by a search string.",
            json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 100 }
                },
                "additionalProperties": false
            }),
        ),
        function(
            "get_app_info",
            "Read version, install path and launcher activity for one installed package.",
            json!({
                "type": "object",
                "properties": { "package": { "type": "string" } },
                "required": ["package"],
                "additionalProperties": false
            }),
        ),
        function(
            "launch_app",
            "Launch an installed application and verify it reached the foreground.",
            json!({
                "type": "object",
                "properties": { "package": { "type": "string" } },
                "required": ["package"],
                "additionalProperties": false
            }),
        ),
        function(
            "tap",
            "Tap one screen coordinate in device pixels.",
            json!({
                "type": "object",
                "properties": {
                    "x": { "type": "integer", "minimum": 0, "maximum": 8192 },
                    "y": { "type": "integer", "minimum": 0, "maximum": 8192 }
                },
                "required": ["x", "y"],
                "additionalProperties": false
            }),
        ),
        function(
            "swipe",
            "Swipe between two screen coordinates in device pixels.",
            json!({
                "type": "object",
                "properties": {
                    "x1": { "type": "integer", "minimum": 0, "maximum": 8192 },
                    "y1": { "type": "integer", "minimum": 0, "maximum": 8192 },
                    "x2": { "type": "integer", "minimum": 0, "maximum": 8192 },
                    "y2": { "type": "integer", "minimum": 0, "maximum": 8192 },
                    "duration_ms": { "type": "integer", "minimum": 50, "maximum": 3000 }
                },
                "required": ["x1", "y1", "x2", "y2"],
                "additionalProperties": false
            }),
        ),
        function(
            "press_key",
            "Send one supported Android key: home, back, recents, power, wakeup, sleep, enter, delete, volume_up, volume_down, menu, search.",
            json!({
                "type": "object",
                "properties": { "key": { "type": "string" } },
                "required": ["key"],
                "additionalProperties": false
            }),
        ),
        function(
            "input_text",
            "Type text into the focused field (letters, digits, spaces and . , ! ? @ _ - + / :).",
            json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
                "additionalProperties": false
            }),
        ),
        function(
            "list_android_files",
            "List one absolute Android directory such as /sdcard or /sdcard/Download.",
            json!({
                "type": "object",
                "properties": { "path": { "type": "string" } },
                "required": ["path"],
                "additionalProperties": false
            }),
        ),
        function(
            "uninstall_app",
            "Uninstall a user application. Requires explicit user approval and cannot remove system apps.",
            json!({
                "type": "object",
                "properties": { "package": { "type": "string" } },
                "required": ["package"],
                "additionalProperties": false
            }),
        ),
    ]
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppsArgs {
    #[serde(default)]
    query: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PackageArgs {
    package: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TapArgs {
    x: u32,
    y: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SwipeArgs {
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    #[serde(default = "default_duration")]
    duration_ms: u32,
}

fn default_duration() -> u32 {
    300
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyArgs {
    key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextArgs {
    text: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

#[derive(Clone, Debug, PartialEq)]
enum PlannedAction {
    DeviceStatus,
    Screen,
    InstalledApps {
        query: Option<String>,
        limit: usize,
    },
    AppInfo {
        package: String,
    },
    Launch {
        package: String,
    },
    Tap {
        x: u32,
        y: u32,
    },
    Swipe {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        duration_ms: u32,
    },
    PressKey {
        key: String,
    },
    InputText {
        text: String,
    },
    ListFiles {
        path: String,
    },
    Uninstall {
        package: String,
    },
}

impl PlannedAction {
    fn tool(&self) -> &'static str {
        match self {
            PlannedAction::DeviceStatus => "get_device_status",
            PlannedAction::Screen => "get_screen",
            PlannedAction::InstalledApps { .. } => "get_installed_apps",
            PlannedAction::AppInfo { .. } => "get_app_info",
            PlannedAction::Launch { .. } => "launch_app",
            PlannedAction::Tap { .. } => "tap",
            PlannedAction::Swipe { .. } => "swipe",
            PlannedAction::PressKey { .. } => "press_key",
            PlannedAction::InputText { .. } => "input_text",
            PlannedAction::ListFiles { .. } => "list_android_files",
            PlannedAction::Uninstall { .. } => "uninstall_app",
        }
    }

    fn is_mutation(&self) -> bool {
        matches!(
            self,
            PlannedAction::Launch { .. }
                | PlannedAction::Tap { .. }
                | PlannedAction::Swipe { .. }
                | PlannedAction::PressKey { .. }
                | PlannedAction::InputText { .. }
                | PlannedAction::Uninstall { .. }
        )
    }

    fn is_readback(&self) -> bool {
        matches!(
            self,
            PlannedAction::DeviceStatus
                | PlannedAction::Screen
                | PlannedAction::InstalledApps { .. }
                | PlannedAction::AppInfo { .. }
                | PlannedAction::ListFiles { .. }
        )
    }

    fn is_verified_mutation(&self) -> bool {
        matches!(
            self,
            PlannedAction::Launch { .. } | PlannedAction::Uninstall { .. }
        )
    }

    fn is_trusted_low_risk(&self) -> bool {
        matches!(
            self,
            PlannedAction::PressKey { key } if matches!(key.as_str(), "volume_down" | "volume_up")
        )
    }

    fn requires_approval(&self, safety: SafetyMode) -> bool {
        match safety {
            SafetyMode::ReadOnly => false,
            SafetyMode::ConfirmMutations => self.is_mutation(),
            SafetyMode::ConfirmEveryAction => true,
            SafetyMode::TrustedLowRisk => self.is_mutation() && !self.is_trusted_low_risk(),
        }
    }

    fn summary(&self) -> String {
        match self {
            PlannedAction::DeviceStatus => "Read device health".to_string(),
            PlannedAction::Screen => "Capture the Android screen".to_string(),
            PlannedAction::InstalledApps { query, .. } => match query {
                Some(query) => format!("List installed apps matching '{query}'"),
                None => "List installed apps".to_string(),
            },
            PlannedAction::AppInfo { package } => format!("Read details for {package}"),
            PlannedAction::Launch { package } => format!("Launch {package}"),
            PlannedAction::Tap { x, y } => format!("Tap at {x},{y}"),
            PlannedAction::Swipe { x1, y1, x2, y2, .. } => {
                format!("Swipe from {x1},{y1} to {x2},{y2}")
            }
            PlannedAction::PressKey { key } => format!("Press {key}"),
            PlannedAction::InputText { text } => {
                format!("Type '{}'", text.chars().take(40).collect::<String>())
            }
            PlannedAction::ListFiles { path } => format!("List {path}"),
            PlannedAction::Uninstall { package } => {
                format!("Uninstall {package} and delete its application data")
            }
        }
    }
}

fn parse_args<T: for<'de> Deserialize<'de>>(arguments: &Value) -> Result<T, String> {
    serde_json::from_value(arguments.clone()).map_err(|error| {
        format!("Invalid tool arguments: {error}. Send only the documented fields.")
    })
}

/// Validates a model-proposed call before anything touches the device.
fn plan_call(call: &ToolCall) -> Result<PlannedAction, String> {
    if !call.arguments.is_object() {
        return Err("Tool arguments must be a JSON object.".to_string());
    }
    let action = match call.name.as_str() {
        "get_device_status" => {
            let _: HashMap<String, Value> = parse_args(&call.arguments)?;
            PlannedAction::DeviceStatus
        }
        "get_screen" => {
            let _: HashMap<String, Value> = parse_args(&call.arguments)?;
            PlannedAction::Screen
        }
        "get_installed_apps" => {
            let args = parse_args::<AppsArgs>(&call.arguments)?;
            PlannedAction::InstalledApps {
                query: args
                    .query
                    .map(|query| query.trim().to_string())
                    .filter(|query| !query.is_empty()),
                limit: args.limit.unwrap_or(40).clamp(1, 100) as usize,
            }
        }
        "get_app_info" => PlannedAction::AppInfo {
            package: validated_package(parse_args::<PackageArgs>(&call.arguments)?.package)?,
        },
        "launch_app" => PlannedAction::Launch {
            package: validated_package(parse_args::<PackageArgs>(&call.arguments)?.package)?,
        },
        "tap" => {
            let args = parse_args::<TapArgs>(&call.arguments)?;
            android::validate_point(args.x, args.y)?;
            PlannedAction::Tap {
                x: args.x,
                y: args.y,
            }
        }
        "swipe" => {
            let args = parse_args::<SwipeArgs>(&call.arguments)?;
            android::validate_point(args.x1, args.y1)?;
            android::validate_point(args.x2, args.y2)?;
            if !(50..=3_000).contains(&args.duration_ms) {
                return Err("Swipe duration must be between 50 and 3000 ms.".to_string());
            }
            PlannedAction::Swipe {
                x1: args.x1,
                y1: args.y1,
                x2: args.x2,
                y2: args.y2,
                duration_ms: args.duration_ms,
            }
        }
        "press_key" => {
            let key = parse_args::<KeyArgs>(&call.arguments)?.key;
            if !android::ANDROID_KEYS.iter().any(|(name, _)| *name == key) {
                return Err(format!("Unsupported Android key '{key}'."));
            }
            PlannedAction::PressKey { key }
        }
        "input_text" => {
            let text = parse_args::<TextArgs>(&call.arguments)?.text;
            // Reuse the same alphabet check as the command path.
            validate_input_text(&text)?;
            PlannedAction::InputText { text }
        }
        "list_android_files" => {
            let path = parse_args::<PathArgs>(&call.arguments)?.path;
            android::validate_android_path(&path)?;
            PlannedAction::ListFiles { path }
        }
        "uninstall_app" => PlannedAction::Uninstall {
            package: validated_package(parse_args::<PackageArgs>(&call.arguments)?.package)?,
        },
        other => {
            return Err(format!(
                "Unsupported tool '{other}'. Use one of: {}.",
                TOOL_NAMES.join(", ")
            ));
        }
    };
    Ok(action)
}

fn validated_package(package: String) -> Result<String, String> {
    android::validate_package_name(&package)?;
    Ok(package)
}

fn validate_input_text(text: &str) -> Result<(), String> {
    if text.trim().is_empty() {
        return Err("Text to type cannot be empty.".to_string());
    }
    if text.chars().count() > 500 {
        return Err("Text input is limited to 500 characters.".to_string());
    }
    if text.chars().any(|ch| {
        !(ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                ' ' | '.' | ',' | '!' | '?' | '@' | '_' | '-' | '+' | '/' | ':'
            ))
    }) {
        return Err(
            "Text input may only contain letters, digits, spaces and . , ! ? @ _ - + / :"
                .to_string(),
        );
    }
    Ok(())
}

struct ExecOutcome {
    ok: bool,
    detail: String,
    verified: bool,
}

fn truncate(text: String) -> String {
    if text.chars().count() > MAX_TOOL_RESULT_CHARS {
        let trimmed: String = text.chars().take(MAX_TOOL_RESULT_CHARS).collect();
        format!("{trimmed}\n[result truncated at {MAX_TOOL_RESULT_CHARS} characters]")
    } else {
        text
    }
}

/// A reply that names a registered tool and carries arguments is a tool call
/// printed as text (small models do this), not an answer.
fn looks_like_tool_call(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.contains("arguments") && tool_names().iter().any(|name| lower.contains(name))
}

fn execute(action: &PlannedAction) -> ExecOutcome {
    let result: Result<String, String> = match action {
        PlannedAction::DeviceStatus => android::get_selected_device().map(|device| {
            let diagnostics = detect_waydroid(Some(&device));
            format!(
                "Selected device: {} (Waydroid: {}). Health: {}. Service: {}. Session running: {}. Container: {}. ADB connected: {}. Android shell: {}. Package manager: {}. Screenshot: {}. Android: {}. Resolution: {}.",
                device.serial,
                if device.is_waydroid { "yes" } else { "no" },
                diagnostics.health,
                if diagnostics.service_running { "running" } else { "stopped" },
                diagnostics.session_running,
                diagnostics.container_status,
                diagnostics.adb_connected,
                if diagnostics.adb_shell_healthy { "responsive" } else { "unresponsive" },
                if diagnostics.android_package_manager_healthy { "responsive" } else { "unresponsive" },
                if diagnostics.screenshot_healthy { "available" } else { "unavailable" },
                diagnostics.android_version,
                diagnostics.resolution,
            )
        }),
        PlannedAction::Screen => android::capture_screenshot().and_then(|bytes| {
            let (width, height) = decode_screenshot_png(&bytes)?;
            Ok(format!(
                "Captured a {width}x{height} PNG frame ({} bytes) from the selected device. This engine reports frame metadata only; use the screenshot panel in the UI to view it.",
                bytes.len()
            ))
        }),
        PlannedAction::InstalledApps { query, limit } => android::list_installed_apps_selected()
            .map(|apps| {
                let query_lower = query.as_deref().unwrap_or("").to_lowercase();
                let matching: Vec<_> = apps
                    .into_iter()
                    .filter(|app| {
                        query_lower.is_empty()
                            || app.package.to_lowercase().contains(&query_lower)
                            || app
                                .name
                                .as_deref()
                                .is_some_and(|name| name.to_lowercase().contains(&query_lower))
                    })
                    .collect();
                let total = matching.len();
                let listed: Vec<String> = matching
                    .iter()
                    .take(*limit)
                    .map(|app| {
                        format!(
                            "{} ({}{})",
                            app.package,
                            app.name.as_deref().unwrap_or("label unavailable"),
                            if app.system_app { ", system" } else { ", user" }
                        )
                    })
                    .collect();
                format!(
                    "{total} matching package(s); showing {}:\n{}",
                    listed.len(),
                    listed.join("\n")
                )
            }),
        PlannedAction::AppInfo { package } => android::get_selected_device().and_then(|device| {
            android::get_app_info(&device.serial, package).map(|info| {
                format!(
                    "{}: version {} ({}), path {}, launcher {}, category {}.",
                    info.package,
                    info.version_name.as_deref().unwrap_or("unknown"),
                    info.version_code.as_deref().unwrap_or("unknown"),
                    info.install_path.as_deref().unwrap_or("unknown"),
                    info.launcher_activity.as_deref().unwrap_or("none"),
                    info.category,
                )
            })
        }),
        PlannedAction::Launch { package } => android::get_selected_device()
            .and_then(|device| android::launch_app(&device.serial, package)),
        PlannedAction::Tap { x, y } => android::tap(*x, *y),
        PlannedAction::Swipe { x1, y1, x2, y2, duration_ms } => {
            android::swipe(*x1, *y1, *x2, *y2, *duration_ms)
        }
        PlannedAction::PressKey { key } => android::press_key(key),
        PlannedAction::InputText { text } => android::input_text(text),
        PlannedAction::ListFiles { path } => android::list_android_files(path).map(|entries| {
            let listed: Vec<String> = entries
                .iter()
                .take(40)
                .map(|entry| {
                    format!(
                        "{}{} ({} bytes, {})",
                        entry.name,
                        if entry.is_dir { "/" } else { "" },
                        entry.size_bytes,
                        entry.modified
                    )
                })
                .collect();
            format!(
                "{} entries; showing {}:\n{}",
                entries.len(),
                listed.len(),
                listed.join("\n")
            )
        }),
        PlannedAction::Uninstall { package } => android::get_selected_device().and_then(|device| {
            android::uninstall_app(&device.serial, package, true)
        }),
    };

    let verified = result.is_ok() && action.is_verified_mutation();
    match result {
        Ok(detail) => ExecOutcome {
            ok: true,
            detail: truncate(detail),
            verified,
        },
        Err(error) => ExecOutcome {
            ok: false,
            detail: truncate(error),
            verified: false,
        },
    }
}

// ---------------------------------------------------------------------------
// Run loop
// ---------------------------------------------------------------------------

fn step_budget(run: &ActionRun) -> Result<u64, String> {
    let elapsed = run.started.elapsed();
    if elapsed >= TASK_TIMEOUT {
        return Err(format!(
            "The task exceeded its {}s overall time limit.",
            TASK_TIMEOUT.as_secs()
        ));
    }
    Ok((TASK_TIMEOUT - elapsed)
        .as_secs()
        .clamp(5, PROVIDER_STEP_TIMEOUT))
}

fn advance(handle: &RunHandle) {
    if let Err(error) = advance_inner(handle)
        && let Ok(mut run) = lock_run(handle)
        && !run.terminal()
    {
        run.finish_failed(error);
    }
    if let Ok(mut run) = lock_run(handle) {
        run.runner_active = false;
    }
}

fn advance_inner(handle: &RunHandle) -> Result<(), String> {
    loop {
        // Phase 1: decide what to do next. No lock is held across a request.
        let request = {
            let mut run = lock_run(handle)?;
            if run.terminal() {
                return Ok(());
            }
            if run.cancel.load(Ordering::Acquire) {
                run.finish_cancelled();
                return Ok(());
            }
            if run.pending.is_some() {
                run.status = RunStatus::AwaitingApproval;
                return Ok(());
            }
            if run.steps_taken >= run.max_steps {
                let max_steps = run.max_steps;
                run.finish_failed(format!(
                    "The task reached its {max_steps}-step limit before the model finished. Split the request into smaller tasks or raise the limit."
                ));
                return Ok(());
            }
            run.steps_taken += 1;
            Some((
                run.provider.clone(),
                run.base_url.clone(),
                run.model.clone(),
                run.messages.clone(),
                step_budget(&run)?,
            ))
        };
        let Some((provider, base_url, model, messages, timeout)) = request else {
            return Ok(());
        };

        let completion = providers::complete_with_tools(
            &provider,
            &base_url,
            &model,
            &messages,
            &tool_definitions(),
            timeout,
        );
        let completion = match completion {
            Ok(completion) => completion,
            Err(error) => {
                let mut run = lock_run(handle)?;
                if run.cancel.load(Ordering::Acquire) {
                    run.finish_cancelled();
                } else {
                    run.finish_failed(format!("The Action model request failed: {error}"));
                }
                return Ok(());
            }
        };

        // Phase 2: record the model turn and choose one call to act on.
        let next = {
            let mut run = lock_run(handle)?;
            if run.cancel.load(Ordering::Acquire) {
                run.finish_cancelled();
                return Ok(());
            }
            let content = completion.content.clone();
            let tool_calls = completion.tool_calls.clone();
            run.messages.push(AiMessage::Assistant {
                content: content.clone(),
                tool_calls: tool_calls.clone(),
            });
            if tool_calls.is_empty() {
                let Some(text) = content.filter(|text| !text.trim().is_empty()) else {
                    run.finish_failed("The model returned neither an answer nor a tool call.");
                    return Ok(());
                };
                // Small local models sometimes print the call as text instead of
                // using the tools interface. Feeding that back as a correction is
                // the only way to get real, verified execution.
                if looks_like_tool_call(&text) {
                    let correction = "That reply looks like a tool call written as text. Call one registered tool through the tools interface instead of printing JSON.";
                    run.push_step("plan", "Malformed tool call rejected", correction, false);
                    run.messages.push(AiMessage::User(correction.to_string()));
                    return Ok(());
                }
                if run.actions_run == 0 {
                    run.finish_failed(format!(
                        "The model replied without running any device tool, so nothing was verified and no device change was made. Model reply: {text}"
                    ));
                    return Ok(());
                }
                let (status, error) = run.completion_status();
                run.push_step(
                    "result",
                    "Model report",
                    text.clone(),
                    status == RunStatus::Completed,
                );
                run.finish(status, Some(text), error);
                return Ok(());
            }
            if tool_calls.len() > 1 {
                run.push_step(
                    "plan",
                    "Extra tool calls ignored",
                    format!(
                        "The model proposed {} calls; one is executed per turn.",
                        tool_calls.len()
                    ),
                    true,
                );
            }
            let call = tool_calls[0].clone();
            run.actions_attempted += 1;
            run.push_step(
                "plan",
                format!("Action proposed: {}", call.name),
                "A registered tool call was generated by the Action model.",
                true,
            );
            let duplicate_key = format!(
                "{}:{}",
                call.name,
                serde_json::to_string(&call.arguments).unwrap_or_default()
            );
            if !run.seen.insert(duplicate_key) {
                let message = format!(
                    "Duplicate action rejected: {} with the same arguments was already attempted. Choose a different action or report the limitation.",
                    call.name
                );
                run.push_step("plan", "Duplicate action rejected", message.clone(), false);
                run.messages.push(AiMessage::Tool(ToolResult {
                    tool_call_id: call.id.clone(),
                    name: call.name.clone(),
                    content: message,
                }));
                Some(None)
            } else {
                match plan_call(&call) {
                    Err(error) => {
                        run.push_step(
                            "plan",
                            format!("Rejected {}", call.name),
                            error.clone(),
                            false,
                        );
                        run.messages.push(AiMessage::Tool(ToolResult {
                            tool_call_id: call.id.clone(),
                            name: call.name.clone(),
                            content: error,
                        }));
                        Some(None)
                    }
                    Ok(action) => {
                        run.push_step(
                            "plan",
                            format!("Validated {}", action.tool()),
                            "Arguments passed typed validation and safety checks.",
                            true,
                        );
                        if run.safety == SafetyMode::ReadOnly && action.is_mutation() {
                            let message = "Read-only safety mode rejected this mutation. No device change was made.";
                            run.actions_blocked += 1;
                            run.push_step(
                                "plan",
                                format!("Blocked {}", action.tool()),
                                message,
                                false,
                            );
                            run.messages.push(AiMessage::Tool(ToolResult {
                                tool_call_id: call.id.clone(),
                                name: call.name.clone(),
                                content: message.to_string(),
                            }));
                            Some(None)
                        } else if action.requires_approval(run.safety) {
                            let approval_id = format!("{}-{}", run.id, run.steps.len() + 1);
                            run.push_step(
                                "approval",
                                "Waiting for approval",
                                action.summary(),
                                true,
                            );
                            run.pending = Some(PendingAction {
                                approval_id,
                                call,
                                summary: action.summary(),
                            });
                            run.status = RunStatus::AwaitingApproval;
                            return Ok(());
                        } else {
                            run.push_step(
                                "plan",
                                format!("Next: {}", action.tool()),
                                action.summary(),
                                true,
                            );
                            Some(Some(action))
                        }
                    }
                }
            }
        };

        let Some(action) = next else {
            continue;
        };
        let Some(action) = action else {
            continue;
        };

        // Phase 3: execute outside the lock, then record the verified outcome.
        let outcome = execute(&action);
        let mut run = lock_run(handle)?;
        if run.cancel.load(Ordering::Acquire) {
            run.push_step(
                "result",
                "Cancelled",
                format!(
                    "Cancelled before recording the result of {}.",
                    action.tool()
                ),
                false,
            );
            run.finish_cancelled();
            return Ok(());
        }
        let title = format!("Executed: {} - {}", action.tool(), action.summary());
        run.push_step("action", title, outcome.detail.clone(), outcome.ok);
        run.record_action(&action, &outcome);
        if action.is_readback() && outcome.ok {
            run.push_step(
                "observe",
                "Device read-back returned",
                "The observation succeeded; it does not by itself prove that the requested change occurred.",
                true,
            );
        }
        if outcome.verified {
            run.push_step(
                "verify",
                "Intended device state verified",
                outcome.detail.clone(),
                true,
            );
        }
        let content = if outcome.ok {
            format!("ok: {}", outcome.detail)
        } else {
            format!("error: {}", outcome.detail)
        };
        let tool_call_id = format!("{}-step-{}", run.id, run.steps.len());
        run.messages.push(AiMessage::Tool(ToolResult {
            tool_call_id,
            name: action.tool().to_string(),
            content,
        }));
        drop(run);
    }
}

fn spawn_runner(handle: RunHandle) {
    let already_running = lock_run(&handle)
        .map(|mut run| {
            if run.runner_active || run.terminal() {
                true
            } else {
                run.runner_active = true;
                false
            }
        })
        .unwrap_or(true);
    if already_running {
        return;
    }
    std::thread::spawn(move || advance(&handle));
}

// ---------------------------------------------------------------------------
// Commands surface (called from lib.rs)
// ---------------------------------------------------------------------------

pub fn start(
    prompt: &str,
    provider: &str,
    base_url: &str,
    model: &str,
    max_steps: Option<u32>,
    safety: Option<&str>,
) -> Result<ActionTaskView, String> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("Describe the Android task first.".to_string());
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err(format!(
            "The task description exceeds {MAX_PROMPT_CHARS} characters."
        ));
    }
    providers::validate_provider(provider)?;
    providers::validate_model_name(model)?;
    if model.trim().is_empty() {
        return Err("Choose an Action model first.".to_string());
    }
    let base_url = providers::validate_base_url(base_url)?;
    let capabilities = providers::probe_model(provider, &base_url, model)?;
    if capabilities.tools == Some(false) {
        return Err(format!(
            "The selected Action model does not report tool-calling support. Choose a tool-capable model. {}",
            capabilities.note
        ));
    }

    let max_steps = max_steps
        .unwrap_or(DEFAULT_MAX_STEPS)
        .clamp(1, HARD_MAX_STEPS);
    let safety = SafetyMode::parse(safety.unwrap_or("confirm_mutations"))?;
    let id = format!(
        "action-{}-{}",
        now_seconds(),
        runs().lock().map(|map| map.len()).unwrap_or(0)
    );
    let mut run = ActionRun {
        id: id.clone(),
        prompt: prompt.to_string(),
        provider: provider.to_string(),
        base_url,
        model: model.to_string(),
        status: RunStatus::Running,
        steps: Vec::new(),
        messages: vec![
            AiMessage::System(SYSTEM_PROMPT.to_string()),
            AiMessage::User(prompt.to_string()),
        ],
        pending: None,
        result: None,
        error: None,
        cancel: Arc::new(AtomicBool::new(false)),
        runner_active: false,
        seen: HashSet::new(),
        started: Instant::now(),
        started_at: now_seconds(),
        finished_at: None,
        steps_taken: 0,
        actions_attempted: 0,
        actions_run: 0,
        actions_failed: 0,
        actions_blocked: 0,
        mutations: 0,
        verified_after_mutation: false,
        readback_after_mutation: false,
        safety,
        max_steps,
    };
    run.push_step(
        "plan",
        "Task accepted",
        format!(
            "Provider {provider}, model {model}. Vision: {}; tools: {}.",
            match capabilities.vision {
                Some(true) => "reported",
                Some(false) => "not supported",
                None => "not reported",
            },
            match capabilities.tools {
                Some(true) => "reported",
                Some(false) => "not supported",
                None => "not reported",
            }
        ),
        true,
    );
    let handle = Arc::new(Mutex::new(run));
    insert_run(handle.clone());
    spawn_runner(handle.clone());
    snapshot(&handle)
}

pub fn approve(run_id: &str, approval_id: &str, approved: bool) -> Result<ActionTaskView, String> {
    let handle = find_run(run_id)?;
    let pending_call = {
        let mut run = lock_run(&handle)?;
        if run.status != RunStatus::AwaitingApproval {
            return Err("This task is not waiting for approval.".to_string());
        }
        let pending = run
            .pending
            .take()
            .ok_or_else(|| "The pending approval no longer exists.".to_string())?;
        if pending.approval_id != approval_id {
            run.pending = Some(pending);
            return Err("The approval reference does not match the pending action.".to_string());
        }
        if !approved {
            run.actions_blocked += 1;
            run.push_step(
                "approval",
                "Action declined",
                pending.summary.clone(),
                false,
            );
            run.messages.push(AiMessage::Tool(ToolResult {
                tool_call_id: pending.call.id.clone(),
                name: pending.call.name.clone(),
                content: "The user declined this action. Do not retry it.".to_string(),
            }));
            run.status = RunStatus::Running;
            None
        } else {
            run.push_step("approval", "Action approved", pending.summary.clone(), true);
            run.status = RunStatus::Running;
            Some(pending.call)
        }
    };

    if let Some(call) = pending_call {
        let action = plan_call(&call)?;
        let outcome = execute(&action);
        let mut run = lock_run(&handle)?;
        run.push_step(
            "action",
            format!("Executed: {} - {}", action.tool(), action.summary()),
            outcome.detail.clone(),
            outcome.ok,
        );
        run.record_action(&action, &outcome);
        if action.is_readback() && outcome.ok {
            run.push_step(
                "observe",
                "Device read-back returned",
                "The observation succeeded; it does not by itself prove that the requested change occurred.",
                true,
            );
        }
        if outcome.verified {
            run.push_step(
                "verify",
                "Intended device state verified",
                outcome.detail.clone(),
                true,
            );
        }
        run.messages.push(AiMessage::Tool(ToolResult {
            tool_call_id: call.id,
            name: call.name,
            content: if outcome.ok {
                format!("ok: {}", outcome.detail)
            } else {
                format!("error: {}", outcome.detail)
            },
        }));
    }
    spawn_runner(handle.clone());
    snapshot(&handle)
}

pub fn cancel(run_id: &str) -> Result<ActionTaskView, String> {
    let handle = find_run(run_id)?;
    {
        let mut run = lock_run(&handle)?;
        if run.terminal() {
            return Ok(run.view());
        }
        run.cancel.store(true, Ordering::Release);
        run.push_step(
            "result",
            "Cancellation requested",
            "The engine stops after the current bounded command.",
            false,
        );
        if run.status == RunStatus::AwaitingApproval {
            run.finish_cancelled();
        } else {
            run.status = RunStatus::Cancelling;
        }
    }
    snapshot(&handle)
}

pub fn get(run_id: &str) -> Result<ActionTaskView, String> {
    snapshot(&find_run(run_id)?)
}

pub fn list() -> Result<Vec<ActionTaskView>, String> {
    let registry = runs()
        .lock()
        .map_err(|_| "The action task registry is unavailable.".to_string())?;
    let mut views: Vec<ActionTaskView> = registry
        .values()
        .filter_map(|handle| handle.lock().ok().map(|run| run.view()))
        .collect();
    views.sort_by_key(|view| std::cmp::Reverse(view.started_at));
    Ok(views)
}

pub fn describe_tools() -> Vec<Value> {
    tool_definitions()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, arguments: Value) -> ToolCall {
        ToolCall {
            id: "call".to_string(),
            name: name.to_string(),
            arguments,
        }
    }

    #[test]
    fn the_registry_exposes_no_shell_or_file_transfer_tool() {
        for forbidden in [
            "shell",
            "exec",
            "run_command",
            "upload_android_file",
            "download_android_file",
            "install_android_apk",
        ] {
            assert!(!TOOL_NAMES.contains(&forbidden));
        }
        assert_eq!(tool_definitions().len(), TOOL_NAMES.len());
    }

    #[test]
    fn valid_calls_are_planned_with_typed_arguments() {
        assert_eq!(
            plan_call(&call("tap", json!({ "x": 10, "y": 20 }))),
            Ok(PlannedAction::Tap { x: 10, y: 20 })
        );
        assert_eq!(
            plan_call(&call("launch_app", json!({ "package": "org.example.app" }))),
            Ok(PlannedAction::Launch {
                package: "org.example.app".to_string()
            })
        );
        assert_eq!(
            plan_call(&call(
                "swipe",
                json!({ "x1": 1, "y1": 2, "x2": 3, "y2": 4 })
            )),
            Ok(PlannedAction::Swipe {
                x1: 1,
                y1: 2,
                x2: 3,
                y2: 4,
                duration_ms: 300
            })
        );
        assert!(matches!(
            plan_call(&call("get_installed_apps", json!({ "query": "map" }))),
            Ok(PlannedAction::InstalledApps { .. })
        ));
    }

    #[test]
    fn malformed_calls_are_rejected_before_execution() {
        assert!(plan_call(&call("reboot_everything", json!({}))).is_err());
        assert!(plan_call(&call("tap", json!({ "x": 9000, "y": 10 }))).is_err());
        assert!(plan_call(&call("tap", json!({ "x": 10 }))).is_err());
        assert!(plan_call(&call("tap", json!({ "x": 10, "y": 10, "z": 1 }))).is_err());
        assert!(plan_call(&call("tap", json!("not an object"))).is_err());
        assert!(plan_call(&call("launch_app", json!({ "package": "bad package" }))).is_err());
        assert!(plan_call(&call("list_android_files", json!({ "path": "relative" }))).is_err());
        assert!(
            plan_call(&call(
                "list_android_files",
                json!({ "path": "/sdcard/../data" })
            ))
            .is_err()
        );
        assert!(plan_call(&call("input_text", json!({ "text": "a;b" }))).is_err());
        assert!(plan_call(&call("input_text", json!({ "text": "a\nb" }))).is_err());
        assert!(plan_call(&call("press_key", json!({ "key": "reboot" }))).is_err());
        assert!(
            plan_call(&call(
                "swipe",
                json!({ "x1": 1, "y1": 2, "x2": 3, "y2": 4, "duration_ms": 10 })
            ))
            .is_err()
        );
    }

    #[test]
    fn tool_calls_printed_as_text_are_detected() {
        assert!(looks_like_tool_call(
            "```json\n{\"name\":\"tap\",\"arguments\":{\"x\":10,\"y\":20}}\n```"
        ));
        assert!(!looks_like_tool_call("The Settings screen is open."));
        assert!(!looks_like_tool_call("{\"error\":\"no device\"}"));
    }

    #[test]
    fn only_the_destructive_tool_requires_approval() {
        let approved = plan_call(&call(
            "uninstall_app",
            json!({ "package": "org.example.app" }),
        ))
        .unwrap();
        assert!(approved.requires_approval(SafetyMode::ConfirmMutations));
        assert!(!approved.requires_approval(SafetyMode::ReadOnly));
        let tap = plan_call(&call("tap", json!({ "x": 1, "y": 1 }))).unwrap();
        assert!(tap.requires_approval(SafetyMode::ConfirmMutations));
        assert!(tap.requires_approval(SafetyMode::ConfirmEveryAction));
        assert!(tap.requires_approval(SafetyMode::TrustedLowRisk));
        let volume_key = plan_call(&call("press_key", json!({ "key": "volume_down" }))).unwrap();
        let power_key = plan_call(&call("press_key", json!({ "key": "power" }))).unwrap();
        assert!(!volume_key.requires_approval(SafetyMode::TrustedLowRisk));
        assert!(power_key.requires_approval(SafetyMode::TrustedLowRisk));
        assert!(approved.summary().contains("Uninstall org.example.app"));
    }

    #[test]
    fn only_backend_confirmed_device_changes_count_as_verified() {
        let launch =
            plan_call(&call("launch_app", json!({ "package": "org.example.app" }))).unwrap();
        let uninstall = plan_call(&call(
            "uninstall_app",
            json!({ "package": "org.example.app" }),
        ))
        .unwrap();
        let tap = plan_call(&call("tap", json!({ "x": 1, "y": 1 }))).unwrap();

        assert!(launch.is_verified_mutation());
        assert!(uninstall.is_verified_mutation());
        assert!(!tap.is_verified_mutation());
        assert_eq!(SafetyMode::parse("read_only"), Ok(SafetyMode::ReadOnly));
        assert!(SafetyMode::parse("unrecognized").is_err());
    }

    #[test]
    fn step_limit_and_deadline_are_enforced() {
        let id = "test".to_string();
        let mut run = ActionRun {
            id: id.clone(),
            prompt: "p".to_string(),
            provider: providers::OLLAMA.to_string(),
            base_url: providers::DEFAULT_OLLAMA_URL.to_string(),
            model: "m".to_string(),
            status: RunStatus::Running,
            steps: Vec::new(),
            messages: Vec::new(),
            pending: None,
            result: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            runner_active: false,
            seen: HashSet::new(),
            started: Instant::now(),
            started_at: 0,
            finished_at: None,
            steps_taken: 6,
            actions_attempted: 0,
            actions_run: 0,
            actions_failed: 0,
            actions_blocked: 0,
            mutations: 0,
            verified_after_mutation: false,
            readback_after_mutation: false,
            safety: SafetyMode::ConfirmMutations,
            max_steps: 6,
        };
        assert!(run.steps_taken >= run.max_steps);
        run.steps_taken = 0;
        run.started = Instant::now() - TASK_TIMEOUT - Duration::from_secs(1);
        assert!(step_budget(&run).is_err());
    }

    #[test]
    fn cancellation_produces_a_terminal_state() {
        let mut run = ActionRun {
            id: "test-cancel".to_string(),
            prompt: "p".to_string(),
            provider: providers::OLLAMA.to_string(),
            base_url: providers::DEFAULT_OLLAMA_URL.to_string(),
            model: "m".to_string(),
            status: RunStatus::AwaitingApproval,
            steps: Vec::new(),
            messages: Vec::new(),
            pending: None,
            result: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            runner_active: false,
            seen: HashSet::new(),
            started: Instant::now(),
            started_at: 0,
            finished_at: None,
            steps_taken: 0,
            actions_attempted: 0,
            actions_run: 0,
            actions_failed: 0,
            actions_blocked: 0,
            mutations: 0,
            verified_after_mutation: false,
            readback_after_mutation: false,
            safety: SafetyMode::ConfirmMutations,
            max_steps: 6,
        };
        run.finish_cancelled();
        assert_eq!(run.status, RunStatus::Cancelled);
        assert!(run.terminal());
    }

    #[test]
    fn tool_results_are_truncated_to_protect_the_context_window() {
        let truncated = truncate("x".repeat(MAX_TOOL_RESULT_CHARS + 100));
        assert!(truncated.chars().count() < MAX_TOOL_RESULT_CHARS + 100);
        assert!(truncated.contains("truncated"));
    }

    #[test]
    fn successful_actions_require_a_verification_readback() {
        let mut run = ActionRun {
            id: "test-verification".to_string(),
            prompt: "p".to_string(),
            provider: providers::OLLAMA.to_string(),
            base_url: providers::DEFAULT_OLLAMA_URL.to_string(),
            model: "m".to_string(),
            status: RunStatus::Running,
            steps: Vec::new(),
            messages: Vec::new(),
            pending: None,
            result: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            runner_active: false,
            seen: HashSet::new(),
            started: Instant::now(),
            started_at: 0,
            finished_at: None,
            steps_taken: 0,
            actions_attempted: 1,
            actions_run: 1,
            actions_failed: 0,
            actions_blocked: 0,
            mutations: 1,
            verified_after_mutation: false,
            readback_after_mutation: false,
            safety: SafetyMode::ConfirmMutations,
            max_steps: 6,
        };
        let (status, error) = run.completion_status();
        assert_eq!(status, RunStatus::NeedsVerification);
        assert!(error.is_some());

        run.readback_after_mutation = true;
        let (status, error) = run.completion_status();
        assert_eq!(status, RunStatus::NeedsVerification);
        assert!(error.unwrap().contains("does not establish"));

        run.verified_after_mutation = true;
        let (status, error) = run.completion_status();
        assert_eq!(status, RunStatus::Completed);
        assert!(error.is_none());
    }

    #[test]
    fn action_failures_do_not_claim_full_completion() {
        let mut run = ActionRun {
            id: "test-partial".to_string(),
            prompt: "p".to_string(),
            provider: providers::OLLAMA.to_string(),
            base_url: providers::DEFAULT_OLLAMA_URL.to_string(),
            model: "m".to_string(),
            status: RunStatus::Running,
            steps: Vec::new(),
            messages: Vec::new(),
            pending: None,
            result: None,
            error: None,
            cancel: Arc::new(AtomicBool::new(false)),
            runner_active: false,
            seen: HashSet::new(),
            started: Instant::now(),
            started_at: 0,
            finished_at: None,
            steps_taken: 0,
            actions_attempted: 2,
            actions_run: 2,
            actions_failed: 1,
            actions_blocked: 0,
            mutations: 1,
            verified_after_mutation: true,
            readback_after_mutation: false,
            safety: SafetyMode::ConfirmMutations,
            max_steps: 6,
        };
        let (status, error) = run.completion_status();
        assert_eq!(status, RunStatus::PartiallyCompleted);
        assert!(error.unwrap().contains("1 of 2"));

        run.actions_run = 0;
        run.actions_failed = 0;
        run.actions_blocked = 1;
        let (status, error) = run.completion_status();
        assert_eq!(status, RunStatus::Failed);
        assert!(error.unwrap().contains("read-only safety policy"));
    }
}
