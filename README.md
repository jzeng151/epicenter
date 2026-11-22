# Epicenter — Implementation Testing Studio

**Don't merge the first patch an agent writes — reproduce the issue, compare multiple independent fixes, benchmark their performance, and inspect their code impact.**

**Epicenter** is a **100% pure-Rust, 7-crate implementation testing studio** built for the **Gemini 4 NYC Hackathon**. Given a bug report or feature request — submitted via **screenshots, screen recordings, voice notes, PDFs, or text** — Epicenter extracts clues from attached media, verifies a failing reproduction test before editing any code, implements multiple distinct approaches in isolated workspaces, benchmarks surviving fixes on the host machine, maps affected callers and test coverage gaps, flags code hazards and shallow workarounds, and generates a 2-speaker audio walkthrough alongside exportable patches.

---

## Multi-Model & Host-Verified Architecture

Each stage uses a dedicated model or local runtime step, with adaptive `generation_config.thinking_level` (`"low"` on fast first passes, escalating to deeper reasoning on retries and escalated fixes):

| Stage | Model / Runtime | What It Does |
| :--- | :--- | :--- |
| **1. Intake** | **Gemini 4 Hydrogen** (`gemini-4-hydrogen`) + **Gemini Decision 1** (`gemini-decision-1`) + `ffmpeg` | Uploads screenshots, `.mp4`/`.webm` recordings, voice notes, PDFs, and logs via Gemini's Files API in parallel with repo setup; extracts video keyframes with `ffmpeg`, runs zero-output-token `/v1beta/decisions` triage (unit-testability probability, dynamic-`K` sizing, and 60-file candidate ranking) alongside Hydrogen, and passes structured findings and ranked files to later stages. |
| **2. Repro Test** | **Gemini 4 Neon** (`gemini-4-neon`) → **Carbon** escalation + **`HostVerifier`** | Writes a standalone failing reproduction test before any source edits. `HostVerifier` applies only the test file against unmodified `HEAD` in an isolated git worktree and confirms a genuine failure via OS exit codes — stopping cleanly without synthetic fallback if the bug cannot be reproduced. |
| **3. Approaches** | **Gemini 4 Carbon** (`gemini-4-carbon`, `thinking_level: "low"`) | Plans 1–5 (guided by Decision 1's calibrated `suggested_k`) distinct implementation approaches spanning *Quick Fix*, *Targeted Refactor / Perf Optimization*, and *Root-Cause Fix*, along with anti-overlap constraints and a micro-benchmark spec. |
| **4. Implement** | **Gemini 4 Neon** (parallel) + **Carbon** fallback + **`HostVerifier`** | Implements and tests all approaches concurrently in isolated git worktrees (up to 3 edit-test iterations with a 90s deadline, escalating to Carbon if all initial attempts fail). `HostVerifier` uses cached git mirrors and `uv`/`npm` environments, verifies real OS test exit codes, and rejects any edits to the locked reproduction test file. |
| **5. Benchmark** | **`HostVerifier`** (`std::time::Instant` + `coverage.json` / LCOV) | Runs interleaved `Baseline → Candidate` trials in one clean host worktree using direct wall-clock timings (`std::time::Instant::now()`) and collects real line-level test coverage (`coverage.json` / LCOV). |
| **6. Risk Audit** | `epicenter-graph` + **Gemini Decision 1** + **Gemini 4 Carbon** (concurrent `join_all`) | Combines AST call-graph tracing (*Modified Code → Direct Callers → Indirect Entrypoints*), line-coverage overlays to spotlight untested callers, calibrated `/v1beta/decisions` 5-level root-cause rubrics + workaround/caller hazard probabilities, and concurrent Carbon code-hazard reviews. |
| **7. Summary** | **Gemini 4 Hydrogen** + **Gemini 3.8 Flash TTS** (`gemini-3.8-flash-tts`) | Writes a 4–6 turn Architect & Auditor walkthrough and synthesizes a ~35s 2-speaker WAV summary served at `/api/sessions/{id}/audio.wav` comparing *Best Overall*, *Safest Fix*, and *Fastest* candidates. |

*(Gemini Managed Agents on `antigravity-preview-09-2026` are also supported in `epicenter-gemini` and power the **Single Agent** baseline in the PR Benchmark.)*

---

## Pure-Rust 7-Crate Workspace

Zero JavaScript or TypeScript source files — shared `serde` wire types compile into both the native `axum` server and the `Leptos` WebAssembly UI:

* `crates/epicenter-common` — WASM-safe domain types, candidate ranking and badge selection, `LiveEvent` streaming protocol, and shared REST DTOs.
* `crates/epicenter-gemini` — Strongly-typed `reqwest` + `serde` client for Gemini `v1beta/interactions` (with adaptive `thinking_level`), Gemini Decisions (`v1beta/decisions` on `gemini-decision-1` for zero-output-token calibrated probabilities & rubrics), Managed Agents (`v1beta/agents` for the Single Agent baseline), resumable Files uploads (`upload/v1beta/files`), SSE streaming, and multi-speaker TTS.
* `crates/epicenter-graph` — Unified `git diff` parser, `tree-sitter` AST symbol/call/import extractor (Python, TypeScript/JS, plus fallback), `coverage.json` + LCOV untested-caller mapper, and `petgraph` concentric impact-map layout engine.
* `crates/epicenter-engine` — 7-stage comparison engine, `HostVerifier` local sandbox (cached bare git mirrors, isolated worktrees, `uv`/`npm` runtimes, real OS exit codes, anti-tampering checks, interleaved benchmarking + coverage), adaptive thinking scheduling, concurrent Carbon risk auditor, benchmark baselines, and offline preview mode when `GEMINI_API_KEY` is unset.
* `crates/epicenter-server` — `axum 0.8` REST + SSE server, WAL `rusqlite` persistence, media serving + `ffmpeg` keyframe extraction (`/api/attachments/{id}`, `/api/attachments/{id}/keyframes`), WAV audio walkthrough endpoint (`/api/sessions/{id}/audio.wav`), `.patch` + Markdown audit bundler, GitHub PR creator, and host-verified **Evaluate Any Merged PR** endpoint (`/api/eval/judge-pr`).
* `crates/epicenter-web` — `leptos 0.8` CSR WebAssembly studio (`trunk`), with multimodal file upload and in-browser Screen + Voice recording (`web-sys` `MediaRecorder`), evidence & keyframe gallery, interactive `<svg>` Code Impact Map, Risk vs. Speed & Latency Benchmark views, 2-speaker audio walkthrough player, and the **PR Benchmark** tab (showing only genuine live runs).
* `crates/epicenter-eval` — CLI evaluation harness (`status`, `inspect`, `verify`, `harvest`, `run`, `report`) comparing **Single Prompt** (with checked-out repository context) vs. **Single Agent** vs. **Epicenter** across **20 real merged GitHub PRs** (`5` Calibration + `15` Held-Out PRs; `15` Python + `5` TypeScript; `6` multimodal cases with `15` HTTP-200 media URLs), graded independently in `HostSandbox` against withheld PR test patches with zero fabricated scores.

---

## Quick Start

### 1. Configure Environment
```bash
cp .env.example .env
# Set GEMINI_API_KEY=... in .env
# Optional: GITHUB_TOKEN=... for GitHub PR creation (on repos your token can push/fork) and higher GitHub API rate limits (5,000 req/hr)
# Live comparisons and PR benchmarks require GEMINI_API_KEY.
# Without GEMINI_API_KEY, the Studio UI offers an offline preview session (never saved as live benchmark results).
```

### 2. Build the WebAssembly UI & Run the Server
```bash
trunk build --config crates/epicenter-web/Trunk.toml --release
cargo run --release -p epicenter-server
```
Open **http://localhost:8787** in Chrome, Edge, or Firefox to use **Studio** (upload or record screenshots, screen captures, and voice notes; inspect the **Code Impact Map**, **Trade-Off & Performance** charts, and **Audio Walkthrough**; export `.patch` files or open a GitHub PR) and **PR Benchmark** (paste any merged GitHub PR URL to run a live head-to-head evaluation).

### 3. Run Tests & the Evaluation Harness
```bash
# Run unit & integration tests across the 7-crate workspace
cargo test --workspace --exclude epicenter-web
cargo test -p epicenter-web

# Verify all 20 real merged PR cases against the GitHub API & media URL checks
cargo run -p epicenter-eval -- verify \
  --dataset eval_runs/five_sigma_20_cases.json \
  --check-github

# Validate dataset integrity & preview the 3-way run matrix (writes zero synthetic outcomes)
cargo run -p epicenter-eval -- run \
  --dataset eval_runs/five_sigma_20_cases.json \
  --split all \
  --dry-run

# Run the live 3-way benchmark graded independently via HostSandbox against withheld PR tests
GEMINI_API_KEY=... GITHUB_TOKEN=... cargo run --release -p epicenter-eval -- run \
  --dataset eval_runs/five_sigma_20_cases.json \
  --split all \
  --grading-mode host-sandbox \
  --concurrency 3
```

See [`eval_runs/FIVE_SIGMA_REPORT.md`](eval_runs/FIVE_SIGMA_REPORT.md) for the verified 20-PR dataset manifest and evaluation protocol.
