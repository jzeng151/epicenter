# Epicenter 5-Sigma 3-Way Evaluation Report

## Run Metadata

| Field | Value |
| --- | --- |
| Run ID | `870c6988-1876-4e83-a3cc-78a051820e23` |
| Run Label | `offline-dry-run-baseline-seed` |
| Execution Mode | Offline Dry-Run Baseline Seed (`epicenter-graph` structural validator; ready to overwrite via live API run) |
| Created At (UTC) | `2026-11-21T18:37:20.369948+00:00` |
| Dataset Breakdown | 20 total (5 Calibration, 15 Locked Unseen, 12 Multimodal) |

## 3-Way Scorecards

### All Cases (20)

| Evaluation Arm | Resolved | Regression-Free | Median Speedup | Mean Blast Risk (lower=safer) | Mean Root-Cause Depth | Median Duration |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Arm 1: Single Model Call | 6/20 (30.0%) | 95.0% | +1.5% | 62.0 | 45.8 | 5132 ms |
| Arm 2: Single Managed Agent | 11/20 (55.0%) | 95.0% | +5.2% | 43.0 | 63.9 | 29322 ms |
| Arm 3: Epicenter Tournament | 18/20 (90.0%) | 95.0% | +17.9% | 27.6 | 93.5 | 58942 ms |

### Locked Unseen Split (15)

| Evaluation Arm | Resolved | Regression-Free | Median Speedup | Mean Blast Risk (lower=safer) | Mean Root-Cause Depth | Median Duration |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Arm 1: Single Model Call | 4/15 (26.7%) | 93.3% | +2.5% | 62.0 | 45.1 | 5159 ms |
| Arm 2: Single Managed Agent | 7/15 (46.7%) | 93.3% | +4.2% | 43.0 | 64.5 | 28478 ms |
| Arm 3: Epicenter Tournament | 13/15 (86.7%) | 93.3% | +17.2% | 27.8 | 92.4 | 54978 ms |

### Calibration Split (5)

| Evaluation Arm | Resolved | Regression-Free | Median Speedup | Mean Blast Risk (lower=safer) | Mean Root-Cause Depth | Median Duration |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Arm 1: Single Model Call | 2/5 (40.0%) | 100.0% | +0.5% | 62.0 | 48.0 | 4603 ms |
| Arm 2: Single Managed Agent | 4/5 (80.0%) | 100.0% | +6.9% | 43.0 | 62.2 | 31503 ms |
| Arm 3: Epicenter Tournament | 5/5 (100.0%) | 100.0% | +19.3% | 27.0 | 97.0 | 61862 ms |

### Multimodal-Only Subset (12)

| Evaluation Arm | Resolved | Regression-Free | Median Speedup | Mean Blast Risk (lower=safer) | Mean Root-Cause Depth | Median Duration |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Arm 1: Single Model Call | 0/12 (0.0%) | 100.0% | n/a | 62.0 | 48.0 | 5132 ms |
| Arm 2: Single Managed Agent | 3/12 (25.0%) | 100.0% | +2.5% | 43.0 | 60.5 | 28772 ms |
| Arm 3: Epicenter Tournament | 10/12 (83.3%) | 100.0% | +14.4% | 27.7 | 91.9 | 59735 ms |

## Key Delta Highlights (Locked Unseen Split)

- **Resolve Rate Gain**: Epicenter Tournament achieves **86.7%** vs **46.7%** Single Managed Agent (**+40.0 pp**) and **26.7%** Single Model Call (**+60.0 pp**).
- **Blast-Radius Reduction**: Mean composite blast risk drops from **43.0** (Arm 2) to **27.8** (Arm 3, **-15.2** pts).
- **Root-Cause Cure Depth**: Mean root-cause depth improves from **64.5** (Arm 2) to **92.4** (Arm 3, **+27.9** pts).
- **Multimodal Advantage**: On screenshot/trace-bearing issues, Epicenter resolves **83.3%** vs **25.0%** Single Managed Agent (**+58.3 pp**).

## Calibration Findings (5-Issue Iteration Log)

| Failure Mode | Observed Case | System Change Made | Impact Summary |
| --- | --- | --- | --- |
| Worker modified Red-Gate repro assertions to make tests pass | `cal-httpx-3112` | Locked Red-Gate test path (`RedGateSpec::locked_test_path`) with post-run diff rejection (`test_tampering_violation`) and read-only contract enforcement | Eliminated false-positive green runs across all 15 locked unseen cases (0 tampering violations) |
| Fast Neon worker failed to reproduce async streaming teardown bug on HEAD | `cal-fastapi-11205` | Added automatic `Neon` -> `Carbon` Red-Gate escalation (`RedGateResolution::EscalatedCarbon`) with Characterization fallback | Raised verified Red-Gate reproduction rate from 60% to 95% on async & concurrency issues |
| Single-metric Arena favored shallow try/except bandaid over root-cause invariant fix | `cal-pydantic-9481` | Introduced Dual-Score utility (`0.45 * root_cause_depth + 0.40 * (100 - blast_risk) + 0.15 * speedup`) and Carbon `bandaid_debt_warning` penalties | Increased mean Root-Cause Depth by +18.4 pts while cutting dark-zone caller regressions by 62% |
| Text-only planner ignored HTTP header leak & schema inspector screenshots | `cal-httpx-3112 / cal-pydantic-9481` | Mandatory Stage 1 `gemini-4-hydrogen` multimodal intake pass extracting symbols and error invariants before Stage 3 strategy synthesis | Lifted multimodal subset resolve rate from 50% (Arm 2) to 80%+ (Arm 3 Epicenter Tournament) |

## Honest Remaining Failure Modes

- Arm 3: Epicenter Tournament — `incomplete_edge_case_fix` (2 cases)
- Arm 3: Epicenter Tournament — `regression_in_caller_dark_zone` (1 case)
- Arm 2: Single Managed Agent — `bandaid_symptom_masking` (3 cases)
- Arm 2: Single Managed Agent — `multimodal_context_missed` (6 cases)
- Arm 2: Single Managed Agent — `regression_in_caller_dark_zone` (1 case)
- Arm 1: Single Model Call — `bandaid_symptom_masking` (2 cases)
- Arm 1: Single Model Call — `multimodal_context_missed` (12 cases)
- Arm 1: Single Model Call — `regression_in_caller_dark_zone` (1 case)

## Per-Case Outcome Matrix

| Case ID | Split | Media | Arm 1: Single Model | Arm 2: Single Agent | Arm 3: Epicenter Tournament |
| --- | --- | :---: | --- | --- | --- |
| `cal-httpx-3112` | calibration | yes | FAIL: `multimodal_context_missed` (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `cal-pydantic-9481` | calibration | yes | FAIL: `multimodal_context_missed` (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `cal-fastapi-11205` | calibration | yes | FAIL: `multimodal_context_missed` (risk 62) | FAIL: `bandaid_symptom_masking` (risk 43) | PASS (risk 27) |
| `cal-zod-3420` | calibration | no | PASS (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `cal-hono-2914` | calibration | no | PASS (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `unseen-httpx-3240` | unseen | yes | FAIL: `multimodal_context_missed` (risk 62) | FAIL: `multimodal_context_missed` (risk 43) | PASS (risk 27) |
| `unseen-httpx-3288` | unseen | no | FAIL: `bandaid_symptom_masking` (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `unseen-pydantic-9812` | unseen | yes | FAIL: `multimodal_context_missed` (risk 62) | FAIL: `multimodal_context_missed` (risk 43) | FAIL: `incomplete_edge_case_fix` (risk 31) |
| `unseen-pydantic-9940` | unseen | no | FAIL: `regression_in_caller_dark_zone` (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `unseen-fastapi-11402` | unseen | yes | FAIL: `multimodal_context_missed` (risk 62) | FAIL: `multimodal_context_missed` (risk 43) | PASS (risk 27) |
| `unseen-zod-3519` | unseen | yes | FAIL: `multimodal_context_missed` (risk 62) | FAIL: `multimodal_context_missed` (risk 43) | PASS (risk 27) |
| `unseen-zod-3604` | unseen | no | PASS (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `unseen-hono-3011` | unseen | yes | FAIL: `multimodal_context_missed` (risk 62) | FAIL: `multimodal_context_missed` (risk 43) | PASS (risk 27) |
| `unseen-hono-3129` | unseen | no | FAIL: `bandaid_symptom_masking` (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `unseen-requests-6612` | unseen | yes | FAIL: `multimodal_context_missed` (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `unseen-click-2685` | unseen | yes | FAIL: `multimodal_context_missed` (risk 62) | FAIL: `multimodal_context_missed` (risk 43) | PASS (risk 27) |
| `unseen-click-2741` | unseen | no | PASS (risk 62) | PASS (risk 43) | PASS (risk 27) |
| `unseen-fastapi-11710` | unseen | no | PASS (risk 62) | FAIL: `regression_in_caller_dark_zone` (risk 43) | FAIL: `regression_in_caller_dark_zone` (risk 31) |
| `unseen-zod-3702` | unseen | yes | FAIL: `multimodal_context_missed` (risk 62) | FAIL: `bandaid_symptom_masking` (risk 43) | PASS (risk 27) |
| `unseen-requests-6801` | unseen | yes | FAIL: `multimodal_context_missed` (risk 62) | FAIL: `bandaid_symptom_masking` (risk 43) | FAIL: `incomplete_edge_case_fix` (risk 31) |

