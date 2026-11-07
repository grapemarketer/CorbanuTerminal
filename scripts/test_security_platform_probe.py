import contextlib
import copy
import datetime as dt
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))

import security_platform_probe as probe


@contextlib.contextmanager
def synthetic_report(target_label: str):
    """Contract data only: no live identity or containment measurement."""
    measured = dt.datetime(2026, 1, 1, tzinfo=dt.timezone.utc)
    report = {
        "contract_version": probe.CONTRACT_VERSION,
        "fixture_protocol": probe.FIXTURE_PROTOCOL,
        "probe_sha256": probe.probe_sha256(),
        "run_id": "2" * 32,
        "target_id": "1" * 64,
        "target_label": target_label,
        "measured_at": probe.iso(measured),
        "expires_at": probe.iso(
            measured + dt.timedelta(seconds=probe.MAX_RESULT_AGE_SECONDS)
        ),
        "target": {
            "os": "unknown",
            "os_version": "synthetic",
            "architecture": "synthetic",
            "python_version": "synthetic",
            "cpu": "synthetic",
        },
        "capabilities": [
            {
                "capability": capability,
                "status": "untested",
                "observation": "not_tested",
                "mechanism": "synthetic_contract_fixture",
                "detail_code": "not_live_measurement",
            }
            for capability in probe.CAPABILITIES
        ],
        "protected_mode_eligible": False,
    }
    with mock.patch.object(probe, "utc_now", return_value=measured):
        yield report


class SecurityPlatformProbeTests(unittest.TestCase):
    def test_contract_fixtures_do_not_measure_live_platform(self) -> None:
        methods = (
            "test_contract_regressions",
            "test_malformed_result_uses_stable_error_path",
            "test_require_eligible_applies_to_validation_modes",
            "test_target_metadata_matches_schema_constraints",
        )
        live_entrypoints = (
            "run_probe",
            "target_identity",
            "target_metadata",
            "start_worker",
            "run_worker",
            "internal_worker",
            "probe_ipc",
            "probe_network",
            "probe_signing",
            "probe_elevation",
            "probe_store",
        )
        for method in methods:
            with self.subTest(method=method), contextlib.ExitStack() as patches:
                guards = [
                    patches.enter_context(
                        mock.patch.object(
                            probe,
                            name,
                            side_effect=AssertionError(f"live probe called: {name}"),
                        )
                    )
                    for name in live_entrypoints
                ]
                result = unittest.TestResult()
                SecurityPlatformProbeTests(method).run(result)
                self.assertEqual(result.testsRun, 1)
                self.assertEqual(result.errors, [])
                self.assertEqual(result.failures, [])
                self.assertEqual(result.skipped, [])
                for guard in guards:
                    guard.assert_not_called()

    def test_git_autocrlf_checkout_preserves_archival_probe_identity(self) -> None:
        repository = Path(__file__).resolve().parents[1]
        probe_relative = Path("scripts/security_platform_probe.py")
        canonical_source = (repository / probe_relative).read_bytes()
        self.assertNotIn(b"\r\n", canonical_source)
        evidence_directory = repository / "qa/security-levels/sprints/PF-27-S03/results"
        evidence_bytes = {
            platform: (evidence_directory / f"{platform}.json").read_bytes()
            for platform in ("linux", "macos", "windows")
        }
        # The disposable repository must not inherit caller Git configuration,
        # attributes, or worktree/index overrides.
        environment = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith("GIT_")
        }
        environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull)

        with tempfile.TemporaryDirectory() as directory:
            checkout = Path(directory)

            def git(*arguments: str) -> None:
                completed = subprocess.run(
                    ["git", *arguments],
                    cwd=checkout,
                    env=environment,
                    capture_output=True,
                    text=True,
                    timeout=30,
                    check=False,
                )
                self.assertEqual(completed.returncode, 0, completed.stderr)

            git("init", "--quiet")
            git("config", "core.autocrlf", "true")
            git("config", "core.attributesFile", os.devnull)
            git("config", "core.safecrlf", "false")
            checked_out_probe = checkout / probe_relative
            checked_out_probe.parent.mkdir()
            checked_out_probe.write_bytes(canonical_source)
            for platform, contents in evidence_bytes.items():
                (checkout / f"{platform}.json").write_bytes(contents)
            git("add", "--", probe_relative.as_posix())

            def check_evidence(stage: str, expected_exit: int) -> None:
                for platform, contents in evidence_bytes.items():
                    with self.subTest(stage=stage, platform=platform):
                        evidence = checkout / f"{platform}.json"
                        self.assertEqual(evidence.read_bytes(), contents)
                        completed = subprocess.run(
                            [
                                sys.executable,
                                str(checked_out_probe),
                                "--validate-evidence",
                                str(evidence),
                            ],
                            capture_output=True,
                            text=True,
                            timeout=30,
                            check=False,
                        )
                        self.assertEqual(
                            completed.returncode, expected_exit, completed.stderr
                        )
                        if expected_exit == 0:
                            self.assertEqual(
                                completed.stdout.strip(),
                                "security-platform-probe: archival evidence valid",
                            )
                            self.assertEqual(completed.stderr, "")
                        else:
                            self.assertEqual(
                                completed.stderr.strip(),
                                "security-platform-probe: ContractError: wrong_probe_identity",
                            )

            # Exercise Git's actual index-to-worktree conversion, without a
            # source attribute, then with the repository's checked-in rules.
            checked_out_probe.unlink()
            git("checkout-index", "--force", "--", probe_relative.as_posix())
            self.assertEqual(
                checked_out_probe.read_bytes(),
                canonical_source.replace(b"\n", b"\r\n"),
            )
            check_evidence("baseline CRLF checkout", 1)

            (checkout / ".gitattributes").write_bytes(
                (repository / ".gitattributes").read_bytes()
            )
            git("add", "--", ".gitattributes")
            checked_out_probe.unlink()
            git("checkout-index", "--force", "--", probe_relative.as_posix())
            self.assertEqual(checked_out_probe.read_bytes(), canonical_source)
            check_evidence("LF-pinned checkout", 0)

            checked_out_probe.write_bytes(canonical_source + b"\n# identity mutation\n")
            check_evidence("genuine source mutation", 1)

    def test_contract_regressions(self) -> None:
        with synthetic_report("self-test") as report:
            with mock.patch.object(probe, "run_probe", return_value=report) as factory:
                probe.self_test()
            factory.assert_called_once_with("self-test")

    def test_malformed_result_uses_stable_error_path(self) -> None:
        with (
            synthetic_report("malformed-result-test") as report,
            tempfile.TemporaryDirectory() as directory,
        ):
            report["capabilities"][0]["status"] = []
            evidence = Path(directory) / "evidence.json"
            evidence.write_text(json.dumps(report), encoding="utf-8")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(
                    probe.main(["--validate-evidence", str(evidence)]),
                    1,
                )
        output = stderr.getvalue()
        self.assertTrue(output.startswith("security-platform-probe: TypeError:"))
        self.assertNotIn("Traceback", output)

    def test_unknown_os_has_no_unbound_boot_identity(self) -> None:
        with mock.patch.object(probe, "target_os", return_value="unknown"):
            with self.assertRaisesRegex(
                probe.ContractError, "boot_identity_unavailable"
            ):
                probe.target_identity()

    def test_require_eligible_applies_to_validation_modes(self) -> None:
        with (
            synthetic_report("require-eligible-test") as report,
            mock.patch.object(
                probe, "target_identity", return_value=report["target_id"]
            ) as identity,
            tempfile.TemporaryDirectory() as directory,
        ):
            self.assertFalse(report["protected_mode_eligible"])
            evidence = Path(directory) / "evidence.json"
            evidence.write_text(json.dumps(report), encoding="utf-8")
            for mode in ("--validate", "--validate-evidence"):
                with self.subTest(mode=mode):
                    with contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(
                            probe.main([mode, str(evidence), "--require-eligible"]),
                            2,
                        )
            identity.assert_called_once_with()

    def test_target_metadata_matches_schema_constraints(self) -> None:
        with synthetic_report("target-metadata-test") as report:
            for field, value in (("cpu", []), ("architecture", "x" * 65)):
                with self.subTest(field=field):
                    malformed = copy.deepcopy(report)
                    malformed["target"][field] = value
                    with self.assertRaisesRegex(
                        probe.ContractError, "invalid_target_metadata"
                    ):
                        probe.validate_report(malformed)

    def test_current_target_rejects_mismatch_but_archive_does_not_bind_target(
        self,
    ) -> None:
        with (
            synthetic_report("wrong-target-test") as report,
            mock.patch.object(
                probe, "target_identity", return_value=report["target_id"]
            ) as identity,
            tempfile.TemporaryDirectory() as directory,
        ):
            report["target_id"] = "3" * 64
            evidence = Path(directory) / "evidence.json"
            evidence.write_text(json.dumps(report), encoding="utf-8")
            stderr = io.StringIO()
            with contextlib.redirect_stderr(stderr):
                self.assertEqual(probe.main(["--validate", str(evidence)]), 1)
            self.assertEqual(
                stderr.getvalue().strip(),
                "security-platform-probe: ContractError: wrong_target_identity",
            )
            identity.assert_called_once_with()
            identity.reset_mock()
            with contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(probe.main(["--validate-evidence", str(evidence)]), 0)
            identity.assert_not_called()

    def test_validation_modes_reject_wrong_probe_source(self) -> None:
        with (
            synthetic_report("wrong-probe-test") as report,
            mock.patch.object(
                probe, "target_identity", return_value=report["target_id"]
            ) as identity,
            tempfile.TemporaryDirectory() as directory,
        ):
            report["probe_sha256"] = "0" * 64
            evidence = Path(directory) / "evidence.json"
            evidence.write_text(json.dumps(report), encoding="utf-8")
            for mode in ("--validate", "--validate-evidence"):
                with self.subTest(mode=mode):
                    stderr = io.StringIO()
                    with contextlib.redirect_stderr(stderr):
                        self.assertEqual(probe.main([mode, str(evidence)]), 1)
                    self.assertEqual(
                        stderr.getvalue().strip(),
                        "security-platform-probe: ContractError: wrong_probe_identity",
                    )
            identity.assert_called_once_with()

    def test_ipc_probe_is_independent_of_long_temp_paths(self) -> None:
        long_root = Path("x" * 240)
        ipc_result = probe.probe_ipc(long_root)
        if sys.platform.startswith("linux"):
            self.assertEqual(ipc_result["status"], "supported")
            self.assertEqual(ipc_result["observation"], "observed_verified")
            self.assertEqual(ipc_result["detail_code"], "peer_pid_uid_verified")
        elif sys.platform == "darwin":
            self.assertEqual(ipc_result["detail_code"], "peer_api_unavailable")
        else:
            self.assertIn(
                ipc_result["detail_code"],
                {"af_unix_unavailable", "peer_uid_api_unavailable"},
            )

    def test_windows_elevation_context_is_fail_closed(self) -> None:
        with mock.patch.object(probe, "target_os", return_value="windows"):
            cases = (
                (True, "allowed", "worker_already_elevated"),
                (
                    False,
                    "unavailable",
                    "worker_unelevated_no_noninteractive_attempt",
                ),
            )
            for elevated, outcome, code in cases:
                with self.subTest(elevated=elevated):
                    with mock.patch.object(
                        probe, "windows_token_is_elevated", return_value=elevated
                    ):
                        self.assertEqual(
                            probe.internal_worker("elevation", {}),
                            {"outcome": outcome, "code": code},
                        )
            for error_type in (OSError, AttributeError, TypeError, ValueError):
                with self.subTest(error_type=error_type.__name__):
                    with mock.patch.object(
                        probe,
                        "windows_token_is_elevated",
                        side_effect=error_type("synthetic"),
                    ):
                        self.assertEqual(
                            probe.internal_worker("elevation", {}),
                            {
                                "outcome": "error",
                                "code": "windows_token_probe_error",
                            },
                        )


if __name__ == "__main__":
    unittest.main()
