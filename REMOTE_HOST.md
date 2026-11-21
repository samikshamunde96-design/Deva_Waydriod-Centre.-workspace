# Remote Waydroid Host Protocol

## Current implementation status

Stage C provides a typed read-only protocol and a Linux host helper. It does not
yet expose mutations, replace the existing Linux adapter, or provide a Windows
UI. It is a deliberately small first transport-independent slice.

## Transport requirement

The recommended transport is SSH between the Windows client and an authenticated
Linux host account. SSH provides server authentication and encryption; the host
helper remains the authorization boundary.

Pairing and identity rules:

- Create one dedicated Linux account or an account using an explicit OpenSSH key.
- Never expose port 22 or the helper to a public network.
- Prefer WireGuard or Tailscale for the LAN/WAN route, and require SSH host-key
  checking on the Windows client.
- Store private keys in Windows credential storage or the OpenSSH agent; never
  put a key or secret in settings JSON or source control.
- Pin the approved SSH host name and host key; do not silently trust unknown hosts.
- Require mutually distinct request and host identities for audit logs.
- Add a Linux-side allowlist of the exact helper executable and read-only
  operations in the SSH `authorized_keys` entry.

The SSH transport must be kept separate from protocol parsing. The helper is not
a general shell endpoint and must never execute `operation` or argument text.

## Envelope

Requests use JSON on standard input:

```json
{
  "protocol_version": 1,
  "request_id": "unique-client-generated-id",
  "operation": "status"
}
```

The host returns JSON on standard output:

```json
{
  "protocol_version": 1,
  "request_id": "unique-client-generated-id",
  "ok": true,
  "result": {},
  "error": null
}
```

Protocol and request limits:

- `protocol_version` must equal `1`.
- `request_id` must be a non-empty string of at most 128 characters.
- A request must not exceed 16 KiB.
- A command response must not exceed 32 MiB.
- Every operation must finish within 30 seconds.
- The host must reject unknown fields and unknown operation names.
- A response must match the original request ID and protocol version.

## Read-only operation allowlist

| Operation | Host effect |
|---|---|
| `ping` | Verify helper availability |
| `status` | Run `waydroid status` and report safe host/session state |
| `list_devices` | Run `adb devices -l` |
| `screenshot` | Execute `adb exec-out screencap -p` on the selected Waydroid device |

No mutation is exposed in this first protocol slice. `start`, `stop`, `restart`,
app install/uninstall, input injection, file transfer, display changes, and
provider calls require separate approved schemas and server-side approval policy.

## Linux helper

Install `host/wcc_host.py` on the Fedora host. The script:

- Uses only fixed executable names resolved from `PATH`.
- Uses fixed argument arrays and bounded subprocess timeouts.
- Rejects JSON larger than 16 KiB.
- Rejects oversized command output.
- Returns typed protocol errors and never returns arbitrary shell output.
- Never accepts reset, wipe, reinitialize, shell, or raw command fields.

Run tests on Linux or a machine with Python:

```bash
python3 -m unittest host/test_wcc_host.py
```

Do not run the live helper against Waydroid during ordinary test or CI.

## Mutation policy for later phases

Mutating operations must use a separate approval token created by the Windows
client and validated on the Linux helper. The token must bind:

- The exact operation name.
- The exact typed arguments.
- The authenticated client identity.
- A short expiry.
- A cryptographically random one-time ID.

The helper must revalidate the selected ADB serial immediately before execution,
hold the operation lock, perform any required readback, and record an audit
outcome. The UI must never treat exit status alone as success.
