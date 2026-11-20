# Waydroid Control Center

A lightweight Fedora KDE / Wayland desktop control panel for managing a real
Waydroid installation.

## Status

Verified against the live host environment (2026-11-18):

- Waydroid 1.6.3 installed; `waydroid-container.service` active
- Session started from the app's *Bring up Waydroid* workflow, container `RUNNING`
- ADB connected to `192.168.240.112:5555`, Android 13, shell and package manager
  responsive
- `suspend_action = freeze` and `auto_adb = False` in `/var/lib/waydroid/waydroid.cfg`

The application distinguishes between healthy, starting, stopped, frozen,
unresponsive, ADB disconnected, and permission-denied states instead of assuming
every command succeeds.

## Panels

- **Dashboard** — health model, device selection, power control, resource usage
- **Live Android** — screenshot polling with tap, swipe and key-event injection
  (this is not a video mirror; each frame is a real `screencap`)
- **Applications** — package listing, app info, launch, APK install/uninstall
- **File Manager** — browse `/sdcard`-style paths, upload and download with
  native dialogs, verified after the transfer
- **Automation** — fixed, backend-validated workflows: bring-up, launch-and-verify,
  save a screenshot
- **AI** — two independent systems: *Chat* (conversation only) and *Action*
  (a bounded agent that reaches the device through a fixed backend tool
  registry, with configurable execution safety and approval for consequential
  actions)
- **Device Settings** — display size and density overrides via Android `wm`
- **Activity Logs** — in-memory outcome log for every backend action

## Stack

- Tauri 2
- Rust
- React + TypeScript
- Vite

## Run

```bash
cd /home/scardeva/Projects/waydroid-control-center
npm install
npm run tauri dev
```

## Notes

- The app does not reset or alter the existing Android image.
- The frontend does not perform destructive actions without a backend validation step.
- **Bring up Waydroid** starts the session if needed, connects ADB, selects the
  Waydroid device and verifies Android readiness. It never resets data.
- `Connect ADB` runs `adb connect` against the IP reported by `waydroid status`;
  ADB still has to be reconnected after every session restart unless
  `auto_adb = True` is set in `/var/lib/waydroid/waydroid.cfg`.
- Both AI systems need a reachable endpoint: `ollama` on `localhost:11434`
  (`ollama serve`), or an OpenAI-compatible `/chat/completions` URL with a
  session-only key. Chat and Action have separate provider/model settings; the
  AI panel reports live connection checks, available models and the last
  successful check. Action history and tool schemas are retained in memory for
  the current app session only.
- Action safety modes are read-only, confirm mutations, confirm every action,
  and trusted low-risk (volume keys only; other mutations still require
  approval). The Action engine cannot run shell commands. Generic UI input
  remains subject to confirmation, and a successful command alone is not
  treated as proof that the requested device state occurred.
