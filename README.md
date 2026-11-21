# Epicenter

**Don't merge the first patch an agent writes—reproduce the fault, collide orthogonal fixes in an isolated Arena, and inspect the blast radius.**

**Epicenter** is a **100% pure-Rust, 7-crate engineering studio** built for the **Gemini 4 NYC Hackathon (`5 Sigma` Track)**. Given a real-world bug report or feature request—captured via **screen-recorded bug videos, UI screenshots, PDF RFCs, voice notes, or live in-browser screen + mic recording**—Epicenter triages multimodal evidence with `gemini-4-hydrogen` and `ffmpeg` keyframe extraction, locks an independently host-verified failing reproduction gate, fans out a dynamic number (`K = 1..=5`, default `3`) of orthogonal strategies across parallel **Gemini 4 Neon** edit-test workers inside an isolated **Host Verification Sandbox (`HostVerifier`)**, benchmarks surviving patches in a single-host `std::time::Instant` interleaved **Arena** with real `coverage.json` tracing, maps **Ring 0 → Ring 1 → Ring 2** blast radius across AST + untested test-coverage dark zones with concurrent **Gemini 4 Carbon** Dual-Score audits, and synthesizes a 2-speaker **Gemini 3.8 Flash TTS** war-room audio briefing.

---

## Gemini 4 Multi-Model & Host-Verified Architecture

Every model and runtime component in Epicenter has a dedicated, non-overlapping engineering job, using adaptive `generation_config.thinking_level` (`"low"` on fast first-pass calls, escalating to deeper default thinking on retries and rescues):

| Stage / Component | Model / Runtime | Engineering Responsibility |
| :--- | :--- | :--- |
| **Stage 1 — Multimodal Intake & Triage** | **Gemini 4 Hydrogen** (`gemini-4-hydrogen`) + `ffmpeg` | Uploads local & remote `.mp4`/`.webm` recordings, `.png` screenshots, `.pdf` specs, audio notes, and logs via Gemini's resumable Files API concurrently with host environment prep; extracts video keyframes via `ffmpeg` (`/api/attachments/{id}/keyframes`) and propagates structured `multimodal_findings` across Stages 2–7. |
| **Stage 2 — Locked Red-Gate** | **Gemini 4 Neon** (`gemini-4-neon`, 2×) → **Carbon** escalation + **`HostVerifier`** | Authors a standalone failing reproduction test (`thinking_level: "low"` on attempt 1, deeper default thinking on attempt 2 and Carbon escalation). `HostVerifier` applies only the repro file on unmodified `HEAD` in an isolated worktree and verifies a genuine assertion failure via real OS exit codes—failing fast honestly with zero synthetic fallback if unverifiable. |
| **Stage 3 — Dynamic-K Orthogonal Planner** | **Gemini 4 Carbon** (`gemini-4-carbon`, `thinking_level: "low"`) | Designs `K = 1..=5` (default `3`) genuinely orthogonal strategies spanning *Surgical Hotfix*, *Algorithmic Optimization / Targeted Refactor*, and *Architectural Root Cause* with explicit `anti_overlap_rules` and a fast self-contained Arena micro-benchmark spec. |
| **Stage 4 — Parallel Edit-Test Workers & `0/K` Rescue** | **Gemini 4 Neon** (parallel) + **Carbon** `0/K` rescue + **`HostVerifier`** | Runs all `K` strategies concurrently (up to 3 host-verified loops, 90s deadline; `"low"` thinking on loop 1, deeper default thinking on retries and `0/K` Carbon rescue). `HostVerifier` manages cached bare git mirrors, isolated worktrees, cached `uv`/`npm` runtimes, real OS test exit codes, and hard anti-tampering rejecting any edit to the locked Red-Gate file. |
| **Stage 5 — Single-Host Interleaved Arena & Coverage** | **`HostVerifier`** (`std::time::Instant` + `coverage.json` / LCOV) | Interleaves `Baseline → Candidate_1 → … → Candidate_M` inside a single clean host worktree timed directly with `std::time::Instant::now()` (zero self-reported LLM timings) and captures real line coverage (`coverage.json` / LCOV) for dark-zone mapping. |
| **Stage 6 — 3-Layer Blast-Radius & Dual-Score Auditor** | `epicenter-graph` + **Gemini 4 Carbon** (concurrent `join_all`, `thinking_level: "low"`) | Combines Layer 1 AST + reverse call-graph (`Ring 0 → Ring 1 → Ring 2`), Layer 2 real test-coverage dark-zone overlay, and Layer 3 concurrent Carbon semantic hazard & **Dual-Score** audits (`Blast Risk` vs. `Root-Cause Depth`, flagging bandaid tech debt and checking multimodal findings). |
| **Stage 7 — 2-Speaker War-Room Audio Briefing** | **Gemini 4 Hydrogen** + **Gemini 3.8 Flash TTS** (`gemini-3.8-flash-tts`) | Drafts a 4–6 turn executive dialogue (`Architect` vs. `Auditor`) and synthesizes a ~35s multi-speaker WAV briefing served at `/api/sessions/{id}/audio.wav` debating *Best Overall (Root-Cause Safe)*, *Safest Emergency Hotfix*, and *Fastest Throughput*. |

*(Gemini Managed Agents on `antigravity-preview-09-2026` remain supported in `epicenter-gemini` and power the Arm 2 single-agent baseline in the 5-Sigma evaluation ladder.)*

---

## Pure-Rust 7-Crate Workspace

Zero JavaScript or TypeScript source files—shared `serde` wire types compile into both the native `axum` server and the `Leptos` WebAssembly studio:

* `crates/epicenter-common` — WASM-safe domain models, Dual-Score Pareto/badge ranking, `LiveEvent` SSE protocol, and shared REST DTOs.
* `crates/epicenter-gemini` — Strongly-typed `reqwest` + `serde` client for Gemini `v1beta/interactions` (including adaptive `generation_config.thinking_level`), Managed Agents (`v1beta/agents` for the Arm 2 eval baseline), resumable Files upload (`upload/v1beta/files`), SSE streaming, and multi-speaker TTS.
* `crates/epicenter-graph` — Unified `git diff` parser, `tree-sitter` AST symbol/call/import extractor (Python, TypeScript/JS, plus multi-language fallback), `coverage.json` + LCOV dark-zone mapper, and `petgraph` concentric `Ring 0 -> Ring 1 -> Ring 2` layout engine.
* `crates/epicenter-engine` — 7-stage `TournamentEngine`, `HostVerifier` local sandbox (cached bare git mirrors, isolated git worktrees, `uv`/`npm` runtimes, real OS exit codes, hard anti-tampering, single-host `std::time::Instant` interleaved Arena + `coverage.json`), adaptive `thinking_level` scheduling, concurrent Carbon Dual-Score auditor, 5-Sigma baselines, and an offline UI preview mode when `GEMINI_API_KEY` is unset.
* `crates/epicenter-server` — `axum 0.8` REST + SSE server, WAL `rusqlite` persistence, media serving + `ffmpeg` keyframe extraction (`/api/attachments/{id}`, `/api/attachments/{id}/keyframes`), real-bug preset (`POST /api/presets/real-bug` for `pallets/click#3884`), WAV audio briefing endpoint (`/api/sessions/{id}/audio.wav`), `.patch` + Markdown audit bundler, 1-click GitHub PR creator, and host-verified **Live Merged PR Judge** (`/api/eval/judge-pr`).
* `crates/epicenter-web` — `leptos 0.8` CSR WebAssembly studio (`trunk`), featuring media-first intake with `web-sys` live Screen + Mic `MediaRecorder` capture, attachment/keyframe preview deck, interactive `<svg>` Blast-Radius Ripple Map, Dual-Score Pareto scatter plot, 2-speaker audio briefing player, and honest Live 5-Sigma Eval / PR Judge views (strictly refusing simulated/dry-run rows).
* `crates/epicenter-eval` — **5 Sigma** CLI harness (`status`, `inspect`, `verify`, `harvest`, `run`, `report`) evaluating *Arm 1: Single Model Call* (with real checked-out repository source context) vs. *Arm 2: Single Managed Agent* vs. *Arm 3: Epicenter Tournament* across **20 live GitHub-verified real merged PRs** (`5` Calibration + `15` Locked Unseen; `15` Python + `5` TypeScript; `6` verified multimodal cases with `15` HTTP-200 media URLs) using independent `HostSandbox` grading against withheld maintainer test patches and zero fabricated dry-run scores.

---

## Quick Start

### 1. Configure Environment
```bash
cp .env.example .env
# Set GEMINI_API_KEY=... in .env (optional: GITHUB_TOKEN=... for 1-click PRs & 5,000 req/hr GitHub API limits)
# Live tournaments and 5-Sigma evaluations require GEMINI_API_KEY.
# Without GEMINI_API_KEY, the Studio UI offers an offline preview session (never persisted as real eval results).
```

### 2. Build the Pure-Rust WASM Studio & Run the Server
```bash
trunk build --config crates/epicenter-web/Trunk.toml --release
cargo run --release -p epicenter-server
```
Open **http://localhost:8787** in Chrome/Edge/Firefox to use Mission Control, upload or record live screen + voice bug reports (with `ffmpeg` keyframe strips), inspect concentric `<svg>` Blast-Radius Dark Zones, listen to the 2-speaker WAV briefing (`/api/sessions/{id}/audio.wav`), and run **Live GitHub PR Judge Mode**.

### 3. Run the Test Suite & `5 Sigma` Evaluation Harness
```bash
# Run all 61 unit & integration tests across the 7-crate workspace (58 native + 3 web)
cargo test --workspace --exclude epicenter-web
cargo test -p epicenter-web

# Audit all 20 real merged PR cases live against GitHub REST API & HTTP-200 media checks
cargo run -p epicenter-eval -- verify \
  --dataset eval_runs/five_sigma_20_cases.json \
  --check-github

# Validate dataset integrity & preview the 3-way run matrix (writes zero synthetic outcomes)
cargo run -p epicenter-eval -- run \
  --dataset eval_runs/five_sigma_20_cases.json \
  --split all \
  --dry-run

# Execute the live 3-way evaluation graded independently via HostSandbox against withheld tests
GEMINI_API_KEY=... GITHUB_TOKEN=... cargo run --release -p epicenter-eval -- run \
  --dataset eval_runs/five_sigma_20_cases.json \
  --split all \
  --grading-mode host-sandbox \
  --concurrency 3
```

See [`eval_runs/FIVE_SIGMA_REPORT.md`](eval_runs/FIVE_SIGMA_REPORT.md) for the verified 20-PR dataset manifest and live evaluation run protocol.
