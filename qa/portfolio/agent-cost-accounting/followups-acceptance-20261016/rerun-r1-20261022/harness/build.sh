#!/bin/bash
# acct-rerun: private dev build of main 63c9daca26 (newest main commit with green postmerge-ci at 11:49Z), same
# toolchain/features as dev-build-online.sh (pf84-rerun's build.sh), own target dir. Not installed anywhere.
set -e
RUNTIME=/Volumes/CorbanuDrive/Corbanu/.codex-work/corbanu-terminal
export PATH="$RUNTIME/rustup/toolchains/1.99.1-aarch64-apple-darwin/bin:/opt/homebrew/bin:/usr/bin:/bin"
export CARGO_HOME="$RUNTIME/cargo" CARGO_TARGET_DIR=/Volumes/CorbanuDrive/Corbanu/.codex-work/targets/acct-rerun
export CARGO_PROFILE_DEV_SPLIT_DEBUGINFO=off CARGO_PROFILE_DEV_DEBUG_ASSERTIONS=true
unset CORBANU_HOME PFTERMINAL_HOME CODEX_HOME
cd /Volumes/CorbanuDrive/Corbanu/worktrees/acct-rerun/codex-rs
git rev-parse HEAD
date -u +%FT%TZ
cargo build --locked -j8 -p codex-cli --bin corbanu --features codex-core/developer-accounting,codex-tui/developer-accounting
echo "BUILD-EXIT=$? $(date -u +%FT%TZ)"
