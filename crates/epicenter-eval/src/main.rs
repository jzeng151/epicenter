use anyhow::Result;
use clap::{Parser, Subcommand};
use epicenter_common::{EvalArm, EvalIssueCase, EvalScorecard};
use epicenter_gemini::GeminiClient;
use epicenter_graph::parse_unified_diff;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "epicenter-eval", about = "5-Sigma evaluation harness for Epicenter")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Inspect configured evaluation arms and Gemini client readiness.
    Status,
    /// Load a JSON dataset of EvalIssueCase items and print a dry-run summary.
    Inspect {
        #[arg(long)]
        dataset: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = dotenvy::dotenv();
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();
    match cli.command {
        Commands::Status => {
            let gemini_ready = GeminiClient::from_env().is_ok();
            println!(
                "epicenter-eval ready | arms={:?} | gemini_api_key_set={}",
                EvalArm::all(),
                gemini_ready
            );
        }
        Commands::Inspect { dataset } => {
            let raw = std::fs::read_to_string(&dataset)?;
            let cases: Vec<EvalIssueCase> = serde_json::from_str(&raw)?;
            let touched_total: usize = cases
                .iter()
                .map(|c| parse_unified_diff(&c.ground_truth_test_patch).len())
                .sum();
            let scorecard = EvalScorecard::from_outcomes(&cases, &[]);
            println!(
                "Loaded {} cases ({} multimodal, {} ground-truth patch files)",
                scorecard.total_cases, scorecard.multimodal_cases, touched_total
            );
        }
    }
    Ok(())
}
