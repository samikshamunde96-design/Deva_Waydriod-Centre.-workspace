import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import App from "../App";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";

const settings = {
  version: 1,
  theme: "dark",
  chat_provider: "ollama",
  chat_model: "chat-model",
  action_provider: "ollama",
  action_model: "action-model",
  action_safety: "confirm_mutations",
  openai_base_url: "https://api.openai.com/v1",
  ollama_base_url: "http://127.0.0.1:11434",
  auto_refresh: false,
  last_android_directory: "/sdcard",
};

function savedSettingsFrom(args?: Parameters<typeof invoke>[1]) {
  if (args && !Array.isArray(args) && typeof args === "object" && "next" in args) {
    return args.next as typeof settings;
  }
  return settings;
}

const baseStatus = {
  installed: true,
  service_running: true,
  version: "1",
  session_running: true,
  container_status: "running",
  suspend_action: "Suspend",
  suspension_active: false,
  health: "Healthy",
  adb_connected: true,
  adb_shell_healthy: true,
  android_package_manager_healthy: true,
  screenshot_healthy: true,
  selected_device: null,
  selected_device_state: "No selection",
  device_name: "Waydroid",
  android_version: "13",
  resolution: "1080x1920",
  cpu_usage: "10%",
  memory_usage: "1 GB",
  storage_usage: "10 GB",
  ip_address: "127.0.0.1",
  session_user: "user",
  wayland_display: ":1",
  vendor_type: "waydroid",
  last_operation: "Ready",
  power_progress: "Idle",
  probe_errors: [],
};

const pendingTask = {
  id: "action-test",
  prompt: "Open Settings",
  provider: "ollama",
  base_url: "http://127.0.0.1:11434",
  model: "action-model",
  status: "awaiting_approval",
  steps: [{ index: 1, kind: "approval", title: "Waiting for approval", detail: "Launch Settings", ok: true }],
  pending_approval: { id: "approval-test", tool: "launch_app", summary: "Launch com.android.settings", arguments: { package: "com.android.settings" } },
  result: null,
  error: null,
  started_at: 1,
  finished_at: null,
  max_steps: 6,
  safety: "confirm_mutations",
  actions_attempted: 1,
  actions_run: 0,
  actions_failed: 0,
  actions_blocked: 0,
  verification: "no_device_change",
};

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  vi.mocked(invoke).mockImplementation(async (command: string, args?: Parameters<typeof invoke>[1]) => {
    switch (command) {
      case "get_dashboard_status":
        return baseStatus;
      case "load_settings":
        return { settings, warning: null };
      case "save_settings":
        return savedSettingsFrom(args);
      case "list_connected_devices":
        return [];
      case "get_selected_android_device":
        throw new Error("No device");
      case "list_provider_models":
        return [
          { name: "chat-model", size_bytes: null, modified_at: null },
          { name: "action-model", size_bytes: null, modified_at: null },
        ];
      case "provider_health":
        return {
          provider: "ollama",
          base_url: "http://127.0.0.1:11434",
          reachable: true,
          credential_configured: false,
          detail: "Connected. 2 model(s) available.",
          models: [
            { name: "chat-model", size_bytes: null, modified_at: null },
            { name: "action-model", size_bytes: null, modified_at: null },
          ],
          last_successful_verification: 1_700_000_000,
        };
      case "action_list":
        return [pendingTask];
      case "action_tools":
        return [{ type: "function", function: { name: "tap", description: "Tap coordinates", parameters: { type: "object" } } }];
      case "action_start":
        return pendingTask;
      case "action_approve":
        return { ...pendingTask, status: "running", pending_approval: null };
      case "action_cancel":
        return { ...pendingTask, status: "cancelled", pending_approval: null, error: "Cancelled by the user." };
      default:
        return null;
    }
  });
});

async function openAssistant() {
  render(<App />);
  fireEvent.click(await screen.findByRole("button", { name: /AI Assistant/i }));
}

describe("AI control center", () => {
  it("shows independent chat and action provider/model selections", async () => {
    await openAssistant();
    expect(await screen.findByLabelText("Chat provider")).toHaveValue("ollama");
    expect(await screen.findByLabelText("Action provider")).toHaveValue("ollama");
    expect(await screen.findByLabelText("Chat model")).toHaveValue("chat-model");
    expect(await screen.findByLabelText("Action model")).toHaveValue("action-model");

    fireEvent.change(screen.getByLabelText("Chat provider"), {
      target: { value: "openai_compatible" },
    });
    await waitFor(() => {
      expect(invoke).toHaveBeenCalledWith(
        "save_settings",
        expect.objectContaining({
          next: expect.objectContaining({
            chat_provider: "openai_compatible",
            action_provider: "ollama",
            action_model: "action-model",
          }),
        }),
      );
    });
  });

  it("discovers models and renders real connection indicators", async () => {
    await openAssistant();
    expect(await screen.findAllByText("Connected")).not.toHaveLength(0);
    expect(screen.getAllByText("Last successful verification:").length).toBeGreaterThan(0);
  });

  it("supports approval, cancellation and execution history", async () => {
    await openAssistant();
    const prompt = await screen.findByLabelText("Action task description");
    fireEvent.change(prompt, { target: { value: "Open Settings" } });
    fireEvent.click(screen.getByRole("button", { name: "Run task" }));
    expect(await screen.findByRole("button", { name: "Approve" })).toBeEnabled();

    fireEvent.click(screen.getByRole("button", { name: "Approve" }));
    await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith("action_approve", expect.any(Object)));

    fireEvent.click(screen.getByRole("button", { name: "Cancel task" }));
    await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith("action_cancel", expect.any(Object)));
    expect(await screen.findByText("cancelled")).toBeInTheDocument();

    expect(await screen.findByText("Action history")).toBeInTheDocument();
  });

  it("shows truthful completion and verification states in history", async () => {
    vi.mocked(invoke).mockImplementation(async (command: string, args?: Parameters<typeof invoke>[1]) => {
      if (command === "load_settings")       return { settings, warning: null };
      if (command === "save_settings") return savedSettingsFrom(args);
      if (command === "get_dashboard_status") return baseStatus;
      if (command === "list_connected_devices") return [];
      if (command === "get_selected_android_device") throw new Error("No device");
      if (command === "provider_health") {
        return {
          provider: "ollama",
          base_url: "http://127.0.0.1:11434",
          reachable: true,
          credential_configured: false,
          detail: "Connected.",
          models: [{ name: "chat-model", size_bytes: null, modified_at: null }],
          last_successful_verification: 1_700_000_000,
        };
      }
      if (command === "action_list") {
        return [
          { ...pendingTask, id: "done", status: "completed", pending_approval: null },
          { ...pendingTask, id: "verify", status: "needs_verification", verification: "readback_only", pending_approval: null },
        ];
      }
      if (command === "action_tools") return [];
      return null;
    });

    await openAssistant();
    expect(await screen.findByText("needs_verification")).toBeInTheDocument();
    expect(screen.getByText("completed")).toBeInTheDocument();
    expect(screen.getByText(/verification readback_only/)).toBeInTheDocument();
  });

  it("renders tool schemas and provider errors", async () => {
    vi.mocked(invoke).mockImplementation(async (command: string, args?: Parameters<typeof invoke>[1]) => {
      if (command === "provider_health") {
        return {
          provider: "ollama",
          base_url: "http://127.0.0.1:11434",
          reachable: false,
          credential_configured: false,
          detail: "Connection refused",
          models: [],
          last_successful_verification: null,
        };
      }
      if (command === "action_tools") {
        return [{ type: "function", function: { name: "tap", description: "Tap coordinates", parameters: { type: "object", properties: { x: { type: "integer" } } } } }];
      }
      if (command === "action_list") return [pendingTask];
      if (command === "load_settings") return { settings, warning: null };
      if (command === "save_settings") return savedSettingsFrom(args);
      if (command === "get_dashboard_status") return baseStatus;
      if (command === "list_connected_devices") return [];
      if (command === "get_selected_android_device") throw new Error("No device");
      if (command === "list_provider_models") return [];
      return null;
    });

    await openAssistant();
    expect(await screen.findAllByText("Unreachable")).not.toHaveLength(0);
    expect(await screen.findAllByText("Connection refused")).not.toHaveLength(0);
    fireEvent.click(await screen.findByText("Available Action tools and input schemas"));
    expect(await screen.findByText("Tap coordinates")).toBeInTheDocument();
  });
});
