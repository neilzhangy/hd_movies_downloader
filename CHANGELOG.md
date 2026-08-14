# Change history

## 3.0.0 — 2026-08-14

- Replaced the Python implementation with a single Rust binary named `hd-movies`.
- Added long-running polling-service mode and `--once` for manual operation.
- Added SQLite-backed queue states, retry diagnostics, and safe migration of the legacy `MOVIES` table.
- Added direct unauthenticated Transmission RPC support using only the local Transmission IP and port.
- Added `--check-transmission` for a read-only deployment health check.
- Updated feed parsing for the current live `tpb.party` table layout while retaining the older layout fallback.
- Added same-jail completed-download organization: keep the largest movie and existing subtitle files, normalize their destination names, and remove managed source folders safely.
- Removed all subtitle-site code, subtitle downloads, archive extraction, and `web_data` behavior.
- Split the implementation into conventional Rust modules for application flow, feed parsing, SQLite, Transmission RPC, and media organization.
- Added FreeBSD service deployment material, README usage, design documentation, unit tests, and a live opt-in TPB integration test.

## Legacy history

The previous Python implementation's history is retained in Git. Version 3.0 is intentionally a new design rather than a compatibility-preserving port of its subtitle and local-file-management features.
