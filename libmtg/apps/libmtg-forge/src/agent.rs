use std::sync::Arc;

use libmtg_decklist::Decklist;
use libmtg_engine::{
    build_catalog, run_game, AlwaysPass, AnnounceChoice, AnnounceOptions, GameEvent,
    LegalAction, ObjId, Objective, PlayerId, Scenario, SimState, SpellFace, Strategy,
    TargetGap, WishOption,
};
use rand::{rngs::SmallRng, SeedableRng};
use serde::{Deserialize, Serialize};

use crate::profile::{keep_v0, role_for, ForgeRole};
use crate::search::evaluate_forge_state;
use crate::strategy::{forge_announce_choice, ForgeStrategy};

pub const AGENT_OBSERVATION_SCHEMA_VERSION: &str = "forge_agent_obs_v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentCardRef {
    /// Opaque, stable only for this game. Agents should echo action ids rather
    /// than manufacture object ids.
    pub object_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentPermanent {
    pub object_id: String,
    pub name: String,
    pub tapped: bool,
    pub artifact: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentMana {
    pub white: i32,
    pub blue: i32,
    pub black: i32,
    pub red: i32,
    pub green: i32,
    pub colorless: i32,
    pub floating_total: i32,
    /// Includes mana the engine predicts can be generated immediately.
    pub potential_total: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentActionKind {
    Pass,
    PlayLand,
    CastSpell,
    ActivateAbility,
    ActivateManaAbility,
}

/// Canonical external-agent representation of one engine-approved legal action.
/// `id` is the only value an external model needs to return.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAction {
    pub id: String,
    pub semantic: String,
    pub kind: AgentActionKind,
    pub card_name: Option<String>,
    /// Opaque source/card object for duplicate-copy disambiguation.
    pub source_object_id: Option<String>,
    pub face: Option<String>,
    pub ability_index: Option<usize>,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentObservation {
    pub schema_version: String,
    pub player: String,
    pub turn: u8,
    pub phase: String,
    pub active_player: Option<String>,
    pub on_play: bool,
    pub life: i32,
    pub opponent_life: i32,
    pub hand: Vec<AgentCardRef>,
    pub opponent_hand_size: i32,
    /// Only identities the engine marks as publicly known. Hidden cards are
    /// intentionally absent even though SimState internally knows them.
    pub known_opponent_hand: Vec<AgentCardRef>,
    pub battlefield: Vec<AgentPermanent>,
    pub opponent_battlefield: Vec<AgentPermanent>,
    pub graveyard: Vec<AgentCardRef>,
    pub opponent_graveyard: Vec<AgentCardRef>,
    pub exile: Vec<AgentCardRef>,
    pub opponent_exile: Vec<AgentCardRef>,
    pub stack: Vec<AgentCardRef>,
    pub library_size: usize,
    pub opponent_library_size: usize,
    /// Revealed/known top card, or the top card while Mystic Forge makes it
    /// visible to its controller. Never populated from a hidden library top.
    pub visible_top: Option<AgentCardRef>,
    pub mana: AgentMana,
    pub legal_actions: Vec<AgentAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentDecision {
    pub action_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAnnouncementObservation {
    pub schema_version: String,
    pub card: AgentCardRef,
    pub available_modes: Vec<usize>,
    pub has_x_cost: bool,
    pub max_x: u32,
    pub alternate_cost_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentAnnouncementDecision {
    pub chosen_mode: usize,
    pub chosen_x: u32,
    pub alt_cost_index: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentWishChoice {
    pub id: String,
    pub name: String,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentWishObservation {
    pub schema_version: String,
    pub choices: Vec<AgentWishChoice>,
}

/// Boundary for learned/cloud agents. The priority decision consumes only an
/// `AgentObservation`, not raw SimState, preventing accidental hidden-info use.
/// Announcement and wish decisions are also structured and validated by the
/// strategy adapter. Returning `None` delegates that decision to the existing
/// Forge heuristic.
pub trait ForgeAgent {
    fn name(&self) -> &str;
    fn choose_action(&mut self, observation: &AgentObservation) -> AgentDecision;

    fn choose_announcement(
        &mut self,
        _observation: &AgentAnnouncementObservation,
    ) -> Option<AgentAnnouncementDecision> {
        None
    }

    fn choose_wish(&mut self, _observation: &AgentWishObservation) -> Option<String> {
        None
    }

    /// Optional structured diagnostics emitted by external/learned agents.
    /// The strategy adapter appends these to the normal decision log after each
    /// agent call. Implementations must never include secrets such as API keys.
    fn drain_diagnostics(&mut self) -> Vec<String> { Vec::new() }
}

fn obj_ref(state: &SimState, id: ObjId) -> AgentCardRef {
    AgentCardRef {
        object_id: format!("{:?}", id),
        name: state.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_default(),
    }
}

fn permanent_ref(state: &SimState, id: ObjId) -> AgentPermanent {
    let name = state.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_default();
    AgentPermanent {
        object_id: format!("{:?}", id),
        name,
        tapped: state.objects.get(&id).and_then(|o| o.bf()).map_or(false, |bf| bf.tapped),
        artifact: state.def_of(id).map_or(false, |d| d.is_artifact()),
    }
}

fn action_description(name: &str, kind: &AgentActionKind, ability_index: Option<usize>) -> String {
    if let Some(idx) = ability_index {
        let specific = match (name, idx) {
            ("Manifold Key", 0) => "Untap another target artifact you control.",
            ("Manifold Key", 1) => "Target creature can't be blocked this turn.",
            ("Basalt Monolith", 0) => "Pay {3}: untap Basalt Monolith.",
            ("Grim Monolith", 0) => "Pay {4}: untap Grim Monolith.",
            ("Mystic Forge", 0) => "Pay 1 life: exile the top card of your library.",
            ("Urza's Saga", 0) => "Create a Construct token.",
            ("Karn, the Great Creator", 0) => "Activate Karn ability 0.",
            ("Karn, the Great Creator", 1) => "Activate Karn ability 1.",
            ("Tezzeret, Cruel Captain", 0) => "Activate Tezzeret ability 0.",
            ("Tezzeret, Cruel Captain", 1) => "Activate Tezzeret ability 1.",
            ("The One Ring", 0) => "Activate The One Ring's draw ability.",
            _ => "Activate this engine-approved ability.",
        };
        return specific.to_string();
    }
    match kind {
        AgentActionKind::Pass => "Pass priority.".to_string(),
        AgentActionKind::PlayLand => format!("Play {name} as the land for this turn."),
        AgentActionKind::CastSpell => format!("Cast {name}."),
        AgentActionKind::ActivateManaAbility => format!("Activate {name}'s priority mana ability."),
        AgentActionKind::ActivateAbility => "Activate this engine-approved ability.".to_string(),
    }
}

/// Cross-game semantic identity for learning/ranking.
pub fn legal_action_semantic(action: &LegalAction, state: &SimState) -> String {
    match action {
        LegalAction::Pass => "pass".to_string(),
        LegalAction::LandDrop(id) => format!("land:{}", card_name(state, *id)),
        LegalAction::CastSpell { card_id, face } => format!("cast:{}:{:?}", card_name(state, *card_id), face),
        LegalAction::ActivateAbility { source_id, ability_index } => {
            format!("activate:{}#{}", card_name(state, *source_id), ability_index)
        }
        LegalAction::ActivateManaAbility { source_id, ability_index } => {
            format!("mana:{}#{}", card_name(state, *source_id), ability_index)
        }
    }
}

/// Per-game opaque identity. Distinguishes duplicate copies but is never parsed
/// back by the agent; the adapter matches it against the legal list it created.
pub fn legal_action_id(action: &LegalAction, state: &SimState) -> String {
    match action {
        LegalAction::Pass => "pass".to_string(),
        LegalAction::LandDrop(id) => format!("land:{}@{:?}", card_name(state, *id), id),
        LegalAction::CastSpell { card_id, face } => {
            format!("cast:{}@{:?}:{:?}", card_name(state, *card_id), card_id, face)
        }
        LegalAction::ActivateAbility { source_id, ability_index } => {
            format!("activate:{}@{:?}#{}", card_name(state, *source_id), source_id, ability_index)
        }
        LegalAction::ActivateManaAbility { source_id, ability_index } => {
            format!("mana:{}@{:?}#{}", card_name(state, *source_id), source_id, ability_index)
        }
    }
}

pub fn agent_action_from_legal(action: &LegalAction, state: &SimState) -> AgentAction {
    let (kind, card_name_value, face, ability_index) = match action {
        LegalAction::Pass => (AgentActionKind::Pass, None, None, None),
        LegalAction::LandDrop(id) => (
            AgentActionKind::PlayLand,
            Some(card_name(state, *id).to_string()),
            None,
            None,
        ),
        LegalAction::CastSpell { card_id, face } => (
            AgentActionKind::CastSpell,
            Some(card_name(state, *card_id).to_string()),
            Some(match face { SpellFace::Main => "main", SpellFace::Back => "back" }.to_string()),
            None,
        ),
        LegalAction::ActivateAbility { source_id, ability_index } => (
            AgentActionKind::ActivateAbility,
            Some(card_name(state, *source_id).to_string()),
            None,
            Some(*ability_index),
        ),
        LegalAction::ActivateManaAbility { source_id, ability_index } => (
            AgentActionKind::ActivateManaAbility,
            Some(card_name(state, *source_id).to_string()),
            None,
            Some(*ability_index),
        ),
    };
    let source_object_id = match action {
        LegalAction::Pass => None,
        LegalAction::LandDrop(id) => Some(format!("{:?}", id)),
        LegalAction::CastSpell { card_id, .. } => Some(format!("{:?}", card_id)),
        LegalAction::ActivateAbility { source_id, .. } | LegalAction::ActivateManaAbility { source_id, .. } => Some(format!("{:?}", source_id)),
    };
    let description = action_description(card_name_value.as_deref().unwrap_or(""), &kind, ability_index);
    AgentAction {
        id: legal_action_id(action, state),
        semantic: legal_action_semantic(action, state),
        kind,
        card_name: card_name_value,
        source_object_id,
        face,
        ability_index,
        description,
    }
}

pub fn build_agent_observation(
    state: &SimState,
    who: PlayerId,
    legal: &[LegalAction],
) -> AgentObservation {
    let mut hand: Vec<_> = state.hand_of(who).map(|o| obj_ref(state, o.id)).collect();
    hand.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.object_id.cmp(&b.object_id)));

    let mut known_opponent_hand: Vec<_> = state.known_hand_of(who.opp()).map(|o| obj_ref(state, o.id)).collect();
    known_opponent_hand.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.object_id.cmp(&b.object_id)));

    let mut battlefield: Vec<_> = state.permanents_of(who).map(|o| permanent_ref(state, o.id)).collect();
    battlefield.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.object_id.cmp(&b.object_id)));
    let mut opponent_battlefield: Vec<_> = state.permanents_of(who.opp()).map(|o| permanent_ref(state, o.id)).collect();
    opponent_battlefield.sort_by(|a, b| a.name.cmp(&b.name).then_with(|| a.object_id.cmp(&b.object_id)));

    let mut graveyard: Vec<_> = state.graveyard_of(who).map(|o| obj_ref(state, o.id)).collect();
    graveyard.sort_by(|a, b| a.name.cmp(&b.name));
    let mut opponent_graveyard: Vec<_> = state.graveyard_of(who.opp()).map(|o| obj_ref(state, o.id)).collect();
    opponent_graveyard.sort_by(|a, b| a.name.cmp(&b.name));
    let mut exile: Vec<_> = state.exile_of(who).map(|o| obj_ref(state, o.id)).collect();
    exile.sort_by(|a, b| a.name.cmp(&b.name));
    let mut opponent_exile: Vec<_> = state.exile_of(who.opp()).map(|o| obj_ref(state, o.id)).collect();
    opponent_exile.sort_by(|a, b| a.name.cmp(&b.name));

    let stack = state.stack.iter().copied().map(|id| obj_ref(state, id)).collect();

    let forge_active = state.permanents_of(who).any(|o| o.catalog_key == "Mystic Forge");
    let top_is_known = forge_active || state.player(who).known_top_len > 0;
    let visible_top = if top_is_known {
        state.player(who).library_order.front().copied().map(|id| obj_ref(state, id))
    } else {
        None
    };

    let pool = &state.player(who).pool;
    AgentObservation {
        schema_version: AGENT_OBSERVATION_SCHEMA_VERSION.to_string(),
        player: who.to_string(),
        turn: state.current_turn,
        phase: format!("{:?}", state.current_phase),
        active_player: state.active_player().map(|p| p.to_string()),
        on_play: state.on_play,
        life: state.player(who).life,
        opponent_life: state.player(who.opp()).life,
        hand,
        opponent_hand_size: state.hand_size(who.opp()),
        known_opponent_hand,
        battlefield,
        opponent_battlefield,
        graveyard,
        opponent_graveyard,
        exile,
        opponent_exile,
        stack,
        library_size: state.library_size(who),
        opponent_library_size: state.library_size(who.opp()),
        visible_top,
        mana: AgentMana {
            white: pool.w,
            blue: pool.u,
            black: pool.b,
            red: pool.r,
            green: pool.g,
            colorless: pool.c,
            floating_total: pool.total,
            potential_total: state.potential_mana(who).total,
        },
        legal_actions: legal.iter().map(|a| agent_action_from_legal(a, state)).collect(),
    }
}

fn card_name(state: &SimState, id: ObjId) -> &str {
    state.objects.get(&id).map(|o| o.catalog_key.as_str()).unwrap_or("")
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

/// Pure observation-only equivalent of the existing priority-table Forge pilot.
pub struct HeuristicForgeAgent;
impl ForgeAgent for HeuristicForgeAgent {
    fn name(&self) -> &str { "heuristic" }

    fn choose_action(&mut self, observation: &AgentObservation) -> AgentDecision {
        if let Some(action) = observation.legal_actions.iter().find(|a| a.kind == AgentActionKind::PlayLand) {
            return AgentDecision { action_id: action.id.clone() };
        }
        let chosen = observation.legal_actions.iter()
            .filter(|a| a.kind != AgentActionKind::Pass)
            .filter(|a| {
                let name = a.card_name.as_deref().unwrap_or("");
                if a.kind == AgentActionKind::ActivateAbility && matches!(name, "Basalt Monolith" | "Grim Monolith") {
                    if let Some(source) = &a.source_object_id {
                        if let Some(perm) = observation.battlefield.iter().find(|p| &p.object_id == source) {
                            return perm.tapped;
                        }
                    }
                }
                if a.kind == AgentActionKind::ActivateAbility && name == "Manifold Key" && a.ability_index == Some(1) {
                    return false;
                }
                true
            })
            .max_by_key(|a| card_priority(a.card_name.as_deref().unwrap_or("")))
            .or_else(|| observation.legal_actions.iter().find(|a| a.kind == AgentActionKind::Pass));
        AgentDecision { action_id: chosen.map(|a| a.id.clone()).unwrap_or_else(|| "pass".to_string()) }
    }
}

pub struct PassForgeAgent;
impl ForgeAgent for PassForgeAgent {
    fn name(&self) -> &str { "pass" }
    fn choose_action(&mut self, observation: &AgentObservation) -> AgentDecision {
        let id = observation.legal_actions.iter()
            .find(|a| a.kind == AgentActionKind::Pass)
            .or_else(|| observation.legal_actions.first())
            .map(|a| a.id.clone())
            .unwrap_or_else(|| "pass".to_string());
        AgentDecision { action_id: id }
    }
}

/// Test-only style agent useful for proving validation/fallback behavior.
pub struct InvalidForgeAgent;
impl ForgeAgent for InvalidForgeAgent {
    fn name(&self) -> &str { "invalid" }
    fn choose_action(&mut self, _observation: &AgentObservation) -> AgentDecision {
        AgentDecision { action_id: "not-a-legal-action".to_string() }
    }
}

/// Bridges the hidden-info-safe external-agent API to the engine Strategy trait.
/// Every returned decision is matched against the legal action list produced for
/// that exact priority window. Unknown ids are logged and delegated to the
/// existing ForgeStrategy rather than executed.
pub struct AgentBackedStrategy {
    who: PlayerId,
    agent: Box<dyn ForgeAgent>,
    fallback: ForgeStrategy,
    decisions: Vec<String>,
}

impl AgentBackedStrategy {
    pub fn new(who: PlayerId, agent: Box<dyn ForgeAgent>) -> Self {
        Self { who, agent, fallback: ForgeStrategy::new(who), decisions: Vec::new() }
    }
}

impl Strategy for AgentBackedStrategy {
    fn declare_attackers(&mut self, state: &SimState) -> Vec<(ObjId, Option<ObjId>)> {
        self.fallback.declare_attackers(state)
    }

    fn declare_blockers(&mut self, state: &SimState) -> Vec<(ObjId, ObjId)> {
        self.fallback.declare_blockers(state)
    }

    fn take_mulligan(&mut self, state: &SimState, mulligans_taken: u32) -> bool {
        // Mulligan decisions stay on the validated Forge bootstrap policy in v1.
        // A later observation schema can surface London-bottom choices separately.
        let hand: Vec<String> = state.hand_of(self.who).map(|c| c.catalog_key.clone()).collect();
        !keep_v0(&hand, mulligans_taken)
    }

    fn choose_action(&mut self, state: &SimState, ap: PlayerId, legal: &[LegalAction]) -> LegalAction {
        let observation = build_agent_observation(state, self.who, legal);
        if let Ok(json) = serde_json::to_string(&observation) {
            self.decisions.push(format!("AGENT_OBS\t{}", json));
        }
        let decision = self.agent.choose_action(&observation);
        self.decisions.extend(self.agent.drain_diagnostics());
        if let Some((idx, _)) = observation.legal_actions.iter().enumerate()
            .find(|(_, a)| a.id == decision.action_id)
        {
            self.decisions.push(format!(
                "agent:{} chose {}",
                self.agent.name(),
                decision.action_id
            ));
            return legal.get(idx).cloned().unwrap_or(LegalAction::Pass);
        }

        let fallback = self.fallback.choose_action(state, ap, legal);
        self.decisions.push(format!(
            "agent:{} INVALID action_id={} -> fallback {}",
            self.agent.name(),
            decision.action_id,
            legal_action_id(&fallback, state)
        ));
        fallback
    }

    fn announce(&mut self, state: &SimState, card_id: ObjId, options: &AnnounceOptions) -> AnnounceChoice {
        let observation = AgentAnnouncementObservation {
            schema_version: AGENT_OBSERVATION_SCHEMA_VERSION.to_string(),
            card: obj_ref(state, card_id),
            available_modes: options.available_modes.clone(),
            has_x_cost: options.has_x_cost,
            max_x: options.max_x,
            alternate_cost_count: options.available_alt_costs.len(),
        };
        let agent_announcement = self.agent.choose_announcement(&observation);
        self.decisions.extend(self.agent.drain_diagnostics());
        if let Some(decision) = agent_announcement {
            let mode_ok = options.available_modes.is_empty() || options.available_modes.contains(&decision.chosen_mode);
            let x_ok = (!options.has_x_cost && decision.chosen_x == 0)
                || (options.has_x_cost && decision.chosen_x <= options.max_x);
            let alt_ok = decision.alt_cost_index.map_or(true, |i| i < options.available_alt_costs.len());
            if mode_ok && x_ok && alt_ok {
                self.decisions.push(format!(
                    "agent:{} announcement {} mode={} x={}",
                    self.agent.name(),
                    observation.card.name,
                    decision.chosen_mode,
                    decision.chosen_x
                ));
                return AnnounceChoice {
                    chosen_mode: decision.chosen_mode,
                    chosen_x: decision.chosen_x,
                    alt_cost_index: decision.alt_cost_index,
                };
            }
            self.decisions.push(format!(
                "agent:{} INVALID announcement for {} -> fallback",
                self.agent.name(), observation.card.name
            ));
        }
        forge_announce_choice(state, card_id, options)
    }

    fn choose_wish(&mut self, effect_id: ObjId, choices: &[WishOption], state: &SimState) -> Option<WishOption> {
        let agent_choices: Vec<AgentWishChoice> = choices.iter().map(|choice| match choice {
            WishOption::Sideboard { index, name } => AgentWishChoice {
                id: format!("sideboard:{}:{}", index, name),
                name: name.clone(),
                source: "sideboard".to_string(),
            },
            WishOption::Exile { id, name } => AgentWishChoice {
                id: format!("exile:{:?}:{}", id, name),
                name: name.clone(),
                source: "exile".to_string(),
            },
        }).collect();
        let observation = AgentWishObservation {
            schema_version: AGENT_OBSERVATION_SCHEMA_VERSION.to_string(),
            choices: agent_choices,
        };
        let agent_wish = self.agent.choose_wish(&observation);
        self.decisions.extend(self.agent.drain_diagnostics());
        if let Some(id) = agent_wish {
            if let Some((idx, _)) = observation.choices.iter().enumerate().find(|(_, c)| c.id == id) {
                self.decisions.push(format!("agent:{} wish chose {}", self.agent.name(), id));
                return choices.get(idx).cloned();
            }
            self.decisions.push(format!("agent:{} INVALID wish={} -> fallback", self.agent.name(), id));
        }
        self.fallback.choose_wish(effect_id, choices, state)
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
        self.fallback.card_fills(card_id, gap, state)
    }

    fn drain_decisions(&mut self) -> Vec<String> {
        std::mem::take(&mut self.decisions)
    }
}

#[derive(Default)]
struct AgentSmokeObjective;
impl Objective for AgentSmokeObjective {
    fn observe(&mut self, _event: &GameEvent, _state: &mut SimState) -> bool { false }
}

#[derive(Debug, Serialize)]
pub struct AgentSmokeSummary {
    pub observation_schema_version: &'static str,
    pub agent: String,
    pub seed: u64,
    pub max_turns: u8,
    pub final_score: f64,
    pub winner: Option<String>,
    pub invalid_decisions: usize,
    pub observations_emitted: usize,
    pub hand: Vec<String>,
    pub battlefield: Vec<String>,
    pub decision_log: Vec<String>,
}

pub fn run_agent_smoke_with_agent(
    deck: &Decklist,
    agent: Box<dyn ForgeAgent>,
    seed: u64,
    max_turns: u8,
) -> Result<AgentSmokeSummary, String> {
    let normalized_name = agent.name().to_string();
    let catalog = build_catalog();
    let opponent = vec![("Island".to_string(), 60, "main".to_string())];
    let mut rng = SmallRng::seed_from_u64(seed);
    let state = run_game(
        Scenario {
            us_label: format!("Forge Agent ({normalized_name})"),
            opp_label: "Inert Opponent".to_string(),
            catalog,
            us_deck: deck.to_engine_deck(),
            opp_deck: opponent,
            us_strategy: Box::new(AgentBackedStrategy::new(PlayerId::Us, agent)),
            opp_strategy: Box::new(AlwaysPass::new(PlayerId::Opp)),
            evaluate_card: Arc::new(|_, _, _| 0.5),
            objective: Box::<AgentSmokeObjective>::default(),
            max_turns,
            on_play: Some(true),
            fixed_us_hand: None,
        },
        &mut rng,
    );

    let invalid_decisions = state.decision_log.iter().filter(|line| line.contains(" INVALID ")).count();
    let observations_emitted = state.decision_log.iter().filter(|line| line.starts_with("AGENT_OBS\t")).count();
    let mut battlefield: Vec<String> = state.permanents_of(PlayerId::Us).map(|o| o.catalog_key.clone()).collect();
    battlefield.sort();
    let mut hand: Vec<String> = state.hand_of(PlayerId::Us).map(|o| o.catalog_key.clone()).collect();
    hand.sort();
    Ok(AgentSmokeSummary {
        observation_schema_version: AGENT_OBSERVATION_SCHEMA_VERSION,
        agent: normalized_name,
        seed,
        max_turns,
        final_score: evaluate_forge_state(&state, PlayerId::Us),
        winner: state.winner.map(|p| p.to_string()),
        invalid_decisions,
        observations_emitted,
        hand,
        battlefield,
        decision_log: state.decision_log.clone(),
    })
}

pub fn run_agent_smoke(
    deck: &Decklist,
    agent_name: &str,
    seed: u64,
    max_turns: u8,
) -> Result<AgentSmokeSummary, String> {
    let agent: Box<dyn ForgeAgent> = match agent_name {
        "heuristic" => Box::new(HeuristicForgeAgent),
        "pass" => Box::new(PassForgeAgent),
        "invalid" => Box::new(InvalidForgeAgent),
        other => return Err(format!("unknown built-in agent '{other}'; expected heuristic, pass, or invalid")),
    };
    run_agent_smoke_with_agent(deck, agent, seed, max_turns)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example_deck() -> Decklist {
        Decklist::parse_text(include_str!("../examples/forge-trinisphere.txt"))
    }

    #[test]
    fn invalid_agent_action_is_rejected_and_falls_back() {
        let result = run_agent_smoke(&example_deck(), "invalid", 1, 1).expect("agent smoke");
        assert!(result.observations_emitted > 0, "agent adapter should emit observations");
        assert!(result.invalid_decisions > 0, "invalid ids must be rejected and logged");
    }

    #[test]
    fn observation_does_not_leak_hidden_opponent_hand_names() {
        let result = run_agent_smoke(&example_deck(), "pass", 2, 1).expect("agent smoke");
        let json = result.decision_log.iter()
            .find_map(|line| line.strip_prefix("AGENT_OBS\t"))
            .expect("at least one observation");
        let observation: AgentObservation = serde_json::from_str(json).expect("valid observation json");
        assert!(observation.opponent_hand_size > 0, "hand size is public information");
        assert!(observation.known_opponent_hand.is_empty(), "hidden opponent identities must stay hidden");
        assert!(!json.contains("Island"), "hidden opponent card names must not leak into observation JSON");
    }
}
