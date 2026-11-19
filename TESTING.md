# Testing notes

## Unit tests

`cargo test` runs only the pure-logic unit tests; the live integration test in
`tests/live_host.rs` is marked `#[ignore]` and is therefore never executed by an
ordinary test run. It covers:

- health classification and container-state normalization
- ADB device parsing, device selection and exact package membership
- binary `AndroidManifest.xml` package extraction
- APK path, package-name, device-serial and Android-path validation
- base64 encoding against RFC 4648 vectors
- toybox `ls -l` parsing, including symlinks and names with spaces
- symlinked directory listing target
- screenshot PNG validation, including truncated input
- workflow step/report aggregation and unknown-workflow rejection
- assistant model and message validation
- activity log ordering

## Manual verification against the live host

```bash
waydroid --version
waydroid status
adb connect "$(waydroid status | sed -n 's/^IP address:[[:space:]]*//p'):5555"
adb devices -l
adb -s <serial> exec-out screencap -p > /tmp/frame.png
adb -s <serial> shell wm size
adb -s <serial> shell wm density
adb -s <serial> shell ls -l /sdcard/
```

In the app:

1. Dashboard → *Bring up Waydroid* → all four steps report success.
2. Dashboard → *Apply target* on the discovered Waydroid device.
3. Live Android → a frame appears; click taps, drag swipes, key buttons work.
4. File Manager → open `/sdcard`, upload a file, confirm it appears, download it.
5. Automation → *Save a screenshot* writes a PNG to `~/Pictures`.
6. Device Settings → read current size/density, apply an override, reset it.
7. Activity Logs → the actions above are listed with their outcomes.
8. AI Assistant → reload models, send a prompt (requires `ollama serve`).

## Static checks

```bash
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test
cd .. && npx tsc --noEmit && npm run build
```

Tauri commands are `async` so multi-second ADB probes never block the webview
event loop.

## Live integration test (explicit opt-in only)

`src-tauri/tests/live_host.rs` drives a **real device**: it starts the Waydroid
session, connects ADB, selects the device, taps, swipes, presses keys, uploads a
file to `/sdcard/Download`, and queries a local Ollama service. It is therefore
ignored by default and must never run in CI or in an ordinary `cargo test`.

Only run it deliberately, on a host where you accept that state change:

```bash
cd src-tauri && cargo test --test live_host -- --ignored
```

A plain `cargo test` compiles the target but reports `0 passed; 0 failed;
1 ignored` for it. Do not remove the `#[ignore]` attribute, and do not add a
wrapper that runs ignored tests automatically.

## What is explicitly not tested automatically

- Destructive Android actions
- Unattended package installation
- Unattended file deletion
- Reset or re-initialization workflows
- Native file dialogs (kdialog/zenity) require a graphical session

Those are intentionally manual.
