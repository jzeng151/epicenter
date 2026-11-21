# Epicenter 5-Sigma 3-Way Evaluation Report

## Run Metadata

| Field | Value |
| --- | --- |
| Run ID | `33b0822f-fbb7-4883-bef0-89f6847c6cc3` |
| Run Label | `live-five-sigma-jzeng151-calibration-host-verified` |
| Execution Mode | Live Gemini 4 API + Independent Host Sandbox Verification (`git apply` + real OS test exit codes) |
| Created At (UTC) | `2026-11-21T21:56:49.196919+00:00` |
| Dataset Breakdown | 5 total (5 Calibration, 0 Locked Unseen, 0 Multimodal) |

## 3-Way Scorecards

### All Cases (5)

| Evaluation Arm | Resolved | Regression-Free | Median Speedup | Mean Blast Risk (lower=safer) | Mean Root-Cause Depth | Median Duration |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Arm 1: Single Model Call | 0/5 (0.0%) | 60.0% | n/a | n/a | n/a | 22994 ms |
| Arm 2: Single Managed Agent | 0/5 (0.0%) | 100.0% | n/a | n/a | n/a | 209309 ms |
| Arm 3: Epicenter Tournament | 1/5 (20.0%) | 40.0% | n/a | 30.8 | 35.3 | 466567 ms |

### Locked Unseen Split (0)

_No outcomes recorded in this subset._

### Calibration Split (5)

| Evaluation Arm | Resolved | Regression-Free | Median Speedup | Mean Blast Risk (lower=safer) | Mean Root-Cause Depth | Median Duration |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| Arm 1: Single Model Call | 0/5 (0.0%) | 60.0% | n/a | n/a | n/a | 22994 ms |
| Arm 2: Single Managed Agent | 0/5 (0.0%) | 100.0% | n/a | n/a | n/a | 209309 ms |
| Arm 3: Epicenter Tournament | 1/5 (20.0%) | 40.0% | n/a | 30.8 | 35.3 | 466567 ms |

### Multimodal-Only Subset (0)

_No outcomes recorded in this subset._

## Calibration Findings (5-Issue Iteration Log)

_No calibration findings recorded for this run._

## Honest Remaining Failure Modes

- Arm 3: Epicenter Tournament — `empty_or_unparseable_diff` (1 case)
- Arm 3: Epicenter Tournament — `ground_truth_test_failed` (1 case)
- Arm 3: Epicenter Tournament — `regression_in_existing_suite` (2 cases)
- Arm 3: Epicenter Tournament — `test_tampering_violation` (1 case)
- Arm 2: Single Managed Agent — `empty_or_unparseable_diff` (5 cases)
- Arm 1: Single Model Call — `empty_or_unparseable_diff` (3 cases)
- Arm 1: Single Model Call — `patch_apply_conflict` (2 cases)

## Per-Case Outcome Matrix

| Case ID | Split | Media | Arm 1: Single Model | Arm 2: Single Agent | Arm 3: Epicenter Tournament |
| --- | --- | :---: | --- | --- | --- |
| `cal-hermes-lyre-58` | calibration | no | FAIL: `empty_or_unparseable_diff` (risk n/a) | FAIL: `empty_or_unparseable_diff` (risk n/a) | FAIL: `test_tampering_violation` (risk n/a) |
| `cal-hermes-lyre-56` | calibration | no | FAIL: `patch_apply_conflict` (risk n/a) | FAIL: `empty_or_unparseable_diff` (risk n/a) | FAIL: `empty_or_unparseable_diff` (risk n/a) |
| `cal-hermes-lyre-55` | calibration | no | FAIL: `empty_or_unparseable_diff` (risk n/a) | FAIL: `empty_or_unparseable_diff` (risk n/a) | FAIL: `regression_in_existing_suite` (risk 29) |
| `cal-hermes-lyre-57` | calibration | no | FAIL: `empty_or_unparseable_diff` (risk n/a) | FAIL: `empty_or_unparseable_diff` (risk n/a) | FAIL: `regression_in_existing_suite` (risk 46) |
| `cal-soilsense-99` | calibration | no | FAIL: `patch_apply_conflict` (risk n/a) | FAIL: `empty_or_unparseable_diff` (risk n/a) | FAIL: `ground_truth_test_failed` (risk 18) |

