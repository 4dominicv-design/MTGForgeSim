//! Doomsday + generic-opponent strategies (concrete `Strategy` content, peeled
//! out of the engine). The engine is generic; all Doomsday content lives in this
//! crate.
use rand::Rng;
use rand::rngs::SmallRng;
use libmtg_engine::*;
use rand::SeedableRng;
use crate::planner::*;

// ── Mana heuristics (moved out of the engine; these are strategy helpers, not
// primitive state projections). They read engine primitives (potential_mana,
// hand_of, mana abilities) but encode Doomsday-specific judgement. ──────────────

/// Doomsday-specific fetch-land knowledge (content, not engine).
pub(crate) fn is_fetch(name: &str) -> bool {
    matches!(name,
        "Polluted Delta" | "Flooded Strand" | "Misty Rainforest" | "Scalding Tarn"
        | "Marsh Flats" | "Bloodstained Mire" | "Windswept Heath" | "Wooded Foothills"
        | "Verdant Catacombs" | "Arid Mesa"
    )
}

/// Mana potential from lands in hand only.  Used for mulligan decisions.
/// Regular lands contribute their mana abilities.  Fetch lands contribute
/// `fetch_colors` (deck-level knowledge of what duals they can find).
pub(crate) fn hand_land_mana(state: &SimState, who: PlayerId, fetch_colors: &[Color]) -> ManaPool {
    let mut p = ManaPool::default();
    for card in state.hand_of(who) {
        let Some(def) = state.catalog.get(&card.catalog_key) else { continue };
        if !def.is_land() { continue; }
        let mas = def.mana_abilities();
        if !mas.is_empty() {
            // Regular mana-producing land.
            accumulate_source_potential(mas, false, &mut p);
        } else if is_fetch(&card.catalog_key) {
            // Fetch land — contributes the deck's fetch colors.
            p.total += 1;
            for &c in fetch_colors {
                match c {
                    Color::White => p.w += 1,
                    Color::Blue  => p.u += 1,
                    Color::Black => p.b += 1,
                    Color::Red   => p.r += 1,
                    Color::Green => p.g += 1,
                }
            }
        }
        // Lands with no mana abilities and not a fetch (e.g. Wasteland) contribute nothing.
    }
    p
}

/// True if `who` can currently produce at least one black mana.
pub(crate) fn has_black_mana(state: &SimState, who: PlayerId) -> bool {
    state.potential_mana(who).b > 0
}

/// What broad role does this card serve in a player's plan?
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CardCategory {
    Mana,        // lands, rituals, petals
    Threat,      // creatures, PWs, Doomsday (combo is a subset of threat)
    Interaction, // counters, removal, discard
    Selection,   // cantrips — digs toward missing pieces
}

/// Opponent characteristics that shift card evaluation weights.
#[derive(Clone, Debug)]
pub struct MatchupInfo {
    pub(crate) opp_has_counters: bool,
    pub(crate) opp_fast_clock: bool,
    /// Colors that fetch lands in this deck can find (deck-level, not per-card).
    pub(crate) fetch_colors: Vec<Color>,
}

impl Default for MatchupInfo {
    fn default() -> Self {
        MatchupInfo {
            opp_has_counters: true,
            opp_fast_clock: false,
            fetch_colors: vec![Color::Blue, Color::Black],
        }
    }
}


// ── Decision logging helpers ─────────────────────────────────────────────────

/// Summarize a hand's category composition for logging. Returns e.g. "(M=2 T=1 I=1 S=2 ?=1)".
fn hand_category_summary(
    state: &SimState,
    who: PlayerId,
    categorize: fn(&str, Option<&CardDef>) -> Option<CardCategory>,
) -> String {
    let (mut m, mut t, mut i, mut s, mut dead) = (0, 0, 0, 0, 0);
    for c in state.hand_of(who) {
        let def = state.def_of(c.id).or_else(|| state.catalog.get(c.catalog_key.as_str()));
        match categorize(&c.catalog_key, def) {
            Some(CardCategory::Mana)        => m += 1,
            Some(CardCategory::Threat)      => t += 1,
            Some(CardCategory::Interaction)  => i += 1,
            Some(CardCategory::Selection)    => s += 1,
            None                             => dead += 1,
        }
    }
    format!("(M={} T={} I={} S={} ?={})", m, t, i, s, dead)
}

// ── DoomsdayStrategy ─────────────────────────────────────────────────────────

pub struct DoomsdayStrategy {
    player_id: PlayerId,
    rng: SmallRng,
    /// Set when a black-producing land must be played next main phase.
    must_land_drop: bool,
    matchup: MatchupInfo,
    /// Accumulated decision-log entries, drained by the engine after each call.
    decisions: Vec<String>,
}

impl DoomsdayStrategy {
    pub fn new(matchup: MatchupInfo) -> Self {
        Self { player_id: PlayerId::Us, rng: SmallRng::from_entropy(),
               must_land_drop: false, matchup, decisions: Vec::new() }
    }
    fn dlog(&mut self, msg: impl Into<String>) { self.decisions.push(msg.into()); }
}

/// Categorize a card for the Doomsday player's pre-DD evaluation.
/// Returns `None` for cards that are dead weight pre-DD (Oracle, Unearth, LED).
pub(crate) fn dd_categorize(name: &str, def: Option<&CardDef>) -> Option<CardCategory> {
    match name {
        // Mana: rituals and fast mana (not LED — discards hand pre-DD)
        "Dark Ritual" | "Lotus Petal" => return Some(CardCategory::Mana),
        // THE combo piece
        "Doomsday" => return Some(CardCategory::Threat),
        // Interaction: counterspells and discard
        "Force of Will" | "Daze" | "Thoughtseize" => return Some(CardCategory::Interaction),
        // Selection: cantrips and free cyclers
        "Brainstorm" | "Ponder" | "Preordain" | "Consider"
        | "Street Wraith" | "Edge of Autumn" => return Some(CardCategory::Selection),
        // Dead pre-DD: only useful in the pile
        "Thassa's Oracle" | "Unearth" | "Lion's Eye Diamond" => return None,
        _ => {}
    }
    let def = def?;
    if def.is_land() { return Some(CardCategory::Mana); }
    if def.is_creature() || matches!(def.kind, CardKind::Planeswalker(_)) {
        return Some(CardCategory::Threat);
    }
    if def.is_instant() { return Some(CardCategory::Interaction); }
    if def.is_sorcery() { return Some(CardCategory::Interaction); }
    None
}

/// Compute the DD player's plan gap from hand + board state.
pub(crate) fn dd_plan_gap(state: &SimState, who: PlayerId, matchup: &MatchupInfo) -> TargetGap {
    // Helper: look up def with catalog fallback.
    let def_or_catalog = |id: ObjId, key: &str| -> Option<&CardDef> {
        state.def_of(id).or_else(|| state.catalog.get(key))
    };

    // ── Mana gap: path to BBB ────────────────────────────────────────────
    let lands_in_play = state.permanents_of(who)
        .filter(|c| def_or_catalog(c.id, &c.catalog_key).map_or(false, |d| d.is_land()))
        .count();
    let rituals_in_hand = state.hand_of(who)
        .filter(|c| matches!(c.catalog_key.as_str(), "Dark Ritual" | "Lotus Petal"))
        .count();
    let mana_sources = lands_in_play + rituals_in_hand;
    // Need ~3 mana sources for BBB (land + ritual, or 3 lands, etc.)
    let mana_gap = ((3.0 - mana_sources as f64) / 3.0).clamp(0.0, 1.0);

    // ── Threat gap: need Doomsday or a creature win-con ─────────────────
    let dd_in_hand = state.hand_of(who).any(|c| c.catalog_key == "Doomsday");
    let has_creature_threat = state.permanents_of(who)
        .any(|c| def_or_catalog(c.id, &c.catalog_key).map_or(false, |d| d.is_creature()));
    let threat_gap = if dd_in_hand {
        0.0
    } else if has_creature_threat {
        0.3 // have a backup plan, but DD is the real goal
    } else {
        1.0
    };

    // ── Interaction gap: matchup-dependent ───────────────────────────────
    let interaction_count = state.hand_of(who)
        .filter(|c| {
            let def = def_or_catalog(c.id, &c.catalog_key);
            dd_categorize(&c.catalog_key, def) == Some(CardCategory::Interaction)
        })
        .count();
    let interaction_gap = if matchup.opp_has_counters {
        // Against blue: want 1-2 interaction pieces to protect DD
        ((2.0 - interaction_count as f64) / 2.0).clamp(0.0, 1.0)
    } else {
        0.1 // non-blue matchup: interaction is low priority
    };

    TargetGap { mana: mana_gap, threat: threat_gap, interaction: interaction_gap }
}

/// Score how much `card_id` fills the DD player's current gap.
pub(crate) fn dd_card_fills(card_id: ObjId, gap: &TargetGap, state: &SimState, who: PlayerId) -> f64 {
    let name = match state.objects.get(&card_id) {
        Some(c) => c.catalog_key.as_str(),
        None => return 0.0,
    };
    let def = state.def_of(card_id).or_else(|| state.catalog.get(name));
    let cat = dd_categorize(name, def);

    match cat {
        Some(CardCategory::Mana) => {
            if gap.mana > 0.0 { 0.4 + 0.5 * gap.mana } else { 0.05 }
        }
        Some(CardCategory::Threat) => {
            if name == "Doomsday" {
                // Second DD in hand is near-worthless
                let other_dd = state.hand_of(who)
                    .filter(|c| c.catalog_key == "Doomsday" && c.id != card_id)
                    .count();
                if other_dd > 0 { 0.1 } else if gap.threat > 0.0 { 0.9 } else { 0.1 }
            } else {
                // Creature threats: valuable but not as critical as DD
                if gap.threat > 0.0 { 0.4 + 0.2 * gap.threat } else { 0.3 }
            }
        }
        Some(CardCategory::Interaction) => {
            if gap.interaction > 0.0 { 0.3 + 0.5 * gap.interaction } else { 0.1 }
        }
        Some(CardCategory::Selection) => {
            // Cantrips: always medium — universal lubricant that digs toward anything
            0.35
        }
        None => 0.05, // Uncategorized: dead pre-DD
    }
}

/// Hand-aware mulligan for DD player. Uses actual mana color production.
/// - Need ≥1 blue source (casts cantrips) OR ≥3 black sources (hardcasts DD).
/// - Mull flood (≥5 lands) or hands with no path to DD (no threats/cantrips).
/// - 4 or fewer cards: always keep.
pub fn dd_should_mulligan(state: &SimState, who: PlayerId, mulligans_taken: u32) -> bool {
    if mulligans_taken >= 3 { return false; } // always keep at 4 cards

    let hand: Vec<(ObjId, Option<CardCategory>)> = state.hand_of(who)
        .map(|c| {
            let def = state.def_of(c.id).or_else(|| state.catalog.get(c.catalog_key.as_str()));
            (c.id, dd_categorize(&c.catalog_key, def))
        })
        .collect();

    let hand_size = hand.len();
    let mana = hand_land_mana(state, who, &[Color::Blue, Color::Black]);
    let has_useful_mana = mana.u >= 1 || mana.b >= 3;
    let threat_count = hand.iter().filter(|(_, cat)| *cat == Some(CardCategory::Threat)).count();
    let selection_count = hand.iter().filter(|(_, cat)| *cat == Some(CardCategory::Selection)).count();
    let interaction_count = hand.iter().filter(|(_, cat)| *cat == Some(CardCategory::Interaction)).count();
    let spells = threat_count + selection_count + interaction_count;

    match mulligans_taken {
        0 => {
            // 7 cards: mull no useful mana, flood, or no path to DD
            if !has_useful_mana { return true; }
            if mana.total >= 5 { return true; }
            // Need at least a cantrip or combo piece to have a plan
            if threat_count == 0 && selection_count == 0 { return true; }
            false
        }
        1 => {
            // 6 cards: need useful mana + ≥1 spell
            if !has_useful_mana { return true; }
            if mana.total >= 5 { return true; }
            if spells == 0 { return true; }
            false
        }
        2 => {
            // 5 cards: keep with any useful mana + ≥1 spell
            if !has_useful_mana && spells == hand_size { return true; }
            false
        }
        _ => false,
    }
}

impl Strategy for DoomsdayStrategy {
    fn drain_decisions(&mut self) -> Vec<String> { std::mem::take(&mut self.decisions) }

    fn choose_action(&mut self, state: &SimState, ap: PlayerId,
                     legal_actions: &[LegalAction]) -> LegalAction {
        let who = self.player_id;
        let t = state.current_turn;
        if who != ap {
            if state.stack.is_empty() { return LegalAction::Pass; }
            return choose_nap_action(state, who, legal_actions, &mut self.rng, &mut self.decisions);
        }
        let in_ninjutsu_step = matches!(state.current_phase,
            Some(TurnPosition::Step(StepKind::DeclareBlockers))
            | Some(TurnPosition::Step(StepKind::FirstStrikeCombatDamage))
            | Some(TurnPosition::Step(StepKind::CombatDamage))
            | Some(TurnPosition::Step(StepKind::EndCombat)));
        if in_ninjutsu_step {
            if let Some(action) = choose_ninjutsu_action(state, who, legal_actions, &mut self.rng) {
                return action;
            }
            return LegalAction::Pass;
        }
        let in_main_phase = matches!(state.current_phase,
            Some(TurnPosition::Phase(PhaseKind::PreCombatMain))
            | Some(TurnPosition::Phase(PhaseKind::PostCombatMain)));
        if !in_main_phase { return LegalAction::Pass; }
        if let Some(action) = choose_ap_react(state, who, legal_actions, &mut self.rng, &mut self.decisions) {
            return action;
        }
        choose_ap_proactive(state, t, who, legal_actions,
            &mut self.must_land_drop, &mut self.rng, &mut self.decisions, dd_plan_quality)
    }

    fn choose_mana_ability(&mut self, state: &SimState, who: PlayerId,
                           available: &[ManaAbilityOption],
                           mana_cost: &ManaCost) -> Option<ManaActivation> {
        // Never auto-crack LED — discarding your hand is a pile-building decision.
        let filtered: Vec<_> = available.iter()
            .filter(|a| state.objects.get(&a.source_id)
                .map_or(true, |c| c.catalog_key != "Lion's Eye Diamond"))
            .cloned()
            .collect();
        let plan = auto_tap_plan_remaining(state, who, mana_cost);
        plan.into_iter().find(|act| {
            filtered.iter().any(|a| a.source_id == act.source_id && a.ability_index == act.ability_index)
        })
    }

    fn announce(&mut self, state: &SimState, card_id: ObjId,
                options: &AnnounceOptions) -> AnnounceChoice {
        // For reactive casts (instants on non-empty stack), try alt costs deterministically.
        announce_with_alt_costs(state, self.player_id, card_id, options, &mut self.rng, false)
    }

    fn declare_attackers(&mut self, state: &SimState) -> Vec<(ObjId, Option<ObjId>)> {
        pick_attackers(self.player_id, state, &mut self.rng)
    }

    fn declare_blockers(&mut self, state: &SimState) -> Vec<(ObjId, ObjId)> {
        pick_blockers(self.player_id, state)
    }

    fn take_mulligan(&mut self, state: &SimState, mulligans_taken: u32) -> bool {
        let who = self.player_id;
        let mull = dd_should_mulligan(state, who, mulligans_taken);
        let hand: Vec<String> = state.hand_of(who).map(|c| c.catalog_key.clone()).collect();
        let cats = hand_category_summary(state, who, dd_categorize);
        self.dlog(format!("T0 {}: mulligan#{} hand=[{}] {} → {}",
            who, mulligans_taken, hand.join(", "), cats,
            if mull { "MULL" } else { "KEEP" }));
        mull
    }

    fn player_id(&self) -> PlayerId { self.player_id }

    fn plan_gap(&self, state: &SimState) -> TargetGap {
        dd_plan_gap(state, self.player_id, &self.matchup)
    }

    fn card_fills(&self, card_id: ObjId, gap: &TargetGap, state: &SimState) -> f64 {
        dd_card_fills(card_id, gap, state, self.player_id)
    }
}

// ── GenericOppStrategy ────────────────────────────────────────────────────────

pub struct GenericOppStrategy {
    player_id: PlayerId,
    rng: SmallRng,
    matchup: MatchupInfo,
    /// Accumulated decision-log entries, drained by the engine after each call.
    decisions: Vec<String>,
}

impl GenericOppStrategy {
    pub(crate) fn new(matchup: MatchupInfo) -> Self {
        Self { player_id: PlayerId::Opp, rng: SmallRng::from_entropy(), matchup, decisions: Vec::new() }
    }
    fn dlog(&mut self, msg: impl Into<String>) { self.decisions.push(msg.into()); }
}

/// Categorize a card for the tempo/fair opponent's evaluation.
pub(crate) fn opp_categorize(name: &str, def: Option<&CardDef>) -> Option<CardCategory> {
    match name {
        // Interaction: counters, removal, discard
        "Force of Will" | "Force of Negation" | "Daze" | "Thoughtseize"
        | "Fatal Push" | "Snuff Out" | "Lightning Bolt" | "Unholy Heat"
        | "Spell Pierce" | "Flusterstorm" | "Pyroblast" | "Hydroblast"
        | "Surgical Extraction" | "Consign to Memory" => return Some(CardCategory::Interaction),
        // Selection: cantrips and free info
        "Brainstorm" | "Ponder" | "Preordain" | "Consider"
        | "Mishra's Bauble" => return Some(CardCategory::Selection),
        _ => {}
    }
    let def = def?;
    if def.is_land() { return Some(CardCategory::Mana); }
    if def.is_creature() || matches!(def.kind, CardKind::Planeswalker(_)) {
        return Some(CardCategory::Threat);
    }
    if matches!(def.kind, CardKind::Artifact(_) | CardKind::Enchantment(_)) {
        return Some(CardCategory::Threat); // equipment, baubles with board presence
    }
    if def.is_instant() || def.is_sorcery() { return Some(CardCategory::Interaction); }
    None
}

/// Evaluate a plan state for a generic opponent (tempo/fair deck).
/// Values threats on board, mana development, interaction held back, and cantrips.
pub(crate) fn opp_plan_quality(
    plan: &TurnPlanState,
    state: &SimState,
) -> f64 {
    let mut score = 0.0;

    for &id in &plan.spells_cast {
        let name = state.objects.get(&id)
            .map(|c| c.catalog_key.as_str()).unwrap_or("");
        let def = state.def_of(id).or_else(|| {
            state.objects.get(&id).and_then(|c| state.catalog.get(c.catalog_key.as_str()))
        });
        let cat = opp_categorize(name, def);
        score += match cat {
            Some(CardCategory::Threat) => 3.0,        // deploying threats is the main goal
            Some(CardCategory::Selection) => 2.0,     // cantrips find threats/answers
            Some(CardCategory::Interaction) => 1.0,   // proactive discard is fine, but hold counters
            Some(CardCategory::Mana) => 0.5,          // mana rocks / rituals
            None => 0.1,
        };
    }

    // Land drops — always valuable.
    score += plan.new_battlefield.len() as f64;

    // Holding interaction is valuable (don't dump your hand recklessly).
    for &id in &plan.hand {
        let name = state.objects.get(&id)
            .map(|c| c.catalog_key.as_str()).unwrap_or("");
        let def = state.def_of(id).or_else(|| {
            state.objects.get(&id).and_then(|c| state.catalog.get(c.catalog_key.as_str()))
        });
        if opp_categorize(name, def) == Some(CardCategory::Interaction) {
            score += 0.3;
        }
    }

    score
}

/// Compute the opponent's plan gap. Tempo decks want 2-3 lands, threats on board, interaction in hand.
pub(crate) fn opp_plan_gap(state: &SimState, who: PlayerId, matchup: &MatchupInfo) -> TargetGap {
    let def_or_catalog = |id: ObjId, key: &str| -> Option<&CardDef> {
        state.def_of(id).or_else(|| state.catalog.get(key))
    };

    // ── Mana gap: tempo wants 2-3 lands ─────────────────────────────────
    let lands = state.permanents_of(who)
        .filter(|c| def_or_catalog(c.id, &c.catalog_key).map_or(false, |d| d.is_land()))
        .count();
    let mana_gap = ((2.0 - lands as f64) / 2.0).clamp(0.0, 1.0);

    // ── Threat gap: need creatures on board ──────────────────────────────
    let threats = state.permanents_of(who)
        .filter(|c| {
            def_or_catalog(c.id, &c.catalog_key)
                .map_or(false, |d| d.is_creature() || matches!(d.kind, CardKind::Planeswalker(_)))
        })
        .count();
    let threat_gap = match threats {
        0 => 1.0,
        1 => 0.4,
        _ => 0.1,
    };

    // ── Interaction gap: premium vs combo, medium vs fair ────────────────
    let interaction_count = state.hand_of(who)
        .filter(|c| {
            let def = def_or_catalog(c.id, &c.catalog_key);
            opp_categorize(&c.catalog_key, def) == Some(CardCategory::Interaction)
        })
        .count();
    let interaction_gap = if !matchup.opp_fast_clock {
        // Facing combo: interaction is critical to stop the kill
        ((2.0 - interaction_count as f64) / 2.0).clamp(0.0, 1.0)
    } else {
        // Facing aggro/tempo: some interaction is nice but not critical
        ((1.0 - interaction_count as f64).max(0.0) * 0.5).clamp(0.0, 0.5)
    };

    TargetGap { mana: mana_gap, threat: threat_gap, interaction: interaction_gap }
}

/// Score how much `card_id` fills the opponent's current gap.
pub(crate) fn opp_card_fills(card_id: ObjId, gap: &TargetGap, state: &SimState, who: PlayerId) -> f64 {
    let name = match state.objects.get(&card_id) {
        Some(c) => c.catalog_key.as_str(),
        None => return 0.0,
    };
    let def = state.def_of(card_id).or_else(|| state.catalog.get(name));
    let cat = opp_categorize(name, def);

    match cat {
        Some(CardCategory::Mana) => {
            if gap.mana > 0.0 { 0.4 + 0.5 * gap.mana } else { 0.05 }
        }
        Some(CardCategory::Threat) => {
            // Surplus copies of on-board threats are less urgent
            let on_board = state.permanents_of(who)
                .filter(|c| c.catalog_key == name)
                .count();
            if on_board > 0 && gap.threat < 0.5 {
                0.2
            } else if gap.threat > 0.0 {
                0.5 + 0.4 * gap.threat
            } else {
                0.3
            }
        }
        Some(CardCategory::Interaction) => {
            if gap.interaction > 0.0 { 0.3 + 0.5 * gap.interaction } else { 0.15 }
        }
        Some(CardCategory::Selection) => 0.35, // cantrips: always medium
        None => 0.05,
    }
}

/// Hand-aware mulligan for opponent (tempo/fair deck). Uses actual mana colors.
/// - Need ≥1 blue source (casts cantrips/interaction).
/// - Mull flood (≥5 lands) or hands with no threats/cantrips.
/// - 4 or fewer cards: always keep.
pub(crate) fn opp_should_mulligan(
    state: &SimState, who: PlayerId, mulligans_taken: u32, fetch_colors: &[Color],
) -> bool {
    if mulligans_taken >= 3 { return false; }

    let hand: Vec<(ObjId, Option<CardCategory>)> = state.hand_of(who)
        .map(|c| {
            let def = state.def_of(c.id).or_else(|| state.catalog.get(c.catalog_key.as_str()));
            (c.id, opp_categorize(&c.catalog_key, def))
        })
        .collect();

    let hand_size = hand.len();
    let mana = hand_land_mana(state, who, fetch_colors);
    let has_useful_mana = mana.u >= 1;
    let threat_count = hand.iter().filter(|(_, cat)| *cat == Some(CardCategory::Threat)).count();
    let selection_count = hand.iter().filter(|(_, cat)| *cat == Some(CardCategory::Selection)).count();
    let interaction_count = hand.iter().filter(|(_, cat)| *cat == Some(CardCategory::Interaction)).count();
    let spells = threat_count + selection_count + interaction_count;

    match mulligans_taken {
        0 => {
            // 7 cards: mull no useful mana, flood, or no threats/cantrips
            if !has_useful_mana { return true; }
            if mana.total >= 5 { return true; }
            if threat_count == 0 && selection_count == 0 { return true; }
            false
        }
        1 => {
            // 6 cards: need useful mana + ≥1 spell
            if !has_useful_mana { return true; }
            if mana.total >= 5 { return true; }
            if spells == 0 { return true; }
            false
        }
        2 => {
            // 5 cards: keep with any useful mana + ≥1 spell
            if !has_useful_mana && spells == hand_size { return true; }
            false
        }
        _ => false,
    }
}

impl Strategy for GenericOppStrategy {
    fn drain_decisions(&mut self) -> Vec<String> { std::mem::take(&mut self.decisions) }

    fn choose_action(&mut self, state: &SimState, ap: PlayerId,
                     legal_actions: &[LegalAction]) -> LegalAction {
        let who = self.player_id;
        let t = state.current_turn;
        if who != ap {
            if state.stack.is_empty() { return LegalAction::Pass; }
            return choose_nap_action(state, who, legal_actions, &mut self.rng, &mut self.decisions);
        }
        let in_ninjutsu_step = matches!(state.current_phase,
            Some(TurnPosition::Step(StepKind::DeclareBlockers))
            | Some(TurnPosition::Step(StepKind::FirstStrikeCombatDamage))
            | Some(TurnPosition::Step(StepKind::CombatDamage))
            | Some(TurnPosition::Step(StepKind::EndCombat)));
        if in_ninjutsu_step {
            if let Some(action) = choose_ninjutsu_action(state, who, legal_actions, &mut self.rng) {
                return action;
            }
            return LegalAction::Pass;
        }
        let in_main_phase = matches!(state.current_phase,
            Some(TurnPosition::Phase(PhaseKind::PreCombatMain))
            | Some(TurnPosition::Phase(PhaseKind::PostCombatMain)));
        if !in_main_phase { return LegalAction::Pass; }
        let mut _md = false;
        choose_ap_proactive(state, t, who, legal_actions, &mut _md, &mut self.rng, &mut self.decisions, opp_plan_quality)
    }

    fn announce(&mut self, state: &SimState, card_id: ObjId,
                options: &AnnounceOptions) -> AnnounceChoice {
        // Probabilistic alt cost selection for opponent counterspells.
        announce_with_alt_costs(state, self.player_id, card_id, options, &mut self.rng, true)
    }

    fn declare_attackers(&mut self, state: &SimState) -> Vec<(ObjId, Option<ObjId>)> {
        pick_attackers(self.player_id, state, &mut self.rng)
    }

    fn declare_blockers(&mut self, state: &SimState) -> Vec<(ObjId, ObjId)> {
        pick_blockers(self.player_id, state)
    }

    fn take_mulligan(&mut self, state: &SimState, mulligans_taken: u32) -> bool {
        let who = self.player_id;
        let mull = opp_should_mulligan(state, who, mulligans_taken, &self.matchup.fetch_colors);
        let cats = hand_category_summary(state, who, opp_categorize);
        self.dlog(format!("T0 {}: mulligan#{} {} → {}",
            who, mulligans_taken, cats,
            if mull { "MULL" } else { "KEEP" }));
        mull
    }

    fn player_id(&self) -> PlayerId { self.player_id }

    fn plan_gap(&self, state: &SimState) -> TargetGap {
        opp_plan_gap(state, self.player_id, &self.matchup)
    }

    fn card_fills(&self, card_id: ObjId, gap: &TargetGap, state: &SimState) -> f64 {
        opp_card_fills(card_id, gap, state, self.player_id)
    }
}

// ── New-protocol helpers ──────────────────────────────────────────────────────

/// Find a counterspell in `legal` targeting the top opposing spell on the stack.
/// `probabilistic`: when true, rolls P(card in hand) and strategic probability.
fn find_counter_in_legal(
    state: &SimState,
    who: PlayerId,
    target_id: ObjId,
    legal: &[LegalAction],
    rng: &mut impl Rng,
    probabilistic: bool,
) -> Option<LegalAction> {
    let target_owner_id = state.stack_item_owner(target_id);
    let target_owner = if target_owner_id == state.us_id { PlayerId::Us } else { PlayerId::Opp };
    let target_has_untapped_lands = state.permanents_of(target_owner).any(|c| {
        c.bf().map_or(false, |bf| !bf.tapped)
            && !state.def_of(c.id).map(|d| d.mana_abilities()).unwrap_or(&[]).is_empty()
    });

    let hand_size = state.hand_size(who);
    let lib_size = state.library_size(who) + hand_size as usize;

    let mut seen = std::collections::HashSet::new();
    for action in legal {
        let LegalAction::CastSpell { card_id, face: SpellFace::Main, .. } = action else { continue };
        let Some(def) = state.def_of(*card_id) else { continue };
        if !def.is_instant() { continue; }
        let name = state.objects.get(card_id).map(|c| c.catalog_key.as_str()).unwrap_or("");
        if !seen.insert(name.to_string()) { continue; }
        let targets = legal_targets(def.target_spec(), who, *card_id, state);
        if !targets.contains(&target_id) { continue; }
        if name == "Daze" && target_has_untapped_lands { continue; }

        if probabilistic {
            let copies = state.hand_of(who).filter(|c| c.catalog_key == name).count();
            let p_have = p_card_in_hand(lib_size, hand_size, copies);
            if !rng.gen_bool(p_have.max(f64::MIN_POSITIVE)) { continue; }
        }

        return Some(action.clone());
    }
    None
}

/// NAP decision (new protocol): try to counter the top opposing spell.
fn choose_nap_action(
    state: &SimState,
    who: PlayerId,
    legal: &[LegalAction],
    rng: &mut impl Rng,
    dlog: &mut Vec<String>,
) -> LegalAction {
    let t = state.current_turn;
    // Find topmost opposing counterable spell on stack.
    for idx in (0..state.stack.len()).rev() {
        let item_id = state.stack[idx];
        let item_owner = state.stack_item_owner(item_id);
        let item_is_counterable = state.stack_item_is_counterable(item_id);
        let item_name = state.stack_item_display_name(item_id).to_string();
        if item_owner != state.player_id(who) && item_is_counterable {
            if !worth_countering(item_id, &item_name, state) {
                break;
            }
            if let Some(action) = find_counter_in_legal(state, who, item_id, legal, rng, true) {
                if let LegalAction::CastSpell { card_id, .. } = &action {
                    let spell_name = state.objects.get(card_id).map_or("?", |c| c.catalog_key.as_str());
                    dlog.push(format!("T{} {}: NAP counter {} targeting {}", t, who, spell_name, item_name));
                }
                return action;
            }
            dlog.push(format!("T{} {}: NAP passes (no counter available for {})", t, who, item_name));
            break;
        }
    }
    LegalAction::Pass
}

/// AP reactive (new protocol): protect our Doomsday from being countered.
fn choose_ap_react(
    state: &SimState,
    who: PlayerId,
    legal: &[LegalAction],
    rng: &mut impl Rng,
    dlog: &mut Vec<String>,
) -> Option<LegalAction> {
    if who != PlayerId::Us || state.stack.is_empty() { return None; }
    let t = state.current_turn;
    let top_id = *state.stack.last()?;
    let top_is_counterable = state.stack_item_is_counterable(top_id);
    let top_owner = state.stack_item_owner(top_id);
    let top_chosen = state.objects.get(&top_id)
        .and_then(|c| c.spell())
        .map(|s| s.chosen_targets.clone())
        .unwrap_or_default();
    let us_id = state.us_id;
    let dd_countered = top_is_counterable
        && top_owner != us_id
        && top_chosen.first().copied()
            .and_then(|id| state.stack.iter().find(|&&s| s == id).map(|_| id))
            .is_some_and(|id| {
                state.objects.get(&id)
                    .map(|c| c.catalog_key == "Doomsday" && state.player_id(c.owner) == us_id)
                    .unwrap_or(false)
            });
    if !dd_countered { return None; }
    if let Some(action) = find_counter_in_legal(state, who, top_id, legal, rng, false) {
        let counter_name = if let LegalAction::CastSpell { card_id, .. } = &action {
            state.objects.get(card_id).map_or("?", |c| c.catalog_key.as_str()).to_string()
        } else { "?".to_string() };
        dlog.push(format!("T{} {}: AP protect DD with {}", t, who, counter_name));
        Some(action)
    } else {
        dlog.push(format!("T{} {}: DD countered — no protection available", t, who));
        Some(LegalAction::Pass)
    }
}

/// True if the ability at `ability_index` on `source_id` is a ninjutsu ability
/// (source_zone: Hand, costs include ReturnFromBattlefield).
fn is_ninjutsu_action(state: &SimState, source_id: ObjId, ability_index: usize) -> bool {
    state.def_of(source_id)
        .and_then(|d| d.abilities().get(ability_index))
        .map_or(false, |ab| {
            // Variant-agnostic: ninjutsu is "hand-source ability whose cost
            // includes a ReturnFromBattlefield". For Legacy storage we
            // scan components; for Ir storage we'd recognise the
            // MoveByChoice(BF→Hand, verb=Return) shape — no card emits
            // that yet, so this falls through to false for Ir.
            matches!(ab.source_zone, SourceZone::Hand)
                && {
                    let libmtg_engine::ir::ability::CostBody::Ir(a) = &ab.costs;
                    action_includes_return_from_bf(a)
                }
        })
}

/// True iff `a` (a cost-tree action) is a hand→exile pitch of a card with
/// the given color. Used by the strategy's probabilistic FoW/FoN gating to
/// sample whether the player has a blue card to pitch.
fn action_pitches_color(a: &libmtg_engine::ir::action::Action, color: Color) -> bool {
    use libmtg_engine::ir::action::Action::*;
    use libmtg_engine::ir::action::MoveVerb;
    use libmtg_engine::ir::expr::ZoneKindSel;
    let filter_pitches = |f: &libmtg_engine::ir::expr::Filter| {
        let libmtg_engine::ir::expr::Filter(expr) = f;
        action_filter_includes_color_lit(expr, color)
    };
    match a {
        MoveByChoice {
            from: ZoneKindSel::Hand,
            to: ZoneKindSel::Exile,
            verb: MoveVerb::Exile,
            filter,
            ..
        } if filter_pitches(filter) => true,
        Sequence(actions) => actions.iter().any(|a| action_pitches_color(a, color)),
        IfThen { then, else_, .. } => {
            action_pitches_color(then, color)
                || else_.as_ref().map_or(false, |e| action_pitches_color(e, color))
        }
        MayDo { action, .. } => action_pitches_color(action, color),
        ForEach { body, .. } => action_pitches_color(body, color),
        Choose { options, .. } => options.iter().any(|o| action_pitches_color(&o.action, color)),
        _ => false,
    }
}

fn action_filter_includes_color_lit(e: &libmtg_engine::ir::expr::Expr, color: Color) -> bool {
    use libmtg_engine::ir::expr::Expr;
    match e {
        Expr::ColorLit(c) if *c == color => true,
        Expr::And(a, b) | Expr::Or(a, b) => {
            action_filter_includes_color_lit(a, color) || action_filter_includes_color_lit(b, color)
        }
        Expr::Not(inner) => action_filter_includes_color_lit(inner, color),
        Expr::Eq(a, b) | Expr::Lt(a, b) | Expr::Le(a, b) | Expr::Gt(a, b) | Expr::Ge(a, b)
        | Expr::Contains(a, b) => {
            action_filter_includes_color_lit(a, color) || action_filter_includes_color_lit(b, color)
        }
        _ => false,
    }
}

/// True iff `a` (a cost-tree action) includes a `MoveByChoice` returning
/// permanents from the battlefield to a hand — the canonical ninjutsu cost
/// shape. Used by the strategy's ninjutsu-action detector.
fn action_includes_return_from_bf(a: &libmtg_engine::ir::action::Action) -> bool {
    use libmtg_engine::ir::action::Action::*;
    use libmtg_engine::ir::action::MoveVerb;
    use libmtg_engine::ir::expr::ZoneKindSel;
    match a {
        MoveByChoice {
            from: ZoneKindSel::Battlefield,
            to: ZoneKindSel::Hand,
            verb: MoveVerb::Return,
            ..
        } => true,
        Sequence(actions) => actions.iter().any(action_includes_return_from_bf),
        IfThen { then, else_, .. } => {
            action_includes_return_from_bf(then)
                || else_.as_ref().map_or(false, |e| action_includes_return_from_bf(e))
        }
        MayDo { action, .. } => action_includes_return_from_bf(action),
        ForEach { body, .. } => action_includes_return_from_bf(body),
        Choose { options, .. } => options.iter().any(|o| action_includes_return_from_bf(&o.action)),
        _ => false,
    }
}

/// Ninjutsu action (new protocol): find a ninjutsu ability in legal actions.
fn choose_ninjutsu_action(
    state: &SimState,
    who: PlayerId,
    legal: &[LegalAction],
    rng: &mut impl Rng,
) -> Option<LegalAction> {
    if state.hand_size(who) <= 0 { return None; }
    let has_unblocked = state.permanents_of(who)
        .any(|c| c.bf().map_or(false, |bf| bf.attacking && bf.unblocked));
    if !has_unblocked { return None; }
    let ninjutsu_actions: Vec<&LegalAction> = legal.iter().filter(|a| {
        matches!(a, LegalAction::ActivateAbility { source_id, ability_index }
            if is_ninjutsu_action(state, *source_id, *ability_index))
    }).collect();
    if ninjutsu_actions.is_empty() { return None; }
    if !rng.gen_bool(0.35) { return None; }
    Some(ninjutsu_actions[rng.gen_range(0..ninjutsu_actions.len())].clone())
}

/// AP proactive decision: use the turn planner with deck-specific evaluation.
fn choose_ap_proactive(
    state: &SimState,
    t: u8,
    who: PlayerId,
    legal: &[LegalAction],
    must_land_drop: &mut bool,
    rng: &mut impl Rng,
    dlog: &mut Vec<String>,
    eval: PlanEvalFn,
) -> LegalAction {
    let plan = make_turn_plan(state, who, eval);

    // Log the plan.
    if !plan.is_empty() {
        let plan_summary: Vec<String> = plan.iter().filter_map(|a| match a {
            PlanAction::CastSpell(id) | PlanAction::LandDrop(id) =>
                state.objects.get(id).map(|c| c.catalog_key.clone()),
            PlanAction::TapForMana { source_id, color, .. } => {
                let src = state.objects.get(source_id)
                    .map(|c| c.catalog_key.as_str()).unwrap_or("?");
                let col = color.map_or("C".to_string(), |c| format!("{:?}", c));
                Some(format!("tap:{}:{}", src, col))
            }
            PlanAction::CrackFetch { source_id, target_id } => {
                let src = state.objects.get(source_id)
                    .map(|c| c.catalog_key.as_str()).unwrap_or("?");
                let tgt = state.objects.get(target_id)
                    .map(|c| c.catalog_key.as_str()).unwrap_or("?");
                Some(format!("fetch:{}→{}", src, tgt))
            }
        }).collect();
        dlog.push(format!("T{} {}: plan=[{}]", t, who, plan_summary.join(" → ")));
    }

    // Execute the first priority-round action from the plan.
    for action in &plan {
        match action {
            PlanAction::CastSpell(id) => {
                if let Some(la) = legal.iter().find(|la| {
                    matches!(la, LegalAction::CastSpell { card_id, .. } if *card_id == *id)
                }) {
                    *must_land_drop = false;
                    let name = state.objects.get(id)
                        .map_or("?", |c| c.catalog_key.as_str());
                    dlog.push(format!("T{} {}: planner → {}", t, who, name));
                    return la.clone();
                }
            }
            PlanAction::LandDrop(id) => {
                if legal.iter().any(|la| matches!(la, LegalAction::LandDrop(lid) if *lid == *id)) {
                    *must_land_drop = false;
                    let name = state.objects.get(id)
                        .map_or("?", |c| c.catalog_key.as_str());
                    dlog.push(format!("T{} {}: planner → land {}", t, who, name));
                    return LegalAction::LandDrop(*id);
                }
            }
            PlanAction::TapForMana { .. } | PlanAction::CrackFetch { .. } => continue,
        }
    }

    // Fallback: try on-board abilities (not yet modeled by planner).
    if let Some(action) = choose_on_board_action(state, who, legal, false, must_land_drop, rng) {
        return action;
    }

    LegalAction::Pass
}

/// Pick an on-board action (abilities) from legal actions.
fn choose_on_board_action(
    state: &SimState,
    who: PlayerId,
    legal: &[LegalAction],
    dd_ready: bool,
    must_land_drop: &mut bool,
    rng: &mut impl Rng,
) -> Option<LegalAction> {
    let mut candidates: Vec<LegalAction> = Vec::new();

    // Collect ability activations from legal actions, categorized.
    for action in legal {
        let LegalAction::ActivateAbility { source_id, ability_index } = action else { continue };
        let Some(def) = state.def_of(*source_id) else { continue };
        let Some(ab) = def.abilities().get(*ability_index) else { continue };
        let tapped = state.objects.get(source_id)
            .and_then(|c| c.bf())
            .map_or(false, |bf| bf.tapped);

        if def.is_land() {
            // Land abilities: 75% roll.
            if !tapped && rng.gen_bool(0.75) {
                candidates.push(action.clone());
            }
        } else if ab.is_loyalty_ability() {
            // Planeswalker loyalty abilities: only postcombat main, empty stack.
            let is_postcombat = matches!(state.current_phase, Some(TurnPosition::Phase(PhaseKind::PostCombatMain)));
            if is_postcombat && state.stack.is_empty() {
                let pw_activated = state.objects.get(source_id)
                    .and_then(|c| c.bf())
                    .map_or(true, |bf| bf.pw_activated_this_turn);
                if !pw_activated {
                    candidates.push(action.clone());
                }
            }
        } else {
            // Non-land, non-loyalty abilities: 75% roll.
            if rng.gen_bool(0.75) {
                candidates.push(action.clone());
            }
        }
    }

    // If DD is nearly castable but we lack black mana, force-fetch for it.
    if who == PlayerId::Us && !dd_ready && !has_black_mana(state, PlayerId::Us)
        && state.hand_of(who).any(|c| c.catalog_key == "Doomsday")
    {
        let mut has_fetch = false;
        for action in legal {
            let LegalAction::ActivateAbility { source_id, ability_index } = action else { continue };
            let Some(def) = state.def_of(*source_id) else { continue };
            let Some(ab) = def.abilities().get(*ability_index) else { continue };
            if ab.is_fetch_ability() {
                has_fetch = true;
                if !candidates.iter().any(|c| matches!(c, LegalAction::ActivateAbility { source_id: sid, .. } if sid == source_id)) {
                    candidates.push(action.clone());
                }
            }
        }
        if !has_fetch {
            *must_land_drop = true;
        }
    }

    // Adventure creatures in exile (on_adventure): cast creature face.
    let on_adventure: Vec<ObjId> = state.on_adventure_of(who).map(|c| c.id).collect();
    for card_id in on_adventure {
        if let Some(action) = legal.iter().find(|a| matches!(a, LegalAction::CastSpell { card_id: cid, face: SpellFace::Main, .. } if *cid == card_id)) {
            if rng.gen_bool(0.75) {
                candidates.push(action.clone());
            }
        }
    }

    if candidates.is_empty() { None } else { Some(candidates.remove(rng.gen_range(0..candidates.len()))) }
}

/// Alt cost selection for announce callback.
/// `probabilistic`: true for opponent (roll strategic prob), false for us (deterministic).
fn announce_with_alt_costs(
    state: &SimState,
    who: PlayerId,
    card_id: ObjId,
    options: &AnnounceOptions,
    rng: &mut impl Rng,
    probabilistic: bool,
) -> AnnounceChoice {
    let chosen_x = if options.has_x_cost { 3 } else { 0 };
    for (i, alt) in options.available_alt_costs.iter().enumerate() {
        let libmtg_engine::ir::ability::CostBody::Ir(action) = &alt.costs;
        let payable = state.hand_size(who) >= alt.hand_min
            && libmtg_engine::ir::cost_exec::build_schema(action, state, who, card_id).is_some();
        if payable {
            if probabilistic {
                // FoW/FoN pitch heuristic: if the alt cost exiles a blue
                // card from hand, gate on probability that the player has
                // a blue card to pitch (build_schema already verified one
                // exists, but the heuristic is for *probabilistic* sampling
                // across simulated runs).
                let has_exile_blue = action_pitches_color(action, Color::Blue);
                if has_exile_blue {
                    let hand_size = state.hand_size(who);
                    let lib_size = state.library_size(who) + hand_size as usize;
                    let n_blue = state.hand_of(who)
                        .filter(|c| c.id != card_id
                            && state.def_of(c.id).map_or(false, |d| !d.is_land() && d.is_blue()))
                        .count();
                    let p_have_blue = p_card_in_hand(lib_size, hand_size, n_blue);
                    if !rng.gen_bool(p_have_blue.max(f64::MIN_POSITIVE)) { continue; }
                }
                let strategic = alt.prob.unwrap_or(0.5);
                if !rng.gen_bool(strategic) { continue; }
            }
            return AnnounceChoice { chosen_mode: 0, alt_cost_index: Some(i), chosen_x };
        }
    }
    AnnounceChoice { chosen_mode: 0, alt_cost_index: None, chosen_x }
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// True if `name` is a spell the NAP considers worth spending a free counterspell (FoW / Daze) on.
/// Cantrips and mana rituals are not worth pitching; permanents and combo pieces are.
fn worth_countering(id: ObjId, name: &str, state: &SimState) -> bool {
    if let Some(def) = state.def_of(id) {
        match &def.kind {
            CardKind::Creature(_) | CardKind::Planeswalker(_)
            | CardKind::Artifact(_) | CardKind::Enchantment(_) => return true,
            _ => {}
        }
    }
    // High-value non-permanent spells: combo kill, mass discard
    matches!(name, "Doomsday" | "Hymn to Tourach" | "Unearth")
}

// ── Combat strategy ───────────────────────────────────────────────────────────

fn pick_attackers(
    ap: PlayerId,
    state: &SimState,
    rng: &mut impl Rng,
) -> Vec<(ObjId, Option<ObjId>)> {
    let nap = ap.opp();
    // Compute NAP blocker stats (ObjId, power) for flying/non-flying checks.
    let nap_blockers: Vec<(ObjId, i32)> = state.permanents_of(nap)
        .filter(|p| !p.bf().map_or(false, |bf| bf.tapped))
        .filter_map(|p| {
            let def = state.def_of(p.id)?;
            if !def.is_creature() { return None; }
            let pow = state.def_of(p.id)
                .and_then(|d| d.as_creature())
                .map(|c| c.power())
                .unwrap_or(1);
            Some((p.id, pow))
        })
        .collect();
    let nap_pw_ids: Vec<ObjId> = state.permanents_of(nap)
        .filter(|p| state.def_of(p.id)
            .map_or(false, |d| matches!(d.kind, CardKind::Planeswalker(_))))
        .map(|p| p.id)
        .collect();
    state.permanents_of(ap)
        .filter(|p| p.bf().map_or(false, |bf| !bf.tapped && (!bf.entered_this_turn || creature_has_keyword(p.id, Keyword::Haste, state))))
        .filter_map(|p| {
            let def = state.def_of(p.id)?;
            if !def.is_creature() { return None; }
            let atk_flies  = creature_has_keyword(p.id, Keyword::Flying, state);
            let atk_shadow = creature_has_keyword(p.id, Keyword::Shadow, state);
            // Sum power of NAP creatures that can block this attacker. CR 702.17b: a creature
            // with reach can block fliers; CR 702.9b: flying can only be blocked by flying or reach.
            let blocking_power: i32 = nap_blockers.iter()
                .filter(|(blk_id, _)| {
                    if atk_flies
                        && !creature_has_keyword(*blk_id, Keyword::Flying, state)
                        && !creature_has_keyword(*blk_id, Keyword::Reach, state)
                    { return false; }
                    let blk_shadow = creature_has_keyword(*blk_id, Keyword::Shadow, state);
                    atk_shadow == blk_shadow
                })
                .map(|(_, pow)| *pow)
                .sum();
            let tgh = state.def_of(p.id)
                .and_then(|d| d.as_creature())
                .map(|c| c.toughness())
                .unwrap_or(1);
            if tgh <= blocking_power { return None; }
            // Randomly attack a NAP planeswalker (50%) or the player.
            let target = if !nap_pw_ids.is_empty() && rng.gen_bool(0.5) {
                Some(nap_pw_ids[rng.gen_range(0..nap_pw_ids.len())])
            } else {
                None
            };
            Some((p.id, target))
        })
        .collect()
}

/// `blocker_player` is the player declaring blockers (the NAP / defending player).
fn pick_blockers(
    blocker_player: PlayerId,
    state: &SimState,
) -> Vec<(ObjId, ObjId)> {
    let nap = blocker_player;
    let mut used_blockers: std::collections::HashSet<ObjId> = Default::default();
    let mut blocks: Vec<(ObjId, ObjId)> = Vec::new();
    for &atk_id in &state.combat_attackers {
        let (atk_pow, atk_tgh) = match state.objects.get(&atk_id)
            .and_then(|p| p.bf().map(|_| ()))
        {
            Some(()) => {
                let pow = state.def_of(atk_id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.power())
                    .unwrap_or(1);
                let tgh = state.def_of(atk_id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.toughness())
                    .unwrap_or(1);
                (pow, tgh)
            }
            None => continue,
        };
        let atk_flies  = creature_has_keyword(atk_id, Keyword::Flying, state);
        let atk_shadow = creature_has_keyword(atk_id, Keyword::Shadow, state);
        let blocker = state.permanents_of(nap)
            .filter(|p| !p.bf().map_or(false, |bf| bf.tapped) && !used_blockers.contains(&p.id))
            .find_map(|p| {
                if !state.def_of(p.id).map(|d| d.is_creature()).unwrap_or(false) { return None; }
                // Flying attackers can only be blocked by flying or reach (CR 702.9b, 702.17b).
                if atk_flies
                    && !creature_has_keyword(p.id, Keyword::Flying, state)
                    && !creature_has_keyword(p.id, Keyword::Reach, state)
                { return None; }
                // Shadow: shadow creatures can only block/be blocked by other shadow creatures.
                let blk_shadow = creature_has_keyword(p.id, Keyword::Shadow, state);
                if atk_shadow != blk_shadow { return None; }
                // CR 702.16c: a creature with protection from [quality] can't be blocked by
                // creatures with that quality.
                if is_protected_from(atk_id, p.id, state) { return None; }
                let blk_pow = state.def_of(p.id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.power())
                    .unwrap_or(1);
                let blk_tgh = state.def_of(p.id)
                    .and_then(|d| d.as_creature())
                    .map(|c| c.toughness())
                    .unwrap_or(1);
                // Good block: kills attacker OR both survive. Not a chump.
                if blk_pow >= atk_tgh || atk_pow < blk_tgh { Some(p.id) } else { None }
            });
        if let Some(blk_id) = blocker {
            used_blockers.insert(blk_id);
            blocks.push((atk_id, blk_id));
        }
    }
    blocks
}

fn p_card_in_hand(library_size: usize, hand_size: i32, copies: usize) -> f64 {
    let t = library_size;
    let h = (hand_size.max(0) as usize).min(t);
    let n = copies;
    if n == 0 || h == 0 { return 0.0; }
    if n >= t { return 1.0; }
    // P(0 in hand) = ∏ᵢ₌₀ʰ⁻¹ (T-N-i)/(T-i)
    let mut p_none: f64 = 1.0;
    for i in 0..h {
        let num = t.saturating_sub(n + i);
        if num == 0 { return 1.0; }
        p_none *= num as f64 / (t - i) as f64;
    }
    (1.0 - p_none).max(0.0)
}
