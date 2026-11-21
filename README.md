# Epicenter

**Don't merge the first patch an agent writes—reproduce the fault, collide orthogonal fixes in an isolated Arena, and inspect the blast radius.**

**Epicenter** is a **100% pure-Rust, 7-crate engineering studio** built for the **Gemini 4 NYC Hackathon (`5 Sigma` Track)**. Given a real-world bug report or feature request—captured via **screen-recorded bug videos, UI screenshots, PDF RFCs, or live in-browser screen + voice recording**—Epicenter locks a verified failing reproduction gate, fans out a dynamic number (`K = 1..=5`) of orthogonal architectural strategies across **Gemini Managed Agent** sandboxes (spanning *Surgical Hotfix* to *Full Root-Cause Refactor*), benchmarks surviving patches in a single noise-controlled **Arena VM**, maps **Ring 0 → Ring 1 → Ring 2** impact across your AST + runtime test-coverage dark zones, and synthesizes a 2-speaker **Gemini 3.8 Flash TTS** war-room briefing.

---

## Gemini 4 Multi-Model Architecture

Every model in Epicenter has a dedicated, non-overlapping engineering job:

| Model / Runtime | API Identifier | Engineering Responsibility |
| :--- | :--- | :--- |
| **Gemini 4 Hydrogen** | `gemini-4-hydrogen` | **Stage 1 — Multimodal Intake & Fast Triage:** Extracts reproduction steps, stack traces, UI states, and suspect symbols from uploaded `.mp4`/`.webm` bug recordings, `.png` screenshots, `.pdf` specs, and voice notes via Gemini's resumable Files API. |
| **Gemini 4 Neon** | `gemini-4-neon` (Managed Agent `epicenter-neon-worker` on `antigravity-preview-09-2026`) | **Stages 2, 4, & 5 — Remote Linux Sandbox Execution:** Writes & verifies the read-only failing reproduction test on `HEAD`, executes `1..=5` parallel candidate implementations (`background: true`), and runs the single-VM interleaved Benchmark Arena + coverage trace. |
| **Gemini 4 Carbon** | `gemini-4-carbon` | **Stages 2b, 3, 4b, & 6 — Deep Reasoning Architect & Auditor:** Escalates tricky reproductions if `Neon` fails twice; designs `K = 1..=5` orthogonal strategies with anti-overlap rules; synthesizes `0/K` rescue patches; and audits diffs + call-graph slices for **Dual-Score** ranking (`Blast Risk` vs. `Root-Cause Depth`, flagging bandaid tech debt). |
| **Gemini 3.8 Flash TTS** | `gemini-3.8-flash-tts` | **Stage 7 — 2-Speaker War-Room Briefing:** Synthesizes a ~35s multi-speaker WAV dialogue (`Architect` vs. `Auditor`) debating the trade-offs between the *Best Overall (Root-Cause Safe)*, *Safest Emergency Hotfix*, and *Fastest Throughput* candidates. |

---

## Pure-Rust 7-Crate Workspace

Zero JavaScript or TypeScript source files—shared `serde` wire types compile into both the native `axum` server and the `Leptos` WebAssembly studio:

* `crates/epicenter-common` — WASM-safe domain models, Dual-Score Pareto/badge ranking, `LiveEvent` SSE protocol, and shared REST DTOs.
* `crates/epicenter-gemini` — Strongly-typed `reqwest` + `serde` client for Gemini `v1beta/interactions`, Managed Agents (`v1beta/agents`), resumable Files upload (`upload/v1beta/files`), SSE streaming, and multi-speaker TTS.
* `crates/epicenter-graph` — Unified `git diff` parser, `tree-sitter` AST symbol/call/import extractor (Python, TypeScript/JS, plus multi-language fallback), `coverage.json` + LCOV dark-zone mapper, and `petgraph` concentric `Ring 0 -> Ring 1 -> Ring 2` layout engine.
* `crates/epicenter-engine` — 7-stage `TournamentEngine`, locked Red-Gate anti-tampering sanitizer, single-VM Arena runner, Dual-Score auditor, baselines, and deterministic offline simulation mode.
* `crates/epicenter-server` — `axum 0.8` REST + SSE server, WAL `rusqlite` persistence, `.patch` + Markdown audit bundler, 1-click GitHub PR creator, and Live Merged PR Judge endpoint.
* `crates/epicenter-web` — `leptos 0.8` CSR WebAssembly studio (`trunk`), featuring `web-sys` live Screen + Mic `MediaRecorder` capture, interactive `<svg>` Blast-Radius Ripple Map, Dual-Score Pareto scatter plot, and 2-speaker audio briefing deck.
* `crates/epicenter-eval` — **5 Sigma** CLI harness (`harvest`, `inspect`, `run`, `report`) evaluating *Single Model Call* vs. *Single Managed Agent* vs. *Epicenter Tournament* on 20 real open-source GitHub bug-fix PRs (`5` Calibration + `15` Locked Unseen, 60% multimodal) against hidden maintainer ground-truth test patches.

---

## Quick Start

### 1. Configure Environment
```bash
cp .env.example .env
# Set GEMINI_API_KEY=... in .env (optional: GITHUB_TOKEN=... for 1-click PRs & 5,000 req/hr eval harvesting)
# Note: Without GEMINI_API_KEY, Epicenter runs in deterministic offline simulation mode for instant UI exploration.
```

### 2. Build the Pure-Rust WASM Studio & Run the Server
```bash
trunk build --config crates/epicenter-web/Trunk.toml --release
cargo run --release -p epicenter-server
```
Open **http://localhost:8787** in Chrome/Edge/Firefox to use Mission Control, record live screen + voice bug reports, explore concentric `<svg>` Blast-Radius Dark Zones, and test **Live GitHub PR Judge Mode**.

### 3. Run the `5 Sigma` Evaluation Suite & Tests
```bash
# Run all 43 unit & integration tests across the workspace
cargo test --workspace --exclude epicenter-web

# Inspect the 20-issue real OSS dataset (5 Calibration + 15 Locked Unseen)
cargo run -p epicenter-eval -- inspect --dataset eval_runs/five_sigma_20_cases.json

# Execute the 3-way evaluation ladder (pass --dry-run when testing offline without API spend)
cargo run -p epicenter-eval -- run --dataset eval_runs/five_sigma_20_cases.json --split unseen --concurrency 3
```

See [`eval_runs/FIVE_SIGMA_REPORT.md`](eval_runs/FIVE_SIGMA_REPORT.md) for the full 3-way scorecard, calibration iteration log, and failure-mode taxonomy.
