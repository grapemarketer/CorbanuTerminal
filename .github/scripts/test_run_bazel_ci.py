#!/usr/bin/env python3

import json
import os
import subprocess
import unittest
from pathlib import Path
from tempfile import TemporaryDirectory


class RunBazelCiTest(unittest.TestCase):
    def test_sdk_stages_required_runtime_companions(self) -> None:
        workflow = (
            Path(__file__).parents[1] / "workflows" / "sdk.yml"
        ).read_text(encoding="utf-8")

        self.assertIn("//codex-rs/cli:codex", workflow)
        self.assertIn("//codex-rs/code-mode-host:codex-code-mode-host", workflow)
        self.assertIn("stage_bazel_binary //codex-rs/cli:codex codex", workflow)
        self.assertIn("codex-code-mode-host", workflow)

    def test_explicit_bazel_wrapper_job_timeouts_allow_cold_local_builds(self) -> None:
        workflows_dir = Path(__file__).parents[1] / "workflows"

        for workflow_path in workflows_dir.glob("*.yml"):
            lines = workflow_path.read_text(encoding="utf-8").splitlines()
            for line_number, line in enumerate(lines):
                if "run-bazel-ci.sh" not in line:
                    continue

                job_start = line_number
                while job_start >= 0:
                    candidate = lines[job_start]
                    if candidate.startswith("  ") and not candidate.startswith("    "):
                        break
                    job_start -= 1

                job_end = line_number + 1
                while job_end < len(lines):
                    candidate = lines[job_end]
                    if candidate.startswith("  ") and not candidate.startswith("    "):
                        break
                    job_end += 1

                timeout_lines = [
                    candidate.strip()
                    for candidate in lines[job_start:job_end]
                    if candidate.strip().startswith("timeout-minutes:")
                ]
                for timeout_line in timeout_lines:
                    timeout = int(timeout_line.split(":", 1)[1].strip())
                    self.assertGreaterEqual(
                        timeout,
                        30,
                        f"{workflow_path.name} Bazel wrapper job needs cold-build headroom",
                    )

    def test_keyless_windows_cross_compile_fails_closed(self) -> None:
        script = Path(__file__).with_name("run-bazel-ci.sh")

        with TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            capture_path = temp_path / "bazel-args.json"
            fake_bazel = temp_path / "bazel"
            fake_bazel.write_text(
                """#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

Path(os.environ["BAZEL_ARGS_CAPTURE"]).write_text(
    json.dumps(sys.argv[1:]), encoding="utf-8"
)
""",
                encoding="utf-8",
            )
            fake_bazel.chmod(0o755)

            env = os.environ.copy()
            env.pop("BUILDBUDDY_API_KEY", None)
            env.update(
                {
                    "BAZEL_ARGS_CAPTURE": str(capture_path),
                    "CODEX_BAZEL_BIN": str(fake_bazel),
                    "CODEX_BAZEL_WINDOWS_PATH": "/usr/bin",
                    "RUNNER_OS": "Windows",
                }
            )

            result = subprocess.run(
                [
                    "bash",
                    str(script),
                    "--windows-cross-compile",
                    "--remote-download-toplevel",
                    "--",
                    "build",
                    "--",
                    "//codex-rs/cli:codex",
                ],
                cwd=script.parents[2],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 2)
            self.assertIn("requires authenticated remote execution", result.stderr)
            self.assertFalse(capture_path.exists(), "Bazel must not run with a false local fallback")

    def test_keyless_native_windows_keeps_the_windows_test_skip_list(self) -> None:
        script = Path(__file__).with_name("run-bazel-ci.sh")

        with TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            capture_path = temp_path / "bazel-args.json"
            fake_bazel = temp_path / "bazel"
            fake_bazel.write_text(
                """#!/usr/bin/env python3
import json
import os
import sys
from pathlib import Path

Path(os.environ["BAZEL_ARGS_CAPTURE"]).write_text(
    json.dumps(sys.argv[1:]), encoding="utf-8"
)
""",
                encoding="utf-8",
            )
            fake_bazel.chmod(0o755)

            env = os.environ.copy()
            env.pop("BUILDBUDDY_API_KEY", None)
            env.update(
                {
                    "BAZEL_ARGS_CAPTURE": str(capture_path),
                    "CODEX_BAZEL_BIN": str(fake_bazel),
                    "CODEX_BAZEL_WINDOWS_PATH": "/usr/bin",
                    "RUNNER_OS": "Windows",
                }
            )

            result = subprocess.run(
                ["bash", str(script), "--", "test", "--", "//..."],
                cwd=script.parents[2],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )

            self.assertEqual(result.returncode, 0, result.stderr)
            args = json.loads(capture_path.read_text(encoding="utf-8"))
            self.assertIn("--config=windows-native-tests", args)
            self.assertIn("--keep_going", args)
            self.assertNotIn("--config=ci-windows", args)

    def run_with_exit_codes(self, exit_codes: list[int]) -> tuple[subprocess.CompletedProcess[str], int]:
        """Run the wrapper with a fake Bazel that exits with each code in turn."""
        script = Path(__file__).with_name("run-bazel-ci.sh")

        with TemporaryDirectory() as temp_dir:
            temp_path = Path(temp_dir)
            counter = temp_path / "calls"
            jvm_out = temp_path / "o" / "server" / "jvm.out"
            jvm_out.parent.mkdir(parents=True)
            jvm_out.write_text("jvm.out: server stderr\n", encoding="utf-8")
            (temp_path / "o" / "java.log").write_text(
                "java.log: NullPointerException at Example.java:1\n"
                "args: --remote_header=x-buildbuddy-api-key=SECRET123 --config=ci\n",
                encoding="utf-8",
            )
            fake_bazel = temp_path / "bazel"
            fake_bazel.write_text(
                f"""#!/usr/bin/env python3
import sys
from pathlib import Path

counter = Path({str(counter)!r})
calls = int(counter.read_text()) + 1 if counter.exists() else 1
counter.write_text(str(calls))
codes = {exit_codes!r}
code = codes[min(calls, len(codes)) - 1]
if code == 37:
    print("FATAL: bazel crashed due to an internal error.")
    print("Server terminated abruptly (error code: 14, error message: 'Socket closed', "
          "log file: '{jvm_out}')")
sys.exit(code)
""",
                encoding="utf-8",
            )
            fake_bazel.chmod(0o755)

            env = os.environ.copy()
            env.pop("BUILDBUDDY_API_KEY", None)
            env.pop("RUNNER_OS", None)
            env["CODEX_BAZEL_BIN"] = str(fake_bazel)

            result = subprocess.run(
                ["bash", str(script), "--", "build", "--", "//codex-rs/cli:codex"],
                cwd=script.parents[2],
                env=env,
                check=False,
                capture_output=True,
                text=True,
            )
            calls = int(counter.read_text()) if counter.exists() else 0
            return result, calls

    def test_a_bazel_server_crash_is_logged_and_retried_once(self) -> None:
        result, calls = self.run_with_exit_codes([37, 0])

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls, 2)
        # Diagnostics go to stderr: callers such as sdk.yml parse stdout.
        self.assertIn("jvm.out: server stderr", result.stderr)
        self.assertIn("java.log: NullPointerException", result.stderr)
        self.assertIn("retrying once", result.stderr)
        self.assertNotIn("java.log", result.stdout)
        self.assertNotIn("SECRET123", result.stdout + result.stderr)
        self.assertIn("--remote_header=<redacted>", result.stderr)

    def test_a_second_bazel_server_crash_fails(self) -> None:
        result, calls = self.run_with_exit_codes([37, 37, 0])

        self.assertEqual(result.returncode, 37)
        self.assertEqual(calls, 2)
        self.assertEqual(result.stderr.count("java.log: NullPointerException"), 2)
        self.assertIn("crashed again on the retry", result.stderr)

    def test_build_and_test_failures_are_not_retried(self) -> None:
        for code in [1, 3]:
            result, calls = self.run_with_exit_codes([code, 0])

            self.assertEqual(result.returncode, code)
            self.assertEqual(calls, 1)


if __name__ == "__main__":
    unittest.main()
