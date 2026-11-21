import io
import json
import sys
import unittest
from contextlib import redirect_stdout
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent))
import wcc_host


class HostDispatchTests(unittest.TestCase):
    def test_unknown_operation_is_rejected(self):
        with self.assertRaises(wcc_host.HostError):
            wcc_host.dispatch("reset")

    def test_list_devices_uses_fixed_adb_arguments(self):
        with patch.object(wcc_host, "bounded_run") as runner:
            runner.return_value = (
                b"List of devices attached\n"
                b"192.168.240.112:5555 device product:waydroid model:Waydroid\n"
            )
            devices = wcc_host.list_devices()

        runner.assert_called_once_with("adb", ["devices", "-l"], 8)
        self.assertEqual(devices[0]["serial"], "192.168.240.112:5555")
        self.assertEqual(devices[0]["product"], "waydroid")

    def test_screenshot_rejects_missing_waydroid_device(self):
        with patch.object(wcc_host, "list_devices", return_value=[
            {"serial": "emulator-5554", "state": "device", "product": "sdk"}
        ]):
            with self.assertRaises(wcc_host.HostError) as caught:
                wcc_host.screenshot()

        self.assertEqual(caught.exception.code, "DEVICE_NOT_FOUND")

    def test_invalid_json_returns_protocol_error(self):
        class FakeStdin:
            buffer = io.BytesIO(b"{not-json")

        with patch.object(wcc_host.sys, "stdin", FakeStdin()):
            stdout = io.StringIO()
            with redirect_stdout(stdout):
                exit_code = wcc_host.main()

        self.assertEqual(exit_code, 2)
        response = json.loads(stdout.getvalue())
        self.assertFalse(response["ok"])
        self.assertEqual(response["error"]["code"], "INVALID_JSON")


if __name__ == "__main__":
    unittest.main()
