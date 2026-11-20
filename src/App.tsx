import {
  useEffect,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type DashboardStatus = {
  installed: boolean;
  service_running: boolean;
  version: string;
  session_running: boolean;
  container_status: string;
  suspend_action: string;
  suspension_active: boolean;
  health: string;
  adb_connected: boolean;
  adb_shell_healthy: boolean;
  android_package_manager_healthy: boolean;
  screenshot_healthy: boolean;
  selected_device: DeviceSelection | null;
  selected_device_state: string;
  device_name: string;
  android_version: string;
  resolution: string;
  cpu_usage: string;
  memory_usage: string;
  storage_usage: string;
  ip_address: string;
  session_user: string;
  wayland_display: string;
  vendor_type: string;
  last_operation: string;
  power_progress: string;
  probe_errors: string[];
};

type DeviceSelection = {
  serial: string;
  product: string;
  model: string;
  device_type: string;
  state: string;
  is_waydroid: boolean;
};

type AppEntry = {
  name: string | null;
  package: string;
  category: string;
  source: string;
  version_name?: string | null;
  version_code?: string | null;
  install_path?: string | null;
  user_app?: boolean;
  system_app?: boolean;
  is_installed?: boolean;
  launcher_activity?: string | null;
};

type AppInfo = {
  name: string | null;
  package: string;
  category: string;
  source: string;
  version_name: string | null;
  version_code: string | null;
  install_path: string | null;
  is_user_app: boolean;
  is_system_app: boolean;
  launcher_activity: string | null;
  launch_action: string;
  install_available: boolean;
};

type ApkFileInfo = {
  path: string;
  filename: string;
  size_bytes: number;
  package_name: string;
};

type AndroidFileEntry = {
  name: string;
  is_dir: boolean;
  is_link: boolean;
  size_bytes: number;
  modified: string;
};

type DisplayInfo = {
  size: string;
  density: string;
};

type WorkflowStep = {
  name: string;
  ok: boolean;
  detail: string;
};

type WorkflowReport = {
  id: string;
  title: string;
  success: boolean;
  steps: WorkflowStep[];
};

type ActivityLogEntry = {
  timestamp: number;
  level: string;
  action: string;
  detail: string;
};

type ChatMessage = {
  role: "user" | "assistant" | "system";
  content: string;
};

type AiSettings = {
  version: number;
  theme: "dark" | "light";
  chat_provider: string;
  chat_model: string;
  action_provider: string;
  action_model: string;
  action_safety: string;
  openai_base_url: string;
  ollama_base_url: string;
  auto_refresh: boolean;
  last_android_directory: string;
};

type SettingsLoad = {
  settings: AiSettings;
  warning: string | null;
};

type ModelMetadata = {
  name: string;
  size_bytes: number | null;
  modified_at: string | null;
};

type ModelCapabilities = {
  provider: string;
  model: string;
  tools: boolean | null;
  vision: boolean | null;
  family: string | null;
  parameter_size: string | null;
  note: string;
};

type ChatReply = {
  provider: string;
  model: string;
  content: string;
};

type ActionStep = {
  index: number;
  kind: string;
  title: string;
  detail: string;
  ok: boolean;
};

type PendingApproval = {
  id: string;
  tool: string;
  summary: string;
  arguments: unknown;
};

type ActionTask = {
  id: string;
  prompt: string;
  provider: string;
  base_url: string;
  model: string;
  status: string;
  steps: ActionStep[];
  pending_approval: PendingApproval | null;
  result: string | null;
  error: string | null;
  started_at: number;
  finished_at: number | null;
  max_steps: number;
  safety: string;
  actions_attempted: number;
  actions_run: number;
  actions_failed: number;
  actions_blocked: number;
  verification: string;
};

type ProviderHealth = {
  provider: string;
  base_url: string;
  reachable: boolean;
  credential_configured: boolean | null;
  detail: string;
  models: ModelMetadata[];
  last_successful_verification: number | null;
};

type ToolDescription = {
  type: string;
  function: {
    name: string;
    description: string;
    parameters: unknown;
  };
};

const terminalTaskStatuses = new Set([
  "completed",
  "needs_verification",
  "partially_completed",
  "failed",
  "cancelled",
]);

function defaultSettings(): AiSettings {
  return {
    version: 1,
    theme: "dark",
    chat_provider: "ollama",
    chat_model: "",
    action_provider: "ollama",
    action_model: "",
    action_safety: "confirm_mutations",
    openai_base_url: "https://api.openai.com/v1",
    ollama_base_url: "http://127.0.0.1:11434",
    auto_refresh: false,
    last_android_directory: "/sdcard",
  };
}

type NavKey =
  | "dashboard"
  | "live"
  | "applications"
  | "files"
  | "automation"
  | "assistant"
  | "settings"
  | "logs";

const navItems: Array<{ key: NavKey; label: string; icon: string }> = [
  { key: "dashboard", label: "Dashboard", icon: "◉" },
  { key: "live", label: "Live Android", icon: "▣" },
  { key: "applications", label: "Applications", icon: "▤" },
  { key: "files", label: "File Manager", icon: "▦" },
  { key: "automation", label: "Automation", icon: "◈" },
  { key: "assistant", label: "AI Assistant", icon: "✦" },
  { key: "settings", label: "Device Settings", icon: "⚙" },
  { key: "logs", label: "Activity Logs", icon: "▤" },
];

const emptyStatus: DashboardStatus = {
  installed: false,
  service_running: false,
  version: "Unavailable",
  session_running: false,
  container_status: "Unknown",
  suspend_action: "Unknown",
  suspension_active: false,
  health: "Unknown",
  adb_connected: false,
  adb_shell_healthy: false,
  android_package_manager_healthy: false,
  screenshot_healthy: false,
  selected_device: null,
  selected_device_state: "No selection",
  device_name: "Unavailable",
  android_version: "Unavailable",
  resolution: "Unavailable",
  cpu_usage: "Unavailable",
  memory_usage: "Unavailable",
  storage_usage: "Unavailable",
  ip_address: "Unavailable",
  session_user: "Unavailable",
  wayland_display: "Unavailable",
  vendor_type: "Unavailable",
  last_operation: "Awaiting system query",
  power_progress: "Idle",
  probe_errors: [],
};

const androidKeys: Array<{ key: string; label: string }> = [
  { key: "back", label: "Back" },
  { key: "home", label: "Home" },
  { key: "recents", label: "Recents" },
  { key: "enter", label: "Enter" },
  { key: "volume_down", label: "Vol −" },
  { key: "volume_up", label: "Vol +" },
  { key: "power", label: "Power" },
];

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KiB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MiB`;
}

function parseDimension(value: string): number | null {
  const trimmed = value.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  return Number(trimmed);
}

function App() {
  const [theme, setTheme] = useState<"dark" | "light">("dark");
  const [activeNav, setActiveNav] = useState<NavKey>("dashboard");
  const [status, setStatus] = useState<DashboardStatus | null>(null);
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [appsLoading, setAppsLoading] = useState(false);
  const [appsError, setAppsError] = useState("");
  const [appInfo, setAppInfo] = useState<AppInfo | null>(null);
  const [selectedAppPackage, setSelectedAppPackage] = useState("");
  const [appInfoLoading, setAppInfoLoading] = useState(false);
  const [appInfoError, setAppInfoError] = useState("");
  const [apkFile, setApkFile] = useState<ApkFileInfo | null>(null);
  const [appBusy, setAppBusy] = useState(false);
  const [devices, setDevices] = useState<DeviceSelection[]>([]);
  const [deviceChoice, setDeviceChoice] = useState("");
  const [query, setQuery] = useState("");
  const [sortMode, setSortMode] = useState<"name" | "category">("name");
  const [showUserApps, setShowUserApps] = useState(true);
  const [showSystemApps, setShowSystemApps] = useState(true);
  const [actionMessage, setActionMessage] = useState("Monitoring device state...");
  const [busy, setBusy] = useState(false);
  const [powerBusy, setPowerBusy] = useState(false);
  const [powerProgress, setPowerProgress] = useState("Idle");
  const [screenshot, setScreenshot] = useState("");
  const [liveError, setLiveError] = useState("");
  const [liveAction, setLiveAction] = useState("");
  const [autoRefresh, setAutoRefresh] = useState(false);
  const [filePath, setFilePath] = useState("/sdcard");
  const [fileEntries, setFileEntries] = useState<AndroidFileEntry[]>([]);
  const [filesLoading, setFilesLoading] = useState(false);
  const [filesError, setFilesError] = useState("");
  const [fileBusy, setFileBusy] = useState(false);
  const [displayInfo, setDisplayInfo] = useState<DisplayInfo | null>(null);
  const [displayWidth, setDisplayWidth] = useState("");
  const [displayHeight, setDisplayHeight] = useState("");
  const [displayDensity, setDisplayDensity] = useState("");
  const [displayBusy, setDisplayBusy] = useState(false);
  const [workflowReport, setWorkflowReport] = useState<WorkflowReport | null>(null);
  const [workflowBusy, setWorkflowBusy] = useState(false);
  const [workflowPackage, setWorkflowPackage] = useState("");
  const [logEntries, setLogEntries] = useState<ActivityLogEntry[]>([]);
  const [logLoading, setLogLoading] = useState(false);
  const [models, setModels] = useState<string[]>([]);
  const [actionModels, setActionModels] = useState<string[]>([]);
  const [chat, setChat] = useState<ChatMessage[]>([]);
  const [chatInput, setChatInput] = useState("");
  const [chatBusy, setChatBusy] = useState(false);
  const [chatError, setChatError] = useState("");
  const [settings, setSettings] = useState<AiSettings>(defaultSettings);
  const [settingsError, setSettingsError] = useState("");
  const [settingsSaved, setSettingsSaved] = useState("");
  const [credentialInput, setCredentialInput] = useState("");
  const [credentialStatus, setCredentialStatus] = useState("");
  const [capabilityNote, setCapabilityNote] = useState("");
  const [actionPrompt, setActionPrompt] = useState("");
  const [actionTask, setActionTask] = useState<ActionTask | null>(null);
  const [actionHistory, setActionHistory] = useState<ActionTask[]>([]);
  const [actionTools, setActionTools] = useState<ToolDescription[]>([]);
  const [actionError, setActionError] = useState("");
  const [actionBusy, setActionBusy] = useState(false);
  const [chatHealth, setChatHealth] = useState<ProviderHealth | null>(null);
  const [actionHealth, setActionHealth] = useState<ProviderHealth | null>(null);
  const restartDialog = useRef<HTMLDialogElement>(null);
  const stopDialog = useRef<HTMLDialogElement>(null);
  const installDialog = useRef<HTMLDialogElement>(null);
  const uninstallDialog = useRef<HTMLDialogElement>(null);
  const [uninstallTarget, setUninstallTarget] = useState<AppInfo | null>(null);
  const dragStart = useRef<{ x: number; y: number } | null>(null);
  const currentStatus = status ?? emptyStatus;

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
  }, [theme]);

  async function refreshStatus(updateMessage = true) {
    setBusy(true);
    try {
      const data = await invoke<DashboardStatus>("get_dashboard_status");
      setStatus(data);
      setPowerProgress(data.power_progress);
      if (updateMessage) {
        setActionMessage(data.last_operation || "Status refreshed");
      }
    } catch (error) {
      // Keep the last known status: replacing it with defaults would show
      // "Waydroid not installed" for a transient query failure.
      setStatus((previous) => previous);
      setActionMessage(`Status error: ${String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  async function refreshDevices() {
    try {
      const found = await invoke<DeviceSelection[]>("list_connected_devices");
      setDevices(found);
      const selected = await invoke<DeviceSelection>("get_selected_android_device").catch(() => null);
      setDeviceChoice(selected?.serial ?? "");
    } catch (error) {
      setDevices([]);
      setActionMessage(`Device discovery failed: ${String(error)}`);
    }
  }

  async function connectAdb() {
    setBusy(true);
    try {
      setActionMessage(await invoke<string>("connect_waydroid_adb"));
      await refreshDevices();
      await refreshStatus(false);
    } catch (error) {
      setActionMessage(`ADB connection failed: ${String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  async function selectDevice(serial = deviceChoice) {
    if (!serial) {
      setActionMessage("Choose a connected ADB device first.");
      return;
    }
    setDeviceChoice(serial);
    setBusy(true);
    try {
      const selected = await invoke<DeviceSelection>("select_android_device", { serial });
      setActionMessage(`Selected ${selected.model || selected.serial}.`);
      await refreshStatus(false);
    } catch (error) {
      setActionMessage(`Device selection failed: ${String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  async function loadApps() {
    setAppsLoading(true);
    setAppsError("");
    try {
      const data = await invoke<AppEntry[]>("list_android_apps");
      setApps(data);
    } catch (error) {
      setApps([]);
      setAppsError(String(error));
    } finally {
      setAppsLoading(false);
    }
  }

  async function showAppInfo(packageName: string) {
    setSelectedAppPackage(packageName);
    setAppInfo(null);
    setAppInfoError("");
    setAppInfoLoading(true);
    try {
      setAppInfo(await invoke<AppInfo>("get_android_app_info", { packageName }));
    } catch (error) {
      setAppInfoError(String(error));
    } finally {
      setAppInfoLoading(false);
    }
  }

  async function chooseApk() {
    setAppBusy(true);
    try {
      const path = await invoke<string | null>("choose_android_apk");
      if (!path) return;
      const details = await invoke<ApkFileInfo>("inspect_android_apk", { apkPath: path });
      setApkFile(details);
      installDialog.current?.showModal();
    } catch (error) {
      setActionMessage(`APK selection failed: ${String(error)}`);
    } finally {
      setAppBusy(false);
    }
  }

  async function installSelectedApk() {
    if (!apkFile) {
      setActionMessage("Choose a valid APK first.");
      return;
    }
    setAppBusy(true);
    try {
      const result = await invoke<string>("install_android_apk", {
        apkPath: apkFile.path,
        approved: true,
      });
      installDialog.current?.close();
      setActionMessage(result);
      await loadApps();
    } catch (error) {
      setActionMessage(`APK installation failed: ${String(error)}`);
    } finally {
      setAppBusy(false);
    }
  }

  async function uninstallSelectedApp() {
    if (!uninstallTarget) return;
    setAppBusy(true);
    try {
      const result = await invoke<string>("uninstall_android_app", {
        packageName: uninstallTarget.package,
        approved: true,
      });
      uninstallDialog.current?.close();
      setUninstallTarget(null);
      setActionMessage(result);
      setAppInfo(null);
      await loadApps();
    } catch (error) {
      setActionMessage(`Uninstall failed: ${String(error)}`);
    } finally {
      setAppBusy(false);
    }
  }

  async function runAction(actionName: string, payload?: string) {
    setBusy(true);
    try {
      const result = await invoke<string>(actionName, payload ? { packageName: payload } : undefined);
      setActionMessage(result);
      await refreshStatus(false);
      if (activeNav === "applications") {
        await loadApps();
      }
    } catch (error) {
      setActionMessage(`Action failed: ${String(error)}`);
    } finally {
      setBusy(false);
    }
  }

  async function runPowerAction(actionName: "wake_android" | "suspend_android") {
    setPowerBusy(true);
    setBusy(true);
    setPowerProgress(actionName === "wake_android" ? "Starting Android wake…" : "Starting Android suspend…");
    const poll = window.setInterval(() => {
      void invoke<string>("get_power_progress").then(setPowerProgress).catch(() => {});
    }, 500);
    try {
      const result = await invoke<string>(actionName);
      setActionMessage(result);
    } catch (error) {
      setActionMessage(`Android power operation failed: ${String(error)}`);
    } finally {
      window.clearInterval(poll);
      setPowerBusy(false);
      setBusy(false);
      await refreshStatus(false);
    }
  }

  async function cancelPowerAction() {
    try {
      setActionMessage(await invoke<string>("cancel_android_operation"));
    } catch (error) {
      setActionMessage(`Cancellation failed: ${String(error)}`);
    }
  }

  function handleStopDialogClose() {
    const dialog = stopDialog.current;
    if (!dialog) return;
    const approved = dialog.returnValue === "confirm";
    dialog.returnValue = "";
    if (approved) {
      void (async () => {
        setBusy(true);
        try {
          setActionMessage(await invoke<string>("stop_waydroid", { approved: true }));
        } catch (error) {
          setActionMessage(`Stop failed: ${String(error)}`);
        } finally {
          setBusy(false);
          await refreshStatus(false);
        }
      })();
    }
  }

  function handleRestartDialogClose() {
    const dialog = restartDialog.current;
    if (!dialog) return;
    const approved = dialog.returnValue === "confirm";
    dialog.returnValue = "";
    if (approved) {
      void (async () => {
        setBusy(true);
        try {
          setActionMessage(await invoke<string>("restart_waydroid", { approved: true }));
        } catch (error) {
          setActionMessage(`Restart failed: ${String(error)}`);
        } finally {
          setBusy(false);
          await refreshStatus(false);
        }
      })();
    }
  }

  async function captureFrame() {
    try {
      const frame = await invoke<string>("get_android_screenshot");
      setScreenshot(frame);
      setLiveError("");
    } catch (error) {
      setLiveError(String(error));
    }
  }

  function screenPoint(event: PointerEvent<HTMLImageElement>) {
    const image = event.currentTarget;
    const rect = image.getBoundingClientRect();
    const clamp = (value: number, maximum: number) =>
      Math.max(0, Math.min(Math.max(maximum - 1, 0), value));
    return {
      x: clamp(Math.round(((event.clientX - rect.left) / rect.width) * image.naturalWidth), image.naturalWidth),
      y: clamp(Math.round(((event.clientY - rect.top) / rect.height) * image.naturalHeight), image.naturalHeight),
    };
  }

  function handleScreenPointerDown(event: PointerEvent<HTMLImageElement>) {
    event.currentTarget.setPointerCapture(event.pointerId);
    dragStart.current = screenPoint(event);
  }

  async function handleScreenPointerUp(event: PointerEvent<HTMLImageElement>) {
    const start = dragStart.current;
    dragStart.current = null;
    if (!start) return;
    const end = screenPoint(event);
    const distance = Math.hypot(end.x - start.x, end.y - start.y);
    try {
      if (distance < 24) {
        setLiveAction(await invoke<string>("tap_android", { x: end.x, y: end.y }));
      } else {
        setLiveAction(
          await invoke<string>("swipe_android", {
            x1: start.x,
            y1: start.y,
            x2: end.x,
            y2: end.y,
            durationMs: 250,
          }),
        );
      }
    } catch (error) {
      setLiveError(String(error));
    }
  }

  async function pressKey(key: string) {
    try {
      setLiveAction(await invoke<string>("press_android_key", { key }));
    } catch (error) {
      setLiveError(String(error));
    }
  }

  async function loadDirectory(path: string) {
    if (!path.startsWith("/")) {
      setFilesError("Android paths must start with '/'.");
      return;
    }
    setFilesLoading(true);
    setFilesError("");
    try {
      const entries = await invoke<AndroidFileEntry[]>("read_android_directory", { path });
      setFileEntries(entries);
      setFilePath(path);
    } catch (error) {
      setFileEntries([]);
      setFilesError(String(error));
    } finally {
      setFilesLoading(false);
    }
  }

  function joinAndroidPath(base: string, name: string) {
    return `${base.replace(/\/+$/, "") || ""}/${name}`;
  }

  function parentAndroidPath(path: string) {
    const trimmed = path.replace(/\/+$/, "");
    const index = trimmed.lastIndexOf("/");
    return index <= 0 ? "/" : trimmed.slice(0, index);
  }

  async function downloadFile(name: string) {
    setFileBusy(true);
    try {
      const saved = await invoke<string | null>("download_android_file", {
        remotePath: joinAndroidPath(filePath, name),
      });
      if (saved) setActionMessage(`Saved to ${saved}.`);
    } catch (error) {
      setActionMessage(`Download failed: ${String(error)}`);
    } finally {
      setFileBusy(false);
    }
  }

  async function uploadFile() {
    setFileBusy(true);
    try {
      const local = await invoke<string | null>("choose_android_upload");
      if (!local) return;
      const result = await invoke<string>("upload_android_file", {
        localPath: local,
        remoteDir: filePath,
      });
      setActionMessage(result);
      await loadDirectory(filePath);
    } catch (error) {
      setActionMessage(`Upload failed: ${String(error)}`);
    } finally {
      setFileBusy(false);
    }
  }

  async function loadDisplay() {
    setDisplayBusy(true);
    try {
      const info = await invoke<DisplayInfo>("get_android_display");
      setDisplayInfo(info);
    } catch (error) {
      setDisplayInfo(null);
      setActionMessage(`Display query failed: ${String(error)}`);
    } finally {
      setDisplayBusy(false);
    }
  }

  async function applyDisplay() {
    const width = parseDimension(displayWidth);
    const height = parseDimension(displayHeight);
    const density = parseDimension(displayDensity);
    if ((width === null) !== (height === null)) {
      setActionMessage("Set both width and height, or leave both empty.");
      return;
    }
    if (width === null && height === null && density === null) {
      setActionMessage("Enter a size or density to apply.");
      return;
    }
    setDisplayBusy(true);
    try {
      setDisplayInfo(
        await invoke<DisplayInfo>("apply_android_display", {
          width,
          height,
          density,
        }),
      );
      setActionMessage("Android display override applied.");
      await refreshStatus(false);
    } catch (error) {
      setActionMessage(`Display change failed: ${String(error)}`);
    } finally {
      setDisplayBusy(false);
    }
  }

  async function resetDisplay() {
    setDisplayBusy(true);
    try {
      setDisplayInfo(await invoke<DisplayInfo>("reset_android_display"));
      setDisplayWidth("");
      setDisplayHeight("");
      setDisplayDensity("");
      setActionMessage("Android display overrides reset.");
      await refreshStatus(false);
    } catch (error) {
      setActionMessage(`Display reset failed: ${String(error)}`);
    } finally {
      setDisplayBusy(false);
    }
  }

  async function runWorkflow(id: "bring_up" | "launch" | "screenshot") {
    setWorkflowBusy(true);
    setWorkflowReport(null);
    setActionMessage(`Running workflow: ${id}…`);
    try {
      const report = await invoke<WorkflowReport>("run_android_workflow", {
        id,
        package: id === "launch" ? workflowPackage.trim() || null : null,
      });
      setWorkflowReport(report);
      setActionMessage(`${report.title}: ${report.success ? "succeeded" : "failed"}`);
      await refreshStatus(false);
      await refreshDevices();
      if (id === "bring_up" && report.success) {
        await loadDirectory(filePath);
      }
    } catch (error) {
      setActionMessage(`Workflow failed: ${String(error)}`);
    } finally {
      setWorkflowBusy(false);
    }
  }

  async function loadLog() {
    setLogLoading(true);
    try {
      setLogEntries(await invoke<ActivityLogEntry[]>("get_activity_log"));
    } catch (error) {
      setActionMessage(`Activity log failed: ${String(error)}`);
    } finally {
      setLogLoading(false);
    }
  }

  async function loadSettings() {
    try {
      const loaded = await invoke<SettingsLoad>("load_settings");
      setSettings(loaded.settings);
      setTheme(loaded.settings.theme);
      setAutoRefresh(loaded.settings.auto_refresh);
      setSettingsError(loaded.warning ?? "");
    } catch (error) {
      setSettingsError(`Settings could not be loaded: ${String(error)}`);
    }
  }

  async function persistSettings(next: AiSettings, message: string) {
    setSettingsSaved("");
    try {
      const saved = await invoke<AiSettings>("save_settings", { next });
      setSettings(saved);
      setSettingsError("");
      setSettingsSaved(message);
      setTheme(saved.theme);
      setAutoRefresh(saved.auto_refresh);
    } catch (error) {
      setSettingsError(String(error));
    }
  }

  function baseUrlFor(provider: string): string {
    return provider === "ollama" ? settings.ollama_base_url : settings.openai_base_url;
  }

  async function loadActionHistory() {
    try {
      setActionHistory(await invoke<ActionTask[]>("action_list"));
      setActionTools(await invoke<ToolDescription[]>("action_tools"));
    } catch (error) {
      setActionError(String(error));
    }
  }

  async function loadModels(role: "chat" | "action") {
    const provider = role === "chat" ? settings.chat_provider : settings.action_provider;
    const baseUrl = baseUrlFor(provider);
    setCapabilityNote("");
    try {
      const health = await invoke<ProviderHealth>("provider_health", {
        provider,
        baseUrl,
      });
      if (role === "chat") setChatHealth(health);
      else setActionHealth(health);
      const names = health.reachable ? health.models.map((model) => model.name) : [];
      if (role === "chat") {
        setModels(names);
        setChatError(health.reachable ? "" : health.detail);
        if (!names.includes(settings.chat_model)) {
          const next = { ...settings, chat_model: names[0] ?? "" };
          await persistSettings(next, "Chat model updated from the provider list.");
        }
      } else {
        setActionModels(names);
        setActionError(health.reachable ? "" : health.detail);
        if (!names.includes(settings.action_model)) {
          await persistSettings(
            { ...settings, action_model: names[0] ?? "" },
            "Action model updated from the provider list.",
          );
        }
      }
    } catch (error) {
      const detail = String(error);
      const unavailable: ProviderHealth = {
        provider,
        base_url: baseUrl,
        reachable: false,
        credential_configured: null,
        detail,
        models: [],
        last_successful_verification: null,
      };
      if (role === "chat") {
        setModels([]);
        setChatHealth(unavailable);
        setChatError(detail);
      } else {
        setActionModels([]);
        setActionHealth(unavailable);
        setActionError(detail);
      }
    }
  }

  async function probeModel(role: "chat" | "action", model: string) {
    const provider = role === "chat" ? settings.chat_provider : settings.action_provider;
    if (!model) return;
    setCapabilityNote("");
    try {
      const capabilities = await invoke<ModelCapabilities>("probe_provider_model", {
        provider,
        baseUrl: baseUrlFor(provider),
        model,
      });
      const format = (value: boolean | null) =>
        value === true ? "reported" : value === false ? "not supported" : "not reported";
      setCapabilityNote(
        `${provider} / ${model}: tool calling ${format(capabilities.tools)}, vision ${format(
          capabilities.vision,
        )}. ${capabilities.note}`,
      );
    } catch (error) {
      setCapabilityNote(String(error));
    }
  }

  async function saveCredential() {
    const secret = credentialInput.trim();
    if (!secret) {
      setCredentialStatus("Enter an API key first.");
      return;
    }
    try {
      await invoke<boolean>("set_provider_credential", {
        provider: "openai_compatible",
        secret,
      });
      setCredentialInput("");
      setCredentialStatus(
        "Key stored in this app session only. It is not written to disk and will be forgotten on exit.",
      );
    } catch (error) {
      setCredentialStatus(String(error));
    }
  }

  async function forgetCredential() {
    try {
      await invoke<boolean>("clear_provider_credential", { provider: "openai_compatible" });
      setCredentialStatus("Session key cleared.");
    } catch (error) {
      setCredentialStatus(String(error));
    }
  }

  async function sendChat() {
    const prompt = chatInput.trim();
    if (!prompt || !settings.chat_model) return;
    const next: ChatMessage[] = [...chat, { role: "user", content: prompt }];
    setChat(next);
    setChatInput("");
    setChatBusy(true);
    setChatError("");
    try {
      const reply = await invoke<ChatReply>("ask_ai_chat", {
        provider: settings.chat_provider,
        baseUrl: baseUrlFor(settings.chat_provider),
        model: settings.chat_model,
        messages: next,
      });
      setChat([...next, { role: "assistant", content: reply.content }]);
    } catch (error) {
      setChatError(String(error));
    } finally {
      setChatBusy(false);
    }
  }

  async function startAction() {
    const prompt = actionPrompt.trim();
    if (!prompt) return;
    if (!settings.action_model) {
      setActionError("Choose an Action model first.");
      return;
    }
    setActionBusy(true);
    setActionError("");
    try {
      const task = await invoke<ActionTask>("action_start", {
        prompt,
        provider: settings.action_provider,
        baseUrl: baseUrlFor(settings.action_provider),
        model: settings.action_model,
        maxSteps: 6,
        safety: settings.action_safety,
      });
      setActionTask(task);
      await loadActionHistory();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setActionBusy(false);
    }
  }

  async function decideAction(approved: boolean) {
    const task = actionTask;
    if (!task?.pending_approval) return;
    setActionBusy(true);
    setActionError("");
    try {
      setActionTask(
        await invoke<ActionTask>("action_approve", {
          runId: task.id,
          approvalId: task.pending_approval.id,
          approved,
        }),
      );
      await loadActionHistory();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setActionBusy(false);
    }
  }

  async function cancelAction() {
    if (!actionTask) return;
    setActionBusy(true);
    try {
      setActionTask(await invoke<ActionTask>("action_cancel", { runId: actionTask.id }));
      await loadActionHistory();
    } catch (error) {
      setActionError(String(error));
    } finally {
      setActionBusy(false);
    }
  }

  function handleChatKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      void sendChat();
    }
  }

  useEffect(() => {
    void refreshStatus();
    void loadSettings();
  }, []);

  useEffect(() => {
    void refreshDevices();
  }, []);

  useEffect(() => {
    setAppInfo(null);
    setSelectedAppPackage("");
    setAppInfoError("");
    if (activeNav === "applications" && currentStatus.selected_device) {
      void loadApps();
    }
    // Loads are triggered by navigation and device changes only; the loader
    // functions are intentionally not dependencies.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNav, currentStatus.selected_device?.serial]);

  useEffect(() => {
    if (activeNav === "files" && currentStatus.selected_device) {
      void loadDirectory(filePath);
    }
    if (activeNav === "settings" && currentStatus.selected_device) {
      void loadDisplay();
    }
    if (activeNav === "logs") {
      void loadLog();
    }
    if (activeNav === "assistant") {
      void loadActionHistory();
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNav, currentStatus.selected_device?.serial]);

  useEffect(() => {
    if (activeNav === "assistant") void loadModels("chat");
    // Load only when Chat's provider selection changes or the assistant opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNav, settings.chat_provider]);

  useEffect(() => {
    if (activeNav === "assistant") void loadModels("action");
    // Load only when Action's provider selection changes or the assistant opens.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeNav, settings.action_provider]);

  useEffect(() => {
    const task = actionTask;
    if (!task || terminalTaskStatuses.has(task.status)) {
      return;
    }
    let cancelled = false;
    const timer = window.setInterval(() => {
      void invoke<ActionTask>("action_get", { runId: task.id })
        .then((next) => {
          if (!cancelled) {
            setActionTask(next);
            if (terminalTaskStatuses.has(next.status)) void loadActionHistory();
          }
        })
        .catch((error) => {
          if (!cancelled) setActionError(`Task progress could not be refreshed: ${String(error)}`);
        });
    }, 1500);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [actionTask?.id, actionTask?.status]);

  useEffect(() => {
    if (activeNav !== "live" || !currentStatus.selected_device) {
      return;
    }
    let cancelled = false;
    let inFlight = false;
    const tick = async () => {
      // Live frames are only useful while the window is visible; a hidden
      // webview would otherwise keep waking the device.
      if (document.hidden) return;
      if (inFlight) return;
      inFlight = true;
      try {
        const frame = await invoke<string>("get_android_screenshot");
        if (!cancelled) {
          setScreenshot(frame);
          setLiveError("");
        }
      } catch (error) {
        if (!cancelled) setLiveError(String(error));
      } finally {
        inFlight = false;
      }
    };
    void tick();
    if (!autoRefresh) {
      return () => {
        cancelled = true;
      };
    }
    const timer = window.setInterval(() => void tick(), 3000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [activeNav, currentStatus.selected_device?.serial, autoRefresh]);

  const filteredApps = [...apps]
    .filter((app) => `${app.name} ${app.package}`.toLowerCase().includes(query.toLowerCase()))
    .filter((app) => {
      if (app.system_app && showSystemApps) return true;
      if (app.user_app && showUserApps) return true;
      if (!app.system_app && !app.user_app) return true;
      return false;
    })
    .sort((a, b) => {
      if (sortMode === "category") {
        return `${a.category} ${a.name}`.localeCompare(`${b.category} ${b.name}`);
      }
      return (a.name ?? a.package).localeCompare(b.name ?? b.package);
    });

  const statusCards = [
    {
      label: "Health",
      value: currentStatus.health,
      tone: currentStatus.health === "Healthy" ? "good" : "warn",
    },
    {
      label: "Session",
      value: currentStatus.session_running ? "Running" : "Stopped",
      tone: currentStatus.session_running ? "good" : "warn",
    },
    {
      label: "Container",
      value: currentStatus.container_status,
      tone: currentStatus.container_status === "RUNNING" ? "good" : "warn",
    },
    {
      label: "Suspension",
      value: currentStatus.suspension_active ? "Active" : "Inactive",
      tone: currentStatus.suspension_active ? "neutral" : "good",
    },
    {
      label: "ADB",
      value: currentStatus.adb_connected ? "Connected" : "Disconnected",
      tone: currentStatus.adb_connected ? "good" : "warn",
    },
    {
      label: "Android shell",
      value: currentStatus.adb_shell_healthy ? "Responsive" : "Unresponsive",
      tone: currentStatus.adb_shell_healthy ? "good" : "warn",
    },
    {
      label: "Package manager",
      value: currentStatus.android_package_manager_healthy ? "Responsive" : "Unresponsive",
      tone: currentStatus.android_package_manager_healthy ? "good" : "warn",
    },
    {
      label: "Screen",
      value: currentStatus.screenshot_healthy ? "Available" : "Unavailable",
      tone: currentStatus.screenshot_healthy ? "good" : "warn",
    },
  ];

  const selectedWaydroid = currentStatus.selected_device?.is_waydroid === true;
  const adbTransportReady =
    currentStatus.adb_connected || devices.some((device) => device.state === "device");

  const renderDashboard = () => (
    <>
      <section className="hero panel">
        <div>
          <p className="eyebrow">System overview</p>
          <h2>
            {currentStatus.health === "Healthy"
              ? "Waydroid is online and responsive"
              : currentStatus.health === "Frozen"
                ? "Waydroid is suspended"
                : currentStatus.selected_device
                  ? "Waydroid is offline or paused"
                  : adbTransportReady
                    ? "Select the ADB target to start monitoring"
                    : "Waydroid is offline or paused"}
          </h2>
        </div>
        <div className="badge-row">
          <span className={`pill ${currentStatus.installed ? "good" : "bad"}`}>
            {currentStatus.installed ? "Waydroid installed" : "Waydroid missing"}
          </span>
          <span className={`pill ${adbTransportReady ? "good" : "bad"}`}>
            {currentStatus.adb_connected
              ? "ADB connected"
              : adbTransportReady
                ? "ADB ready — select a target"
                : "ADB unavailable"}
          </span>
        </div>
      </section>

      <section className="panel device-picker">
        <div>
          <h3>ADB device target</h3>
          <p>Choose and apply a device explicitly. Android operations never switch targets automatically.</p>
        </div>
        <label htmlFor="adb-device">Connected device</label>
        <div className="device-select-row">
          <select
            id="adb-device"
            value={deviceChoice}
            onChange={(event) => setDeviceChoice(event.target.value)}
            disabled={busy}
          >
            <option value="">Select a device</option>
            {devices.map((device) => (
              <option key={device.serial} value={device.serial} disabled={device.state !== "device"}>
                {device.model || device.serial} — {device.serial} · {device.product || "product unknown"}
                {device.is_waydroid ? " · Waydroid" : ""}
              </option>
            ))}
          </select>
          <button type="button" onClick={() => void selectDevice()} disabled={busy || !deviceChoice}>
            Apply target
          </button>
          <button type="button" className="ghost" onClick={() => void refreshDevices()} disabled={busy}>
            Refresh devices
          </button>
          <button type="button" className="ghost" onClick={() => void connectAdb()} disabled={busy}>
            Connect ADB
          </button>
        </div>
        <ul className="device-list" aria-label="Discovered ADB devices">
          {devices.map((device) => (
            <li key={device.serial}>
              <div>
                <strong>{device.model || "Model unknown"}</strong>
                <small>{device.serial}</small>
                <span>
                  Product: {device.product || "unknown"} · Device type: {device.device_type || "unknown"}
                  {" · "}{device.state} · {device.is_waydroid ? "Waydroid" : "Other Android device"}
                </span>
              </div>
              <button
                type="button"
                onClick={() => void selectDevice(device.serial)}
                disabled={busy || device.state !== "device"}
              >
                {currentStatus.selected_device?.serial === device.serial ? "Selected" : "Select"}
              </button>
            </li>
          ))}
          {devices.length === 0 && <li className="empty-devices">No ADB devices discovered.</li>}
        </ul>
        <p className="device-summary" role="status">
          Selected: {currentStatus.selected_device
            ? `${currentStatus.selected_device.model || currentStatus.selected_device.serial} (${currentStatus.selected_device.serial}) · product ${currentStatus.selected_device.product || "unknown"} · device type ${currentStatus.selected_device.device_type || "unknown"} · ${currentStatus.selected_device_state} · ${currentStatus.selected_device.is_waydroid ? "Waydroid" : "Other Android device"}`
            : "None"}
        </p>
      </section>

      <section className="action-row panel">
        <button
          type="button"
          className="primary"
          onClick={() => void runWorkflow("bring_up")}
          disabled={busy || workflowBusy}
        >
          {workflowBusy ? "Working…" : "Bring up Waydroid"}
        </button>
        <button type="button" className="primary" onClick={() => void runPowerAction("wake_android")} disabled={busy || !selectedWaydroid}>
          Wake Android
        </button>
        <button type="button" onClick={() => void runPowerAction("suspend_android")} disabled={busy || !selectedWaydroid}>
          Suspend Android
        </button>
        <button type="button" onClick={() => void runAction("start_waydroid")} disabled={busy}>
          Start session
        </button>
        <button type="button" onClick={() => stopDialog.current?.showModal()} disabled={busy}>
          Stop session
        </button>
        <button type="button" onClick={() => void runAction("open_android_ui")} disabled={busy || !selectedWaydroid}>
          Open Android
        </button>
        <button type="button" onClick={() => restartDialog.current?.showModal()} disabled={busy}>
          Restart session
        </button>
        <button type="button" className="ghost" onClick={() => { void refreshStatus(); void refreshDevices(); }} disabled={busy}>
          Refresh diagnostics
        </button>
        {powerBusy && <button type="button" className="ghost" onClick={() => void cancelPowerAction()}>Cancel operation</button>}
      </section>
      <section className="panel operation-progress" aria-live="polite" aria-atomic="true">
        <span>Android operation</span>
        {powerBusy && <progress aria-label="Android operation in progress" />}
        <strong>{powerProgress}</strong>
      </section>

      <section className="stats-grid">
        {statusCards.map((card) => (
          <article key={card.label} className="panel stats-card">
            <span className="card-label">{card.label}</span>
            <strong className={card.tone}>{card.value}</strong>
          </article>
        ))}
      </section>

      <section className="detail-grid">
        <article className="panel">
          <h3>Platform details</h3>
          <ul className="detail-list">
            <li><span>Health</span> <strong>{currentStatus.health}</strong></li>
            <li><span>Service</span> <strong>{currentStatus.service_running ? "Running" : "Stopped"}</strong></li>
            <li><span>Session</span> <strong>{currentStatus.session_running ? "Running" : "Stopped"}</strong></li>
            <li><span>Container</span> <strong>{currentStatus.container_status}</strong></li>
            <li><span>Suspend action</span> <strong>{currentStatus.suspend_action}</strong></li>
            <li><span>Suspension</span> <strong>{currentStatus.suspension_active ? "Active" : "Inactive"}</strong></li>
            <li><span>ADB</span> <strong>{currentStatus.adb_connected ? "Connected" : "Disconnected"}</strong></li>
            <li><span>Android shell</span> <strong>{currentStatus.adb_shell_healthy ? "Responsive" : "Unresponsive"}</strong></li>
            <li><span>Package manager</span> <strong>{currentStatus.android_package_manager_healthy ? "Responsive" : "Unresponsive"}</strong></li>
            <li><span>Screenshot</span> <strong>{currentStatus.screenshot_healthy ? "Available" : "Unavailable"}</strong></li>
            <li><span>Selected device</span> <strong>{currentStatus.selected_device?.serial ?? "None"}</strong></li>
            <li><span>Vendor</span> <strong>{currentStatus.vendor_type}</strong></li>
            <li><span>IP address</span> <strong>{currentStatus.ip_address}</strong></li>
            <li><span>Wayland</span> <strong>{currentStatus.wayland_display}</strong></li>
            <li><span>User</span> <strong>{currentStatus.session_user}</strong></li>
          </ul>
        </article>

        <article className="panel">
          <h3>Resource usage</h3>
          <ul className="detail-list">
            <li><span>CPU load (1m)</span> <strong>{currentStatus.cpu_usage}</strong></li>
            <li><span>Memory</span> <strong>{currentStatus.memory_usage}</strong></li>
            <li><span>Storage</span> <strong>{currentStatus.storage_usage}</strong></li>
            <li><span>Resolution</span> <strong>{currentStatus.resolution}</strong></li>
            <li><span>Android version</span> <strong>{currentStatus.android_version}</strong></li>
          </ul>
        </article>
      </section>
      {currentStatus.probe_errors.length > 0 && (
        <section className="panel diagnostic-errors" aria-labelledby="diagnostic-errors-title">
          <h3 id="diagnostic-errors-title">Probe details</h3>
          <ul>{currentStatus.probe_errors.map((error, index) => <li key={`${index}-${error}`}>{error}</li>)}</ul>
        </section>
      )}
    </>
  );

  const renderLive = () => (
    <section className="panel control-panel">
      <div className="panel-head">
        <div>
          <h2>Live Android</h2>
          <p>Frames come from a real screen capture of the selected device. Click the frame to tap, drag to swipe, or use the key buttons below.</p>
        </div>
        <div className="head-actions">
          <label className="inline-check">
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(event) => setAutoRefresh(event.target.checked)}
            />
            Auto-refresh
          </label>
          <button type="button" className="ghost" onClick={() => void captureFrame()} disabled={!currentStatus.selected_device}>
            Capture frame
          </button>
        </div>
      </div>

      {!currentStatus.selected_device ? (
        <div className="empty-state">Select an ADB device on the Dashboard to view and control its screen.</div>
      ) : (
        <div className="screen-stage">
          {screenshot ? (
            <img
              className="device-screen"
              src={`data:image/png;base64,${screenshot}`}
              alt="Live Android screen"
              draggable={false}
              onPointerDown={handleScreenPointerDown}
              onPointerUp={(event) => void handleScreenPointerUp(event)}
            />
          ) : (
            <div className="empty-state">
              {liveError ? "No frame available yet." : "Waiting for the first frame…"}
            </div>
          )}
        </div>
      )}

      <div className="key-row">
        {androidKeys.map((item) => (
          <button
            key={item.key}
            type="button"
            className="ghost"
            onClick={() => void pressKey(item.key)}
            disabled={!currentStatus.selected_device}
          >
            {item.label}
          </button>
        ))}
      </div>

      <div className="live-status" role="status" aria-live="polite">
        <span>Resolution</span>
        <strong>{currentStatus.resolution}</strong>
        <span>Last input</span>
        <strong>{liveAction || "None yet"}</strong>
      </div>

      {liveError && (
        <div className="empty-state inline-error" role="alert">
          <span>{liveError}</span>
          <button
            type="button"
            className="ghost"
            onClick={() => void runPowerAction("wake_android")}
            disabled={busy || !selectedWaydroid}
          >
            Wake Android and retry
          </button>
        </div>
      )}
    </section>
  );

  const renderFiles = () => (
    <section className="panel control-panel">
      <div className="panel-head">
        <div>
          <h2>File manager</h2>
          <p>Browse the Android filesystem over ADB. Uploads and downloads use a native file dialog and are verified afterwards.</p>
        </div>
      </div>

      <div className="toolbar-row">
        <input
          type="text"
          value={filePath}
          onChange={(event) => setFilePath(event.target.value)}
          placeholder="/sdcard"
          aria-label="Android directory path"
          spellCheck={false}
        />
        <button type="button" className="ghost" onClick={() => void loadDirectory(filePath)} disabled={!currentStatus.selected_device || filesLoading}>
          Open
        </button>
        <button
          type="button"
          className="ghost"
          onClick={() => void loadDirectory(parentAndroidPath(filePath))}
          disabled={!currentStatus.selected_device || filesLoading || filePath === "/"}
        >
          Up
        </button>
        <button type="button" className="primary" onClick={() => void uploadFile()} disabled={!currentStatus.selected_device || fileBusy}>
          {fileBusy ? "Working…" : "Upload file…"}
        </button>
      </div>

      {!currentStatus.selected_device && (
        <div className="empty-state">Select an ADB device on the Dashboard to browse files.</div>
      )}
      {filesError && <div className="empty-state" role="alert">{filesError}</div>}
      {filesLoading ? (
        <div className="empty-state" role="status">Reading {filePath}…</div>
      ) : (
        <div className="file-list">
          {fileEntries.map((entry) => (
            <div key={entry.name} className="file-row">
              <div className="app-meta">
                <span className="file-icon" aria-hidden="true">{entry.is_dir ? "▸" : "▤"}</span>
                <div>
                  <strong>{entry.name}</strong>
                  <small>
                    {entry.is_dir ? "Directory" : formatBytes(entry.size_bytes)}
                    {entry.modified ? ` · ${entry.modified}` : ""}
                    {entry.is_link ? " · symlink" : ""}
                  </small>
                </div>
              </div>
              <div className="app-actions">
                {entry.is_dir ? (
                  <button type="button" className="ghost" onClick={() => void loadDirectory(joinAndroidPath(filePath, entry.name))}>
                    Open
                  </button>
                ) : (
                  <button type="button" className="ghost" onClick={() => void downloadFile(entry.name)} disabled={fileBusy}>
                    Download
                  </button>
                )}
              </div>
            </div>
          ))}
          {!filesLoading && fileEntries.length === 0 && !filesError && currentStatus.selected_device && (
            <div className="empty-state">This directory is empty or not readable.</div>
          )}
        </div>
      )}
      <p className="hint">Paths may contain letters, digits, '/', '.', '_' and '-'. Names with spaces or other characters are rejected before reaching the device shell.</p>
    </section>
  );

  const renderAutomation = () => (
    <section className="panel control-panel">
      <div className="panel-head">
        <div>
          <h2>Automation center</h2>
          <p>Each workflow is a fixed, backend-validated sequence. Steps report their own outcome, so a partial run stays visible.</p>
        </div>
      </div>

      <div className="workflow-grid">
        <article className="mini-card">
          <h3>Bring up Waydroid</h3>
          <p>Start the session if needed, connect ADB, select the Waydroid device, and verify Android readiness.</p>
          <button type="button" className="primary" onClick={() => void runWorkflow("bring_up")} disabled={workflowBusy}>
            Run bring-up
          </button>
        </article>
        <article className="mini-card">
          <h3>Launch and verify</h3>
          <p>Ensure Android is awake, launch the package, then confirm it reached the foreground.</p>
          <input
            type="text"
            value={workflowPackage}
            onChange={(event) => setWorkflowPackage(event.target.value)}
            placeholder="org.example.app"
            aria-label="Package to launch"
            spellCheck={false}
          />
          <button
            type="button"
            className="primary"
            onClick={() => void runWorkflow("launch")}
            disabled={workflowBusy || !currentStatus.selected_device || !workflowPackage.trim()}
          >
            Launch package
          </button>
        </article>
        <article className="mini-card">
          <h3>Save a screenshot</h3>
          <p>Wake Android if it is frozen, capture a validated PNG, and save it to your Pictures folder.</p>
          <button
            type="button"
            className="primary"
            onClick={() => void runWorkflow("screenshot")}
            disabled={workflowBusy || !currentStatus.selected_device}
          >
            Capture to file
          </button>
        </article>
      </div>

      {workflowReport && (
        <section className="workflow-report" aria-live="polite">
          <h3>
            {workflowReport.title}
            <span className={`pill ${workflowReport.success ? "good" : "bad"}`}>
              {workflowReport.success ? "Succeeded" : "Failed"}
            </span>
          </h3>
          <ol className="step-list">
            {workflowReport.steps.map((step, index) => (
              <li key={`${index}-${step.name}`} className={step.ok ? "step ok" : "step error"}>
                <strong>{step.name}</strong>
                <span>{step.detail}</span>
              </li>
            ))}
          </ol>
        </section>
      )}
    </section>
  );

  const renderAssistant = () => (
    <section className="panel control-panel">
      <div className="panel-head">
        <div>
          <h2>AI providers</h2>
          <p>
            Chat answers questions only. Actions run device tools through a fixed allowlist and
            require approval for destructive steps. Credentials stay in memory for this session.
          </p>
        </div>
      </div>

      {settingsError && <div className="empty-state" role="alert">{settingsError}</div>}
      {settingsSaved && <div className="empty-state" role="status">{settingsSaved}</div>}

      <div className="detail-grid">
        <article className="mini-card">
          <h3>Chat model</h3>
          <div className="field-grid">
            <label>
              Provider
              <select
                value={settings.chat_provider}
                aria-label="Chat provider"
                onChange={(event) =>
                  void persistSettings(
                    { ...settings, chat_provider: event.target.value, chat_model: "" },
                    "Chat provider saved. Reload models to choose one.",
                  )
                }
              >
                <option value="ollama">Ollama (local)</option>
                <option value="openai_compatible">OpenAI-compatible</option>
              </select>
            </label>
            <label>
              Base URL
              <input
                value={baseUrlFor(settings.chat_provider)}
                onChange={(event) =>
                  setSettings({
                    ...settings,
                    ...(settings.chat_provider === "ollama"
                      ? { ollama_base_url: event.target.value }
                      : { openai_base_url: event.target.value }),
                  })
                }
                onBlur={() =>
                  void persistSettings(settings, "Chat base URL saved.").then(() =>
                    loadModels("chat"),
                  )
                }
                aria-label="Chat provider base URL"
              />
            </label>
            <label>
              Model
              <select
                value={settings.chat_model}
                aria-label="Chat model"
                onChange={(event) => {
                  void persistSettings(
                    { ...settings, chat_model: event.target.value },
                    "Chat model saved.",
                  );
                  void probeModel("chat", event.target.value);
                }}
              >
                <option value="">Choose a model</option>
                {models.map((model) => (
                  <option key={model} value={model}>{model}</option>
                ))}
              </select>
            </label>
          </div>
          <div className="toolbar-row">
            <button type="button" className="ghost" onClick={() => void loadModels("chat")}>
              Reload models
            </button>
          </div>

        </article>

        <article className="mini-card">
          <h3>Action model</h3>
          <div className="field-grid">
            <label>
              Provider
              <select
                value={settings.action_provider}
                aria-label="Action provider"
                onChange={(event) =>
                  void persistSettings(
                    { ...settings, action_provider: event.target.value, action_model: "" },
                    "Action provider saved. Reload models to choose one.",
                  )
                }
              >
                <option value="ollama">Ollama (local)</option>
                <option value="openai_compatible">OpenAI-compatible</option>
              </select>
            </label>
            <label>
              Base URL
              <input
                value={baseUrlFor(settings.action_provider)}
                onChange={(event) =>
                  setSettings({
                    ...settings,
                    ...(settings.action_provider === "ollama"
                      ? { ollama_base_url: event.target.value }
                      : { openai_base_url: event.target.value }),
                  })
                }
                onBlur={() =>
                  void persistSettings(settings, "Action base URL saved.").then(() =>
                    loadModels("action"),
                  )
                }
                aria-label="Action provider base URL"
              />
            </label>
            <label>
              Model
              <select
                value={settings.action_model}
                aria-label="Action model"
                onChange={(event) => {
                  void persistSettings(
                    { ...settings, action_model: event.target.value },
                    "Action model saved.",
                  );
                  void probeModel("action", event.target.value);
                }}
              >
                <option value="">Choose a model</option>
                {actionModels.map((model) => (
                  <option key={model} value={model}>{model}</option>
                ))}
              </select>
            </label>
            <label>
              Safety mode
              <select
                value={settings.action_safety}
                onChange={(event) =>
                  void persistSettings(
                    { ...settings, action_safety: event.target.value },
                    "Action safety mode saved.",
                  )
                }
              >
                <option value="read_only">Read-only inspection</option>
                <option value="confirm_mutations">Confirm before mutations</option>
                <option value="confirm_every_action">Confirm every action</option>
                <option value="trusted_low_risk">Trust volume keys only; confirm other changes</option>
              </select>
            </label>
          </div>
          <div className="toolbar-row">
            <button type="button" className="ghost" onClick={() => void loadModels("action")}>
              Reload models
            </button>
          </div>
        </article>
      </div>

      {(settings.chat_provider === "openai_compatible" ||
        settings.action_provider === "openai_compatible") && (
        <article className="mini-card">
          <h3>Shared OpenAI-compatible credential</h3>
          <div className="field-grid">
            <label>
              Session API key
              <input
                type="password"
                value={credentialInput}
                onChange={(event) => setCredentialInput(event.target.value)}
                autoComplete="off"
                placeholder="Not stored on disk"
              />
            </label>
            <div className="toolbar-row">
              <button type="button" onClick={() => void saveCredential()}>Use for this session</button>
              <button type="button" className="ghost" onClick={() => void forgetCredential()}>Forget key</button>
            </div>
            {credentialStatus && <p className="hint">{credentialStatus}</p>}
          </div>
        </article>
      )}

      {capabilityNote && <p className="hint" role="status">{capabilityNote}</p>}

      <div className="provider-health-grid" aria-label="Provider connection status">
        {(["chat", "action"] as const).map((role) => {
          const health = role === "chat" ? chatHealth : actionHealth;
          const provider = role === "chat" ? settings.chat_provider : settings.action_provider;
          const model = role === "chat" ? settings.chat_model : settings.action_model;
          return (
            <article className="mini-card provider-health" key={role}>
              <div className="provider-health-heading">
                <h3>{role === "chat" ? "Chat provider" : "Action provider"}</h3>
                <span className={`pill ${health?.reachable ? "good" : "bad"}`}>
                  {health ? (health.reachable ? "Connected" : "Unreachable") : "Not checked"}
                </span>
              </div>
              <p><strong>Provider:</strong> {provider}</p>
              <p><strong>Selected model:</strong> {model || "None"}</p>
              <p><strong>URL:</strong> {health?.base_url || baseUrlFor(provider)}</p>
              <p>
                <strong>Last successful verification:</strong>{" "}
                {health?.last_successful_verification
                  ? new Date(health.last_successful_verification * 1000).toLocaleString()
                  : "Never"}
              </p>
              {health && !health.reachable && (
                <p className="health-error" role="alert">{health.detail}</p>
              )}
            </article>
          );
        })}
      </div>

      <div className="panel-head">
        <div>
          <h2>Chat</h2>
          <p>Answers are generated by the selected model and must be verified before you act on them.</p>
        </div>
      </div>

      {chatError && <div className="empty-state" role="alert">{chatError}</div>}

      <div className="chat-log">
        {chat.length === 0 && !chatBusy ? (
          <div className="empty-state">
            Ask about Waydroid, ADB or Android behaviour. This conversation cannot change the device.
          </div>
        ) : (
          <>
            {chat.map((message, index) => (
              <div key={`${index}-${message.role}`} className={`chat-bubble ${message.role}`}>
                <strong>{message.role === "user" ? "You" : "Assistant"}</strong>
                <p>{message.content}</p>
              </div>
            ))}
            {chatBusy && (
              <div className="chat-bubble assistant">
                <strong>Assistant</strong>
                <p>Waiting for {settings.chat_model || "a model"}…</p>
              </div>
            )}
          </>
        )}
      </div>

      <div className="toolbar-row chat-input-row">
        <textarea
          value={chatInput}
          onChange={(event) => setChatInput(event.target.value)}
          onKeyDown={handleChatKeyDown}
          rows={3}
          placeholder="Ask the selected model… (Enter sends, Shift+Enter adds a line)"
          aria-label="Message to the chat model"
        />
        <button
          type="button"
          className="primary"
          onClick={() => void sendChat()}
          disabled={chatBusy || !settings.chat_model || !chatInput.trim()}
        >
          {chatBusy ? "Waiting…" : "Send"}
        </button>
        <button type="button" className="ghost" onClick={() => setChat([])} disabled={chatBusy || chat.length === 0}>
          Clear
        </button>
      </div>

      <div className="panel-head">
        <div>
          <h2>Action task</h2>
          <p>
            The model picks one tool per step from a fixed allowlist (no shell). Uninstalling an app
            always asks for your approval first.
          </p>
        </div>
      </div>

      {actionError && <div className="empty-state" role="alert">{actionError}</div>}

      <div className="toolbar-row chat-input-row">
        <textarea
          value={actionPrompt}
          onChange={(event) => setActionPrompt(event.target.value)}
          rows={2}
          placeholder="Describe a task, for example: open Settings and report the Android version"
          aria-label="Action task description"
        />
        <button
          type="button"
          className="primary"
          onClick={() => void startAction()}
          disabled={actionBusy || !actionPrompt.trim() || !settings.action_model}
        >
          {actionBusy ? "Starting…" : "Run task"}
        </button>
      </div>

      {actionTask && (
        <section className="workflow-report" aria-live="polite">
          <h3>
            {actionTask.prompt}
            <span className={`pill ${actionTask.status === "completed" ? "good" : actionTask.status === "failed" ? "bad" : "warn"}`}>
              {actionTask.status}
            </span>
          </h3>
          <p className="hint">
            {actionTask.provider} / {actionTask.model} · {actionTask.safety} · step budget {actionTask.max_steps}
          </p>
          <p className="hint">
            Actions attempted: {actionTask.actions_attempted} · executed: {actionTask.actions_run} · successful: {actionTask.actions_run - actionTask.actions_failed} · failed: {actionTask.actions_failed} · not executed: {actionTask.actions_blocked} · verification: {actionTask.verification}
          </p>
          <ol className="step-list">
            {actionTask.steps.map((step) => (
              <li key={step.index} className={step.ok ? "step ok" : "step error"}>
                <strong>{step.title}</strong>
                <span>{step.detail}</span>
              </li>
            ))}
          </ol>
          {actionTask.pending_approval && (
            <div className="toolbar-row" role="alert">
              <span>Approve: {actionTask.pending_approval.summary}</span>
              <button type="button" className="primary" onClick={() => void decideAction(true)} disabled={actionBusy}>
                Approve
              </button>
              <button type="button" onClick={() => void decideAction(false)} disabled={actionBusy}>
                Decline
              </button>
            </div>
          )}
          {!terminalTaskStatuses.has(actionTask.status) && (
            <div className="toolbar-row">
              <button type="button" className="ghost" onClick={() => void cancelAction()} disabled={actionBusy}>
                Cancel task
              </button>
            </div>
          )}
          {actionTask.error && <div className="empty-state" role="alert">{actionTask.error}</div>}
        </section>
      )}

      <div className="panel-head">
        <div>
          <h2>Action history</h2>
          <p>Recent tasks retained by this app session, including prompts and verification evidence.</p>
        </div>
        <button type="button" className="ghost" onClick={() => void loadActionHistory()}>
          Refresh
        </button>
      </div>

      {actionHistory.length === 0 ? (
        <div className="empty-state">No Action tasks yet.</div>
      ) : (
        <div className="action-history">
          {actionHistory.map((task) => (
            <article className="mini-card action-history-item" key={task.id}>
              <div className="provider-health-heading">
                <button
                  type="button"
                  className="history-title"
                  onClick={() => setActionTask(task)}
                >
                  {task.prompt}
                </button>
                <span className={`pill ${task.status === "completed" ? "good" : task.status === "failed" ? "bad" : "warn"}`}>
                  {task.status}
                </span>
              </div>
              <p className="hint">
                {task.provider} / {task.model} · {task.safety}
              </p>
              <p className="hint">
                Attempted {task.actions_attempted} · executed {task.actions_run} · successful {task.actions_run - task.actions_failed} · failed {task.actions_failed} · not executed {task.actions_blocked} · verification {task.verification}
              </p>
              {task.pending_approval && (
                <p role="status"><strong>Approval request:</strong> {task.pending_approval.summary}</p>
              )}
              {task.error && <p className="health-error" role="alert">{task.error}</p>}
              {task.result && <p>{task.result}</p>}
              <details>
                <summary>Execution details ({task.steps.length} steps)</summary>
                <ol className="step-list">
                  {task.steps.map((step) => (
                    <li key={step.index} className={step.ok ? "step ok" : "step error"}>
                      <strong>{step.title}</strong>
                      <span>{step.detail}</span>
                    </li>
                  ))}
                </ol>
              </details>
            </article>
          ))}
        </div>
      )}

      {actionTools.length > 0 && (
        <details className="tool-schema">
          <summary>Available Action tools and input schemas</summary>
          <div className="tool-list">
            {actionTools.map((tool) => (
              <article className="mini-card" key={tool.function.name}>
                <h3>{tool.function.name}</h3>
                <p>{tool.function.description}</p>
                <pre>{JSON.stringify(tool.function.parameters, null, 2)}</pre>
              </article>
            ))}
          </div>
        </details>
      )}
    </section>
  );

  const renderSettings = () => (
    <section className="panel control-panel">
      <div className="panel-head">
        <div>
          <h2>Device settings</h2>
          <p>Display overrides are applied with Android's own <code>wm</code> service and can be reset at any time.</p>
        </div>
        <div className="head-actions">
          <button type="button" className="ghost" onClick={() => void loadDisplay()} disabled={!currentStatus.selected_device || displayBusy}>
            Read current
          </button>
        </div>
      </div>

      <div className="detail-grid">
        <article className="mini-card">
          <h3>Preferences</h3>
          <label>
            <input
              type="checkbox"
              checked={autoRefresh}
              onChange={(event) =>
                void persistSettings(
                  { ...settings, auto_refresh: event.target.checked },
                  "Live refresh preference saved.",
                )
              }
            />{" "}
            Refresh live frames automatically every 3 seconds
          </label>
          <p className="hint">
            Stored in $XDG_CONFIG_HOME/waydroid-control-center/settings.json. AI credentials are
            never written to disk.
          </p>
          {settingsError && <p className="hint" role="alert">{settingsError}</p>}
          {settingsSaved && <p className="hint" role="status">{settingsSaved}</p>}
        </article>
      </div>

      <div className="detail-grid">
        <article className="mini-card">
          <h3>Current display</h3>
          <ul className="detail-list">
            <li><span>Size</span><strong className="multiline">{displayInfo?.size ?? "Not queried"}</strong></li>
            <li><span>Density</span><strong className="multiline">{displayInfo?.density ?? "Not queried"}</strong></li>
            <li><span>Captured resolution</span><strong>{currentStatus.resolution}</strong></li>
          </ul>
        </article>
        <article className="mini-card">
          <h3>Apply override</h3>
          <div className="field-grid">
            <label>
              Width
              <input type="text" inputMode="numeric" value={displayWidth} onChange={(event) => setDisplayWidth(event.target.value)} placeholder="1920" />
            </label>
            <label>
              Height
              <input type="text" inputMode="numeric" value={displayHeight} onChange={(event) => setDisplayHeight(event.target.value)} placeholder="1080" />
            </label>
            <label>
              Density (dpi)
              <input type="text" inputMode="numeric" value={displayDensity} onChange={(event) => setDisplayDensity(event.target.value)} placeholder="240" />
            </label>
          </div>
          <div className="app-actions">
            <button type="button" className="primary" onClick={() => void applyDisplay()} disabled={!currentStatus.selected_device || displayBusy}>
              Apply
            </button>
            <button type="button" className="ghost" onClick={() => void resetDisplay()} disabled={!currentStatus.selected_device || displayBusy}>
              Reset overrides
            </button>
          </div>
          <p className="hint">Size must be 320x240–3840x2160; density 120–640 dpi. Some apps need a restart to pick up a new size.</p>
        </article>
      </div>

      <ul className="detail-list settings-facts">
        <li><span>Waydroid version</span><strong>{currentStatus.version}</strong></li>
        <li><span>Android version</span><strong>{currentStatus.android_version}</strong></li>
        <li><span>Vendor type</span><strong>{currentStatus.vendor_type}</strong></li>
        <li><span>Wayland display</span><strong>{currentStatus.wayland_display}</strong></li>
        <li><span>Suspend action</span><strong>{currentStatus.suspend_action}</strong></li>
      </ul>
    </section>
  );

  const renderLogs = () => (
    <section className="panel control-panel">
      <div className="panel-head">
        <div>
          <h2>Activity logs</h2>
          <p>Every backend action records its own outcome here. Entries are kept in memory for this run only.</p>
        </div>
        <div className="head-actions">
          <button type="button" className="ghost" onClick={() => void loadLog()} disabled={logLoading}>
            {logLoading ? "Refreshing…" : "Refresh"}
          </button>
        </div>
      </div>
      {logEntries.length === 0 ? (
        <div className="empty-state">{logLoading ? "Loading activity…" : "No actions recorded yet in this session."}</div>
      ) : (
        <ul className="log-list">
          {logEntries.map((entry, index) => (
            <li key={`${entry.timestamp}-${index}`} className={entry.level === "error" ? "log-error" : "log-info"}>
              <time dateTime={new Date(entry.timestamp * 1000).toISOString()}>
                {new Date(entry.timestamp * 1000).toLocaleTimeString()}
              </time>
              <strong>{entry.action}</strong>
              <span>{entry.detail}</span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );

  const renderPanel = () => {
    switch (activeNav) {
      case "live":
        return renderLive();
      case "applications":
        return (
          <section className="panel placeholder">
            <h2>Application manager</h2>
            <div className="toolbar-row">
              <input
                type="search"
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder="Search package or app name"
              />
              <select value={sortMode} onChange={(event) => setSortMode(event.target.value as "name" | "category")}>
                <option value="name">Sort by name</option>
                <option value="category">Sort by category</option>
              </select>
              <button type="button" className="ghost" onClick={() => void loadApps()}>
                {appsLoading ? "Refreshing…" : "Refresh"}
              </button>
              <button type="button" className="primary" onClick={() => void chooseApk()} disabled={!currentStatus.selected_device || appBusy}>
                Install APK
              </button>
            </div>
            <div className="toolbar-row">
              <label>
                <input type="checkbox" checked={showUserApps} onChange={(event) => setShowUserApps(event.target.checked)} />
                User apps
              </label>
              <label>
                <input type="checkbox" checked={showSystemApps} onChange={(event) => setShowSystemApps(event.target.checked)} />
                System apps
              </label>
            </div>
            <div className="mini-grid app-grid">
              <div className="mini-card">
                <span>Installed</span>
                <strong>{apps.length}</strong>
              </div>
              <div className="mini-card">
                <span>Visible</span>
                <strong>{filteredApps.length}</strong>
              </div>
              <div className="mini-card">
                <span>Target</span>
                <strong>{currentStatus.selected_device?.serial ?? currentStatus.device_name}</strong>
              </div>
              <div className="mini-card">
                <span>Android</span>
                <strong>{currentStatus.android_version}</strong>
              </div>
            </div>
            {!currentStatus.selected_device && (
              <div className="empty-state">Select an ADB device from the Dashboard to manage applications.</div>
            )}
            {appsError && <div className="empty-state" role="alert">Application list failed: {appsError}</div>}
            <div className="app-list">
              {appsLoading ? (
                <div className="empty-state" role="status">Loading installed packages…</div>
              ) : filteredApps.length === 0 && !appsError ? (
                <div className="empty-state">No installed packages match the current filters.</div>
              ) : (
                filteredApps.map((app) => (
                  <div key={app.package} className="app-row">
                    <div className="app-meta">
                      <div>
                        <strong>{app.name || "Application label unavailable"}</strong>
                        <small>{app.package}</small>
                        <small className="muted">{app.category || "Android"}</small>
                      </div>
                    </div>
                    <div className="app-actions">
                      <button type="button" className="ghost" onClick={() => void showAppInfo(app.package)} disabled={appBusy}>
                        Info
                      </button>
                      <button type="button" className="ghost" onClick={() => void runAction("launch_android_app", app.package)} disabled={appBusy}>
                        Launch
                      </button>
                    </div>
                  </div>
                ))
              )}
            </div>
            {(appInfoLoading || appInfoError || appInfo) && (
              <section className="app-detail" aria-live="polite">
                <h3>{appInfoLoading ? `Loading ${selectedAppPackage}…` : appInfo?.name || selectedAppPackage || "Application details"}</h3>
                {appInfoError && <p role="alert">Details unavailable: {appInfoError}</p>}
                {appInfo && (
                  <>
                    <ul className="detail-list">
                      <li><span>Package</span><strong>{appInfo.package}</strong></li>
                      <li><span>Version name</span><strong>{appInfo.version_name ?? "Not reported"}</strong></li>
                      <li><span>Version code</span><strong>{appInfo.version_code ?? "Not reported"}</strong></li>
                      <li><span>Install path</span><strong>{appInfo.install_path ?? "Not reported"}</strong></li>
                      <li><span>Classification</span><strong>{appInfo.is_system_app ? "System" : appInfo.is_user_app ? "User" : "Not reported"}</strong></li>
                      <li><span>Launcher activity</span><strong>{appInfo.launcher_activity ?? "None found"}</strong></li>
                      <li><span>Application label</span><strong>{appInfo.name ?? "Unavailable; package id is shown above"}</strong></li>
                    </ul>
                    <div className="app-actions">
                      <button type="button" onClick={() => void runAction("launch_android_app", appInfo.package)} disabled={appBusy}>
                        Launch
                      </button>
                      {appInfo.is_user_app && !appInfo.is_system_app && (
                        <button
                          type="button"
                          className="ghost"
                          onClick={() => {
                            setUninstallTarget(appInfo);
                            uninstallDialog.current?.showModal();
                          }}
                          disabled={appBusy}
                        >
                          Uninstall
                        </button>
                      )}
                      <button
                        type="button"
                        className="ghost"
                        onClick={() => void invoke<string>("get_android_foreground_app")
                          .then((foreground) => setActionMessage(`Foreground application: ${foreground}`))
                          .catch((error) => setActionMessage(`Foreground check failed: ${String(error)}`))}
                      >
                        Check foreground
                      </button>
                    </div>
                  </>
                )}
              </section>
            )}
          </section>
        );
      case "files":
        return renderFiles();
      case "automation":
        return renderAutomation();
      case "assistant":
        return renderAssistant();
      case "settings":
        return renderSettings();
      case "logs":
        return renderLogs();
      default:
        return renderDashboard();
    }
  };

  return (
    <div className={`app-shell ${theme}`}>
      <aside className="sidebar">
        <div className="brand">
          <div className="brand-mark">W</div>
          <div>
            <strong>Waydroid</strong>
            <span>Control Center</span>
          </div>
        </div>

        <nav className="nav-list">
          {navItems.map((item) => (
            <button
              key={item.key}
              type="button"
              className={item.key === activeNav ? "nav-item active" : "nav-item"}
              onClick={() => setActiveNav(item.key)}
            >
              <span aria-hidden="true">{item.icon}</span>
              {item.label}
            </button>
          ))}
        </nav>
      </aside>

      <main className="content">
        <header className="topbar panel">
          <div>
            <p className="eyebrow">Host environment</p>
            <h1>Fedora KDE / Wayland</h1>
          </div>

          <div className="topbar-actions">
            <span className={`status-dot ${currentStatus.session_running ? "online" : "offline"}`} aria-hidden="true" />
            <button type="button" className="ghost" onClick={() => void refreshStatus()} disabled={busy}>
              {busy ? "Refreshing..." : "Refresh"}
            </button>
            <button
              type="button"
              className="theme-toggle"
              onClick={() => {
                const next = theme === "dark" ? "light" : "dark";
                setTheme(next);
                void persistSettings({ ...settings, theme: next }, "Theme preference saved.");
              }}
            >
              {theme === "dark" ? "Light" : "Dark"}
            </button>
          </div>
        </header>

        <div className="message-bar panel" role="status" aria-live="polite">
          <span className="message-label">Status</span>
          <strong>{actionMessage}</strong>
        </div>

        {activeNav === "dashboard" ? renderDashboard() : renderPanel()}
      </main>
      <dialog ref={installDialog} className="restart-dialog" aria-labelledby="install-dialog-title">
        <form method="dialog" onSubmit={(event) => event.preventDefault()}>
          <h2 id="install-dialog-title">Review APK installation</h2>
          {apkFile && (
            <ul className="detail-list">
              <li><span>File</span><strong>{apkFile.filename}</strong></li>
              <li><span>Size</span><strong>{(apkFile.size_bytes / (1024 * 1024)).toFixed(2)} MiB</strong></li>
              <li><span>Package</span><strong>{apkFile.package_name}</strong></li>
              <li><span>Target device</span><strong>{currentStatus.selected_device?.serial ?? "No device selected"}</strong></li>
            </ul>
          )}
          <p>
            Only install APKs from a source you trust. Android will enforce its normal package and signature protections.
            Review the selected file and device before continuing.
          </p>
          <div className="dialog-actions">
            <button type="button" onClick={() => installDialog.current?.close()} disabled={appBusy}>Cancel</button>
            <button type="button" className="primary" onClick={() => void installSelectedApk()} disabled={!apkFile || !currentStatus.selected_device || appBusy}>
              {appBusy ? "Installing…" : "Approve and install"}
            </button>
          </div>
        </form>
      </dialog>
      <dialog ref={uninstallDialog} className="restart-dialog" aria-labelledby="uninstall-dialog-title">
        <div>
          <h2 id="uninstall-dialog-title">Uninstall application?</h2>
          {uninstallTarget && (
            <ul className="detail-list">
              <li><span>Application</span><strong>{uninstallTarget.name ?? "Label unavailable"}</strong></li>
              <li><span>Package</span><strong>{uninstallTarget.package}</strong></li>
              <li><span>Classification</span><strong>{uninstallTarget.category}</strong></li>
              <li><span>Target device</span><strong>{currentStatus.selected_device?.serial ?? "No device selected"}</strong></li>
            </ul>
          )}
          <p>This removes the application for the device and may delete its application data. This action cannot be undone.</p>
          <div className="dialog-actions">
            <button type="button" onClick={() => { uninstallDialog.current?.close(); setUninstallTarget(null); }} disabled={appBusy}>Cancel</button>
            <button type="button" className="primary" onClick={() => void uninstallSelectedApp()} disabled={!uninstallTarget || appBusy}>
              {appBusy ? "Uninstalling…" : "Confirm uninstall"}
            </button>
          </div>
        </div>
      </dialog>
      <dialog
        ref={stopDialog}
        className="restart-dialog"
        aria-labelledby="stop-dialog-title"
        onClose={handleStopDialogClose}
      >
        <form method="dialog">
          <h2 id="stop-dialog-title">Stop Waydroid session?</h2>
          <p>
            This ends the Android session and closes running apps. Android data is not deleted.
            Continue only if you accept the interruption.
          </p>
          <div className="dialog-actions">
            <button type="submit" value="cancel" autoFocus>Cancel</button>
            <button type="submit" value="confirm" className="primary">Stop session</button>
          </div>
        </form>
      </dialog>
      <dialog
        ref={restartDialog}
        className="restart-dialog"
        aria-labelledby="restart-dialog-title"
        onClose={handleRestartDialogClose}
      >
        <form method="dialog">
          <h2 id="restart-dialog-title">Restart Waydroid session?</h2>
          <p>
            This interrupts the active Android session and may close running apps. It does not reset
            Android data. Continue only if you accept the interruption.
          </p>
          <div className="dialog-actions">
            <button type="submit" value="cancel" autoFocus>Cancel</button>
            <button type="submit" value="confirm" className="primary">Restart session</button>
          </div>
        </form>
      </dialog>
    </div>
  );
}

export default App;
