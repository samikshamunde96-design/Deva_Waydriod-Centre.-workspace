# Testing notes

## Unit tests

`cargo test` runs only the pure-logic unit tests; every file in `tests/` is
marked `#[ignore]` and is therefore never executed by an ordinary test run. The
unit tests cover:

- health classification and container-state normalization
- ADB device parsing, device selection and exact package membership
- binary `AndroidManifest.xml` package extraction
- APK path, package-name, device-serial and Android-path validation
- base64 encoding against RFC 4648 vectors
- toybox `ls -l` parsing, including symlinks and names with spaces
- symlinked directory listing target
- screenshot PNG validation, including truncated input
- workflow step/report aggregation and unknown-workflow rejection
- provider request shaping, model-name/message validation and credential redaction
- Action engine tool registry, typed argument validation, duplicate and
  malformed-call rejection, step/deadline limits and cancellation
- settings validation, atomic save and corrupt-file recovery
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
cd src-tauri && cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test --lib
cd .. && npx tsc --noEmit && npm run build
```

Tauri commands are `async` so multi-second ADB probes never block the webview
event loop.

## Live integration tests (explicit opt-in only)

Every target in `src-tauri/tests/` drives a real resource and carries
`#[ignore]`, so no ordinary `cargo test` can start Waydroid, inject input,
upload files or modify Android state:

- `live_host.rs` — starts the Waydroid session, connects ADB, selects the
  device, taps, swipes, presses keys, uploads a file to `/sdcard/Download` and
  queries a local Ollama service.
- `live_action.rs` — runs the Action engine against a real local Ollama model.
  It is restricted to allowlisted read tools, but those still read the real
  device, so it is not a normal test either.
- `live_provider.rs` — lists models and sends a chat turn to a local Ollama
  service.

Run one deliberately, on a host where you accept the state change:

```bash
cd src-tauri
cargo test --test live_host -- --ignored      # starts Waydroid and injects input
cargo test --test live_action -- --ignored    # needs `ollama serve`
cargo test --test live_provider -- --ignored  # needs `ollama serve`
```

A plain `cargo test` or `cargo test --all-targets` compiles these targets but
reports them as ignored. Never remove an `#[ignore]` attribute, never add a
wrapper that runs ignored tests, and never pass `--include-ignored` in CI.

## What is explicitly not tested automatically

- Destructive Android actions
- Unattended package installation
- Unattended file deletion
- Reset or re-initialization workflows
- Native file dialogs (kdialog/zenity) require a graphical session

Those are intentionally manual.
