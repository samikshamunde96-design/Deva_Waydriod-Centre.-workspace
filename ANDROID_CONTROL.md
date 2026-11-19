# Android control notes

## Suspend and resume behavior

Waydroid 1.6.3 uses `suspend_action = freeze` to freeze its LXC container when
Android is suspended. The application treats a frozen container as an expected
suspension only when that setting is present and the Waydroid session is
running. A user-initiated Android operation may then request
`ContainerManager.Unfreeze`, and waits for the selected device's ADB shell,
package manager, and screenshot capability before proceeding.

The application does not start a stopped Waydroid session as a side effect of
an Android operation. The one exception is explicit: the *Bring up Waydroid*
workflow is labelled as starting the session. No reset, reinitialization, data
deletion, or automatic configuration change is performed.

## Device targeting and diagnostics

ADB devices are listed individually with their serial, model, product, device
type, connection state, and Waydroid identification. No device is selected
automatically. Android operations use the explicitly selected serial, verify
that it remains connected, and hold an operation lock so selection cannot
change mid-operation.

The dashboard reports Waydroid service, session, container, suspension, ADB
transport, Android shell, package manager, and screenshot status separately.
ADB transport alone is not considered proof that Android is healthy.

## Screen view and input

*Live Android* captures frames with `adb exec-out screencap -p` and validates
each frame as a decodable PNG before it reaches the webview. It is screenshot
polling, not a hardware video mirror: each frame costs roughly 1–1.5 s on this
host. Clicks are converted to device coordinates and sent as
`input tap`; drags become `input swipe`; the key row sends a fixed allowlist of
`input keyevent` codes (home, back, recents, enter, delete, volume, power,
wakeup, sleep, menu, search). No arbitrary key code is accepted.

## File transfer

`/sdcard`-style paths are validated before use: absolute, no `..`, and only
letters, digits, `/`, `.`, `_` and `-`, because the path is passed through the
device shell. Names containing spaces are therefore rejected rather than
escaped. Uploads use `adb push` and are verified by listing the destination
afterwards; downloads use `adb pull` into a path chosen in a native dialog and
are verified by reading the saved file.

## Display overrides

`wm size` and `wm density` are applied with bounded values (320x240–3840x2160,
120–640 dpi) and can be reset with `wm size reset` / `wm density reset`. The
captured resolution on the dashboard is read from the screenshot itself, so it
reflects what Android is actually rendering.

## Last observed host state

Re-checked 2026-11-18 with the session RUNNING and ADB connected to
`192.168.240.112:5555`:

- `waydroid status`: Session RUNNING, Container RUNNING, MAINLINE vendor
- Screenshot: valid 1920x1080 PNG
- `wm size`: Physical size 1920x1080; `wm density`: physical 180, override 240
- Package manager responded to `pm path android`
- Bring-up workflow: session check, ADB connect, device selection and Android
  readiness all passed
- Upload to `/sdcard/Download` succeeded and was verified by listing

ADB occasionally stalls for several seconds after input injection or a launcher
restart. Every command is bounded, so the UI reports a timeout instead of
hanging, and the operation can be retried.

## Supported operations and limitations

- Device discovery and explicit device selection
- Bounded health diagnostics and progress reporting
- User-initiated wake and suspend through Waydroid's container manager
- App listing, app info, launch with foreground verification
- APK inspection, installation and uninstallation with explicit approval
- Screenshot polling with tap, swipe and key-event injection
- File browsing, upload and download with post-transfer verification
- Display size and density overrides with reset
- Explicit "Connect ADB" action and a full bring-up workflow

Not implemented: real-time video mirroring, APK signature inspection beyond the
package identifier, root operations, and any AI-driven device control. No
arbitrary terminal command is exposed to the frontend.
