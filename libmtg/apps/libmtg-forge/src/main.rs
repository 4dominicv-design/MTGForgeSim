use std::{fs, path::PathBuf};

use clap::{Parser, Subcommand};
use libmtg_decklist::Decklist;
use libmtg_forge::{
    audit, compare_jev_to_search, compare_openings, compare_search_smoke, generate_training_jsonl,
    jev_check_from_env, run_agent_smoke, run_agent_smoke_with_agent, run_matchup,
    run_paired_matchup, simulate_openings, JevForgeAgent,
};

#[derive(Parser)]
#[command(name = "forge-lab", about = "Mystic Forge simulation/experiment harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check deck size and rules-engine implementation coverage.
    Audit { deck: PathBuf },
    /// Deterministic opening-hand/London-mulligan profile (no gameplay claims).
    Openings {
        deck: PathBuf,
        #[arg(long, default_value_t = 100_000)] games: u64,
        #[arg(long, default_value_t = 1)] seed: u64,
    },
    /// Paired-seed A/B opening-hand comparison.
    Compare {
        deck_a: PathBuf,
        deck_b: PathBuf,
        #[arg(long, default_value_t = 100_000)] games: u64,
        #[arg(long, default_value_t = 1)] seed: u64,
    },
    /// Run actual games only when both mainboards have complete engine coverage.
    Matchup {
        forge: PathBuf,
        opponent: PathBuf,
        #[arg(long, default_value_t = 10_000)] games: u64,
        #[arg(long, default_value_t = 1)] seed: u64,
        #[arg(long, default_value_t = 12)] max_turns: u8,
    },
    /// Paired-seed full-game A/B comparison against one opponent.
    MatchupCompare {
        deck_a: PathBuf,
        deck_b: PathBuf,
        opponent: PathBuf,
        #[arg(long, default_value_t = 10_000)] games: u64,
        #[arg(long, default_value_t = 1)] seed: u64,
        #[arg(long, default_value_t = 12)] max_turns: u8,
    },
    /// Compare the baseline Forge pilot to the bounded rollout pilot against an inert opponent.
    /// Diagnostic only: inspect decision traces and final position scores before training.
    SearchSmoke {
        deck: PathBuf,
        #[arg(long, default_value_t = 1)] seed: u64,
        #[arg(long, default_value_t = 3)] max_turns: u8,
        #[arg(long, default_value_t = 4)] rollouts: usize,
    },
    /// Exercise the hidden-information-safe agent API against an inert opponent.
    /// Agents: heuristic (normal), pass (sanity check), invalid (validation/fallback test).
    AgentSmoke {
        deck: PathBuf,
        #[arg(long, default_value = "heuristic")] agent: String,
        #[arg(long, default_value_t = 1)] seed: u64,
        #[arg(long, default_value_t = 3)] max_turns: u8,
    },
    /// Verify JEV_API_KEY and show the TypeSafe models available to this account.
    JevCheck,
    /// Compare Jev and the cheap heuristic against the rollout teacher on the
    /// exact same hidden-info-safe observations.
    AgentCompare {
        deck: PathBuf,
        #[arg(long, default_value_t = 5)] games: u64,
        #[arg(long, default_value_t = 1)] seed: u64,
        #[arg(long, default_value_t = 3)] max_turns: u8,
        #[arg(long, default_value_t = 1)] rollouts: usize,
        #[arg(long, default_value_t = 20)] max_disagreements: usize,
    },
    /// Generate JSONL decisions labeled by the rollout-search teacher.
    GenerateTraining {
        deck: PathBuf,
        output: PathBuf,
        #[arg(long, default_value_t = 10_000)] games: u64,
        #[arg(long, default_value_t = 1)] seed: u64,
        #[arg(long, default_value_t = 3)] max_turns: u8,
        #[arg(long, default_value_t = 4)] rollouts: usize,
    },
}

fn load(path: &PathBuf) -> Result<Decklist, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let deck = Decklist::parse_text(&text);
    if deck.main_count() == 0 { return Err(format!("{}: no mainboard cards parsed", path.display())); }
    Ok(deck)
}

fn run_cli() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Command::Audit { deck } => {
            let d = load(&deck)?;
            println!("{}", serde_json::to_string_pretty(&audit(&d)).unwrap());
        }
        Command::Openings { deck, games, seed } => {
            let d = load(&deck)?;
            println!("{}", serde_json::to_string_pretty(&simulate_openings(&d, games, seed)).unwrap());
        }
        Command::Compare { deck_a, deck_b, games, seed } => {
            let a = load(&deck_a)?;
            let b = load(&deck_b)?;
            println!("{}", serde_json::to_string_pretty(&compare_openings(&a, &b, games, seed)).unwrap());
        }
        Command::Matchup { forge, opponent, games, seed, max_turns } => {
            let f = load(&forge)?;
            let o = load(&opponent)?;
            let result = run_matchup(&f, &o, games, seed, max_turns)?;
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Command::MatchupCompare { deck_a, deck_b, opponent, games, seed, max_turns } => {
            let a = load(&deck_a)?;
            let b = load(&deck_b)?;
            let o = load(&opponent)?;
            let result = run_paired_matchup(&a, &b, &o, games, seed, max_turns)?;
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Command::SearchSmoke { deck, seed, max_turns, rollouts } => {
            let d = load(&deck)?;
            let result = compare_search_smoke(&d, seed, max_turns, rollouts);
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Command::AgentSmoke { deck, agent, seed, max_turns } => {
            let d = load(&deck)?;
            let result = if agent == "jev" {
                run_agent_smoke_with_agent(&d, Box::new(JevForgeAgent::from_env()?), seed, max_turns)?
            } else {
                run_agent_smoke(&d, &agent, seed, max_turns)?
            };
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Command::JevCheck => {
            let result = jev_check_from_env()?;
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Command::AgentCompare { deck, games, seed, max_turns, rollouts, max_disagreements } => {
            let d = load(&deck)?;
            let result = compare_jev_to_search(&d, games, seed, max_turns, rollouts, max_disagreements)?;
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
        Command::GenerateTraining { deck, output, games, seed, max_turns, rollouts } => {
            let d = load(&deck)?;
            let result = generate_training_jsonl(&d, games, seed, max_turns, rollouts, &output)?;
            println!("{}", serde_json::to_string_pretty(&result).unwrap());
        }
    }
    Ok(())
}


fn main() -> Result<(), String> {
    // On Windows the executable main thread can have a comparatively small
    // stack. Search traverses nested rules/IR evaluators and state forks, so
    // run the CLI body on an explicitly-sized worker stack. This changes no
    // simulation semantics; it only prevents finite deep evaluator calls from
    // exhausting the process main stack.
    std::thread::Builder::new()
        .name("forge-lab".to_string())
        .stack_size(16 * 1024 * 1024)
        .spawn(run_cli)
        .map_err(|e| format!("failed to start forge-lab worker: {e}"))?
        .join()
        .map_err(|_| "forge-lab worker panicked".to_string())?
}
