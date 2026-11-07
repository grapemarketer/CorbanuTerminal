# CI baseline for 23be0b3883b13b1ae8758c14cddd19c64d6b3de3

| Workflow | Run | Status | Job | Conclusion | Tests |
|---|---|---|---|---|---|
| CodeQL | 42307698699 | cancelled | | | |
| | | | Analyze (actions) | success |  |
| | | | Analyze (c-cpp) | success |  |
| | | | Analyze (javascript-typescript) | success |  |
| | | | Analyze (python) | success |  |
| | | | Analyze (rust) | cancelled |  |
| blocking-ci | 42307698904 | success | | | |
| | | | Bazel / Bazel clippy on ${{ matrix.os }} for ${{ matrix.target }} | skipped |  |
| | | | Bazel / Bazel test on ${{ matrix.os }} for ${{ matrix.target }} | skipped |  |
| | | | Bazel / Bazel test on windows-latest for x86_64-pc-windows-gnullvm | success |  |
| | | | Bazel / Bazel test on windows-latest for x86_64-pc-windows-gnullvm (native main) | skipped |  |
| | | | Bazel / Bazel test on windows-latest for x86_64-pc-windows-gnullvm shard 1/4 | success |  |
| | | | Bazel / Bazel test on windows-latest for x86_64-pc-windows-gnullvm shard 2/4 | success |  |
| | | | Bazel / Bazel test on windows-latest for x86_64-pc-windows-gnullvm shard 3/4 | success |  |
| | | | Bazel / Bazel test on windows-latest for x86_64-pc-windows-gnullvm shard 4/4 | success |  |
| | | | Bazel / Native Windows MSVC product build | success |  |
| | | | Bazel / Verify release build on ${{ matrix.os }} for ${{ matrix.target }} | skipped |  |
| | | | Blob size policy / Blob size policy | success |  |
| | | | CI required | success |  |
| | | | Codespell / Check for spelling errors | success |  |
| | | | cargo-deny / cargo-deny | success |  |
| | | | repo-checks / build-test | success |  |
| | | | rust-ci / App-server schema fixtures | skipped |  |
| | | | rust-ci / Argument comment lint - Linux | success |  |
| | | | rust-ci / Argument comment lint - Windows | success |  |
| | | | rust-ci / Argument comment lint - changed crates | skipped |  |
| | | | rust-ci / Argument comment lint - macOS | success |  |
| | | | rust-ci / Argument comment lint package | success |  |
| | | | rust-ci / CI results (required) | success |  |
| | | | rust-ci / Core security tests — macos-15 / cargo test codex-core security:: | success | cargo test 6 binaries, 1678 passed, 0 failed, 10 ignored |
| | | | rust-ci / Core security tests — ubuntu-24.04 / cargo test codex-core security:: | success | cargo test 5 binaries, 1613 passed, 0 failed, 10 ignored |
| | | | rust-ci / Detect changed areas | success |  |
| | | | rust-ci / Format / etc | success |  |
| | | | rust-ci / Lint/Build — ubuntu-24.04 - x86_64-unknown-linux-gnu / cargo clippy | success |  |
| | | | rust-ci / Lint/Build — windows-2022 - x86_64-pc-windows-msvc / cargo clippy | success |  |
| | | | rust-ci / Windows native tests | skipped |  |
| | | | rust-ci / cargo shear | success |  |
| | | | sdk / Detect TypeScript SDK changes | skipped |  |
| | | | sdk / TypeScript SDK | skipped |  |
| | | | sdk / python-sdk | success |  |
| | | | sdk / sdks | skipped |  |
| corbanu-terminal-ci | 42307698836 | success | | | |
| | | | Build and package Corbanu Terminal | success |  |
| postmerge-ci | 42307698990 | success | | | |
| | | | Postmerge CI results | success |  |
| | | | Report main status | success |  |
| | | | rust-ci-full / Argument comment lint package | success |  |
| | | | rust-ci-full / Core security tests — macos-15 / cargo test codex-core security:: | success | cargo test 6 binaries, 1678 passed, 0 failed, 10 ignored |
| | | | rust-ci-full / Format / etc | success |  |
| | | | rust-ci-full / Full CI results | success |  |
| | | | rust-ci-full / Lint/Build — ubuntu-24.04 - x86_64-unknown-linux-gnu / cargo clippy | success |  |
| | | | rust-ci-full / Lint/Build — ubuntu-24.04 - x86_64-unknown-linux-musl (release) / cargo clippy | success |  |
| | | | rust-ci-full / Lint/Build — ubuntu-24.04 - x86_64-unknown-linux-musl / cargo clippy | success |  |
| | | | rust-ci-full / Lint/Build — windows-2022 - x86_64-pc-windows-msvc / cargo clippy | success |  |
| | | | rust-ci-full / Tests — ubuntu-24.04 - x86_64-unknown-linux-gnu / Build nextest archive | success |  |
| | | | rust-ci-full / Tests — ubuntu-24.04 - x86_64-unknown-linux-gnu / Platform result | success |  |
| | | | rust-ci-full / Tests — ubuntu-24.04 - x86_64-unknown-linux-gnu / Tests shard 1/4 | success | nextest 5169 run, 5169 passed, 0 flaky, 0 failed, 15281 skipped (484s) |
| | | | rust-ci-full / Tests — ubuntu-24.04 - x86_64-unknown-linux-gnu / Tests shard 2/4 | success | nextest 4962 run, 4962 passed, 0 flaky, 0 failed, 15488 skipped (361s) |
| | | | rust-ci-full / Tests — ubuntu-24.04 - x86_64-unknown-linux-gnu / Tests shard 3/4 | success | nextest 5085 run, 5085 passed, 0 flaky, 0 failed, 15365 skipped (533s) |
| | | | rust-ci-full / Tests — ubuntu-24.04 - x86_64-unknown-linux-gnu / Tests shard 4/4 | success | nextest 5157 run, 5157 passed, 0 flaky, 0 failed, 15293 skipped (509s) |
| | | | rust-ci-full / cargo shear | success |  |
| | | | v8-canary / Build ${{ matrix.variant }} ${{ matrix.target }} | skipped |  |
| | | | v8-canary / Build ptrcomp-sandbox ${{ matrix.target }} from source | skipped |  |
| | | | v8-canary / metadata | success |  |
| tmux smoke | 42307698779 | success | | | |
| | | | Ubuntu tmux smoke | success | nextest 84 run, 84 passed, 0 flaky, 0 failed, 25 skipped (762s) |
