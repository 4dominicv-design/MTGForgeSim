use std::{collections::BTreeMap, env, time::{Duration, Instant}};

use libmtg_decklist::Decklist;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::agent::{
    AgentAnnouncementDecision, AgentAnnouncementObservation, AgentDecision, AgentObservation,
    AgentWishObservation, ForgeAgent, HeuristicForgeAgent,
};
use crate::search::run_search_smoke_once;

const DEFAULT_BASE_URL: &str = "https://api.typesafe.ai";
const DEFAULT_MODEL: &str = "jev-latest";
const DEFAULT_TIMEOUT_SECONDS: u64 = 20;
/// TypeSafe public launch price on 2026-09-26. Override with
/// JEV_INPUT_COST_PER_MTOK if pricing changes.
const DEFAULT_INPUT_COST_PER_MTOK: f64 = 0.042;

#[derive(Debug, Clone)]
pub struct JevConfig {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
    pub timeout_seconds: u64,
    pub input_cost_per_mtok: f64,
}

impl JevConfig {
    pub fn from_env() -> Result<Self, String> {
        let api_key = env::var("JEV_API_KEY")
            .map_err(|_| "JEV_API_KEY is not set. Create a TypeSafe API key, then set $env:JEV_API_KEY on PowerShell.".to_string())?;
        if api_key.trim().is_empty() {
            return Err("JEV_API_KEY is empty".to_string());
        }
        let timeout_seconds = env::var("JEV_TIMEOUT_SECONDS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(DEFAULT_TIMEOUT_SECONDS);
        let input_cost_per_mtok = env::var("JEV_INPUT_COST_PER_MTOK")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(DEFAULT_INPUT_COST_PER_MTOK);
        Ok(Self {
            api_key,
            base_url: env::var("JEV_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
            model: env::var("JEV_MODEL").unwrap_or_else(|_| DEFAULT_MODEL.to_string()),
            timeout_seconds,
            input_cost_per_mtok,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JevMetric {
    pub decision_kind: String,
    pub model: Option<String>,
    pub latency_ms: u64,
    pub confidence: Option<f64>,
    /// Probabilities remapped from short API choice keys to engine action ids.
    pub probabilities: BTreeMap<String, f64>,
    pub selected_id: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub error: Option<String>,
}

impl JevMetric {
    fn error(kind: &str, latency_ms: u64, message: String) -> Self {
        Self {
            decision_kind: kind.to_string(), model: None, latency_ms, confidence: None,
            probabilities: BTreeMap::new(), selected_id: None,
            input_tokens: 0, output_tokens: 0, error: Some(message),
        }
    }
}

#[derive(Debug, Deserialize)]
struct SystemOneResponse {
    model: String,
    answers: BTreeMap<String, ChoiceAnswer>,
    usage: Usage,
}

#[derive(Debug, Deserialize)]
struct ChoiceAnswer {
    #[allow(dead_code)]
    #[serde(rename = "type")]
    kind: String,
    choice: String,
    confidence: f64,
    probabilities: BTreeMap<String, f64>,
}

#[derive(Debug, Deserialize)]
struct Usage {
    input_tokens: u64,
    output_tokens: u64,
}

#[derive(Debug, Serialize)]
pub struct JevModelInfo {
    pub name: String,
    pub description: String,
    pub release_date: String,
}

#[derive(Debug, Serialize)]
pub struct JevCheckSummary {
    pub base_url: String,
    pub requested_model: String,
    pub model_available: bool,
    pub models: Vec<JevModelInfo>,
}

#[derive(Debug, Deserialize)]
struct ModelsResponse { models: Vec<JevModelInfoWire> }
#[derive(Debug, Deserialize)]
struct JevModelInfoWire { name: String, description: String, release_date: String }

#[derive(Clone)]
struct ChoiceSpec {
    short_key: String,
    external_id: String,
    description: String,
}

pub struct JevForgeAgent {
    cfg: JevConfig,
    http: ureq::Agent,
    metrics: Vec<JevMetric>,
}

impl JevForgeAgent {
    pub fn new(cfg: JevConfig) -> Self {
        let http = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(cfg.timeout_seconds.max(1)))
            .build();
        Self { cfg, http, metrics: Vec::new() }
    }

    pub fn from_env() -> Result<Self, String> { Ok(Self::new(JevConfig::from_env()?)) }

    pub fn config(&self) -> &JevConfig { &self.cfg }

    pub fn take_metrics(&mut self) -> Vec<JevMetric> { std::mem::take(&mut self.metrics) }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.cfg.base_url.trim_end_matches('/'), path.trim_start_matches('/'))
    }

    fn post_choice(&mut self, kind: &str, state: Value, instructions: &str, choices: Vec<ChoiceSpec>) -> Result<(String, JevMetric), String> {
        if choices.is_empty() { return Err("no choices supplied to Jev".to_string()); }
        if choices.len() > 255 { return Err(format!("{} choices exceed Jev's 255-choice limit", choices.len())); }

        let mut criteria = serde_json::Map::new();
        for choice in &choices {
            criteria.insert(choice.short_key.clone(), Value::String(choice.description.clone()));
        }
        let payload = json!({
            "state": state,
            "model": self.cfg.model,
            "questions": {
                "decision": {
                    "type": "choice",
                    "instructions": instructions,
                    "criteria": criteria
                }
            }
        });

        let started = Instant::now();
        let response = self.http
            .post(&self.endpoint("v1/systemone"))
            .set("Authorization", &format!("Bearer {}", self.cfg.api_key))
            .set("Content-Type", "application/json")
            .send_json(payload);
        let latency_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        let response = match response {
            Ok(r) => r,
            Err(e) => {
                let msg = format!("Jev request failed: {e}");
                self.metrics.push(JevMetric::error(kind, latency_ms, msg.clone()));
                return Err(msg);
            }
        };
        let decoded: SystemOneResponse = match response.into_json() {
            Ok(v) => v,
            Err(e) => {
                let msg = format!("Jev response JSON failed: {e}");
                self.metrics.push(JevMetric::error(kind, latency_ms, msg.clone()));
                return Err(msg);
            }
        };
        let answer = decoded.answers.get("decision")
            .ok_or_else(|| "Jev response omitted answers.decision".to_string())?;
        let chosen = choices.iter().find(|c| c.short_key == answer.choice)
            .ok_or_else(|| format!("Jev returned unknown choice key '{}'", answer.choice))?;
        let mut probabilities = BTreeMap::new();
        for (key, probability) in &answer.probabilities {
            if let Some(spec) = choices.iter().find(|c| &c.short_key == key) {
                probabilities.insert(spec.external_id.clone(), *probability);
            }
        }
        let metric = JevMetric {
            decision_kind: kind.to_string(),
            model: Some(decoded.model),
            latency_ms,
            confidence: Some(answer.confidence),
            probabilities,
            selected_id: Some(chosen.external_id.clone()),
            input_tokens: decoded.usage.input_tokens,
            output_tokens: decoded.usage.output_tokens,
            error: None,
        };
        self.metrics.push(metric.clone());
        Ok((chosen.external_id.clone(), metric))
    }

    fn compact_state(observation: &AgentObservation) -> Value {
        let mut value = serde_json::to_value(observation).unwrap_or_else(|_| json!({}));
        if let Some(obj) = value.as_object_mut() {
            // Legal actions are already represented as typed Choice criteria; omitting
            // them here avoids paying to send the same data twice.
            obj.remove("legal_actions");
            obj.insert("deck_strategy".to_string(), json!({
                "archetype": "Legacy Mystic Forge",
                "goal": "Maximize long-term probability of winning. Develop fast mana and artifact engines, exploit Mystic Forge, Manifold Key, Monoliths, The One Ring, Karn, Urza's Saga, Transmute Artifact, and Paradox Engine synergies. Preserve resources only when doing so improves future winning chances. Treat hidden opponent cards as unknown."
            }));
        }
        value
    }

    fn action_choices(observation: &AgentObservation) -> Vec<ChoiceSpec> {
        observation.legal_actions.iter().enumerate().map(|(idx, action)| ChoiceSpec {
            short_key: format!("a{idx}"),
            external_id: action.id.clone(),
            description: format!("{} | semantic={} | id={}", action.description, action.semantic, action.id),
        }).collect()
    }

    fn announcement_choices(observation: &AgentAnnouncementObservation) -> Vec<ChoiceSpec> {
        let modes = if observation.available_modes.is_empty() { vec![0] } else { observation.available_modes.clone() };
        let xs: Vec<u32> = if observation.has_x_cost { (0..=observation.max_x).collect() } else { vec![0] };
        let mut out = Vec::new();
        for mode in modes {
            for x in &xs {
                // v1 deliberately leaves alternate-cost choice to the bootstrap
                // heuristic; the Forge cards under current study do not require an
                // agent-selected alternate cost to exercise their core lines.
                let external_id = format!("mode={mode};x={x};alt=none");
                let mode_desc = if observation.card.name == "Kozilek's Command" {
                    match mode {
                        0 => "create X Spawn + scry X then draw",
                        1 => "create X Spawn + exile creature with mana value <= X",
                        2 => "create X Spawn + exile up to X graveyard cards",
                        3 => "scry X then draw + exile creature with mana value <= X",
                        4 => "scry X then draw + exile up to X graveyard cards",
                        5 => "exile creature with mana value <= X + exile up to X graveyard cards",
                        _ => "engine modal choice",
                    }
                } else { "engine modal choice" };
                out.push(ChoiceSpec {
                    short_key: format!("m{}_x{}", mode, x),
                    external_id,
                    description: format!("Choose mode {mode} ({mode_desc}) with X={x} for {}.", observation.card.name),
                });
            }
        }
        out
    }

    fn parse_announcement_id(id: &str) -> Option<AgentAnnouncementDecision> {
        let mut mode = None;
        let mut x = None;
        for part in id.split(';') {
            if let Some(v) = part.strip_prefix("mode=") { mode = v.parse::<usize>().ok(); }
            if let Some(v) = part.strip_prefix("x=") { x = v.parse::<u32>().ok(); }
        }
        Some(AgentAnnouncementDecision { chosen_mode: mode?, chosen_x: x?, alt_cost_index: None })
    }

    pub fn check_models(&self) -> Result<JevCheckSummary, String> {
        let response = self.http
            .get(&self.endpoint("v1/models"))
            .set("Authorization", &format!("Bearer {}", self.cfg.api_key))
            .call()
            .map_err(|e| format!("Jev model-list request failed: {e}"))?;
        let decoded: ModelsResponse = response.into_json()
            .map_err(|e| format!("Jev model-list JSON failed: {e}"))?;
        let models: Vec<JevModelInfo> = decoded.models.into_iter().map(|m| JevModelInfo {
            name: m.name, description: m.description, release_date: m.release_date,
        }).collect();
        Ok(JevCheckSummary {
            base_url: self.cfg.base_url.clone(),
            requested_model: self.cfg.model.clone(),
            model_available: models.iter().any(|m| m.name == self.cfg.model),
            models,
        })
    }
}

impl ForgeAgent for JevForgeAgent {
    fn name(&self) -> &str { "jev" }

    fn choose_action(&mut self, observation: &AgentObservation) -> AgentDecision {
        let choices = Self::action_choices(observation);
        let instructions = "Choose exactly one engine-legal action for the Legacy Mystic Forge player. Maximize long-term probability of winning, not immediate material score. Consider sequencing, future mana, metalcraft, engine assembly, card advantage, and whether passing preserves a genuinely useful option. Do not invent actions or assume hidden opponent cards.";
        match self.post_choice("priority_action", Self::compact_state(observation), instructions, choices) {
            Ok((action_id, _)) => AgentDecision { action_id },
            Err(_) => AgentDecision { action_id: "__jev_error__".to_string() },
        }
    }

    fn choose_announcement(&mut self, observation: &AgentAnnouncementObservation) -> Option<AgentAnnouncementDecision> {
        let choices = Self::announcement_choices(observation);
        if choices.len() <= 1 { return None; }
        let state = serde_json::to_value(observation).ok()?;
        let instructions = "Choose the spell announcement that maximizes long-term probability of winning. X must be affordable and the listed mode is engine-legal. Avoid spending extra mana merely because a larger X is available if preserving mana enables a stronger line.";
        let (id, _) = self.post_choice("spell_announcement", state, instructions, choices).ok()?;
        Self::parse_announcement_id(&id)
    }

    fn choose_wish(&mut self, observation: &AgentWishObservation) -> Option<String> {
        if observation.choices.is_empty() { return None; }
        let choices: Vec<ChoiceSpec> = observation.choices.iter().enumerate().map(|(idx, c)| ChoiceSpec {
            short_key: format!("w{idx}"),
            external_id: c.id.clone(),
            description: format!("Take {} from {} (id={}).", c.name, c.source, c.id),
        }).collect();
        let state = serde_json::to_value(observation).ok()?;
        let instructions = "Choose the Karn wish target that most improves the Mystic Forge player's probability of winning from the current game plan. Choose only among the supplied artifact options.";
        self.post_choice("karn_wish", state, instructions, choices).ok().map(|(id, _)| id)
    }

    fn drain_diagnostics(&mut self) -> Vec<String> {
        self.take_metrics().into_iter().filter_map(|metric| {
            serde_json::to_string(&metric).ok().map(|json| format!("JEV_METRIC\t{json}"))
        }).collect()
    }
}

pub fn jev_check_from_env() -> Result<JevCheckSummary, String> {
    JevForgeAgent::from_env()?.check_models()
}

#[derive(Debug, Serialize)]
pub struct JevDisagreement {
    pub game_index: u64,
    pub game_seed: u64,
    pub turn: u8,
    pub phase: String,
    pub teacher_action_id: String,
    pub teacher_action: String,
    pub jev_action_id: String,
    pub heuristic_action_id: String,
    pub jev_confidence: Option<f64>,
    pub jev_probabilities: BTreeMap<String, f64>,
    pub legal_actions: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct JevCompareSummary {
    pub requested_model: String,
    pub resolved_models: Vec<String>,
    pub games: u64,
    pub seed: u64,
    pub max_turns: u8,
    pub rollouts_per_action: usize,
    pub teacher_decisions: u64,
    pub jev_successful_decisions: u64,
    pub jev_errors: u64,
    pub jev_teacher_agreements: u64,
    pub heuristic_teacher_agreements: u64,
    pub jev_teacher_agreement_rate: Option<f64>,
    pub heuristic_teacher_agreement_rate: Option<f64>,
    pub average_jev_confidence: Option<f64>,
    pub average_jev_latency_ms: Option<f64>,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub estimated_input_cost_usd: f64,
    pub disagreements: Vec<JevDisagreement>,
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Evaluate Jev on the exact observations labeled by the rollout teacher.
/// This is more meaningful than comparing two independently-diverging games:
/// Jev, heuristic, and search all see the identical state and identical legal actions.
pub fn compare_jev_to_search(
    deck: &Decklist,
    games: u64,
    seed: u64,
    max_turns: u8,
    rollouts_per_action: usize,
    max_disagreements: usize,
) -> Result<JevCompareSummary, String> {
    let cfg = JevConfig::from_env()?;
    let requested_model = cfg.model.clone();
    let price = cfg.input_cost_per_mtok;
    let mut jev = JevForgeAgent::new(cfg);
    let mut heuristic = HeuristicForgeAgent;

    let mut teacher_decisions = 0u64;
    let mut jev_successful = 0u64;
    let mut jev_errors = 0u64;
    let mut jev_agree = 0u64;
    let mut heuristic_agree = 0u64;
    let mut confidence_sum = 0.0;
    let mut confidence_n = 0u64;
    let mut latency_sum = 0u64;
    let mut input_tokens = 0u64;
    let mut output_tokens = 0u64;
    let mut models = Vec::<String>::new();
    let mut disagreements = Vec::new();

    for game_index in 0..games {
        let game_seed = splitmix64(seed.wrapping_add(game_index));
        let summary = run_search_smoke_once(deck, game_seed, max_turns, rollouts_per_action);
        let mut pending_observation: Option<AgentObservation> = None;
        for line in &summary.decision_log {
            if let Some(json) = line.strip_prefix("AGENT_OBS\t") {
                pending_observation = serde_json::from_str(json).ok();
                continue;
            }
            let Some(json) = line.strip_prefix("FORGE_TRAIN\t") else { continue };
            let Some(mut observation) = pending_observation.take() else { continue };
            let record: Value = serde_json::from_str(json)
                .map_err(|e| format!("teacher record JSON failed: {e}"))?;
            let teacher_action_id = record.get("chosen_action_id").and_then(Value::as_str).unwrap_or("pass").to_string();
            let teacher_action = record.get("chosen").and_then(Value::as_str).unwrap_or("pass").to_string();
            // Compare all pilots over the exact candidate set the rollout teacher
            // actually scored, rather than actions pruned by its cheap pre-ranker.
            if let Some(candidates) = record.get("candidates").and_then(Value::as_array) {
                let candidate_ids: std::collections::HashSet<String> = candidates.iter()
                    .filter_map(|c| c.get("action_id").and_then(Value::as_str).map(str::to_string))
                    .collect();
                observation.legal_actions.retain(|a| candidate_ids.contains(&a.id));
            }
            if observation.legal_actions.is_empty() { continue; }
            teacher_decisions += 1;

            let heuristic_id = heuristic.choose_action(&observation).action_id;
            if heuristic_id == teacher_action_id { heuristic_agree += 1; }

            let jev_id = jev.choose_action(&observation).action_id;
            let metrics = jev.take_metrics();
            let metric = metrics.last().cloned().unwrap_or_else(|| JevMetric::error("priority_action", 0, "missing Jev metric".to_string()));
            if let Some(model) = &metric.model {
                if !models.contains(model) { models.push(model.clone()); }
            }
            latency_sum = latency_sum.saturating_add(metric.latency_ms);
            input_tokens = input_tokens.saturating_add(metric.input_tokens);
            output_tokens = output_tokens.saturating_add(metric.output_tokens);
            if let Some(conf) = metric.confidence { confidence_sum += conf; confidence_n += 1; }

            if metric.error.is_some() || jev_id == "__jev_error__" {
                jev_errors += 1;
                continue;
            }
            jev_successful += 1;
            if jev_id == teacher_action_id { jev_agree += 1; }
            else if disagreements.len() < max_disagreements {
                disagreements.push(JevDisagreement {
                    game_index,
                    game_seed,
                    turn: observation.turn,
                    phase: observation.phase.clone(),
                    teacher_action_id: teacher_action_id.clone(),
                    teacher_action: teacher_action.clone(),
                    jev_action_id: jev_id.clone(),
                    heuristic_action_id: heuristic_id,
                    jev_confidence: metric.confidence,
                    jev_probabilities: metric.probabilities.clone(),
                    legal_actions: observation.legal_actions.iter().map(|a| format!("{} :: {}", a.id, a.description)).collect(),
                });
            }
        }
    }

    Ok(JevCompareSummary {
        requested_model,
        resolved_models: models,
        games, seed, max_turns, rollouts_per_action,
        teacher_decisions,
        jev_successful_decisions: jev_successful,
        jev_errors,
        jev_teacher_agreements: jev_agree,
        heuristic_teacher_agreements: heuristic_agree,
        jev_teacher_agreement_rate: (jev_successful > 0).then_some(jev_agree as f64 / jev_successful as f64),
        heuristic_teacher_agreement_rate: (teacher_decisions > 0).then_some(heuristic_agree as f64 / teacher_decisions as f64),
        average_jev_confidence: (confidence_n > 0).then_some(confidence_sum / confidence_n as f64),
        average_jev_latency_ms: (jev_successful + jev_errors > 0).then_some(latency_sum as f64 / (jev_successful + jev_errors) as f64),
        total_input_tokens: input_tokens,
        total_output_tokens: output_tokens,
        estimated_input_cost_usd: input_tokens as f64 / 1_000_000.0 * price,
        disagreements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{AgentAction, AgentActionKind, AgentMana};

    fn tiny_observation() -> AgentObservation {
        AgentObservation {
            schema_version: "forge_agent_obs_v1".to_string(), player: "Us".to_string(), turn: 1,
            phase: "PreCombatMain".to_string(), active_player: Some("Us".to_string()), on_play: true,
            life: 20, opponent_life: 20, hand: vec![], opponent_hand_size: 7,
            known_opponent_hand: vec![], battlefield: vec![], opponent_battlefield: vec![],
            graveyard: vec![], opponent_graveyard: vec![], exile: vec![], opponent_exile: vec![], stack: vec![],
            library_size: 53, opponent_library_size: 53, visible_top: None,
            mana: AgentMana { white: 0, blue: 0, black: 0, red: 0, green: 0, colorless: 0, floating_total: 0, potential_total: 1 },
            legal_actions: vec![
                AgentAction { id: "a-real-id".to_string(), semantic: "cast:Manifold Key:Main".to_string(), kind: AgentActionKind::CastSpell,
                    card_name: Some("Manifold Key".to_string()), source_object_id: Some("ObjId(1)".to_string()), face: Some("main".to_string()), ability_index: None,
                    description: "Cast Manifold Key.".to_string() },
                AgentAction { id: "pass".to_string(), semantic: "pass".to_string(), kind: AgentActionKind::Pass,
                    card_name: None, source_object_id: None, face: None, ability_index: None, description: "Pass priority.".to_string() },
            ],
        }
    }

    #[test]
    fn jev_choice_keys_are_short_and_map_back_to_action_ids() {
        let obs = tiny_observation();
        let choices = JevForgeAgent::action_choices(&obs);
        assert_eq!(choices[0].short_key, "a0");
        assert_eq!(choices[0].external_id, "a-real-id");
        assert_eq!(choices[1].short_key, "a1");
        assert_eq!(choices[1].external_id, "pass");
    }

    #[test]
    fn jev_compact_state_does_not_duplicate_legal_actions() {
        let state = JevForgeAgent::compact_state(&tiny_observation());
        assert!(state.get("legal_actions").is_none());
        assert!(state.get("deck_strategy").is_some());
    }

    #[test]
    fn announcement_id_round_trips() {
        let parsed = JevForgeAgent::parse_announcement_id("mode=4;x=2;alt=none").unwrap();
        assert_eq!(parsed.chosen_mode, 4);
        assert_eq!(parsed.chosen_x, 2);
        assert_eq!(parsed.alt_cost_index, None);
    }
}
