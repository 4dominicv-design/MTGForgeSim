use std::{fs::{self, File}, io::{BufWriter, Write}, path::{Path, PathBuf}, sync::Arc};
use libmtg_decklist::Decklist;
use libmtg_engine::{build_catalog, run_game, GameEvent, Objective, PlayerId, Scenario, SimState, Strategy};
use rand::{rngs::SmallRng, SeedableRng};
use serde::{Deserialize, Serialize};
use crate::{lookahead::{LookaheadConfig, MatchupLookaheadStrategy, LOOKAHEAD_VERSION},
    matchup::ensure_supported, strategy::{BaselineOpponentStrategy, ForgeStrategy}};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeckSpec { pub name: String, pub path: PathBuf }
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComparisonManifest { pub builds: Vec<DeckSpec>, pub opponents: Vec<DeckSpec> }
#[derive(Clone, Copy, Debug, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PilotSelection { Heuristic, Lookahead, Both }
#[derive(Clone, Debug, Serialize)]
pub struct ComparisonConfig {
    pub games: u64, pub seed: u64, pub max_turns: u8,
    pub pilot: PilotSelection, pub search: LookaheadConfig, pub trace: bool,
}
impl ComparisonConfig {
    fn validate(&self) -> Result<(), String> {
        if self.games < 2 || self.games % 2 != 0 { return Err("--games must be a positive even number (at least 2) for balanced play/draw".into()); }
        if self.max_turns == 0 { return Err("--max-turns must be positive".into()); }
        self.search.validate()
    }
}
#[derive(Serialize)]
struct DeckSnapshot { name: String, path: PathBuf, fingerprint: String, text: String, parsed: Decklist }
#[derive(Default, Debug, Serialize)]
pub struct Counts { pub wins: u64, pub losses: u64, pub draws: u64, pub unresolved: u64, pub invalid: u64 }
impl Counts {
    fn add(&mut self, outcome: &str) {
        match outcome { "win" => self.wins += 1, "loss" => self.losses += 1,
            "draw" => self.draws += 1, "unresolved" => self.unresolved += 1, _ => self.invalid += 1 }
    }
}
#[derive(Serialize)]
pub struct CellResult {
    pub build: String, pub opponent: String, pub pilot: String,
    pub counts: Counts, pub on_play: Counts, pub on_draw: Counts,
    pub win_pct_decided: Option<f64>, pub wilson_95_pct_decided: Option<[f64; 2]>,
    pub observed_wins_pct_scheduled: Option<f64>,
    pub search_decisions_written: u64,
}
#[derive(Serialize)]
pub struct ComparisonReport {
    schema: &'static str, pub complete: bool,
    source_fingerprint: &'static str, git_revision: &'static str,
    package_version: &'static str, search_version: &'static str,
    opponent_pilot: &'static str, information_model: &'static str,
    scheduling: &'static str, interval_method: &'static str,
    pub config: ComparisonConfig,
    builds: Vec<DeckSnapshot>, opponents: Vec<DeckSnapshot>,
    pub results: Vec<CellResult>,
}
fn fingerprint(bytes: &[u8]) -> String {
    let hash = bytes.iter().fold(0xcbf29ce484222325u64, |h, &b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("fnv1a64:{hash:016x}")
}
pub fn wilson(wins: u64, total: u64) -> Option<[f64; 2]> {
    if total == 0 { return None; }
    let n = total as f64; let p = wins as f64 / n; let z2 = 1.96 * 1.96;
    let centre = (p + z2 / (2.0*n)) / (1.0 + z2/n);
    let radius = 1.96 * ((p*(1.0-p)/n + z2/(4.0*n*n)).sqrt()) / (1.0 + z2/n);
    Some([100.0*(centre-radius).max(0.0), 100.0*(centre+radius).min(1.0)])
}
fn load_specs(specs: &[DeckSpec], base: &Path) -> Result<Vec<DeckSnapshot>, String> {
    let mut names = std::collections::HashSet::new();
    specs.iter().map(|spec| {
        if spec.name.trim().is_empty() || !names.insert(spec.name.clone()) { return Err("deck names must be nonempty and unique within their group".into()); }
        let path = base.join(&spec.path);
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let parsed = Decklist::parse_text(&text);
        if parsed.main_count() == 0 { return Err(format!("{}: empty mainboard", path.display())); }
        Ok(DeckSnapshot { name: spec.name.clone(), path, fingerprint: fingerprint(text.as_bytes()), text, parsed })
    }).collect()
}
#[derive(Default)]
struct MatchObjective;
impl Objective for MatchObjective { fn observe(&mut self, _: &GameEvent, _: &mut SimState) -> bool { false } }
fn seed_for(seed: u64, index: u64) -> u64 {
    let mut x = seed.wrapping_add(index).wrapping_add(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}
fn write_jsonl(out: &mut impl Write, value: &serde_json::Value) -> Result<(), String> {
    serde_json::to_writer(&mut *out, value).map_err(|e| e.to_string())?;
    out.write_all(b"\n").map_err(|e| e.to_string())
}
fn save_report(dir: &Path, report: &ComparisonReport) -> Result<(), String> {
    fs::write(dir.join("report.json"), serde_json::to_vec_pretty(report).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
}

/// Matrix of standalone preboard games; no sideboarding or match-win claims.
/// Refuses to overwrite an existing output directory.
pub fn compare_decks(manifest_path: &Path, output: &Path, cfg: ComparisonConfig) -> Result<ComparisonReport, String> {
    cfg.validate()?;
    let manifest_text = fs::read_to_string(manifest_path).map_err(|e| e.to_string())?;
    let manifest: ComparisonManifest = serde_json::from_str(&manifest_text).map_err(|e| e.to_string())?;
    if manifest.builds.len() < 2 || manifest.opponents.is_empty() { return Err("manifest needs at least two builds and one opponent".into()); }
    let base = manifest_path.parent().unwrap_or(Path::new("."));
    let builds = load_specs(&manifest.builds, base)?;
    let opponents = load_specs(&manifest.opponents, base)?;
    let catalog = build_catalog();
    for d in builds.iter().chain(&opponents) { ensure_supported(&d.name, &d.parsed, &catalog)?; }
    let mut report = ComparisonReport {
        schema: "forge_comparison_v1", complete: false, source_fingerprint: env!("FORGE_SOURCE_FINGERPRINT"),
        git_revision: env!("FORGE_GIT_REVISION"), package_version: env!("CARGO_PKG_VERSION"), search_version: LOOKAHEAD_VERSION,
        opponent_pilot: "baseline_opponent_v1_scripted_not_competitive",
        information_model: "open decklists; hidden hand allocation and unknown library order sampled for lookahead",
        scheduling: "identical seed_for(master,index) and alternating play/draw across every build and pilot; mulligans can diverge",
        interval_method: "95% Wilson on wins/(wins+losses), conditional on decided games; suppressed if invalid games occur",
        config: cfg.clone(), builds, opponents, results: Vec::new(),
    };
    fs::create_dir(output).map_err(|e| format!("cannot create new output directory {}: {e}", output.display()))?;
    save_report(output, &report)?;
    let mut games_file = BufWriter::new(File::create(output.join("games.jsonl")).map_err(|e| e.to_string())?);
    let mut decisions_file = BufWriter::new(File::create(output.join("decisions.jsonl")).map_err(|e| e.to_string())?);
    let pilots: &[&str] = match cfg.pilot { PilotSelection::Heuristic => &["heuristic"], PilotSelection::Lookahead => &["lookahead"], PilotSelection::Both => &["heuristic", "lookahead"] };
    for opponent in &report.opponents {
        for build in &report.builds {
            for &pilot in pilots {
                let mut cell = CellResult { build: build.name.clone(), opponent: opponent.name.clone(), pilot: pilot.into(),
                    counts: Counts::default(), on_play: Counts::default(), on_draw: Counts::default(),
                    win_pct_decided: None, wilson_95_pct_decided: None, observed_wins_pct_scheduled: None, search_decisions_written: 0 };
                for i in 0..cfg.games {
                    let seed = seed_for(cfg.seed, i); let on_play = i % 2 == 0;
                    let strategy: Box<dyn Strategy> = if pilot == "lookahead" { Box::new(MatchupLookaheadStrategy::new(PlayerId::Us, cfg.search, seed)) }
                        else { Box::new(ForgeStrategy::new(PlayerId::Us)) };
                    let mut rng = SmallRng::seed_from_u64(seed);
                    let state = run_game(Scenario {
                        us_label: build.name.clone(), opp_label: opponent.name.clone(), catalog: catalog.clone(),
                        us_deck: build.parsed.to_engine_deck(), opp_deck: opponent.parsed.to_engine_deck(),
                        us_strategy: strategy, opp_strategy: Box::new(BaselineOpponentStrategy::new(PlayerId::Opp)),
                        evaluate_card: Arc::new(|_, _, _| 0.5), objective: Box::<MatchObjective>::default(),
                        max_turns: cfg.max_turns, on_play: Some(on_play), fixed_us_hand: None,
                    }, &mut rng);
                    let outcome = if state.invalid_actions > 0 { "invalid" } else { match state.winner {
                        Some(PlayerId::Us) => "win", Some(PlayerId::Opp) => "loss", None if state.terminal => "draw", None => "unresolved" } };
                    cell.counts.add(outcome);
                    if on_play { cell.on_play.add(outcome); } else { cell.on_draw.add(outcome); }
                    let mut record = serde_json::json!({"build": build.name, "opponent": opponent.name, "pilot": pilot,
                        "game_index": i, "game_seed": seed, "on_play": on_play, "outcome": outcome,
                        "turn_reached": state.current_turn, "invalid_actions": state.invalid_actions,
                        "source_fingerprint": env!("FORGE_SOURCE_FINGERPRINT")});
                    if outcome != "invalid" {
                        for line in &state.decision_log {
                            if let Some(json) = line.strip_prefix("MATCHUP_SEARCH\t") {
                                let decision: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
                                let mut training = record.clone();
                                training["decision"] = decision;
                                training["terminal_outcome_available"] = serde_json::json!(outcome != "unresolved");
                                write_jsonl(&mut decisions_file, &training)?;
                                cell.search_decisions_written += 1;
                            }
                        }
                    }
                    if cfg.trace { record["log"] = serde_json::json!(state.log); }
                    write_jsonl(&mut games_file, &record)?;
                    games_file.flush().map_err(|e| e.to_string())?;
                    decisions_file.flush().map_err(|e| e.to_string())?;
                    eprintln!("[compare-decks] {} vs {} / {}: {}/{} ({})", build.name, opponent.name, pilot, i+1, cfg.games, outcome);
                }
                let decided = cell.counts.wins + cell.counts.losses;
                if cell.counts.invalid == 0 {
                    cell.wilson_95_pct_decided = wilson(cell.counts.wins, decided);
                    cell.win_pct_decided = (decided > 0).then(|| 100.0*cell.counts.wins as f64/decided as f64);
                    cell.observed_wins_pct_scheduled = Some(100.0*cell.counts.wins as f64/cfg.games as f64);
                }
                report.results.push(cell);
                save_report(output, &report)?;
            }
        }
    }
    report.complete = true;
    save_report(output, &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn intervals_and_input_validation() {
        assert!(wilson(0,0).is_none());
        let bounds = wilson(50,100).unwrap();
        assert!((bounds[0]-40.383).abs()<0.01 && (bounds[1]-59.617).abs()<0.01);
        assert_eq!(wilson(0,100).unwrap()[0],0.0);
        assert!((wilson(100,100).unwrap()[1]-100.0).abs()<1e-9);
        let mut cfg = ComparisonConfig { games: 2, seed: 1, max_turns: 20, pilot: PilotSelection::Both, search: LookaheadConfig::default(), trace: false };
        assert!(cfg.validate().is_ok()); cfg.games=3; assert!(cfg.validate().is_err());
    }
}
