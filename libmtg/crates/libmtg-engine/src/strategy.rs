use super::*;

// ── Card evaluation types ────────────────────────────────────────────────────


/// How far the current game state is from the player's target hand shape.
/// Each field is 0.0 (fully satisfied) to 1.0+ (completely missing).
#[derive(Clone, Debug, Default)]
pub struct TargetGap {
    pub mana: f64,
    pub threat: f64,
    pub interaction: f64,
}


/// A card Karn or a similar wish effect may move into the game.
#[derive(Clone, Debug)]
pub enum WishOption {
    /// A card in the scenario sideboard. `index` is its current sideboard index.
    Sideboard { index: usize, name: String },
    /// A face-up card the player owns in exile.
    Exile { id: ObjId, name: String },
}

// ── Strategy trait ────────────────────────────────────────────────────────────

pub trait Strategy {
    fn declare_attackers(&mut self, state: &SimState) -> Vec<(ObjId, Option<ObjId>)>;
    fn declare_blockers(&mut self, state: &SimState) -> Vec<(ObjId, ObjId)>;
    fn take_mulligan(&mut self, state: &SimState, mulligans_taken: u32) -> bool;

    /// Called when an ability resolves with a `ChoiceSpec` (CR "choose" ≠ "target").
    /// `effect_id` is the `ObjId` of the ability object that is resolving.
    /// Default: pick the first valid option.
    fn choose_for_effect(&mut self, _effect_id: ObjId, choices: &[ObjId], _state: &SimState) -> Option<ObjId> {
        choices.first().copied()
    }

    /// Choose a card for a wish effect such as Karn, the Great Creator -2.
    /// Default: first legal option. Applications can rank sideboard and exile
    /// options together without exposing those cards to ordinary zone logic.
    fn choose_wish(&mut self, _effect_id: ObjId, choices: &[WishOption], _state: &SimState) -> Option<WishOption> {
        choices.first().cloned()
    }

    // ── CR 601.2 state machine methods ──────────────────────────────────

    /// Pick from engine-provided legal actions.
    fn choose_action(&mut self, _state: &SimState, _ap: PlayerId,
                     _legal_actions: &[LegalAction]) -> LegalAction {
        LegalAction::Pass
    }

    /// 601.2b: choose mode, alt cost, X. Default: mode 0, no alt cost,
    /// and X=min(3, currently-payable maximum).
    fn announce(&mut self, _state: &SimState, _card_id: ObjId,
                options: &AnnounceOptions) -> AnnounceChoice {
        let chosen_x = if options.has_x_cost { options.max_x.min(3) } else { 0 };
        AnnounceChoice { chosen_mode: 0, alt_cost_index: None, chosen_x }
    }

    /// 601.2g: pick a mana ability to activate, or None to stop.
    /// `mana_cost` is the *remaining unpaid shortfall* returned by the cost
    /// executor, not the spell's original full mana cost.  Default: plan
    /// directly against that residual.
    fn choose_mana_ability(&mut self, state: &SimState, who: PlayerId,
                           available: &[ManaAbilityOption],
                           mana_cost: &ManaCost) -> Option<ManaActivation> {
        let plan = auto_tap_plan_remaining(state, who, mana_cost);
        plan.into_iter().find(|act| {
            available.iter().any(|a| a.source_id == act.source_id && a.ability_index == act.ability_index)
        })
    }

    /// 601.2c: choose targets from legal candidates. Default: pick_targets heuristic.
    fn choose_targets(&mut self, state: &SimState, _card_id: ObjId,
                      legal: &[ObjId], spec: &TargetSpec) -> Vec<ObjId> {
        pick_targets(spec, legal, state)
    }

    /// Phase 3 cost-IR: announcement-time decision plan for an IR cost tree.
    ///
    /// Returns a `BindEnv` answering every `Decision` in `schema`. Replaces the
    /// per-decision callbacks (`announce` / `choose_targets` / `choose_mana_ability`
    /// / `choose_cost_payment`) for IR-cost cards: one structured call covers
    /// modes, targets, and cost bindings in a single round-trip.
    ///
    /// Default impl picks the first `count` candidates for `Objects` decisions,
    /// the first payable index for `Branch`, and a sensible default for `Number`
    /// (XLife/XMana = min(3, max); Replicate = 0). Strategies override to plan
    /// across decisions (e.g. choose Force-of-Will pitch only if the card is
    /// safely held).
    fn propose_announcement(
        &mut self,
        _state: &SimState,
        _source: ObjId,
        schema: &crate::ir::cost::CostSchema,
    ) -> crate::ir::executor::BindEnv {
        default_announcement(schema)
    }

    /// Phase 3 cost-IR: resolution-time payment plan (CR 701 "as ~ resolves, …"
    /// kicker-style payments). Same `CostSchema`/`BindEnv` shape as
    /// `propose_announcement`; called at a different point in the pipeline.
    /// Default impl is identical to `propose_announcement`.
    fn propose_resolution_payment(
        &mut self,
        _state: &SimState,
        _source: ObjId,
        schema: &crate::ir::cost::CostSchema,
    ) -> crate::ir::executor::BindEnv {
        default_announcement(schema)
    }

    /// CR 509.1h: when an attacker is blocked by 2+ creatures, the attacking player
    /// chooses the damage assignment order. Default: keep declaration order.
    fn order_blockers(&mut self, _state: &SimState, _attacker_id: ObjId,
                      blockers: &[ObjId]) -> Vec<ObjId> {
        blockers.to_vec()
    }

    /// CR 510.1c: the attacking player divides the attacker's combat damage among
    /// `ordered_blockers`, but must assign at least lethal damage to each blocker
    /// before assigning any to the next one. Returns one amount per blocker; sum ≤
    /// `total_damage`. The engine spills any leftover (`total_damage - sum`) to the
    /// defending player or planeswalker only when the attacker has trample.
    /// Without trample, leftover is dumped onto the last blocker (CR 510.1c sentence 3).
    ///
    /// Default heuristic: assign exactly lethal in order, then put any remainder onto
    /// the last blocker (or leave for trample spillover, decided by the engine).
    /// `lethal_per_blocker` is the engine-computed minimum for each blocker, already
    /// reflecting deathtouch (CR 702.2c) and pre-existing damage.
    fn assign_combat_damage(&mut self, _state: &SimState, _attacker_id: ObjId,
                            ordered_blockers: &[ObjId], total_damage: i32,
                            lethal_per_blocker: &[i32], has_trample: bool) -> Vec<i32> {
        let n = ordered_blockers.len();
        let mut out = vec![0i32; n];
        let mut remaining = total_damage.max(0);
        for i in 0..n {
            if remaining <= 0 { break; }
            let lethal = lethal_per_blocker[i].max(0);
            let take = remaining.min(lethal);
            out[i] = take;
            remaining -= take;
        }
        // Without trample, all damage must be assigned to blockers — pile the rest on the last.
        if !has_trample && remaining > 0 && n > 0 {
            out[n - 1] += remaining;
        }
        out
    }

    // ── Card evaluation (Phase 3) ──────────────────────────────────────────

    /// The player this strategy controls.
    fn player_id(&self) -> PlayerId;

    /// What categories does my plan still need? Considers hand + board + game state.
    fn plan_gap(&self, state: &SimState) -> TargetGap;

    /// How much does this card close the current gap? Higher = more useful.
    fn card_fills(&self, card_id: ObjId, gap: &TargetGap, state: &SimState) -> f64;

    /// A typed, non-object choice during resolution (CR 601.2 / 700): choose a
    /// color, name a creature type / card, pick a mode, pay a ward tax,
    /// may-put-on-battlefield, may-attach. Reached per-player via `with_strategy`.
    fn resolve_choice(&mut self, _source: ObjId, req: &ChoiceRequest, _state: &SimState) -> ChoiceResult {
        match req {
            ChoiceRequest::Color                    => ChoiceResult::Color(Color::Blue),
            ChoiceRequest::CreatureType             => ChoiceResult::CreatureType("Wizard".to_string()),
            ChoiceRequest::CardName                 => ChoiceResult::CardName(String::new()),
            ChoiceRequest::Mode(_)                  => ChoiceResult::Mode(0),
            ChoiceRequest::WardPayment {..}         => ChoiceResult::Bool(true),
            ChoiceRequest::MayPutOnBattlefield {..} => ChoiceResult::OptionalObject(None),
            ChoiceRequest::MayAttach                => ChoiceResult::Bool(true),
        }
    }

    /// Surveil (CR 701.30): true = put in graveyard, false = keep on top.
    /// Default: bin cards the evaluator scores below threshold (delegates to
    /// `state.evaluate_card`, matching the old run_game install).
    fn surveil_choice(&mut self, id: ObjId, state: &SimState) -> bool {
        let who = self.player_id();
        let eval = std::sync::Arc::clone(&state.evaluate_card);
        eval(who, id, state) < 0.3
    }

    /// Forced sacrifice (CR 701.16): pick which permanent to sacrifice. Default: first.
    fn sacrifice_choice(&mut self, _who: PlayerId, candidates: &[ObjId], _state: &SimState) -> Option<ObjId> {
        candidates.first().copied()
    }

    /// Resolution choices for Transmute Artifact. The original choice hooks
    /// remain the default so existing strategies keep their behavior.
    fn choose_transmute_sacrifice(&mut self, _source: ObjId, who: PlayerId,
        choices: &[ObjId], state: &SimState) -> Option<ObjId> {
        self.sacrifice_choice(who, choices, state)
    }

    fn choose_transmute_target(&mut self, source: ObjId, choices: &[ObjId],
        _payable: &[ObjId], state: &SimState) -> Option<ObjId> {
        self.choose_for_effect(source, choices, state)
    }

    /// "Put them back in any order" (Ponder, scry-then-arrange, etc.): given the
    /// looked-at library cards (current top-to-bottom), return them in the desired
    /// top-to-bottom order — a genuine player decision, not an engine sort.
    /// Default: highest-value first (via the evaluator), matching the old heuristic.
    fn order_top_library(&mut self, cards: &[ObjId], state: &SimState) -> Vec<ObjId> {
        let who = self.player_id();
        let eval = std::sync::Arc::clone(&state.evaluate_card);
        let mut scored: Vec<(ObjId, f64)> = cards.iter()
            .map(|&id| (id, eval(who, id, state)))
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().map(|(id, _)| id).collect()
    }

    /// Scry N (CR 701.18): look at the top N cards, then put any number on the
    /// bottom and the rest back on top in any order. Returns
    /// `(keep_on_top_in_order, to_bottom)` — both player decisions (which to bottom
    /// + the order of the kept). The engine sanitizes the result (only the
    /// looked-at ids, deduped; any omitted card is kept on top).
    /// Default: keep cards the evaluator scores ≥ 0.3 (in look order), bottom the
    /// rest — matching the old engine `scry_by_evaluator` behavior.
    fn scry(&mut self, top: &[ObjId], state: &SimState) -> (Vec<ObjId>, Vec<ObjId>) {
        let who = self.player_id();
        let eval = std::sync::Arc::clone(&state.evaluate_card);
        let (mut keep, mut bottom) = (Vec::new(), Vec::new());
        for &id in top {
            if eval(who, id, state) >= 0.3 { keep.push(id); } else { bottom.push(id); }
        }
        (keep, bottom)
    }

    /// "Put N cards from your hand on top of your library in any order"
    /// (Brainstorm). Returns the chosen cards top-to-bottom (`chosen[0]` ends up
    /// closest to the top, drawn first). The engine sanitizes (only hand ids,
    /// deduped, truncated to `count`). `top` is the placement (always true for
    /// Brainstorm; `false` would be bottom).
    /// Default: the `count` lowest-value cards (via the evaluator) — matching the
    /// old engine behavior of binning the worst cards.
    fn put_on_library(&mut self, count: usize, candidates: &[ObjId], _top: bool,
                      state: &SimState) -> Vec<ObjId> {
        let who = self.player_id();
        let eval = std::sync::Arc::clone(&state.evaluate_card);
        let mut scored: Vec<(ObjId, f64)> = candidates.iter()
            .map(|&id| (id, eval(who, id, state)))
            .collect();
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(count).map(|(id, _)| id).collect()
    }

    /// Drain accumulated decision-log entries. Called by the engine after each
    /// strategy invocation; entries are appended to `SimState::decision_log`.
    fn drain_decisions(&mut self) -> Vec<String> { Vec::new() }

    /// London mulligan: pick N cards to put on bottom (lowest-value cards).
    fn london_bottom(&self, state: &SimState, n: usize) -> Vec<ObjId> {
        let gap = self.plan_gap(state);
        let who = self.player_id();
        let mut scored: Vec<(ObjId, f64)> = state.hand_of(who)
            .map(|c| (c.id, self.card_fills(c.id, &gap, state)))
            .collect();
        scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.into_iter().take(n).map(|(id, _)| id).collect()
    }
}

/// A do-nothing strategy: no attacks/blocks, never mulligans, and the trait's
/// default decisions everywhere else (e.g. `choose_mana_ability` = `auto_tap_plan`,
/// `choose_targets` = `pick_targets`). A real, reusable `Strategy` — most notably
/// the **goldfish opponent** (a player that never interacts) — that apps install
/// explicitly via `set_strategy`. It is NOT substituted silently in production:
/// `with_strategy` only falls back to it under `cfg(test)`; outside tests a missing
/// strategy panics (a player must always have one).
pub struct AlwaysPass {
    player_id: PlayerId,
}

impl AlwaysPass {
    pub fn new(player_id: PlayerId) -> Self {
        AlwaysPass { player_id }
    }
}

impl Strategy for AlwaysPass {
    fn declare_attackers(&mut self, _state: &SimState) -> Vec<(ObjId, Option<ObjId>)> { Vec::new() }
    fn declare_blockers(&mut self, _state: &SimState) -> Vec<(ObjId, ObjId)> { Vec::new() }
    fn take_mulligan(&mut self, _state: &SimState, _mulligans_taken: u32) -> bool { false }
    fn player_id(&self) -> PlayerId { self.player_id }
    fn plan_gap(&self, _state: &SimState) -> TargetGap { TargetGap::default() }
    fn card_fills(&self, _card_id: ObjId, _gap: &TargetGap, _state: &SimState) -> f64 { 0.0 }
}

// ── TestStrategy (test-only) ───────────────────────────────────────────────────

/// A configurable test-player strategy: a real `Strategy` impl whose
/// resolution-time decisions (typed choices, surveil, may-do, sacrifice) are set
/// by data via the builder, with every other decision falling back to the trait
/// defaults (no attacks/blocks, never mulligans). Tests `set_strategy` one of
/// these on the relevant player to force a specific choice — the same mechanism
/// the engine uses, rather than mutating a global callback. Shared by both
/// `tests.rs` and `ir/tests.rs`.
#[cfg(test)]
pub struct TestStrategy {
    player_id: PlayerId,
    color: Option<Color>,
    card_name: Option<String>,
    /// Forces `Mode(n)` for MayDo/Choose decisions; None → trait default (Mode 0).
    mode: Option<usize>,
    /// MayPutOnBattlefield: put the first candidate instead of declining.
    put_first_candidate: bool,
    /// Surveil: Some(true) = mill, Some(false) = keep, None = evaluator default.
    surveil: Option<bool>,
    /// Forced sacrifice: pick the smallest-`ObjId` candidate (deterministic
    /// regardless of HashMap iteration order); false → trait default (first).
    sacrifice_min_id: bool,
    /// Library ordering: reverse the looked-at cards (proves OrderTop routes the
    /// arrangement through the strategy, not an engine sort); false → trait default.
    order_reverse: bool,
    /// Scripted combat declarations so engine combat-mechanic tests can drive
    /// attacks/blocks deterministically without depending on any concrete
    /// content strategy. Empty = no attacks/blocks (trait default).
    attackers: Vec<(ObjId, Option<ObjId>)>,
    blockers: Vec<(ObjId, ObjId)>,
    /// Scripted CR 509.1h damage-assignment order override (per attacker).
    block_order: Option<Vec<ObjId>>,
    /// Scripted CR 510.1c combat-damage assignment (per ordered blocker).
    damage_assignment: Option<Vec<i32>>,
    /// Scripted priority actions: each `choose_action` call pops the front of this
    /// queue (then returns `Pass`). Lets engine tests drive a specific cast/activation
    /// deterministically without any content strategy's heuristics.
    actions: std::collections::VecDeque<LegalAction>,
}

#[cfg(test)]
impl TestStrategy {
    pub(crate) fn new(player_id: PlayerId) -> Self {
        TestStrategy {
            player_id, color: None, card_name: None, mode: None,
            put_first_candidate: false, surveil: None, sacrifice_min_id: false,
            order_reverse: false,
            attackers: Vec::new(), blockers: Vec::new(),
            block_order: None, damage_assignment: None,
            actions: std::collections::VecDeque::new(),
        }
    }
    pub(crate) fn color(mut self, c: Color) -> Self { self.color = Some(c); self }
    pub(crate) fn card_name(mut self, n: impl Into<String>) -> Self { self.card_name = Some(n.into()); self }
    pub(crate) fn mode(mut self, m: usize) -> Self { self.mode = Some(m); self }
    pub(crate) fn put_first_candidate(mut self) -> Self { self.put_first_candidate = true; self }
    pub(crate) fn surveil(mut self, mill: bool) -> Self { self.surveil = Some(mill); self }
    pub(crate) fn sacrifice_min_id(mut self) -> Self { self.sacrifice_min_id = true; self }
    pub(crate) fn order_reverse(mut self) -> Self { self.order_reverse = true; self }
    /// Declare these attackers (id, optional planeswalker/target) on DeclareAttackers.
    pub(crate) fn attacking(mut self, atk: Vec<(ObjId, Option<ObjId>)>) -> Self { self.attackers = atk; self }
    /// Declare these (attacker, blocker) pairs on DeclareBlockers.
    pub(crate) fn blocking(mut self, blk: Vec<(ObjId, ObjId)>) -> Self { self.blockers = blk; self }
    /// Override the damage-assignment order for a multiply-blocked attacker.
    pub(crate) fn block_order(mut self, order: Vec<ObjId>) -> Self { self.block_order = Some(order); self }
    /// Override the combat-damage split across ordered blockers.
    pub(crate) fn damage_assignment(mut self, d: Vec<i32>) -> Self { self.damage_assignment = Some(d); self }
    /// Queue a priority action (cast/activate) for the next `choose_action` call.
    pub(crate) fn action(mut self, a: LegalAction) -> Self { self.actions.push_back(a); self }
}

#[cfg(test)]
impl Strategy for TestStrategy {
    fn declare_attackers(&mut self, _s: &SimState) -> Vec<(ObjId, Option<ObjId>)> { self.attackers.clone() }
    fn declare_blockers(&mut self, _s: &SimState) -> Vec<(ObjId, ObjId)> { self.blockers.clone() }
    fn take_mulligan(&mut self, _s: &SimState, _m: u32) -> bool { false }

    fn choose_action(&mut self, _s: &SimState, _ap: PlayerId, legal: &[LegalAction]) -> LegalAction {
        // Pop the next scripted action if it is currently legal; otherwise pass.
        while let Some(a) = self.actions.pop_front() {
            if legal.iter().any(|l| l == &a) { return a; }
        }
        LegalAction::Pass
    }

    fn order_blockers(&mut self, _s: &SimState, _atk: ObjId, blockers: &[ObjId]) -> Vec<ObjId> {
        self.block_order.clone().unwrap_or_else(|| blockers.to_vec())
    }

    fn assign_combat_damage(&mut self, s: &SimState, atk: ObjId, blockers: &[ObjId],
                            total: i32, lethal: &[i32], trample: bool) -> Vec<i32> {
        match &self.damage_assignment {
            Some(a) => a.clone(),
            None => AlwaysPass::new(self.player_id)
                .assign_combat_damage(s, atk, blockers, total, lethal, trample),
        }
    }
    fn player_id(&self) -> PlayerId { self.player_id }
    fn plan_gap(&self, _s: &SimState) -> TargetGap { TargetGap::default() }
    fn card_fills(&self, _i: ObjId, _g: &TargetGap, _s: &SimState) -> f64 { 0.0 }

    fn resolve_choice(&mut self, source: ObjId, req: &ChoiceRequest, state: &SimState) -> ChoiceResult {
        match req {
            ChoiceRequest::Color if self.color.is_some() =>
                ChoiceResult::Color(self.color.unwrap()),
            ChoiceRequest::CardName if self.card_name.is_some() =>
                ChoiceResult::CardName(self.card_name.clone().unwrap()),
            ChoiceRequest::Mode(_) if self.mode.is_some() =>
                ChoiceResult::Mode(self.mode.unwrap()),
            ChoiceRequest::MayPutOnBattlefield { candidates } if self.put_first_candidate =>
                ChoiceResult::OptionalObject(candidates.first().copied()),
            // Anything unset falls back to the trait default policy.
            _ => AlwaysPass::new(self.player_id).resolve_choice(source, req, state),
        }
    }

    fn surveil_choice(&mut self, id: ObjId, state: &SimState) -> bool {
        match self.surveil {
            Some(mill) => mill,
            None => AlwaysPass::new(self.player_id).surveil_choice(id, state),
        }
    }

    fn sacrifice_choice(&mut self, who: PlayerId, candidates: &[ObjId], state: &SimState) -> Option<ObjId> {
        if self.sacrifice_min_id {
            candidates.iter().min_by_key(|id| id.0).copied()
        } else {
            AlwaysPass::new(self.player_id).sacrifice_choice(who, candidates, state)
        }
    }

    fn order_top_library(&mut self, cards: &[ObjId], state: &SimState) -> Vec<ObjId> {
        if self.order_reverse {
            cards.iter().rev().copied().collect()
        } else {
            AlwaysPass::new(self.player_id).order_top_library(cards, state)
        }
    }
}

// ── Hand and board action enumeration ─────────────────────────────────────────

/// Check whether an ability can be activated (cost payable + valid target exists).
/// `source_untapped` must be true when the source is an untapped permanent.
fn ability_available(
    ability: &AbilityDef,
    state: &SimState,
    who: PlayerId,
    source_id: ObjId,
    source_untapped: bool,
) -> bool {
    // Legacy per-ability suppression flag (CE-set). Retained as a fallback;
    // the declarative path is the action-Restriction below.
    if !ability.activatable {
        return false;
    }
    // Action-Restriction on the source permanent (Null Rod / Karn: "artifact
    // abilities can't be activated"; Disruptor Flute: "abilities of the named
    // card can't be activated unless they're mana abilities"). Keyed on the
    // source, so it covers every ability the source has, incl. granted ones —
    // CR 101.2 "can't beats can". `ability` here is a non-mana `AbilityDef` (mana
    // abilities live in `mana_abilities()` and route through the mana sub-loop's
    // `mana_ability_restricted`), so the mana exemption is structurally N/A.
    if crate::ir::executor::action_restricted(
        state, crate::ir::ability::ActionKind::Activate, source_id) {
        return false;
    }
    if !crate::loyalty_activation_allowed(state, who, source_id, ability) {
        return false;
    }
    // Sorcery-speed abilities (loyalty, etc.) require empty stack.
    if ability.timing == ActivationTiming::Sorcery && !state.stack.is_empty() {
        return false;
    }
    // A tap cost cannot be paid by an already-tapped source. The cost schema
    // describes announcement choices; a fixed-source Tap emits no choice.
    if ability.costs.requires_tap_self() && !source_untapped {
        return false;
    }
    let cost_payable = {
        let crate::ir::ability::CostBody::Ir(action) = &ability.costs;
        // build_schema only checks object/branch/number decisions; PayMana
        // emits no decision, so it doesn't verify mana availability. Add
        // an explicit potential-mana check via the helper on CostBody so
        // strategy doesn't enumerate activations it can't afford (which
        // would cause the activation pipeline to silently no-op the cost
        // and infinite-loop on retry).
        let schema_ok = crate::ir::cost_exec::build_schema(action, state, who, source_id).is_some();
        let mana_ok = match ability.costs.first_mana_cost() {
            Some(mc) => crate::can_pay_with_auto_mana(state, who, &mc,
                ability.costs.requires_tap_self().then_some(source_id)),
            None => true,
        };
        schema_ok && mana_ok
    };
    cost_payable
        && (ability.target_spec.is_none() || has_valid_target(&ability.target_spec, state, who, source_id))
}

/// True if the player can currently afford to cast `name` via any available cost.
///
/// Tries the standard mana cost first; falls back to alternate costs (e.g. delve, pitch).
/// For XLife additional costs, uses strategy default X=3 (`choose_x_for_spell` default).
fn spell_is_affordable(
    card_id: ObjId,
    def: &CardDef,
    state: &SimState,
    who: PlayerId,
) -> bool {
    let mut cost = parse_mana_cost(def.mana_cost());
    if def.delve() && cost.generic > 0 {
        let gy_len = state.graveyard_of(who).count() as i32;
        cost.generic = (cost.generic - gy_len).max(0);
    }
    cost = crate::apply_casting_cost_rules(cost, def);
    let mana_is_usable = !def.mana_cost().is_empty()
        && crate::can_pay_spell_with_auto_mana(state, who, &cost, card_id);
    let base_payable = if mana_is_usable {
        true
    } else {
        def.alternate_costs().iter().any(|c| {
            if state.hand_size(who) < c.hand_min { return false; }
            if c.condition.as_ref().map_or(false, |f| !f(who, state)) { return false; }
            let crate::ir::ability::CostBody::Ir(action) = &c.costs;
            if crate::ir::cost_exec::build_schema(action, state, who, card_id).is_none() { return false; }
            let base = c.costs.first_mana_cost().unwrap_or_default();
            let floored = crate::apply_casting_cost_rules(base.clone(), def);
            let extra = (floored.mana_value() - base.mana_value()).max(0);
            let extra_cost = ManaCost { generic: extra, ..Default::default() };
            state.potential_mana(who).can_pay(&extra_cost)
        })
    };
    // A variable-X spell is legal if at least X=0 is payable.  The exact X is
    // announced later from AnnounceOptions::max_x; checking X=3 here wrongly
    // hid spells such as Kozilek's Command when only X=0 or X=1 was affordable.
    let legal_x = 0u32;
    base_payable && can_pay_additional_ir_cost(state, who, card_id, &def.additional_costs, legal_x)
}


/// Build the complete list of legal actions for a player (new-protocol).
/// Used by the engine to present choices to `choose_action`.
fn can_play_land_at_current_timing(state: &SimState, who: PlayerId) -> bool {
    // CR 305.1: a player may play a land only during a main phase of their own
    // turn, when the stack is empty, and while they have priority. The engine
    // calls action collection only for a priority holder, so the final three
    // conditions here are active player + main phase + empty stack.
    // `None` / UNSET remains permissive for legacy unit-test states that do not
    // install a turn position. Full games always have concrete values.
    let active_ok = state.active_player().map_or(true, |ap| ap == who);
    let phase_ok = match state.current_phase {
        None => true,
        Some(TurnPosition::Phase(PhaseKind::PreCombatMain | PhaseKind::PostCombatMain)) => true,
        _ => false,
    };
    state.stack.is_empty() && active_ok && phase_ok
}

fn can_cast_at_current_timing(state: &SimState, who: PlayerId, def: &CardDef, face: SpellFace) -> bool {
    let is_instant = match face {
        SpellFace::Main => def.is_instant() || def.has_keyword(Keyword::Flash),
        SpellFace::Back => def.adventure()
            .map(|b| b.is_instant() || b.has_keyword(Keyword::Flash))
            .unwrap_or(false),
    };
    if is_instant {
        return true;
    }
    let active_ok = state.current_ap == ObjId::UNSET || state.current_ap == state.player_id(who);
    let phase_ok = match state.current_phase {
        None => true, // bare unit-test states do not always install a turn position
        Some(TurnPosition::Phase(PhaseKind::PreCombatMain | PhaseKind::PostCombatMain)) => true,
        _ => false,
    };
    state.stack.is_empty() && active_ok && phase_ok
}

pub(crate) fn collect_legal_actions(state: &SimState, who: PlayerId) -> Vec<LegalAction> {
    let mut actions: Vec<LegalAction> = vec![LegalAction::Pass];

    // ── Castable spells from hand ────────────────────────────────────────────
    let hand_cards: Vec<(ObjId, String)> = state.hand_of(who)
        .map(|c| (c.id, c.catalog_key.clone()))
        .collect();
    let mut seen_names: std::collections::HashSet<String> = Default::default();
    for (card_id, name) in &hand_cards {
        let Some(def) = state.def_of(*card_id) else { continue };
        if def.is_land() { continue; }
        if !def.castable { continue; }
        // CR 101.2 "can't beats can": an action-Restriction (Lavinia) gates the
        // *permitted* cast. AND-NOT over `castable`, so a granted cast (Dauthi /
        // Flashback) under a restriction is still illegal, order-independent.
        if crate::ir::executor::action_restricted(state, crate::ir::ability::ActionKind::Cast, *card_id) {
            continue;
        }
        if !card_has_implementation(def) { continue; }
        if !can_cast_at_current_timing(state, who, def, SpellFace::Main) { continue; }
        if def.legendary() && state.permanents_of(who).any(|c| c.catalog_key == name.as_str()) { continue; }
        if !def.target_spec().is_none() && !has_valid_target(def.target_spec(), state, who, *card_id) { continue; }
        if !spell_is_affordable(*card_id, def, state, who) { continue; }
        if seen_names.insert(name.clone()) {
            actions.push(LegalAction::CastSpell { card_id: *card_id, face: SpellFace::Main });
        }
        // Adventure back-face.
        if let Some(face) = def.adventure() {
            if !can_cast_at_current_timing(state, who, def, SpellFace::Back) { continue; }
            if !face.mana_cost().is_empty() {
                let cost = parse_mana_cost(face.mana_cost());
                if !state.potential_mana(who).can_pay(&cost) { continue; }
            }
            if !face.target_spec().is_none() && !has_valid_target(face.target_spec(), state, who, *card_id) { continue; }
            actions.push(LegalAction::CastSpell { card_id: *card_id, face: SpellFace::Back });
        }
    }

    // ── In-hand abilities (cycling, channel, ninjutsu, etc.) ───────────────────
    for (card_id, _name) in &hand_cards {
        let Some(def) = state.def_of(*card_id) else { continue };
        for (idx, ab) in def.abilities().iter().enumerate() {
            if !matches!(ab.source_zone, SourceZone::Hand) { continue; }
            if !ability_available(ab, state, who, *card_id, true) { continue; }
            actions.push(LegalAction::ActivateAbility { source_id: *card_id, ability_index: idx });
        }
    }

    // ── Battlefield abilities (non-mana) ─────────────────────────────────────
    let perms: Vec<(ObjId, bool)> = state.permanents_of(who)
        .map(|p| (p.id, !p.bf().map_or(false, |bf| bf.tapped)))
        .collect();
    for (perm_id, untapped) in &perms {
        let Some(def) = state.def_of(*perm_id) else { continue };
        for (idx, ab) in def.abilities().iter().enumerate() {
            if !ability_available(ab, state, who, *perm_id, *untapped) { continue; }
            actions.push(LegalAction::ActivateAbility { source_id: *perm_id, ability_index: idx });
        }
    }

    // ── Mana abilities with non-default timing (LED: instant-only) ─────────
    for (perm_id, untapped) in &perms {
        let Some(def) = state.def_of(*perm_id) else { continue };
        for (idx, ma) in def.mana_abilities().iter().enumerate() {
            if !ma.activatable { continue; }
            if ma.timing == ActivationTiming::Default { continue; } // handled in mana sub-loop
            if ma.timing == ActivationTiming::Sorcery && !state.stack.is_empty() { continue; }
            if !matches!(ma.source_zone, SourceZone::Battlefield) { continue; }
            if ma.costs.requires_tap_self() && !untapped { continue; }
            if ma.condition.as_ref().map_or(false, |cond| !obj_matches(cond, *perm_id, state)) { continue; }
            let crate::ir::ability::CostBody::Ir(action) = &ma.costs;
            if crate::ir::cost_exec::build_schema(action, state, who, *perm_id).is_none() { continue; }
            actions.push(LegalAction::ActivateManaAbility { source_id: *perm_id, ability_index: idx });
        }
    }

    // ── Land drops ───────────────────────────────────────────────────────────
    if state.player(who).lands_played_this_turn < 1 && can_play_land_at_current_timing(state, who) {
        for (card_id, _name) in &hand_cards {
            let Some(def) = state.def_of(*card_id) else { continue };
            if def.is_land() {
                actions.push(LegalAction::LandDrop(*card_id));
            }
        }
    }

    // ── Adventure creatures in exile (cast creature face) ──────────────────────
    for card in state.on_adventure_of(who) {
        let card_id = card.id;
        if let Some(def) = state.def_of(card_id) {
            // Same cast-Restriction gate as the hand loop — a zone-agnostic
            // restriction (Lavinia) must catch casts from any zone; a zone-scoped
            // one (Grafdigger's: GY/library) simply won't match this exile cast.
            if crate::ir::executor::action_restricted(state, crate::ir::ability::ActionKind::Cast, card_id) { continue; }
            if !can_cast_at_current_timing(state, who, def, SpellFace::Main) { continue; }
            let cost = parse_mana_cost(def.mana_cost());
            if state.potential_mana(who).can_pay(&cost) {
                if !def.target_spec().is_none() && !has_valid_target(def.target_spec(), state, who, card_id) { continue; }
                actions.push(LegalAction::CastSpell { card_id, face: SpellFace::Main });
            }
        }
    }

    // ── Castable cards in exile (Dauthi Voidwalker CE grants castable + free alt cost) ──
    for card in state.exile_of(who) {
        let card_id = card.id;
        let Some(def) = state.def_of(card_id) else { continue };
        if def.is_land() { continue; }
        if !def.castable { continue; }
        // Cast-Restriction gate. A Dauthi-granted exile cast under Grafdigger's
        // ("can't cast from GY/library") is *not* restricted — exile ≠ GY/library —
        // so it falls out of the zone-scoped subject with no special-casing; under
        // Lavinia (zone-agnostic) it is correctly restricted (CR 101.2 "can't beats can").
        if crate::ir::executor::action_restricted(state, crate::ir::ability::ActionKind::Cast, card_id) { continue; }
        if !card_has_implementation(def) { continue; }
        if !can_cast_at_current_timing(state, who, def, SpellFace::Main) { continue; }
        if !def.target_spec().is_none() && !has_valid_target(def.target_spec(), state, who, card_id) { continue; }
        if !spell_is_affordable(card_id, def, state, who) { continue; }
        actions.push(LegalAction::CastSpell { card_id, face: SpellFace::Main });
    }


    // ── Castable top card of library (Mystic Forge / future top-cast effects) ──
    // Continuous effects may mark qualifying library cards `castable`; only the
    // actual top card is enumerated here, preserving "from the top" semantics.
    if let Some(&card_id) = state.player(who).library_order.front() {
        if let Some(def) = state.def_of(card_id) {
            if !def.is_land() && def.castable
                && !crate::ir::executor::action_restricted(state, crate::ir::ability::ActionKind::Cast, card_id)
                && card_has_implementation(def)
                && can_cast_at_current_timing(state, who, def, SpellFace::Main)
                && (def.target_spec().is_none() || has_valid_target(def.target_spec(), state, who, card_id))
                && spell_is_affordable(card_id, def, state, who)
            {
                actions.push(LegalAction::CastSpell { card_id, face: SpellFace::Main });
            }
        }
    }

    actions
}

/// Phase 3 default `propose_announcement` body.
///
/// Walks the schema and answers each `Decision` with a sensible fallback:
/// - `Objects { candidates, count }`: take the first `count` candidates. Phase
///   4+ will sharpen this per-card via Strategy overrides; the floor is "any
///   legal answer is better than none" so the game can keep moving.
/// - `Branch { payable, .. }`: pick the first payable index.
/// - `Number { kind, max }`: XLife/XMana clamp at `min(3, max)` (matches the
///   legacy `announce` X=3 default); Replicate defaults to 0 (no copies).
///
/// Strategies override to plan across decisions; this default exists so every
/// strategy keeps compiling without churn during Phase 3.
pub(crate) fn default_announcement(
    schema: &crate::ir::cost::CostSchema,
) -> crate::ir::executor::BindEnv {
    let mut env = crate::ir::executor::BindEnv::new();
    fill_default_announcement(schema, &mut env);
    env
}

/// Fill `env` with default answers for every decision in `schema`, recursing
/// into the first-payable branch of any `Branch` decision so a chosen Choose
/// option's nested decisions (e.g. a discard pick) are also answered.
fn fill_default_announcement(
    schema: &crate::ir::cost::CostSchema,
    env: &mut crate::ir::executor::BindEnv,
) {
    use crate::ir::cost::{DecisionKind, NumberKind};
    use crate::ir::expr::Value;
    for d in &schema.decisions {
        match &d.kind {
            DecisionKind::Objects { candidates, count } => {
                let n = *count as usize;
                let picked: Vec<ObjId> = candidates.iter().take(n).copied().collect();
                let value = if n == 1 && picked.len() == 1 {
                    Value::Obj(picked[0])
                } else {
                    Value::ObjSet(picked)
                };
                env.bindings.insert(d.binding, value);
            }
            DecisionKind::Branch { payable, branches, .. } => {
                let i = *payable.first().unwrap_or(&0);
                env.bindings.insert(d.binding, Value::Num(i as i64));
                if let Some(sub) = branches.get(i) {
                    fill_default_announcement(sub, env);
                }
            }
            DecisionKind::Number { kind, max } => {
                let default = match kind {
                    NumberKind::XLife | NumberKind::XMana => 3u32.min(*max),
                    NumberKind::Replicate => 0,
                };
                env.bindings.insert(d.binding, Value::Num(default as i64));
            }
        }
    }
}
