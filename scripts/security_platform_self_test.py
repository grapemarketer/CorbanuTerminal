#!/usr/bin/env python3
"""Run the platform self-test with bounded, non-sensitive failure diagnostics."""

import ctypes
import subprocess
import sys
from pathlib import Path

import security_platform_probe as probe

# The probe's Windows boot-identity query arguments, verbatim. The probe source
# is pinned by the SHA-256 in archival evidence, so it cannot share a constant;
# a regression test keeps the two in step.
WINDOWS_BOOT_QUERY = (
    "-NoProfile",
    "-NonInteractive",
    "-Command",
    "(Get-CimInstance Win32_OperatingSystem).LastBootUpTime"
    ".ToUniversalTime().ToString('o')",
)
WARM_UP_TIMEOUT_SECONDS = 60


def warm_windows_boot_query() -> None:
    """Start PowerShell and CIM once before the probe's own timed query.

    On a cold hosted runner the first query can exceed the probe's fixed
    eight-second budget (#1416). The outcome is ignored: the probe still runs
    its single query under that unchanged budget and fails closed without it.
    """
    if probe.target_os() != "windows":
        return
    try:
        buffer = ctypes.create_unicode_buffer(32_768)
        length = ctypes.windll.kernel32.GetWindowsDirectoryW(buffer, len(buffer))
        if not 0 < length < len(buffer):
            return
        powershell = (
            Path(buffer.value)
            / "System32"
            / "WindowsPowerShell"
            / "v1.0"
            / "powershell.exe"
        )
        subprocess.run(
            [str(powershell), *WINDOWS_BOOT_QUERY],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            timeout=WARM_UP_TIMEOUT_SECONDS,
            check=False,
        )
    except Exception:
        # Advisory only: any failure is left to the probe's own timed query.
        pass


def main() -> int:
    try:
        warm_windows_boot_query()
        probe.self_test()
    except Exception as error:
        category = "self_test_error"
        if isinstance(error, probe.ContractError) and error.args == (
            "boot_identity_unavailable",
        ):
            category = "boot_identity_cause_unknown"
            if isinstance(error.__cause__, subprocess.TimeoutExpired):
                category = "boot_identity_timeout"
            elif isinstance(error.__cause__, OSError):
                category = "boot_identity_os_error"
        # Exception text can contain commands, paths or captured process output.
        # Only classify the actual explicit cause; do not infer from elapsed time.
        print(
            f"security-platform-self-test: failed; category={category}", file=sys.stderr
        )
        return 1
    print("security-platform-probe: 8 contract regressions passed")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
