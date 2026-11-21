# Epicenter 5-Sigma 3-Way Evaluation Status Report

> **Status: Verified 100% First-Party Dataset Manifest Ready for Live Host-Sandbox Evaluation**
>
> All 20 benchmark cases in `eval_runs/five_sigma_20_cases.json` come **exclusively from `jzeng151`-owned public non-fork repositories** (`jzeng151/hermes-lyre`, `jzeng151/soilsense`, and `jzeng151/pop-engine`). Zero external third-party source code is used anywhere in the evaluation suite.
>
> No synthetic outcomes, diffs, speedups, blast-risk scores, or fabricated calibration claims exist in this repository. Future `eval_runs/latest_five_sigma.json` and scorecard tables will be populated exclusively from live Gemini 4 API executions graded independently via `GradingMode::HostSandbox` (`git apply` + real OS `pytest`/`vitest` exit codes + real checked-out repository `RepoCallGraph` blast-radius analysis).

## Verified First-Party GitHub Dataset (`eval_runs/five_sigma_20_cases.json`)

Every case in `eval_runs/five_sigma_20_cases.json` was constructed from a real merged pull request in a `jzeng151`-owned public non-fork repository, verified locally with `git apply --check` + fail-to-pass `pytest`/`vitest` execution on `parent_commit`, and audited live against the GitHub REST API via `epicenter-eval verify --dataset eval_runs/five_sigma_20_cases.json --check-github`:

| Metric | Value |
| --- | --- |
| Total Verified First-Party PR Cases | **20 / 20 PASS** (live GitHub API commit + merged PR + HTTP-200 media check) |
| Repository Ownership | **100% `jzeng151`-owned public non-fork repos** (0 external third-party repos) |
| Split Discipline | **5 Calibration** + **15 Locked Unseen** |
| Language & Repo Breakdown | **10 Python** (`jzeng151/hermes-lyre`: 4 Cal, 6 Unseen) + **10 TypeScript** (`jzeng151/soilsense`: 1 Cal, 6 Unseen; `jzeng151/pop-engine`: 3 Unseen) |
| Verified Multimodal Cases | **4 cases** (`unseen-soilsense-112`, `unseen-soilsense-115`, `unseen-soilsense-120`, `unseen-soilsense-128`) with **16 HTTP-200 verified PNG/JPEG screenshot URLs** committed directly in those PRs |
| Hidden Ground-Truth Isolation | 100% of `ground_truth_test_patch` hunks modify executable test files only (`.py` / `.test.ts`), cleanly verified with `git apply --check` against `parent_commit` and strictly withheld from all 3 arms |

### 20 Real Verified First-Party Pull Requests (`jzeng151`)

| Case ID | Repository | Real Merged PR | Real Base Commit | Language | Split | Verified Media |
| --- | --- | --- | --- | --- | --- | ---: |
| `cal-hermes-lyre-58` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#58](https://github.com/jzeng151/hermes-lyre/pull/58) | `938d22c8` | Python | `calibration` | 0 |
| `cal-hermes-lyre-56` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#56](https://github.com/jzeng151/hermes-lyre/pull/56) | `f03ea6ce` | Python | `calibration` | 0 |
| `cal-hermes-lyre-55` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#55](https://github.com/jzeng151/hermes-lyre/pull/55) | `7ec1345e` | Python | `calibration` | 0 |
| `cal-hermes-lyre-57` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#57](https://github.com/jzeng151/hermes-lyre/pull/57) | `2b4bd810` | Python | `calibration` | 0 |
| `cal-soilsense-99` | [jzeng151/soilsense](https://github.com/jzeng151/soilsense) | [#99](https://github.com/jzeng151/soilsense/pull/99) | `cbdc312f` | TypeScript | `calibration` | 0 |
| `unseen-hermes-lyre-50` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#50](https://github.com/jzeng151/hermes-lyre/pull/50) | `8ea03a62` | Python | `locked_unseen` | 0 |
| `unseen-hermes-lyre-49` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#49](https://github.com/jzeng151/hermes-lyre/pull/49) | `d4061a25` | Python | `locked_unseen` | 0 |
| `unseen-hermes-lyre-42` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#42](https://github.com/jzeng151/hermes-lyre/pull/42) | `90a68c94` | Python | `locked_unseen` | 0 |
| `unseen-hermes-lyre-40` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#40](https://github.com/jzeng151/hermes-lyre/pull/40) | `40b669a3` | Python | `locked_unseen` | 0 |
| `unseen-hermes-lyre-37` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#37](https://github.com/jzeng151/hermes-lyre/pull/37) | `72ed5422` | Python | `locked_unseen` | 0 |
| `unseen-hermes-lyre-36` | [jzeng151/hermes-lyre](https://github.com/jzeng151/hermes-lyre) | [#36](https://github.com/jzeng151/hermes-lyre/pull/36) | `6db7ae15` | Python | `locked_unseen` | 0 |
| `unseen-soilsense-122` | [jzeng151/soilsense](https://github.com/jzeng151/soilsense) | [#122](https://github.com/jzeng151/soilsense/pull/122) | `62ba35cf` | TypeScript | `locked_unseen` | 0 |
| `unseen-soilsense-118` | [jzeng151/soilsense](https://github.com/jzeng151/soilsense) | [#118](https://github.com/jzeng151/soilsense/pull/118) | `995aa93e` | TypeScript | `locked_unseen` | 0 |
| `unseen-soilsense-112` | [jzeng151/soilsense](https://github.com/jzeng151/soilsense) | [#112](https://github.com/jzeng151/soilsense/pull/112) | `30716af1` | TypeScript | `locked_unseen` | 4 |
| `unseen-soilsense-115` | [jzeng151/soilsense](https://github.com/jzeng151/soilsense) | [#115](https://github.com/jzeng151/soilsense/pull/115) | `ac9db24a` | TypeScript | `locked_unseen` | 4 |
| `unseen-soilsense-120` | [jzeng151/soilsense](https://github.com/jzeng151/soilsense) | [#120](https://github.com/jzeng151/soilsense/pull/120) | `73f65f84` | TypeScript | `locked_unseen` | 4 |
| `unseen-soilsense-128` | [jzeng151/soilsense](https://github.com/jzeng151/soilsense) | [#128](https://github.com/jzeng151/soilsense/pull/128) | `6f69aacd` | TypeScript | `locked_unseen` | 4 |
| `unseen-pop-engine-289` | [jzeng151/pop-engine](https://github.com/jzeng151/pop-engine) | [#289](https://github.com/jzeng151/pop-engine/pull/289) | `11346309` | TypeScript | `locked_unseen` | 0 |
| `unseen-pop-engine-254` | [jzeng151/pop-engine](https://github.com/jzeng151/pop-engine) | [#254](https://github.com/jzeng151/pop-engine/pull/254) | `88fe22d2` | TypeScript | `locked_unseen` | 0 |
| `unseen-pop-engine-244` | [jzeng151/pop-engine](https://github.com/jzeng151/pop-engine) | [#244](https://github.com/jzeng151/pop-engine/pull/244) | `7a81baec` | TypeScript | `locked_unseen` | 0 |

## How to Verify and Execute the Live Host-Verified 3-Way Evaluation

1. **Audit dataset integrity against live GitHub REST API & media URLs:**

```bash
GITHUB_TOKEN="$(gh auth token)" cargo run -p epicenter-eval -- verify \
  --dataset eval_runs/five_sigma_20_cases.json \
  --check-github
```

2. **Preview the validated live run matrix without writing synthetic results:**

```bash
cargo run -p epicenter-eval -- run \
  --dataset eval_runs/five_sigma_20_cases.json \
  --split all \
  --dry-run
```

3. **Execute the live 3-way evaluation (Arm 1: Single Model Call, Arm 2: Single Managed Agent, Arm 3: Epicenter Tournament with real downloaded media attachments) with Independent Host Sandbox Grading (`HostSandbox`):**

```bash
GEMINI_API_KEY=... GITHUB_TOKEN="$(gh auth token)" cargo run --release -p epicenter-eval -- run \
  --dataset eval_runs/five_sigma_20_cases.json \
  --split all \
  --grading-mode host-sandbox \
  --concurrency 3 \
  --output-json eval_runs/latest_five_sigma.json \
  --output-md eval_runs/FIVE_SIGMA_REPORT.md
```

Optional: pass genuinely observed calibration iteration notes via `--calibration-findings <path/to/findings.json>`.
