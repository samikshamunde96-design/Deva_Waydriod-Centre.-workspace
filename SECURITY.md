# Security model

## Principles

- Never execute arbitrary shell commands from frontend input
- Only allow backend-validated commands with fixed arguments
- Use timeout-bounded shell invocations to avoid hanging the UI
- Redact sensitive values from command output or logs
- Treat Android data as untrusted input
- Require explicit approval for any destructive or data-changing action

## Current safeguards

- Commands are executed through Rust with fixed `bash -lc` arguments and per-command timeouts
- No host filesystem browsing is exposed to the UI
- No automatic root, reset, or reinstall commands are triggered
- The app exposes only a narrow, intentional set of device actions
