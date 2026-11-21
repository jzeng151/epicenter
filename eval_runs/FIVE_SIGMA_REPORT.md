# Epicenter 5-Sigma 3-Way Evaluation Status Report

> **Status: Awaiting Live Host-Verified 3-Way Evaluation Run**
>
> The prior fabricated offline dry-run baseline (`eval_runs/latest_five_sigma.json`, label `offline-dry-run-baseline-seed`) and all synthetic evaluation/grading/calibration code paths (`synthesize_dry_run_submission`, `grade_offline_deterministic`, `compute_structural_blast_from_diff`, `default_calibration_findings`, `--seed-dry-run`) have been **permanently purged**.
>
> No synthetic outcomes, diffs, speedups, blast-risk scores, or calibration claims exist in this repository. Future `eval_runs/latest_five_sigma.json` and scorecard tables will be populated exclusively from live Gemini 4 API executions graded independently via `GradingMode::HostSandbox` (`git apply` + real OS test exit codes + real checked-out repository `RepoCallGraph` blast-radius analysis).

## Verified Real GitHub Dataset (`eval_runs/five_sigma_20_cases.json`)

Every case in `eval_runs/five_sigma_20_cases.json` was harvested directly from merged GitHub bug-fix pull requests and audited with `epicenter-eval verify --dataset eval_runs/five_sigma_20_cases.json --check-github`:

| Metric | Value |
| --- | --- |
| Total Verified Real PR Cases | **20 / 20 PASS** (live GitHub API commit + merged PR + HTTP-200 media check) |
| Split Discipline | **5 Calibration** + **15 Locked Unseen** |
| Language Breakdown | **15 Python** (9 OSS repos) + **5 TypeScript** (2 OSS repos) |
| Verified Multimodal Cases | **6 cases** (3 Calibration, 3 Locked Unseen) with **15 HTTP-200 verified image/attachment URLs** |
| Hidden Ground-Truth Isolation | 100% of `ground_truth_test_patch` hunks modify test files only; strictly withheld from all 3 arms |

### 20 Real Verified GitHub Pull Requests

| Case ID | Repository | Real Merged PR | Real Base Commit | Split | Verified Media |
| --- | --- | --- | --- | --- | ---: |
| `cal-pydantic-12147` | [pydantic/pydantic](https://github.com/pydantic/pydantic) | [#12147](https://github.com/pydantic/pydantic/pull/12147) | `b5cbc5d6` | `calibration` | 9 |
| `cal-hono-4754` | [honojs/hono](https://github.com/honojs/hono) | [#4754](https://github.com/honojs/hono/pull/4754) | `2de30d7c` | `calibration` | 1 |
| `cal-click-3902` | [pallets/click](https://github.com/pallets/click) | [#3902](https://github.com/pallets/click/pull/3902) | `8abb77a8` | `calibration` | 1 |
| `cal-flask-6133` | [pallets/flask](https://github.com/pallets/flask) | [#6133](https://github.com/pallets/flask/pull/6133) | `3596b1ab` | `calibration` | 0 |
| `cal-werkzeug-3327` | [pallets/werkzeug](https://github.com/pallets/werkzeug) | [#3327](https://github.com/pallets/werkzeug/pull/3327) | `0819a2a9` | `calibration` | 0 |
| `unseen-hono-4521` | [honojs/hono](https://github.com/honojs/hono) | [#4521](https://github.com/honojs/hono/pull/4521) | `4960c063` | `locked_unseen` | 1 |
| `unseen-hono-5166` | [honojs/hono](https://github.com/honojs/hono) | [#5166](https://github.com/honojs/hono/pull/5166) | `26d8e42b` | `locked_unseen` | 2 |
| `unseen-pydantic-13326` | [pydantic/pydantic](https://github.com/pydantic/pydantic) | [#13326](https://github.com/pydantic/pydantic/pull/13326) | `74a0d3a7` | `locked_unseen` | 1 |
| `unseen-requests-7502` | [psf/requests](https://github.com/psf/requests) | [#7502](https://github.com/psf/requests/pull/7502) | `661970d1` | `locked_unseen` | 0 |
| `unseen-urllib3-5347` | [urllib3/urllib3](https://github.com/urllib3/urllib3) | [#5347](https://github.com/urllib3/urllib3/pull/5347) | `5feb9934` | `locked_unseen` | 0 |
| `unseen-fastapi-16739` | [tiangolo/fastapi](https://github.com/tiangolo/fastapi) | [#16739](https://github.com/tiangolo/fastapi/pull/16739) | `70301919` | `locked_unseen` | 0 |
| `unseen-zod-6764` | [colinhacks/zod](https://github.com/colinhacks/zod) | [#6764](https://github.com/colinhacks/zod/pull/6764) | `00f532f1` | `locked_unseen` | 0 |
| `unseen-rich-4079` | [Textualize/rich](https://github.com/Textualize/rich) | [#4079](https://github.com/Textualize/rich/pull/4079) | `19c67b9a` | `locked_unseen` | 0 |
| `unseen-pytest-15409` | [pytest-dev/pytest](https://github.com/pytest-dev/pytest) | [#15409](https://github.com/pytest-dev/pytest/pull/15409) | `36eed6b7` | `locked_unseen` | 0 |
| `unseen-click-3884` | [pallets/click](https://github.com/pallets/click) | [#3884](https://github.com/pallets/click/pull/3884) | `bb695aae` | `locked_unseen` | 0 |
| `unseen-flask-6096` | [pallets/flask](https://github.com/pallets/flask) | [#6096](https://github.com/pallets/flask/pull/6096) | `514fc6b3` | `locked_unseen` | 0 |
| `unseen-werkzeug-3325` | [pallets/werkzeug](https://github.com/pallets/werkzeug) | [#3325](https://github.com/pallets/werkzeug/pull/3325) | `15fc22b6` | `locked_unseen` | 0 |
| `unseen-urllib3-5315` | [urllib3/urllib3](https://github.com/urllib3/urllib3) | [#5315](https://github.com/urllib3/urllib3/pull/5315) | `c5347bbd` | `locked_unseen` | 0 |
| `unseen-fastapi-16734` | [tiangolo/fastapi](https://github.com/tiangolo/fastapi) | [#16734](https://github.com/tiangolo/fastapi/pull/16734) | `e1144f69` | `locked_unseen` | 0 |
| `unseen-zod-6763` | [colinhacks/zod](https://github.com/colinhacks/zod) | [#6763](https://github.com/colinhacks/zod/pull/6763) | `00f532f1` | `locked_unseen` | 0 |

## How to Verify and Execute the Live Host-Verified 3-Way Evaluation

1. **Audit dataset integrity against live GitHub REST API & media URLs:**

```bash
cargo run -p epicenter-eval -- verify \
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
GEMINI_API_KEY=... GITHUB_TOKEN=... cargo run --release -p epicenter-eval -- run \
  --dataset eval_runs/five_sigma_20_cases.json \
  --split all \
  --grading-mode host-sandbox \
  --concurrency 3 \
  --output-json eval_runs/latest_five_sigma.json \
  --output-md eval_runs/FIVE_SIGMA_REPORT.md
```

Optional: pass genuinely observed calibration iteration notes via `--calibration-findings <path/to/findings.json>`.
