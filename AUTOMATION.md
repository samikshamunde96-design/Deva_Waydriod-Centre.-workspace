# Automation design

The automation layer is conservative and separated from the AI runtime. It
accepts structured workflow identifiers only; the frontend cannot supply a
sequence of shell commands.

## Implemented workflows

| id | title | steps |
|----|-------|-------|
| `bring_up` | Bring up Waydroid and connect ADB | check/start session → connect ADB → select the Waydroid device → verify Android readiness |
| `launch` | Launch and verify an application | verify readiness → resolve launcher and start → confirm foreground package |
| `screenshot` | Capture a screenshot to the host | verify readiness → capture validated PNG → save under `$XDG_PICTURES_DIR` or `~/Pictures` |

Each step reports `{ name, ok, detail }` and the report is successful only when
every step passed. A partial run is therefore visible instead of being collapsed
into one success flag. Every workflow run is also recorded in the activity log.

## Rules

- Steps reuse the same primitives as the individual commands; there is no second
  implementation of ADB calls.
- No workflow resets, wipes, reinstalls or reconfigures Waydroid.
- `bring_up` may start a stopped session because that is its stated purpose;
  other workflows never start or restart the session.
- The AI assistant has no tools and cannot trigger a workflow.
