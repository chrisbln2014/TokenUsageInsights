# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project overview

Token 戰情室 (Token Usage Insights) is a local-first dashboard that reads on-disk usage logs/session transcripts from multiple AI CLIs (Google Antigravity CLI, GitHub Copilot CLI, VS Code Copilot Chat, Codex CLI, Claude Code, Cursor, Grok, Pi, OMP, Muse) and presents daily/monthly/yearly token consumption, cost estimates, model breakdowns, and full session timeline reconstruction. It never calls AI provider APIs itself — all data comes from local logs and a local SQLite database.

## Build, test, and development commands

- `cargo run` — start the local dashboard on `http://localhost:3003`.
- `cargo build --release` — production build (also required before installing the systemd service).
- `cargo test` — run the Rust test suite (includes `tests/*.rs` integration tests and `#[cfg(test)]` unit tests embedded in `src/handlers/*.rs`, `src/main.rs`, etc.).
- `cargo fmt` — format before committing.
- `cargo clippy --all-targets --all-features` — extra lint pass when touching backend logic.
- `make lint` / `make all` — convenience wrappers around fmt/clippy/check/test/build-release (see `Makefile` for the full target list, including systemd service management targets on Linux).
- Windows: `scripts\build.ps1` runs `cargo test --release` then `cargo build --release --all-targets`, treating any compiler warning as a build failure. Use `-AllowWarnings` only for local iteration, never for a final build.
- Service install: render the unit file with `sed "s|<PROJECT_DIR>|$PWD|g" shell/token-usage-insights.service`, or use `make service-file` / `make install-service`.
- npm packaging: `npm ci --ignore-scripts`, `npm test`, `npm pack --dry-run --ignore-scripts`. Publish additionally requires the current commit to carry the exact version tag and matching GitHub Release assets.

**Zero-warning policy**: every build (`cargo build`, `cargo build --release`, `cargo test`, `scripts\build.ps1`) must complete with zero compiler warnings and zero errors across the unified `token-usage-insights` bin target. Fix warnings at the source; use a narrowly-scoped `#[allow(...)]` with a justification comment only when unavoidable.

## Architecture

### Process model (`src/main.rs`)

A single Axum binary serves two very different modes depending on environment/args, dispatched at the top of `main()` before normal startup:

1. **Snapshot export** (`--export-snapshot <path>`) — syncs local logs into SQLite then dumps a portable JSON snapshot (`src/snapshot.rs`) and exits. Used to produce a file for Cloud Run.
2. **CLI subcommands** (`cli::run`) — `update` / `export` / `export-all` / `import` (see `src/cli.rs`), handled and exited before the server ever starts.
3. **Snapshot dashboard mode** (`snapshot::snapshot_mode_enabled()`, driven by `TOKEN_USAGE_INSIGHTS_DATA_SOURCE`) — a read-only Cloud Run deployment mode with no local install, no SQLite writes, and no self-update. It serves a separate, smaller router (`build_snapshot_router`) backed entirely by an in-memory `DashboardSnapshot` loaded from a local file, `DRIVE_SNAPSHOT_FILE_ID` (Google Drive, via `GOOGLE_ACCESS_TOKEN`/service-account env vars), or similar — see `src/snapshot.rs`. Several endpoints (session-search, export/import, model-sessions) intentionally return 501 in this mode; `main.rs`'s own test suite (`snapshot_router_answers_every_frontend_api_path_with_json`) cross-checks the snapshot router against every `/api/...` path literally referenced in `static/app.js`, so a new frontend endpoint must be added to that router (even if only as `unsupported_in_snapshot_mode`) or the test fails.
4. **Standard server mode** — normal local/self-hosted operation: initializes the SQLite schema, runs startup update-recovery (`updater::perform_startup_recovery`), starts a background loop that periodically migrates legacy per-assistant DBs and incrementally syncs usage logs into SQLite (`spawn_usage_sync_task`), optionally checks for/applies self-updates in the background, and serves the full API router plus `static/` via `ServeDir`.

Shutdown in standard mode is coordinated through a `shutdown_reason` channel so that a graceful stop, an OS signal, a Windows-service-runner stop request, and a background auto-update can all race safely: the background sync task is always drained to completion (SQLite writes are never interrupted mid-write) before the process exits or restarts into a newer version.

### Self-update / handoff machinery (`src/updater.rs`, largest non-data module)

Handles GitHub release discovery, checksum-verified download, atomic binary replacement, and a backup/handoff/commit protocol (`.backup`, `.handing_off` markers under the install dir) so an interrupted update can be detected and rolled back on the next startup (`updater::perform_startup_recovery`). On Windows under the service runner, commit/cleanup is deliberately deferred to the runner itself (after it confirms the new process is healthy) rather than done by the process in-process — see the `is_windows_service_runner()` branches in `main.rs`.

### Data layer (`src/db.rs`, largest module overall)

Owns the SQLite schema (`init_db`), per-assistant source directory resolution (`get_claude_dir`, `get_codex_dir`, `get_cursor_dir`, `get_grok_dir`, `get_pi_dir`, `get_omp_dir`, `get_muse_dir`, etc. — each overridable via its own `_DIR` env var, see `src/paths.rs::env_path` for the `~`/`$HOME`/`%USERPROFILE%` expansion rules), incremental log→SQLite sync (`sync_usage_logs`), legacy multi-DB migration (`migrate_old_databases`), and query/import/export functions consumed by `src/handlers/*`.

### Per-assistant parsers (`src/timeline.rs`)

Each assistant has its own `parse_<assistant>_timeline` function that turns that CLI's native on-disk format (JSONL usage logs, VS Code `chatSessions` storage, Codex/Claude Code JSONL sessions, Cursor's SQLite state DB, etc.) into a shared `TimelineItem` enum used to reconstruct the session drawer UI. Adding support for a new assistant means: a source-directory resolver in `db.rs`, a `parse_<x>_timeline` in `timeline.rs`, an entry in `handlers::normalize_assistant_name` / `is_supported_assistant`, and (for anything besides the always-supported four) an entry in `snapshot::ASSISTANTS`.

Some assistants have their own dedicated module for parsing/collection logic beyond what fits in `timeline.rs`: `src/grok.rs`, `src/muse.rs`, `src/omp.rs`, `src/pi.rs`, `src/vscode.rs` (Copilot Chat).

### HTTP layer (`src/handlers/`)

Route handlers are split by time granularity: `daily.rs`, `monthly.rs`, `yearly.rs`, plus `misc.rs` for cross-cutting endpoints (pricing, sync trigger, rate-limit, setup-info). `handlers/mod.rs` holds assistant-name normalization/validation and the shared aggregation structs (`UsageAggregation`, `ModelUsageAggregation`, `SessionUsageAggregation`) used across granularities. Every data route is parameterized by `:assistant` and mirrored in the snapshot router (`main.rs::build_snapshot_router`) with either a real implementation or a 501 stub.

### Pricing (`src/pricing.rs` + `pricing.csv`)

Cost estimation is entirely local: `pricing.csv` at the repo root is the source of truth for per-model rates, loaded and matched against usage entries to compute `cost_usd`. There is no live API call to a pricing service.

### Frontend (`static/`)

Plain JavaScript (no build step) served directly via `ServeDir`: `app.js` (main dashboard logic), `chart-utils.js`, `session-utils.js`, `i18n.js` (bilingual zh-TW/en UI strings), `styles.css`, `index.html`. `public/` is the separate GitHub Pages landing site and is not served by the Rust binary.

### npm distribution (`npm/`)

`npm/cli.cjs` is a thin npx wrapper that downloads the matching prebuilt release binary (`npm/install.cjs`) with checksum validation; `npm/prepublish-check.cjs` enforces the tag/asset consistency required before publishing.

## Coding style

Standard Rust formatting (4-space indent, `snake_case`). Keep route handlers thin; push data access/parsing into `src/db.rs`, `src/timeline.rs`, or a per-assistant module. In frontend files, keep plain JS readable with descriptive camelCase names (e.g. `currentAssistant`, `monthlyChartInstance`). Preserve existing bilingual UI text and never rename assistant identifiers (`antigravity`, `copilot`, `codex`, `claude`, `cursor`, `grok`, `pi`, `omp`, `muse`) — they are persisted keys, not just display labels.

## Testing guidelines

Prefer deterministic fixtures by pointing `INSIGHTS_DIR` (and the relevant per-assistant `_DIR` env var) to a temporary folder, matching the existing yearly-handler test pattern in `src/handlers/`. Run `cargo test` after any API, database, or parsing change. When adding a new frontend `/api/...` call in `static/app.js`, also update `build_snapshot_router` in `src/main.rs` (the embedded test parses `app.js` for every literal `/api/` path and asserts each one is routed).

## Commit and release conventions

- Every commit message must be **comprehensive, detailed Traditional Chinese (zh-TW)** using Taiwan terminology, in Conventional Commits format (`feat(web):`, `fix(pricing):`, etc.), with a subject line, problem context, a file/module change breakdown (`變更細節`), and explicit verification commands/results (`驗證項目`).
- Commit immediately once editing and verification are done for a task; don't leave uncommitted changes without being asked to.
- Every release must update `CHANGELOG.md` (moving `Unreleased`/`未發行` items into a dated version heading, derived from both git log and the actual diff) **before** tagging, and must keep `CHANGELOG.md`, GitHub Release notes, `Cargo.toml`/`Cargo.lock`, the workflow-generated `VERSION` file, `package.json`/`package-lock.json`, and README version examples all in sync. See `AGENTS.md` for the full AI-assisted release completion checklist (verifying the workflow, the public Release, npm publish/smoke-test, and writing real zh-TW release notes rather than accepting auto-generated boilerplate).

## Security & configuration

Local-first; reads from `~/.token-usage-insights`, `~/.gemini/antigravity-cli`, `~/.copilot`, `~/.codex`, `~/.claude`, `~/.cursor`, `~/.grok`, `~/.pi`, `~/.omp`, each overridable via `INSIGHTS_DIR`, `ANTIGRAVITY_DIR`, `COPILOT_DIR`, `CODEX_DIR`, `CLAUDE_DIR`, `CURSOR_DIR`, `GROK_DIR`, `PI_DIR`, `OMP_DIR`. Snapshot/Cloud Run mode adds `TOKEN_USAGE_INSIGHTS_DATA_SOURCE`, `TOKEN_USAGE_INSIGHTS_SNAPSHOT_PATH`, `DRIVE_SNAPSHOT_FILE_ID`, `GOOGLE_ACCESS_TOKEN`, `DRIVE_TOKEN_SCOPE`/`DRIVE_TOKEN_SOURCE`, `DRIVE_SERVICE_ACCOUNT_EMAIL`. Never commit local database files, session logs, or personal paths captured during testing.
