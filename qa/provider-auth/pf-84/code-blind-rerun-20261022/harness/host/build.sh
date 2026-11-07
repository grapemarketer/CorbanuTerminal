#!/bin/bash
# pf84-rerun: private dev build of main f61397ca41 (same toolchain/features as dev-build-online.sh), own target dir.
set -e
RUNTIME=/Volumes/CorbanuDrive/Corbanu/.codex-work/corbanu-terminal
export PATH="$RUNTIME/rustup/toolchains/1.99.1-aarch64-apple-darwin/bin:/opt/homebrew/bin:/usr/bin:/bin"
export CARGO_HOME="$RUNTIME/cargo" CARGO_TARGET_DIR=<Q>-target
export CARGO_PROFILE_DEV_SPLIT_DEBUGINFO=off CARGO_PROFILE_DEV_DEBUG_ASSERTIONS=true
unset CORBANU_HOME PFTERMINAL_HOME CODEX_HOME
cd /Volumes/CorbanuDrive/Corbanu/worktrees/pf84-rerun/codex-rs
date -u +%FT%TZ
cargo build --locked -j8 -p codex-cli --bin corbanu --features codex-core/developer-accounting,codex-tui/developer-accounting
echo "BUILD-EXIT=$? $(date -u +%FT%TZ)"
