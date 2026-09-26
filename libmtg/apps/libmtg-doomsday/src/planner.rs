use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use libmtg_engine::*;
use crate::strategy::*;

// ── Turn plan state ─────────────────────────────────────────────────────────

/// Lightweight snapshot of turn-relevant state for the main-phase planner.
/// The planner projects future states by cloning and modifying this struct.
#[derive(Clone)]
pub struct TurnPlanState {
    /// Current floating mana.
    pub(crate) pool: ManaPool,
    /// Permanents that are tapped (pre-existing + newly tapped by plan actions).
    pub(crate) tapped: HashSet<ObjId>,
    /// Cards currently in hand (by ObjId).
    pub(crate) hand: Vec<ObjId>,
    /// Whether the land drop has been used this turn.
    pub(crate) land_drop_used: bool,
    /// Spells cast during this plan sequence (for quality evaluation).
    pub(crate) spells_cast: Vec<ObjId>,
    /// Cards played as lands during this plan (not yet on SimState's battlefield).
    pub(crate) new_battlefield: Vec<ObjId>,
    /// Permanents sacrificed during this plan (removed from board).
    pub(crate) sacrificed: HashSet<ObjId>,
    /// Cards in library available for fetch search targets.
    pub(crate) library: Vec<ObjId>,
}

// ── Plan actions ────────────────────────────────────────────────────────────

/// An action the planner can consider taking during a main phase.
#[derive(Clone, Debug)]
pub enum PlanAction {
    /// Tap (or sac) a mana source to add mana to pool.
    TapForMana { source_id: ObjId, ability_index: usize, color: Option<Color> },
    /// Cast a spell from hand (costs mana from pool).
    CastSpell(ObjId),
    /// Play a land from hand.
    LandDrop(ObjId),
    /// Crack a fetch land to put a land from library onto the battlefield.
    CrackFetch { source_id: ObjId, target_id: ObjId },
}

impl PlanAction {
    /// True if this is a mana-tapping action (handled by the mana sub-loop, not priority).
    pub(crate) fn is_tap(&self) -> bool {
        matches!(self, PlanAction::TapForMana { .. })
    }
}

// ── Enumeration ─────────────────────────────────────────────────────────────

/// Enumerate all legal plan actions from the current plan state.
/// `state` is read-only — used for card definitions and board information.
pub(crate) fn enumerate_plan_actions(
    plan: &TurnPlanState,
    state: &SimState,
    who: PlayerId,
) -> Vec<PlanAction> {
    let mut actions = Vec::new();

    // ── Tap mana sources: existing permanents ───────────────────────────
    for card in state.permanents_of(who) {
        if plan.tapped.contains(&card.id) { continue; }
        if plan.sacrificed.contains(&card.id) { continue; }
        enumerate_mana_taps(card.id, state, plan, &mut actions);
    }

    // ── Tap mana sources: newly played lands ────────────────────────────
    for &card_id in &plan.new_battlefield {
        if plan.tapped.contains(&card_id) { continue; }
        enumerate_mana_taps(card_id, state, plan, &mut actions);
    }

    // ── Cast spells from hand ───────────────────────────────────────────
    for &card_id in &plan.hand {
        let Some(def) = plan_def_of(card_id, state) else { continue };
        if def.is_land() { continue; }
        // Use materialized castable if available (reflects CEs like Cage/Lavinia);
        // otherwise hand cards are castable by default (catalog has castable=false).
        let castable = state.def_of(card_id).map_or(true, |d| d.castable);
        if !castable { continue; }
        let cost = parse_mana_cost(def.mana_cost());
        if plan.pool.can_pay(&cost) {
            actions.push(PlanAction::CastSpell(card_id));
        }
    }

    // ── Land drops ──────────────────────────────────────────────────────
    if !plan.land_drop_used {
        for &card_id in &plan.hand {
            let Some(def) = plan_def_of(card_id, state) else { continue };
            if def.is_land() {
                actions.push(PlanAction::LandDrop(card_id));
            }
        }
    }

    // ── Crack fetch lands ──────────────────────────────────────────────
    // Existing battlefield permanents with fetch abilities.
    for card in state.permanents_of(who) {
        if plan.tapped.contains(&card.id) { continue; }
        if plan.sacrificed.contains(&card.id) { continue; }
        enumerate_fetch_cracks(card.id, state, plan, &mut actions);
    }
    // Newly played lands with fetch abilities.
    for &card_id in &plan.new_battlefield {
        if plan.sacrificed.contains(&card_id) { continue; }
        enumerate_fetch_cracks(card_id, state, plan, &mut actions);
    }

    actions
}

/// Look up a card's definition, with catalog fallback for cards the planner
/// has moved out of their original zone (e.g. lands played from hand).
fn plan_def_of<'a>(card_id: ObjId, state: &'a SimState) -> Option<&'a CardDef> {
    state.def_of(card_id).or_else(|| {
        let key = &state.objects.get(&card_id)?.catalog_key;
        state.catalog.get(key.as_str())
    })
}

/// Enumerate TapForMana actions for a single source (one per producible color).
fn enumerate_mana_taps(
    source_id: ObjId,
    state: &SimState,
    plan: &TurnPlanState,
    actions: &mut Vec<PlanAction>,
) {
    let Some(def) = plan_def_of(source_id, state) else { return };
    let mas = def.mana_abilities();
    for (idx, ma) in mas.iter().enumerate() {
        if !ma.activatable { continue; }
        if ma.timing != ActivationTiming::Default { continue; }
        if !matches!(ma.source_zone, SourceZone::Battlefield) { continue; }
        // Check tap cost — source must be untapped.
        let requires_tap = ma.costs.requires_tap_self();
        if requires_tap && plan.tapped.contains(&source_id) { continue; }
        // Check condition predicate (e.g. Metalcraft for Mox Opal).
        if ma.condition.as_ref().map_or(false, |cond| !obj_matches(cond, source_id, state)) { continue; }
        // One action per producible color, or one colorless action.
        if ma.produces.is_empty() {
            actions.push(PlanAction::TapForMana { source_id, ability_index: idx, color: None });
        } else {
            for &color in &ma.produces {
                actions.push(PlanAction::TapForMana {
                    source_id, ability_index: idx, color: Some(color),
                });
            }
        }
    }
}

/// Enumerate CrackFetch actions for a single source.
/// Each mana-producing land in the library is a possible fetch target.
fn enumerate_fetch_cracks(
    source_id: ObjId,
    state: &SimState,
    plan: &TurnPlanState,
    actions: &mut Vec<PlanAction>,
) {
    let Some(def) = plan_def_of(source_id, state) else { return };
    let has_fetch = match &def.kind {
        CardKind::Land(l) => l.abilities.iter().any(|a| a.is_fetch_ability()),
        _ => false,
    };
    if !has_fetch { return; }

    // Each library land with mana abilities is a valid fetch target.
    let mut seen = HashSet::new();
    for &target_id in &plan.library {
        let Some(tdef) = plan_def_of(target_id, state) else { continue };
        if !tdef.is_land() { continue; }
        if tdef.mana_abilities().is_empty() { continue; }
        // Deduplicate by catalog key — fetching any of 4 Underground Seas is equivalent.
        let key = state.objects.get(&target_id).map(|o| o.catalog_key.as_str()).unwrap_or("");
        if !seen.insert(key.to_string()) { continue; }
        actions.push(PlanAction::CrackFetch { source_id, target_id });
    }
}

// ── State transitions ───────────────────────────────────────────────────────

/// Apply a plan action to produce the next plan state.
/// Deterministic — no randomness, no stochastic draws.
pub(crate) fn apply_plan_action(
    plan: &TurnPlanState,
    action: &PlanAction,
    state: &SimState,
) -> TurnPlanState {
    let mut next = plan.clone();
    match action {
        PlanAction::TapForMana { source_id, ability_index, color } => {
            let def = plan_def_of(*source_id, state);
            let ma = def.and_then(|d| d.mana_abilities().get(*ability_index));
            if let Some(ma) = ma {
                // Mark tapped if ability requires tap.
                let requires_tap = ma.costs.requires_tap_self();
                if requires_tap {
                    next.tapped.insert(*source_id);
                }
                // Mark sacrificed if ability requires sac.
                let requires_sac = ma.costs.requires_sac_self();
                if requires_sac {
                    next.sacrificed.insert(*source_id);
                }
                // Add mana to pool.
                let count = ma.produces_count as i32;
                next.pool.total += count;
                if let Some(c) = color {
                    match c {
                        Color::White => next.pool.w += count,
                        Color::Blue  => next.pool.u += count,
                        Color::Black => next.pool.b += count,
                        Color::Red   => next.pool.r += count,
                        Color::Green => next.pool.g += count,
                    }
                } else {
                    next.pool.c += count;
                }
            }
        }
        PlanAction::CastSpell(card_id) => {
            let def = plan_def_of(*card_id, state);
            if let Some(def) = def {
                // Spend mana.
                let cost = parse_mana_cost(def.mana_cost());
                next.pool.spend(&cost);
            }
            // Remove from hand.
            next.hand.retain(|&id| id != *card_id);
            // Record the cast.
            next.spells_cast.push(*card_id);
            // Apply known mana-producing spell effects.
            let name = state.objects.get(card_id)
                .map(|c| c.catalog_key.as_str()).unwrap_or("");
            if let Some(production) = spell_mana_production(name) {
                next.pool.add(&production);
            }
        }
        PlanAction::LandDrop(card_id) => {
            // Remove from hand, add to board, mark land drop used.
            next.hand.retain(|&id| id != *card_id);
            next.new_battlefield.push(*card_id);
            next.land_drop_used = true;
        }
        PlanAction::CrackFetch { source_id, target_id } => {
            // Sacrifice the fetch, move target from library to battlefield.
            next.sacrificed.insert(*source_id);
            next.library.retain(|&id| id != *target_id);
            next.new_battlefield.push(*target_id);
        }
    }
    next
}

/// Known mana-producing spell effects (hardcoded — spell effects are closures,
/// not structured data, so we can't derive this from card definitions yet).
fn spell_mana_production(name: &str) -> Option<ManaPool> {
    match name {
        "Dark Ritual" => Some(ManaPool { b: 3, total: 3, ..Default::default() }),
        _ => None,
    }
}

// ── Quality function type ───────────────────────────────────────────────────

/// Evaluation function that scores a plan state. Higher = better.
/// Each deck archetype provides its own implementation.
pub type PlanEvalFn = fn(&TurnPlanState, &SimState) -> f64;

/// Evaluate a plan state for the Doomsday pilot.
/// DD cast dominates; otherwise value cantrips, interaction, and land drops.
pub(crate) fn dd_plan_quality(
    plan: &TurnPlanState,
    state: &SimState,
) -> f64 {
    // DD cast is the ultimate goal — dominates everything else.
    let dd_cast = plan.spells_cast.iter().any(|&id| {
        state.objects.get(&id).map_or(false, |c| c.catalog_key == "Doomsday")
    });
    if dd_cast { return 100.0; }

    let mut score = 0.0;

    // Value from spells cast this sequence.
    for &id in &plan.spells_cast {
        let name = state.objects.get(&id)
            .map(|c| c.catalog_key.as_str()).unwrap_or("");
        let def = plan_def_of(id, state);
        let cat = dd_categorize(name, def);
        score += match cat {
            Some(CardCategory::Selection) => 2.0,    // cantrips dig toward DD
            Some(CardCategory::Threat) => 3.0,        // threats advance the game
            Some(CardCategory::Interaction) => 1.5,   // proactive interaction (Thoughtseize)
            Some(CardCategory::Mana) => {
                // Dark Ritual without DD follow-up is wasted BBB (drains at end of step).
                if name == "Dark Ritual" { 0.0 } else { 0.5 }
            }
            None => 0.1,
        };
    }

    // Land drops add mana sources — always valuable pre-DD.
    score += plan.new_battlefield.len() as f64;

    // Protection in hand — keeping counters available for DD is worth something.
    for &id in &plan.hand {
        let name = state.objects.get(&id)
            .map(|c| c.catalog_key.as_str()).unwrap_or("");
        if matches!(name, "Force of Will" | "Daze") {
            score += 0.5;
        }
    }

    score
}


// ── Search ──────────────────────────────────────────────────────────────────

const MAX_PLAN_DEPTH: usize = 10;

/// Compute a hash key for a plan state, treating set-like fields as order-independent.
/// Two states that reach the same pool/tapped/hand/etc. by different action orderings
/// produce the same key, so the transposition table deduplicates them.
fn plan_state_key(plan: &TurnPlanState) -> u64 {
    let mut h = DefaultHasher::new();
    // Pool — order of these fields is fixed.
    plan.pool.w.hash(&mut h);
    plan.pool.u.hash(&mut h);
    plan.pool.b.hash(&mut h);
    plan.pool.r.hash(&mut h);
    plan.pool.g.hash(&mut h);
    plan.pool.c.hash(&mut h);
    plan.pool.total.hash(&mut h);
    plan.land_drop_used.hash(&mut h);
    // Sets: use commutative combination (sum of hashes) so order doesn't matter.
    // XOR would collapse {A,B} with {C,D} when A^B == C^D; sum is safer.
    let set_hash = |ids: &HashSet<ObjId>| -> u64 {
        ids.iter().fold(0u64, |acc, id| {
            let mut sh = DefaultHasher::new();
            id.hash(&mut sh);
            acc.wrapping_add(sh.finish())
        })
    };
    set_hash(&plan.tapped).hash(&mut h);
    set_hash(&plan.sacrificed).hash(&mut h);
    // Vec fields treated as sets (order doesn't affect plan quality).
    let vec_hash = |ids: &[ObjId]| -> u64 {
        ids.iter().fold(0u64, |acc, id| {
            let mut sh = DefaultHasher::new();
            id.hash(&mut sh);
            acc.wrapping_add(sh.finish())
        })
    };
    vec_hash(&plan.hand).hash(&mut h);
    vec_hash(&plan.spells_cast).hash(&mut h);
    vec_hash(&plan.new_battlefield).hash(&mut h);
    h.finish()
}

/// Depth-limited search with transposition table.
/// Returns (quality, action sequence) for the best plan found.
fn best_plan(
    plan: &TurnPlanState,
    depth: usize,
    state: &SimState,
    who: PlayerId,
    eval: PlanEvalFn,
    tt: &mut HashMap<(u64, usize), f64>,
) -> (f64, Vec<PlanAction>) {
    let baseline = eval(plan, state);

    if depth == 0 {
        return (baseline, Vec::new());
    }

    // Transposition check: if we've evaluated this state at >= this depth, reuse the score.
    let key = plan_state_key(plan);
    if let Some(&cached_q) = tt.get(&(key, depth)) {
        // We know the best quality from this state — return it without the action path.
        // (The action path was already found on the first visit; the caller only needs
        // the quality to compare against other branches.)
        return (cached_q, Vec::new());
    }

    let legal = enumerate_plan_actions(plan, state, who);
    if legal.is_empty() {
        tt.insert((key, depth), baseline);
        return (baseline, Vec::new());
    }

    // "Do nothing" is the baseline — beat it or pass.
    let mut best_quality = baseline;
    let mut best_actions: Vec<PlanAction> = Vec::new();

    for action in &legal {
        let next = apply_plan_action(plan, action, state);
        let (q, mut tail) = best_plan(&next, depth - 1, state, who, eval, tt);
        if q > best_quality {
            best_quality = q;
            best_actions = Vec::with_capacity(1 + tail.len());
            best_actions.push(action.clone());
            best_actions.append(&mut tail);
        }
        // Early exit: can't do better than 100.
        if best_quality >= 100.0 {
            break;
        }
    }

    tt.insert((key, depth), best_quality);
    (best_quality, best_actions)
}

/// Top-level entry point: find the optimal action sequence for this main phase.
/// Returns the full plan (including TapForMana steps for internal bookkeeping).
pub(crate) fn make_turn_plan(
    state: &SimState,
    who: PlayerId,
    eval: PlanEvalFn,
) -> Vec<PlanAction> {
    // Only plan on empty stack (sorcery-speed actions).
    if !state.stack.is_empty() {
        return Vec::new();
    }
    let plan_state = extract_plan_state(state, who);
    let mut tt = HashMap::new();
    let (_quality, actions) = best_plan(&plan_state, MAX_PLAN_DEPTH, state, who, eval, &mut tt);
    actions
}

// ── Extraction ──────────────────────────────────────────────────────────────

/// Extract a TurnPlanState snapshot from the current SimState.
pub(crate) fn extract_plan_state(state: &SimState, who: PlayerId) -> TurnPlanState {
    let pool = state.player(who).pool.clone();
    let tapped: HashSet<ObjId> = state.permanents_of(who)
        .filter(|c| c.bf().map_or(false, |bf| bf.tapped))
        .map(|c| c.id)
        .collect();
    let hand: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
    let land_drop_used = state.player(who).lands_played_this_turn >= 1;

    let library: Vec<ObjId> = state.player(who).library_order.iter().copied().collect();

    TurnPlanState {
        pool,
        tapped,
        hand,
        land_drop_used,
        spells_cast: Vec::new(),
        new_battlefield: Vec::new(),
        sacrificed: HashSet::new(),
        library,
    }
}
