# Active plans

This directory contains the product initiatives authorized for implementation.
At most three plan files may declare `status: active`. Each reserves one sprint,
or up to three when it declares `parallel_lanes` (the security plan since 2026-10-06).

Current active initiatives:

- [1. P1 security hardening](p1-security-hardening.md) (from 2026-10-08; replaced the [closed P0 security plan](../completed/main-2026-10-08-p0-security-levels.md))
- [2. Upstream Codex sync to rust-v0.167.0](upstream-sync-rust-v0.167.0.md) (from 2026-11-07; D304, #1532)
- [3. Task Node integration and beta planning](initiative-delivery-control.md)

Run `python3 docs/plans/check.py` before adding, moving, or closing a plan.
