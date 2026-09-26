use std::{fs::File, io::Write, path::Path, sync::Arc};

use libmtg_decklist::Decklist;
use libmtg_engine::{
    build_catalog, run_game, simulate_priority_action, AlwaysPass, AnnounceChoice, AnnounceOptions, CardKind,
    GameEvent, LegalAction, ObjId, Objective, PhaseKind, PlayerId, Scenario, SimState, Strategy, TargetGap,
    TurnPosition, WishOption,
};
use rand::{rngs::SmallRng, SeedableRng};
use serde::Serialize;

use crate::agent::{build_agent_observation, legal_action_id, legal_action_semantic, AGENT_OBSERVATION_SCHEMA_VERSION};
use crate::profile::{keep_v0, role_for, ForgeRole};
use crate::strategy::{forge_announce_choice, ForgeStrategy};

const FORGE_EVALUATOR_VERSION: &str = "forge_eval_v2";


#[derive(Debug, Serialize)]
struct SearchActionScore {
    /// Stable within one game/branch and distinguishes duplicate permanents.
    action_id: String,
    /// Semantic action identity used by the learned model across games.
    action: String,
    score: f64,
}

#[derive(Debug, Serialize)]
struct SearchDecisionRecord {
    evaluator_version: &'static str,
    observation_schema_version: &'static str,
    game_seed: u64,
    decision_index: u64,
    turn: u8,
    phase: String,
    life: i32,
    opponent_life: i32,
    hand: Vec<String>,
    battlefield: Vec<String>,
    potential_mana: i32,
    next_turn_mana: f64,
    artifact_count: usize,
    metalcraft: bool,
    forge_active: bool,
    visible_top: Option<String>,
    state_score_before: f64,
    candidates: Vec<SearchActionScore>,
    chosen_action_id: String,
    chosen: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ForgeSearchConfig {
    /// Independent forward simulations for each candidate action.
    pub rollouts_per_action: usize,
    /// Cheap pre-ranking cap. Pass is always retained even when the cap is hit.
    pub max_candidates: usize,
    /// Master seed; each decision/action/rollout derives a deterministic sub-seed.
    pub seed: u64,
}

impl Default for ForgeSearchConfig {
    fn default() -> Self {
        Self { rollouts_per_action: 4, max_candidates: 8, seed: 1 }
    }
}

/// First search-based Forge pilot.
///
/// This version is intentionally a *goldfish* search policy. Each candidate
/// legal priority action is played on an isolated state fork, the existing
/// heuristic Forge policy finishes that priority window, and the resulting
/// position is evaluated. The opponent continuation always passes, so this
/// version cannot gain hidden-information advantage from an opponent hand.
///
/// The next matchup-capable version should replace `AlwaysPass` with sampled
/// determinizations of the opponent's hidden cards.
pub struct ForgeSearchStrategy {
    who: PlayerId,
    cfg: ForgeSearchConfig,
    decision_index: u64,
    decisions: Vec<String>,
}

impl ForgeSearchStrategy {
    pub fn new(who: PlayerId, cfg: ForgeSearchConfig) -> Self {
        Self { who, cfg, decision_index: 0, decisions: Vec::new() }
    }

    fn card_priority(name: &str) -> i32 {
        match name {
            "Mystic Forge" => 100,
            "Karn, the Great Creator" => 95,
            "The One Ring" => 92,
            "Tezzeret, Cruel Captain" => 88,
            "Trinisphere" => 86,
            "Grim Monolith" => 84,
            "Basalt Monolith" => 83,
            "Manifold Key" => 80,
            "Transmute Artifact" => 78,
            "Relic of Sauron" => 72,
            "Paradox Engine" => 70,
            "Kozilek's Command" => 60,
            "Giant's Boulder" => 58,
            _ => 40,
        }
    }

    fn action_hint_score(action: &LegalAction, state: &SimState) -> i32 {
        match action {
            LegalAction::Pass => -10,
            LegalAction::LandDrop(id) => 90 + Self::card_priority(card_name(state, *id)) / 10,
            LegalAction::CastSpell { card_id, .. } => Self::card_priority(card_name(state, *card_id)),
            LegalAction::ActivateAbility { source_id, ability_index } => {
                Self::card_priority(card_name(state, *source_id)) + 4 - (*ability_index as i32)
            }
            LegalAction::ActivateManaAbility { source_id, .. } => {
                Self::card_priority(card_name(state, *source_id)) / 2
            }
        }
    }

    fn is_goldfish_search_window(&self, state: &SimState) -> bool {
        if !state.stack.is_empty() {
            return false;
        }
        if state.active_player() != Some(self.who) {
            return false;
        }
        matches!(
            state.current_phase,
            Some(TurnPosition::Phase(PhaseKind::PreCombatMain | PhaseKind::PostCombatMain))
        )
    }

    fn action_is_obviously_no_progress(&self, action: &LegalAction, state: &SimState) -> bool {
        let LegalAction::ActivateAbility { source_id, ability_index } = action else { return false };
        let Some(obj) = state.objects.get(source_id) else { return true };
        let name = obj.catalog_key.as_str();

        if matches!(name, "Basalt Monolith" | "Grim Monolith") {
            // Their only non-mana activated ability in this simulator is the paid
            // untap ability. If already untapped, activation can return to the
            // same state after using the Monolith itself to fund the cost.
            return obj.bf().map_or(false, |bf| !bf.tapped);
        }

        if name == "Manifold Key" {
            // Ability 1 only changes combat blocking. The current goldfish pilot
            // never attacks, so spending search budget on it cannot improve a
            // solitaire outcome.
            if *ability_index == 1 {
                return true;
            }
            if *ability_index == 0 {
                // Untapping an already-untapped artifact is legal but strategically
                // zero-progress. Search it only if another tapped artifact exists.
                let has_tapped_artifact = state.permanents_of(self.who).any(|perm| {
                    perm.id != *source_id
                        && perm.bf().map_or(false, |bf| bf.tapped)
                        && state.def_of(perm.id).map_or(false, |d| d.is_artifact())
                });
                if !has_tapped_artifact {
                    return true;
                }
            }
        }

        false
    }

    fn candidate_actions(&self, legal: &[LegalAction], state: &SimState) -> Vec<LegalAction> {
        let mut scored: Vec<(i32, usize, LegalAction)> = legal.iter().cloned().enumerate()
            .filter(|(_, action)| !self.action_is_obviously_no_progress(action, state))
            .map(|(idx, action)| (Self::action_hint_score(&action, state), idx, action))
            .collect();
        scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));

        let mut out: Vec<LegalAction> = scored.into_iter()
            .take(self.cfg.max_candidates.max(1))
            .map(|(_, _, a)| a)
            .collect();
        if legal.iter().any(|a| matches!(a, LegalAction::Pass))
            && !out.iter().any(|a| matches!(a, LegalAction::Pass))
        {
            out.push(LegalAction::Pass);
        }
        out
    }

    fn evaluate_action(
        &self,
        state: &SimState,
        ap: PlayerId,
        action: &LegalAction,
        action_index: usize,
    ) -> Option<f64> {
        let n = self.cfg.rollouts_per_action.max(1);
        let mut total = 0.0;
        for rollout in 0..n {
            let rollout_seed = splitmix64(
                self.cfg.seed
                    ^ self.decision_index.wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    ^ (action_index as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9)
                    ^ rollout as u64,
            );
            let branch = simulate_priority_action(
                state,
                ap,
                self.who,
                action.clone(),
                rollout_seed,
                Box::new(ForgeStrategy::new(PlayerId::Us)),
                Box::new(AlwaysPass::new(PlayerId::Opp)),
            );
            let score = evaluate_forge_state(&branch, self.who);
            // Legitimate terminal scores are +/- 1,000,000. Larger magnitudes
            // indicate a malformed/aborted branch and must never become labels.
            if !score.is_finite() || score.abs() > 1_500_000.0 {
                return None;
            }
            total += score;
        }
        Some(total / n as f64)
    }

}

impl Strategy for ForgeSearchStrategy {
    fn declare_attackers(&mut self, _state: &SimState) -> Vec<(ObjId, Option<ObjId>)> { Vec::new() }
    fn declare_blockers(&mut self, _state: &SimState) -> Vec<(ObjId, ObjId)> { Vec::new() }

    fn take_mulligan(&mut self, state: &SimState, mulligans_taken: u32) -> bool {
        let hand: Vec<String> = state.hand_of(self.who).map(|c| c.catalog_key.clone()).collect();
        !keep_v0(&hand, mulligans_taken)
    }

    fn choose_action(&mut self, state: &SimState, ap: PlayerId, legal: &[LegalAction]) -> LegalAction {
        if legal.len() <= 1 {
            return legal.first().cloned().unwrap_or(LegalAction::Pass);
        }

        // Goldfish search only spends compute in our own empty-stack main phases.
        // Outside those windows, passing is both safe and dramatically cheaper
        // than repeatedly evaluating Key activations through every combat step.
        if !self.is_goldfish_search_window(state) {
            return legal.iter().find(|a| matches!(a, LegalAction::Pass))
                .cloned()
                .unwrap_or_else(|| legal[0].clone());
        }

        let candidates = self.candidate_actions(legal, state);
        let non_pass = candidates.iter().filter(|a| !matches!(a, LegalAction::Pass)).count();
        if non_pass == 0 {
            return LegalAction::Pass;
        }

        let mut best: Option<(f64, LegalAction)> = None;
        let mut trace = Vec::new();
        let mut scored_actions = Vec::new();
        for (idx, action) in candidates.iter().enumerate() {
            let action_id = legal_action_id(action, state);
            let action_label = legal_action_semantic(action, state);
            let Some(score) = self.evaluate_action(state, ap, action, idx) else {
                self.decisions.push(format!("forge-search pruned invalid branch {}", action_id));
                continue;
            };
            trace.push(format!("{}={:.3}", action_id, score));
            scored_actions.push(SearchActionScore {
                action_id,
                action: action_label,
                score,
            });
            if best.as_ref().map_or(true, |(best_score, _)| score > *best_score) {
                best = Some((score, action.clone()));
            }
        }

        let chosen = best.map(|(_, a)| a).unwrap_or(LegalAction::Pass);
        let chosen_action_id = legal_action_id(&chosen, state);
        let chosen_label = legal_action_semantic(&chosen, state);

        // If every non-pass branch was invalid, return Pass without emitting a
        // training row. There is no trustworthy teacher label in that state.
        if scored_actions.iter().all(|c| c.action == "pass") {
            return LegalAction::Pass;
        }

        self.decision_index = self.decision_index.wrapping_add(1);
        self.decisions.push(format!(
            "forge-search d{} chose {} | {}",
            self.decision_index,
            chosen_action_id,
            trace.join(", ")
        ));

        let mut hand: Vec<String> = state.hand_of(self.who).map(|o| o.catalog_key.clone()).collect();
        hand.sort();
        let mut battlefield: Vec<String> = state.permanents_of(self.who).map(|o| o.catalog_key.clone()).collect();
        battlefield.sort();
        let forge_active = battlefield.iter().any(|n| n == "Mystic Forge");
        let visible_top = if forge_active {
            state.player(self.who).library_order.front()
                .and_then(|id| state.objects.get(id))
                .map(|o| o.catalog_key.clone())
        } else {
            None
        };
        let artifact_count = state.permanents_of(self.who)
            .filter(|obj| state.def_of(obj.id).map_or(false, |d| d.is_artifact()))
            .count();
        let agent_observation = build_agent_observation(state, self.who, legal);
        if let Ok(json) = serde_json::to_string(&agent_observation) {
            self.decisions.push(format!("AGENT_OBS\t{}", json));
        }
        let record = SearchDecisionRecord {
            evaluator_version: FORGE_EVALUATOR_VERSION,
            observation_schema_version: AGENT_OBSERVATION_SCHEMA_VERSION,
            game_seed: self.cfg.seed,
            decision_index: self.decision_index,
            turn: state.current_turn,
            phase: format!("{:?}", state.current_phase),
            life: state.player(self.who).life,
            opponent_life: state.player(self.who.opp()).life,
            hand,
            battlefield,
            potential_mana: state.potential_mana(self.who).total,
            next_turn_mana: forge_next_turn_mana_capacity(state, self.who, artifact_count),
            artifact_count,
            metalcraft: artifact_count >= 3,
            forge_active,
            visible_top,
            state_score_before: evaluate_forge_state(state, self.who),
            candidates: scored_actions,
            chosen_action_id,
            chosen: chosen_label,
        };
        if let Ok(json) = serde_json::to_string(&record) {
            self.decisions.push(format!("FORGE_TRAIN\t{}", json));
        }
        chosen
    }

    fn announce(&mut self, state: &SimState, card_id: ObjId, options: &AnnounceOptions) -> AnnounceChoice {
        forge_announce_choice(state, card_id, options)
    }

    fn choose_wish(&mut self, _effect_id: ObjId, choices: &[WishOption], _state: &SimState) -> Option<WishOption> {
        choices.iter().max_by_key(|choice| {
            let name = match choice {
                WishOption::Sideboard { name, .. } | WishOption::Exile { name, .. } => name.as_str(),
            };
            Self::card_priority(name)
        }).cloned()
    }

    fn player_id(&self) -> PlayerId { self.who }

    fn plan_gap(&self, state: &SimState) -> TargetGap {
        let has_engine = state.permanents_of(self.who).any(|c| role_for(&c.catalog_key) == ForgeRole::Engine)
            || state.hand_of(self.who).any(|c| role_for(&c.catalog_key) == ForgeRole::Engine);
        TargetGap {
            mana: if state.permanents_of(self.who).any(|c| matches!(role_for(&c.catalog_key), ForgeRole::Land | ForgeRole::FastMana)) { 0.2 } else { 1.0 },
            threat: if has_engine { 0.0 } else { 1.0 },
            interaction: 0.5,
        }
    }

    fn card_fills(&self, card_id: ObjId, gap: &TargetGap, state: &SimState) -> f64 {
        let Some(obj) = state.objects.get(&card_id) else { return 0.0 };
        match role_for(&obj.catalog_key) {
            ForgeRole::Land | ForgeRole::FastMana => 1.0 * gap.mana,
            ForgeRole::Engine | ForgeRole::Tutor => 1.0 * gap.threat,
            ForgeRole::Lock => 0.8 * gap.interaction,
            ForgeRole::Payoff => 0.45,
            ForgeRole::Utility => 0.25,
        }
    }

    fn drain_decisions(&mut self) -> Vec<String> {
        std::mem::take(&mut self.decisions)
    }
}

/// Interpretable Forge position evaluator used by the rollout teacher.
///
/// v2 deliberately values *infrastructure* rather than only resources that are
/// untapped at this exact priority window.  The v1 evaluator overweighted
/// `potential_mana()`, which made "pass and keep everything untapped/in hand"
/// look better than deploying artifacts that improve the following turn.
///
/// This is still a bootstrap evaluator, not a claim of optimal Magic play.  Its
/// job is to give short rollouts a sensible strategic horizon without requiring
/// deeper/expensive search on a low-power machine.
pub fn evaluate_forge_state(state: &SimState, who: PlayerId) -> f64 {
    if state.winner == Some(who) { return 1_000_000.0; }
    if state.winner == Some(who.opp()) { return -1_000_000.0; }

    let me = state.player(who);
    let them = state.player(who.opp());

    // Generic resources are intentionally modest.  In particular, current
    // untapped mana is only a small tiebreaker; future mana infrastructure is
    // scored separately below.
    let mut score = 0.0;
    score += (me.life - them.life) as f64 * 0.03;
    score += state.hand_size(who) as f64 * 0.18;
    score -= state.hand_size(who.opp()) as f64 * 0.03;
    score += state.potential_mana(who).total as f64 * 0.10;

    let artifact_count = state.permanents_of(who)
        .filter(|obj| state.def_of(obj.id).map_or(false, |d| d.is_artifact()))
        .count();
    score += artifact_count as f64 * 0.15;
    if artifact_count >= 3 {
        // Metalcraft is strategically important even when Mox Opal is not
        // currently present: it turns future/drawn Opals on immediately and is
        // also a useful proxy for having developed the artifact engine.
        score += 1.40;
    }

    let next_turn_mana = forge_next_turn_mana_capacity(state, who, artifact_count);
    score += next_turn_mana * 0.65;

    let mut has_forge = false;
    let mut has_key = false;
    let mut has_monolith = false;
    let mut has_paradox = false;
    let mut has_ring = false;
    let mut mana_rocks = 0usize;

    for obj in state.permanents_of(who) {
        let name = obj.catalog_key.as_str();
        score += match name {
            "Mystic Forge" => { has_forge = true; 5.5 }
            "The One Ring" => { has_ring = true; 5.2 }
            "Karn, the Great Creator" => 4.3,
            "Tezzeret, Cruel Captain" => 4.2,
            "Paradox Engine" => { has_paradox = true; 5.8 }
            "Manifold Key" => { has_key = true; 1.4 }
            "Grim Monolith" | "Basalt Monolith" => {
                has_monolith = true;
                mana_rocks += 1;
                1.6
            }
            "Relic of Sauron" => { mana_rocks += 1; 2.0 }
            "Trinisphere" => 0.4, // deliberately low in goldfish mode
            "Giant's Boulder" => 1.9,
            "Urza's Saga" => 2.4,
            // Mana contribution is handled by forge_next_turn_mana_capacity().
            "Ancient Tomb" | "Urza's Workshop" | "Planar Nexus" | "Urza's Tower" => 0.2,
            "Lotus Petal" | "Mox Opal" => { mana_rocks += 1; 0.3 }
            _ => 0.2,
        };
    }

    // Engine-piece combinations are worth more than the sum of their parts.
    if has_key && has_monolith { score += 2.0; }
    if has_ring && has_key { score += 0.8; }
    if has_forge && has_key { score += 0.4; }
    if has_forge && has_monolith { score += 0.5; }
    if has_paradox && has_forge { score += 2.5; }
    if has_paradox { score += mana_rocks as f64 * 0.35; }

    // Looking at the exact top card is legal only while Mystic Forge is active.
    // Reward a castable-looking top because it represents an additional virtual
    // card rather than merely another object in hand.
    if has_forge {
        if let Some(&top_id) = state.player(who).library_order.front() {
            if let Some(top) = state.objects.get(&top_id) {
                if state.def_of(top_id).map_or(false, |d| matches!(&d.kind, CardKind::Artifact(_)) || d.is_land()) {
                    score += 1.6;
                }
                if matches!(top.catalog_key.as_str(), "Mystic Forge" | "The One Ring" | "Karn, the Great Creator") {
                    score += 0.8;
                }
            }
        }
    }

    score
}

/// Approximate mana capacity after a normal untap step.  This intentionally
/// ignores whether ordinary lands/artifacts are tapped *right now* so spending
/// Workshop/Tomb to develop the board is not punished as if that mana were lost
/// forever.  Monoliths are handled separately because they do not untap during
/// the untap step.
fn forge_next_turn_mana_capacity(state: &SimState, who: PlayerId, artifact_count: usize) -> f64 {
    let mut base = 0.0;
    let mut untapped_monoliths = 0usize;
    let mut tapped_monoliths = 0usize;
    let mut has_key = false;

    for obj in state.permanents_of(who) {
        let tapped = obj.bf().map_or(false, |bf| bf.tapped);
        match obj.catalog_key.as_str() {
            "Ancient Tomb" => base += 2.0,
            "Urza's Workshop" => base += 3.0,
            "Urza's Tower" | "Planar Nexus" | "Urza's Saga" => base += 1.0,
            "Relic of Sauron" => base += 2.0,
            "Lotus Petal" => base += 1.0,
            "Mox Opal" if artifact_count >= 3 => base += 1.0,
            "Manifold Key" => has_key = true,
            "Grim Monolith" | "Basalt Monolith" => {
                if tapped { tapped_monoliths += 1; }
                else { untapped_monoliths += 1; }
            }
            // Giant's Boulder is a mana filter, not additional mana.
            _ => {}
        }
    }

    base += untapped_monoliths as f64 * 3.0;

    // A Key that untaps next turn can turn one tapped Monolith into roughly +2
    // net mana (pay 1 to Key, receive 3 from the Monolith), provided some other
    // source can fund the Key activation.  Count only one such Monolith because
    // a single Key can normally activate only once without another untap effect.
    if has_key && tapped_monoliths > 0 && base >= 1.0 {
        base += 2.0;
    }

    base
}

fn card_name(state: &SimState, id: ObjId) -> &str {
    state.objects.get(&id).map(|o| o.catalog_key.as_str()).unwrap_or("")
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}


#[derive(Default)]
struct SearchSmokeObjective;
impl Objective for SearchSmokeObjective {
    fn observe(&mut self, _event: &GameEvent, _state: &mut SimState) -> bool { false }
}

#[derive(Debug, Serialize)]
pub struct PilotRunSummary {
    pub final_score: f64,
    pub winner: Option<String>,
    pub hand: Vec<String>,
    pub battlefield: Vec<String>,
    pub decision_log: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct SearchSmokeComparison {
    pub evaluator_version: &'static str,
    pub seed: u64,
    pub max_turns: u8,
    pub rollouts_per_action: usize,
    pub heuristic: PilotRunSummary,
    pub search: PilotRunSummary,
    pub search_minus_heuristic_score: f64,
}

/// Run the same Forge deck/seed against an inert 60-card opponent once with the
/// baseline priority-table pilot and once with the bounded rollout pilot. This
/// is a diagnostic, not a matchup win-rate measurement: its purpose is to make
/// search choices and regressions visible before training a model from them.
pub fn compare_search_smoke(
    deck: &Decklist,
    seed: u64,
    max_turns: u8,
    rollouts_per_action: usize,
) -> SearchSmokeComparison {
    eprintln!("[search-smoke] running heuristic baseline...");
    let heuristic = run_smoke_once(deck, seed, max_turns, false, rollouts_per_action);
    eprintln!("[search-smoke] heuristic baseline complete; running rollout pilot...");
    let search = run_smoke_once(deck, seed, max_turns, true, rollouts_per_action);
    eprintln!("[search-smoke] rollout pilot complete.");
    SearchSmokeComparison {
        evaluator_version: FORGE_EVALUATOR_VERSION,
        seed,
        max_turns,
        rollouts_per_action,
        search_minus_heuristic_score: search.final_score - heuristic.final_score,
        heuristic,
        search,
    }
}

/// Run only the rollout-search pilot for one deterministic seed. This is
/// exposed for external-agent evaluation so Jev and the heuristic can be
/// compared against the exact observations labeled by the search teacher.
pub fn run_search_smoke_once(
    deck: &Decklist,
    seed: u64,
    max_turns: u8,
    rollouts_per_action: usize,
) -> PilotRunSummary {
    run_smoke_once(deck, seed, max_turns, true, rollouts_per_action)
}

fn run_smoke_once(
    deck: &Decklist,
    seed: u64,
    max_turns: u8,
    use_search: bool,
    rollouts_per_action: usize,
) -> PilotRunSummary {
    let catalog = build_catalog();
    let opponent = vec![("Island".to_string(), 60, "main".to_string())];
    let us_strategy: Box<dyn Strategy> = if use_search {
        Box::new(ForgeSearchStrategy::new(
            PlayerId::Us,
            ForgeSearchConfig { rollouts_per_action, max_candidates: 8, seed },
        ))
    } else {
        Box::new(ForgeStrategy::new(PlayerId::Us))
    };
    let mut rng = SmallRng::seed_from_u64(seed);
    let state = run_game(
        Scenario {
            us_label: if use_search { "Forge Search" } else { "Forge Heuristic" }.to_string(),
            opp_label: "Inert Opponent".to_string(),
            catalog,
            us_deck: deck.to_engine_deck(),
            opp_deck: opponent,
            us_strategy,
            opp_strategy: Box::new(AlwaysPass::new(PlayerId::Opp)),
            evaluate_card: Arc::new(|_, _, _| 0.5),
            objective: Box::<SearchSmokeObjective>::default(),
            max_turns,
            on_play: Some(true),
            fixed_us_hand: None,
        },
        &mut rng,
    );
    let mut battlefield: Vec<String> = state.permanents_of(PlayerId::Us)
        .map(|o| o.catalog_key.clone())
        .collect();
    battlefield.sort();
    let mut hand: Vec<String> = state.hand_of(PlayerId::Us).map(|o| o.catalog_key.clone()).collect();
    hand.sort();
    PilotRunSummary {
        final_score: evaluate_forge_state(&state, PlayerId::Us),
        winner: state.winner.map(|p| p.to_string()),
        hand,
        battlefield,
        decision_log: state.decision_log.clone(),
    }
}


#[derive(Debug, Serialize)]
pub struct TrainingGenerationStats {
    pub evaluator_version: &'static str,
    pub games: u64,
    pub decisions_written: u64,
    pub seed: u64,
    pub max_turns: u8,
    pub rollouts_per_action: usize,
    pub output: String,
}

/// Generate newline-delimited JSON training examples from the search teacher.
/// Each row contains one decision state plus every candidate action and its
/// rollout score. Final game score/outcome are appended after the game ends so
/// the same file can train either an action-ranker or a coarse value model.
pub fn generate_training_jsonl(
    deck: &Decklist,
    games: u64,
    seed: u64,
    max_turns: u8,
    rollouts_per_action: usize,
    output: &Path,
) -> Result<TrainingGenerationStats, String> {
    let mut file = File::create(output).map_err(|e| format!("{}: {e}", output.display()))?;
    let mut decisions_written = 0u64;

    for game_index in 0..games {
        let game_seed = splitmix64(seed.wrapping_add(game_index));
        let summary = run_smoke_once(deck, game_seed, max_turns, true, rollouts_per_action);
        for line in &summary.decision_log {
            let Some(json) = line.strip_prefix("FORGE_TRAIN\t") else { continue };
            let mut value: serde_json::Value = serde_json::from_str(json)
                .map_err(|e| format!("training record serialization bug: {e}"))?;
            if let Some(obj) = value.as_object_mut() {
                obj.insert("game_index".to_string(), serde_json::json!(game_index));
                obj.insert("final_score".to_string(), serde_json::json!(summary.final_score));
                obj.insert("final_outcome".to_string(), serde_json::json!(summary.winner.clone()));
            }
            serde_json::to_writer(&mut file, &value)
                .map_err(|e| format!("{}: {e}", output.display()))?;
            file.write_all(b"\n").map_err(|e| format!("{}: {e}", output.display()))?;
            decisions_written += 1;
        }
    }

    Ok(TrainingGenerationStats {
        evaluator_version: FORGE_EVALUATOR_VERSION,
        games,
        decisions_written,
        seed,
        max_turns,
        rollouts_per_action,
        output: output.display().to_string(),
    })
}
