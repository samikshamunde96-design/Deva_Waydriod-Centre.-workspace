# Architecture

## Overview

The application follows a conservative desktop control-center pattern:

- Frontend: React + TypeScript + Vite
- Desktop shell: Tauri 2
- Host integration: Rust commands executed through a bounded `timeout` wrapper instead of an unbounded shell
- Android bridge: ADB for device discovery, package inspection, screen capture, input injection and file transfer
- Safety policy: verify before claiming success; never destroy or reset the current Waydroid image

## Key modules

- `src-tauri/src/lib.rs`: Tauri entry, command surface, activity logging hooks
- `src-tauri/src/waydroid/mod.rs`: installation, session and health detection, power control, ADB connection
- `src-tauri/src/android/mod.rs`: ADB-backed packages, launch, screen capture, input, files and display
- `src-tauri/src/workflow.rs`: fixed, validated multi-step workflows with per-step results
- `src-tauri/src/activity.rs`: bounded in-memory outcome log
- `src-tauri/src/providers.rs`: chat/action provider adapters, model discovery,
  capability probes and session-only credentials
- `src-tauri/src/http.rs`: bounded `curl` transport used by every provider call
- `src-tauri/src/agent.rs`: bounded Action engine with a fixed tool registry
- `src-tauri/src/settings.rs`: validated, atomically written preferences

## Health model

The backend returns an explicit health classification to the UI:

- Healthy
- Starting
- Stopped
- Frozen
- Unresponsive
- ADB disconnected
- Permission denied
- Unknown

## Design principles

- Do not silently override a timeout
- Do not restart the container just to hide a problem
- Do not claim Android actions succeeded without verification
- Keep the UI responsive by using bounded command timeouts
- Treat every shell input (paths, package names, key names, coordinates) as
  untrusted and validate it against a fixed allowlist before it reaches `adb`
- Keep the AI surfaces separate: chat is a plain conversation, while the Action
  engine reaches the device only through its fixed tool registry, never a shell
