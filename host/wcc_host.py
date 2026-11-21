#!/usr/bin/env python3

import json
import shutil
import subprocess
import sys


PROTOCOL_VERSION = 1
MAX_REQUEST_BYTES = 16384
MAX_OUTPUT_BYTES = 33554432
MAX_TIMEOUT_SECONDS = 30


class HostError(Exception):
    def __init__(self, code, message):
        super().__init__(message)
        self.code = code
        self.message = message


def bounded_run(program, arguments, timeout=8):
    executable = shutil.which(program)
    if executable is None:
        raise HostError("DEPENDENCY_MISSING", "{} is not installed".format(program))
    try:
        output = subprocess.run(
            [executable] + arguments,
            stdin=subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=min(timeout, MAX_TIMEOUT_SECONDS),
            check=False,
        )
    except subprocess.TimeoutExpired:
        raise HostError("TIMEOUT", "{} timed out".format(program))

    if len(output.stdout) > MAX_OUTPUT_BYTES:
        raise HostError("OUTPUT_TOO_LARGE", "{} returned too much data".format(program))

    if output.returncode != 0:
        detail = output.stderr.decode("utf-8", "replace").strip()
        if not detail:
            detail = output.stdout.decode("utf-8", "replace").strip()
        raise HostError("COMMAND_FAILED", detail or "{} failed".format(program))
    return output.stdout


def status():
    waydroid = shutil.which("waydroid")
    if waydroid is None:
        return {
            "installed": False,
            "session_running": False,
            "waydroid_status": None,
            "probe_errors": ["waydroid is not installed"],
        }

    try:
        status_output = bounded_run("waydroid", ["status"], 8).decode("utf-8", "replace")
    except HostError as error:
        return {
            "installed": True,
            "session_running": False,
            "waydroid_status": None,
            "probe_errors": [error.message],
        }

    session_running = any(
        line.strip().lower().startswith("session:")
        and line.split(":", 1)[1].strip().lower() == "running"
        for line in status_output.splitlines()
    )
    ip_address = None
    for line in status_output.splitlines():
        if line.strip().lower().startswith("ip address:"):
            ip_address = line.split(":", 1)[1].strip()
            break

    return {
        "installed": True,
        "session_running": session_running,
        "waydroid_status": status_output,
        "ip_address": ip_address,
        "probe_errors": [],
    }


def list_devices():
    raw = bounded_run("adb", ["devices", "-l"], 8)
    lines = raw.decode("utf-8", "replace").splitlines()
    devices = []
    for line in lines[1:]:
        fields = line.split()
        if not fields:
            continue
        serial = fields[0]
        state = fields[1] if len(fields) > 1 else "unknown"
        details = {}
        for field in fields[2:]:
            if ":" in field:
                key, value = field.split(":", 1)
                details[key] = value
        devices.append(
            {
                "serial": serial,
                "state": state,
                "model": details.get("model"),
                "product": details.get("product"),
            }
        )
    return devices


def screenshot():
    devices = list_devices()
    waydroid_devices = [
        device
        for device in devices
        if device["state"] == "device"
        and (
            "waydroid" in (device["serial"] or "").lower()
            or "waydroid" in (device["product"] or "").lower()
        )
    ]
    if not waydroid_devices:
        raise HostError("DEVICE_NOT_FOUND", "No connected Waydroid ADB device is available")

    serial = waydroid_devices[0]["serial"]
    raw = bounded_run(
        "adb",
        ["-s", serial, "exec-out", "screencap", "-p"],
        10,
    )
    if len(raw) < 8 or raw[:8] != b"\x89PNG\r\n\x1a\n":
        raise HostError("INVALID_SCREENSHOT", "ADB did not return a valid PNG")
    import base64

    return {
        "serial": serial,
        "png_base64": base64.b64encode(raw).decode("ascii"),
        "bytes": len(raw),
    }


def dispatch(operation):
    if operation == "ping":
        return {"ok": True}
    if operation == "status":
        return status()
    if operation == "list_devices":
        return list_devices()
    if operation == "screenshot":
        return screenshot()
    raise HostError("UNSUPPORTED_OPERATION", "Unknown read-only operation")


def response(request_id, result=None, error=None):
    if error is not None:
        return {
            "protocol_version": PROTOCOL_VERSION,
            "request_id": request_id,
            "ok": False,
            "result": None,
            "error": {"code": error.code, "message": error.message},
        }
    return {
        "protocol_version": PROTOCOL_VERSION,
        "request_id": request_id,
        "ok": True,
        "result": result,
        "error": None,
    }


def main():
    raw = sys.stdin.buffer.read(MAX_REQUEST_BYTES + 1)
    if len(raw) > MAX_REQUEST_BYTES:
        print(
            json.dumps(
                response(
                    "",
                    error=HostError("REQUEST_TOO_LARGE", "Request is too large"),
                )
            )
        )
        return 2

    try:
        request = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        print(
            json.dumps(
                response(
                    "",
                    error=HostError("INVALID_JSON", "Request is not valid JSON"),
                )
            )
        )
        return 2

    if not isinstance(request, dict):
        print(
            json.dumps(
                response(
                    "",
                    error=HostError("INVALID_REQUEST", "Request must be a JSON object"),
                )
            )
        )
        return 2

    unknown_fields = set(request) - {
        "protocol_version",
        "request_id",
        "operation",
    }
    if unknown_fields:
        print(
            json.dumps(
                response(
                    "",
                    error=HostError(
                        "INVALID_REQUEST",
                        "Request contains unknown fields",
                    ),
                )
            )
        )
        return 2

    request_id = request.get("request_id")
    if not isinstance(request_id, str) or not request_id or len(request_id) > 128:
        print(
            json.dumps(
                response(
                    "",
                    error=HostError("INVALID_REQUEST", "Invalid request ID"),
                )
            )
        )
        return 2

    if request.get("protocol_version") != PROTOCOL_VERSION:
        print(
            json.dumps(
                response(
                    request_id,
                    error=HostError(
                        "UNSUPPORTED_PROTOCOL",
                        "Unsupported remote protocol version",
                    ),
                )
            )
        )
        return 2

    operation = request.get("operation")
    if operation not in ("ping", "status", "list_devices", "screenshot"):
        print(
            json.dumps(
                response(
                    request_id,
                    error=HostError(
                        "UNSUPPORTED_OPERATION",
                        "The operation is not in the read-only allowlist",
                    ),
                )
            )
        )
        return 2

    try:
        result = dispatch(operation)
    except HostError as error:
        print(json.dumps(response(request_id, error=error)))
        return 1

    print(json.dumps(response(request_id, result=result)))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
