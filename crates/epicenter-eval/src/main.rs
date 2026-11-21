use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use epicenter_common::{EvalArm, EvalIssueCase, EvalScorecard, FiveSigmaEval};
use epicenter_eval::grader::GradingMode;
use epicenter_eval::harvester::{
    harvest_repositories, harvest_single_pr, HarvestConfig, DEFAULT_TARGET_REPOS,
};
use epicenter_eval::report::{compute_subset_scorecards, write_eval_artifacts};
use epicenter_eval::runner::{
    load_calibration_findings, run_evaluation, validate_dry_run_plan, RunConfig, SplitFilter,
};
use epicenter_eval::verify::{ensure_verify_passed, verify_cases};
use epicenter_gemini::GeminiClient;
use epicenter_graph::parse_unified_diff;
use std::path::{Path, PathBuf};

#[derive(Debug, Parser)]
#[command(
    name = "epicenter-eval",
    about = "5-Sigma 3-way GitHub PR evaluation harness, dataset harvester, verifier, and independent host grader for Epicenter"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Inspect configured evaluation arms and Gemini/GitHub API readiness.
    Status,
    /// Load a JSON dataset of EvalIssueCase items and print a validation summary.
    Inspect {
        #[arg(long, default_value = "eval_runs/five_sigma_20_cases.json")]
        dataset: PathBuf,
    },
    /// Audit a dataset JSON for synthetic commit SHAs, fake diff indices, placeholder media URLs,
    /// and (with --check-github) verify every commit, merged PR, and media URL live against GitHub.
    Verify {
        #[arg(long, default_value = "eval_runs/five_sigma_20_cases.json")]
        dataset: PathBuf,
        /// Query GitHub REST API and issue HTTP requests to verify every commit, merged PR, and media URL.
        #[arg(long, default_value_t = false)]
        check_github: bool,
    },
    /// Harvest real merged bug-fix PRs from external Python & TypeScript OSS repositories (or a single PR URL).
    Harvest {
        /// Destination JSON path for harvested EvalIssueCase records.
        #[arg(long, default_value = "eval_runs/harvested.json")]
        output: PathBuf,
        /// Optional single merged GitHub PR URL to harvest as an EvalSplit::LiveJudge case.
        #[arg(long)]
        pr: Option<String>,
        /// Comma-separated owner/repo list (defaults to 12 Python & TypeScript OSS repos).
        #[arg(long)]
        repos: Option<String>,
        /// Maximum cases harvested per repository.
        #[arg(long, default_value_t = 3)]
        max_per_repo: usize,
        /// Maximum total cases to harvest across all repositories.
        #[arg(long, default_value_t = 20)]
        max_cases: usize,
        /// Number of cases tagged as Calibration (remaining tagged LockedUnseen).
        #[arg(long, default_value_t = 5)]
        calibration_count: usize,
    },
    /// Run the live 3-way 5-Sigma evaluation across SingleModelCall, SingleManagedAgent, and EpicenterTournament.
    Run {
        #[arg(long, default_value = "eval_runs/five_sigma_20_cases.json")]
        dataset: PathBuf,
        #[arg(long, value_enum, default_value_t = SplitFilter::All)]
        split: SplitFilter,
        #[arg(long, default_value_t = 3)]
        concurrency: usize,
        /// Independent grading backend (defaults to HostSandbox).
        #[arg(long, value_enum, default_value_t = GradingMode::HostSandbox)]
        grading_mode: GradingMode,
        /// ONLY validate dataset integrity and print the planned live run matrix (never synthesizes outcomes or writes report artifacts).
        #[arg(long, default_value_t = false)]
        dry_run: bool,
        /// Optional path to JSON file containing genuinely observed CalibrationFinding entries.
        #[arg(long)]
        calibration_findings: Option<PathBuf>,
        #[arg(long, default_value = "eval_runs/latest_five_sigma.json")]
        output_json: PathBuf,
        #[arg(long, default_value = "eval_runs/FIVE_SIGMA_REPORT.md")]
        output_md: PathBuf,
        #[arg(long)]
        label: Option<String>,
    },
    /// Render FIVE_SIGMA_REPORT.md and normalized JSON from an existing live FiveSigmaEval JSON artifact.
    Report {
        #[arg(long, default_value = "eval_runs/latest_five_sigma.json")]
        input: PathBuf,
        #[arg(long, default_value = "eval_runs/latest_five_sigma.json")]
        output_json: PathBuf,
        #[arg(long, default_value = "eval_runs/FIVE_SIGMA_REPORT.md")]
        output_md: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "epicenter_eval=info".into()),
        )
        .init();

    let cli = Cli::parse();
    let github_token = std::env::var("GITHUB_TOKEN")
        .ok()
        .filter(|s| !s.trim().is_empty());

    match cli.command {
        Commands::Status => {
            let gemini_ready = GeminiClient::from_env().is_ok();
            println!(
                "epicenter-eval ready | arms={:?} | default_grader=HostSandbox | gemini_api_key_set={} | github_token_set={}",
                EvalArm::all(),
                gemini_ready,
                github_token.is_some()
            );
        }
        Commands::Inspect { dataset } => {
            let raw = std::fs::read_to_string(&dataset)
                .with_context(|| format!("failed to read dataset {}", dataset.display()))?;
            let cases: Vec<EvalIssueCase> = serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse dataset {}", dataset.display()))?;
            let touched_total: usize = cases
                .iter()
                .map(|c| parse_unified_diff(&c.ground_truth_test_patch).len())
                .sum();
            let scorecard = EvalScorecard::from_outcomes(&cases, &[]);
            println!(
                "Loaded {} cases ({} calibration, {} unseen, {} multimodal, {} ground-truth test files)",
                scorecard.total_cases,
                scorecard.calibration_cases,
                scorecard.unseen_cases,
                scorecard.multimodal_cases,
                touched_total
            );
        }
        Commands::Verify {
            dataset,
            check_github,
        } => {
            let raw = std::fs::read_to_string(&dataset)
                .with_context(|| format!("failed to read dataset {}", dataset.display()))?;
            let cases: Vec<EvalIssueCase> = serde_json::from_str(&raw)
                .with_context(|| format!("failed to parse dataset {}", dataset.display()))?;

            let report = verify_cases(&cases, check_github, github_token.as_deref()).await?;
            println!(
                "Dataset Verification ({}) | {}/{} passed | multimodal={} | verified_media_urls={}",
                if check_github {
                    "static + live GitHub API"
                } else {
                    "static integrity"
                },
                report.passed_cases,
                report.total_cases,
                report.multimodal_cases,
                report.verified_media_urls
            );
            for st in &report.case_statuses {
                let short_sha = &st.parent_commit[..st.parent_commit.len().min(8)];
                let pr_str = st
                    .pr_number
                    .map(|n| format!("#{n}"))
                    .unwrap_or_else(|| "none".to_string());
                if st.passed {
                    println!(
                        "  [PASS] {:<28} {:<8} {} media={} {}",
                        st.case_id, pr_str, short_sha, st.media_count, st.repo_url
                    );
                } else {
                    println!(
                        "  [FAIL] {:<28} {:<8} {} -> {}",
                        st.case_id,
                        pr_str,
                        short_sha,
                        st.errors.join("; ")
                    );
                }
            }
            ensure_verify_passed(&report)?;
        }
        Commands::Harvest {
            output,
            pr,
            repos,
            max_per_repo,
            max_cases,
            calibration_count,
        } => {
            let cases = if let Some(pr_url) = pr {
                let single = harvest_single_pr(&pr_url, github_token.as_deref()).await?;
                vec![single]
            } else {
                let repo_list = match repos {
                    Some(raw) => raw
                        .split(',')
                        .map(str::trim)
                        .filter(|s| !s.is_empty())
                        .map(String::from)
                        .collect(),
                    None => DEFAULT_TARGET_REPOS.iter().map(|s| s.to_string()).collect(),
                };
                let cfg = HarvestConfig {
                    repos: repo_list,
                    max_prs_per_repo: max_per_repo,
                    max_total_cases: max_cases,
                    calibration_count,
                    prefer_multimodal: true,
                    github_token,
                };
                harvest_repositories(&cfg).await?
            };

            if let Some(parent) = output.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)?;
                }
            }
            let json = serde_json::to_string_pretty(&cases)?;
            std::fs::write(&output, json)
                .with_context(|| format!("failed to write {}", output.display()))?;
            let mm = cases.iter().filter(|c| c.has_multimodal_media).count();
            println!(
                "Harvested {} verified real cases ({} multimodal) -> {}",
                cases.len(),
                mm,
                output.display()
            );
        }
        Commands::Run {
            dataset,
            split,
            concurrency,
            grading_mode,
            dry_run,
            calibration_findings,
            output_json,
            output_md,
            label,
        } => {
            let raw = std::fs::read_to_string(&dataset)
                .with_context(|| format!("failed to read dataset {}", dataset.display()))?;
            let cases: Vec<EvalIssueCase> = serde_json::from_str(&raw)?;
            let loaded_findings = load_calibration_findings(calibration_findings.as_deref())?;

            let run_cfg = RunConfig {
                split,
                concurrency,
                dry_run,
                grading_mode,
                label,
                github_token,
                calibration_findings: loaded_findings,
            };

            if dry_run {
                let plan = validate_dry_run_plan(&cases, &run_cfg)?;
                print!("{}", plan.render_summary());
                return Ok(());
            }

            let eval = run_evaluation(&cases, &run_cfg).await?;
            write_eval_artifacts(&eval, &output_json, &output_md)?;
            print_scorecard_summary(&eval, &output_json, &output_md);
        }
        Commands::Report {
            input,
            output_json,
            output_md,
        } => {
            let raw = std::fs::read_to_string(&input).with_context(|| {
                format!(
                    "failed to read live evaluation artifact {} (run `epicenter-eval run` to generate real live results)",
                    input.display()
                )
            })?;
            let eval: FiveSigmaEval = serde_json::from_str(&raw).with_context(|| {
                format!("failed to parse FiveSigmaEval JSON from {}", input.display())
            })?;

            write_eval_artifacts(&eval, &output_json, &output_md)?;
            print_scorecard_summary(&eval, &output_json, &output_md);
        }
    }

    Ok(())
}

fn print_scorecard_summary(eval: &FiveSigmaEval, json_path: &Path, md_path: &Path) {
    let subsets = compute_subset_scorecards(eval);
    println!(
        "5-Sigma Evaluation Complete | run_id={} | label={} | cases={} (cal={}, unseen={}, mm={})",
        eval.run_id,
        eval.label,
        subsets.all.total_cases,
        subsets.all.calibration_cases,
        subsets.all.unseen_cases,
        subsets.all.multimodal_cases
    );
    for m in &subsets.all.arm_metrics {
        println!(
            "  - {:<30} resolved={}/{} ({:.1}%) | reg_free={:.1}% | blast={} | rc_depth={}",
            m.arm.label(),
            m.resolved_count,
            m.total_cases,
            m.resolved_rate_pct,
            m.regression_free_rate_pct,
            m.mean_blast_risk_score
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "n/a".to_string()),
            m.mean_root_cause_depth
                .map(|v| format!("{v:.1}"))
                .unwrap_or_else(|| "n/a".to_string())
        );
    }
    println!(
        "Wrote artifacts: {} and {}",
        json_path.display(),
        md_path.display()
    );
}
