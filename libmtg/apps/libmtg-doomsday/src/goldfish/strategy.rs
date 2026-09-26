//! `DDGoldfishStrategy` — cast Doomsday as fast as possible, before a cutoff turn.
//!
//! This is NOT a reuse of the baseline `DoomsdayStrategy`. Every *decision* is
//! driven by the backward resource solver in [`crate::goldfish::recipe`]:
//!
//! - **When to go off:** cast Doomsday the instant a complete recipe is
//!   assemblable this turn (`recipe::sufficient`); then mechanically assemble the
//!   mana (land / petal / fetch / ritual) and cast it.
//! - **What to do otherwise:** develop a black source and dig with cantrips to
//!   raise `P(cast Doomsday by cutoff)`.
//! - **How to resolve each cantrip / tutor / fetch / scry:** the solver's
//!   recommendations — `recipe::best_top_choice` for Ponder, and the
//!   `recipe::dd_card_value` valuation for fetch/tutor targets, Flow State's dig,
//!   Brainstorm's put-back, scry, and surveil.
//! - **Whether to keep the opening hand:** a threshold on `recipe::p_cast_by`.
//!
//! The solver is the brain; this strategy is only the hands — it translates those
//! verdicts into engine `LegalAction`s. The mechanical "how to emit an action"
//! shape is cribbed from `DoomsdayStrategy`; none of its heuristics are.

use std::cmp::Ordering;

use libmtg_engine::*;

use super::recipe::{self, CardRole};

const DOOMSDAY: &str = "Doomsday";
const FANTASTICAR: &str = "The Fantasticar";
/// Self-targetable counters we cast on our OWN spell (via their free alternative cost)
/// purely for the cast-slot — the 4th noncreature spell that triggers the car pop.
const DAZE: &str = "Daze";
const FORCE_OF_WILL: &str = "Force of Will";

/// The cutoff turn past which a Doomsday is assumed too slow (Wastelanded / raced).
/// The strategy plays to maximise `P(cast Doomsday by cutoff)`.
pub const DEFAULT_CUTOFF: u32 = 4;

/// Horizon for the "execute a slower line anyway" fallback when no line lands by the
/// cutoff and there's nothing to dig — so we still cast eventually rather than pass.
const FALLBACK_HORIZON: u32 = 10;

pub struct DDGoldfishStrategy {
    player_id: PlayerId,
    /// "Cast by this turn" objective (1-based). Drives mulligans + cantrip choices.
    cutoff: u32,
    /// Opening-hand discipline (Keep7 / Realistic / Aggressive).
    mull_mode: super::mull::MullMode,
    /// Set by `order_top_library` (Ponder) when the solver wants to shuffle; consumed
    /// by the immediately-following `MayDo`(shuffle) `resolve_choice`.
    pending_shuffle: bool,
    /// A/B debug mode: also evaluate the reference value-table heuristic at each
    /// selection decision and log (`DIFF …`) wherever it disagrees with the
    /// principled policy. Off in normal play.
    compare: bool,
    /// When true, also pursue The Fantasticar pop (cast the car + dump free noncreature
    /// spells) whenever it can send strictly sooner than Doomsday. False = Doomsday-only
    /// baseline (the two-wincon-speedup comparison).
    car_enabled: bool,
    decisions: Vec<String>,
}

impl DDGoldfishStrategy {
    /// Cast-ASAP pilot with the default ([`MullMode::Realistic`]) mulligan.
    pub fn new(cutoff: u32) -> Self {
        Self::with_mull_mode(cutoff, super::mull::MullMode::default())
    }

    /// Cast-ASAP pilot driving an explicit mulligan mode.
    pub fn with_mull_mode(cutoff: u32, mull_mode: super::mull::MullMode) -> Self {
        Self {
            player_id: PlayerId::Us,
            cutoff: cutoff.max(1),
            mull_mode,
            pending_shuffle: false,
            compare: false,
            car_enabled: false,
            decisions: Vec::new(),
        }
    }

    /// Enable pursuing The Fantasticar pop as a second wincon (see `car_enabled`).
    pub fn with_car(mut self, on: bool) -> Self {
        self.car_enabled = on;
        self
    }

    /// Like [`new`], but logs `DIFF …` at every selection decision where the
    /// reference value-table heuristic would choose differently (A/B debugging).
    pub fn new_comparing(cutoff: u32) -> Self {
        let mut s = Self::new(cutoff);
        s.compare = true;
        s
    }

    fn dlog(&mut self, msg: impl Into<String>) {
        self.decisions.push(msg.into());
    }

    /// Log a cantrip/selection decision stamped with the current game-log position, so the
    /// UI can interleave it into the play sequence in exact temporal order. `msg` is
    /// "T{n} · {reason} ‖ {fate tokens}"; the stamp is how many log lines exist right now
    /// (the decision happens between that line and the next one appended).
    fn dig(&mut self, state: &SimState, msg: String) {
        self.dlog(format!("DIG @{} {}", state.log.len(), msg));
    }

    /// Card name for logging.
    fn nm(state: &SimState, id: ObjId) -> String {
        state.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_else(|| "?".to_string())
    }

    /// Whether we actually *hold* the payoff (Doomsday) in hand. A tutor that can
    /// fetch Doomsday does NOT count — it must still be cast and resolved before we
    /// hold the payoff. This is what gates casting the tutor: `payoff_in_hand` is
    /// true when a tutor is in hand, so using it here would (wrongly) conclude the
    /// payoff is already secured and assemble mana for a Doomsday we never acquire.
    fn holds_payoff(&self, state: &SimState, who: PlayerId) -> bool {
        state
            .hand_of(who)
            .any(|c| recipe::card_role(state, who, c.id) == CardRole::Payoff)
    }

    /// A complete recipe is assemblable this turn — emit the next mana-assembly
    /// step toward casting Doomsday. We try the steps that don't spend a card first
    /// (land drop, then deploy a petal, then crack a fetch) and only cast a ritual
    /// if still short; `choose_action` casts Doomsday itself as soon as it becomes
    /// castable, so a ritual is only ever cast when genuinely needed.
    fn assemble_step(&self, state: &SimState, who: PlayerId, legal: &[LegalAction]) -> Option<LegalAction> {
        use CardRole::*;
        // 0. If the deterministic line runs through a tutor (Personal Tutor) and we
        //    don't yet hold Doomsday, cast the tutor to put it on top — otherwise we
        //    assemble BBB for a payoff we never acquire. Falls through to a land drop
        //    when no blue source is online yet, then casts the tutor on re-entry.
        if !self.holds_payoff(state, who) {
            if let Some(a) = first_cast(state, who, legal, &[PayoffTutor]) {
                return Some(a);
            }
        }
        // 1. Play a black source (untapped land / fetch / tapland).
        if let Some(a) = best_land_drop(state, who, legal, &[BlackLandUntapped, Fetch, BlackLandTapped]) {
            return Some(a);
        }
        // 2. Deploy a free mana artifact (Lotus Petal).
        if let Some(a) = first_cast(state, who, legal, &[Petal]) {
            return Some(a);
        }
        // 3. Crack a fetch already in play to get a black source online.
        if let Some(a) = fetch_activation(state, legal) {
            return Some(a);
        }
        // 4. Cast a ritual (Dark Ritual) to finish the mana.
        if let Some(a) = first_cast(state, who, legal, &[Ritual]) {
            return Some(a);
        }
        None
    }

    /// Not castable this turn — develop a black source and dig toward the cutoff.
    fn develop_and_dig(&self, state: &SimState, who: PlayerId, legal: &[LegalAction]) -> Option<LegalAction> {
        use CardRole::*;
        // a) Develop mana: play a land (untapped black first so it can cast a cantrip
        //    this turn, then fetch, tapland, any blue source).
        if let Some(a) = best_land_drop(state, who, legal,
            &[BlackLandUntapped, Fetch, BlackLandTapped, BlueSource]) {
            return Some(a);
        }
        // b) Crack a fetch in play to put a dual online (a usable U/B source).
        if let Some(a) = fetch_activation(state, legal) {
            return Some(a);
        }
        // c) If we don't hold Doomsday itself, cast a tutor for it (having the tutor
        //    in hand is not the same as holding the payoff — it must be cast).
        if !self.holds_payoff(state, who) {
            if let Some(a) = first_cast(state, who, legal, &[PayoffTutor]) {
                return Some(a);
            }
        }
        // d) Dig with a cantrip (Ponder / Brainstorm / Consider / Flow State / …).
        if let Some(a) = first_cast(state, who, legal, &[Cantrip]) {
            return Some(a);
        }
        None
    }

    /// P(SEND by cutoff) if `id` were acquired now — staged on top (`to_top`, a tutor:
    /// drawn next turn) or drawn into hand this turn (a dig). The unified objective (P of
    /// resolving Doomsday OR popping the car), so a revealed car / car fuel scores just
    /// like a Doomsday piece — this is what makes the deck dig toward and keep the car.
    fn candidate_p(&self, state: &SimState, id: ObjId, to_top: bool) -> f64 {
        let who = self.player_id;
        let key = state.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_default();
        if to_top {
            recipe::p_send_by_with_known_top(state, who, &[key.as_str()], false, self.cutoff)
        } else {
            recipe::p_send_by_full(state, who, &[key.as_str()], &[], true, self.cutoff)
        }
    }

    /// E[TTD] if `id` were acquired now (staged on top / drawn to hand) — the same
    /// objective as `candidate_p` but in TURNS, which (unlike a probability) does NOT
    /// saturate. This is the tie-break that separates two by-cutoff lines: a fetched Sea
    /// pays a tutor's pip sooner than a Swamp, and staging Doomsday is a turn faster than
    /// staging a tutor for it (the chained-tutor +1 in `deterministic_cast_turn_full`).
    fn candidate_ttd(&self, state: &SimState, id: ObjId, to_top: bool) -> f64 {
        let who = self.player_id;
        let key = state.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_default();
        recipe::e_ttd_with_known_top(state, who, &[key.as_str()], !to_top, self.cutoff)
    }

    /// Whether `id` fills an outstanding requirement of the plan: the payoff (until one
    /// is secured), any black-mana source, or a cantrip while we still lack a
    /// deterministic line by the cutoff (so digging is still needed). Solver-derived
    /// (`card_role` / `payoff_in_hand` / `deterministic_cast_turn`) — no value table.
    /// Used for "which cards can I afford to lose" (Brainstorm put-back, mulligan
    /// bottoming); the visible-path requirement set will sharpen this later.
    fn card_needed(&self, state: &SimState, id: ObjId) -> bool {
        let who = self.player_id;
        match recipe::card_role(state, who, id) {
            // Keep the payoff itself — only a REDUNDANT copy is expendable. We keep the
            // FIRST payoff in hand (canonical) and treat any further copy as buriable.
            // (Bug guard: `payoff_in_hand` is true *because of this very card*, so we
            // must not read it as "already secured" and bury our own Doomsday.)
            CardRole::Payoff | CardRole::PayoffTutor => state
                .hand_of(who)
                .find(|c| matches!(recipe::card_role(state, who, c.id),
                                   CardRole::Payoff | CardRole::PayoffTutor))
                .map_or(true, |first| first.id == id),
            CardRole::Ritual
            | CardRole::Petal
            | CardRole::BlackLandUntapped
            | CardRole::Fetch
            | CardRole::BlackLandTapped
            | CardRole::BlueSource => true,
            CardRole::Cantrip => recipe::deterministic_cast_turn(state, who, self.cutoff.max(1))
                .map_or(true, |t| t > self.cutoff),
            CardRole::Other => false,
        }
    }
}

// ── Action-emission helpers ───────────────────────────────────────────────────
//
// Choosing WHICH legal action serves the solver's plan. The only judgement here is
// *land ordering* (which black source to play when several would serve) — a
// genuinely hard sub-problem the user has flagged as an allowed heuristic.
// Everything else is a structural role match: cards of the same role are
// interchangeable for the plan, and the solver decides *that* a role advances it.

/// Land-drop preference ordinal — the allowed land-ordering heuristic. An untapped
/// black source is best (mana now), then a fetch (mana now via a crack), then a
/// tapland (mana next turn), then a bare blue source.
fn land_priority(role: CardRole) -> u8 {
    match role {
        CardRole::BlackLandUntapped => 4,
        CardRole::Fetch => 3,
        CardRole::BlackLandTapped => 2,
        CardRole::BlueSource => 1,
        _ => 0,
    }
}

/// Last-ditch trinary keep-ordering — **cantrip > relevant > irrelevant** — used ONLY
/// when the principled rules (det-ttd / min-ttd / p_cast_by) don't separate the
/// options (sanctioned alongside land ordering). Cantrips rank highest because their
/// downstream digging is value the objective can't score (chained cantrips are
/// intractable); combo-relevant cards (mana / payoff / tutors) outrank dead bricks.
/// Higher = keep longer / bury later.
fn keep_rank(state: &SimState, who: PlayerId, id: ObjId) -> u8 {
    match recipe::card_role(state, who, id) {
        CardRole::Cantrip => 2,
        CardRole::Other => 0, // irrelevant brick
        _ => 1,               // relevant: payoff / tutor / ritual / petal / land / blue source
    }
}

/// Acquire-target dominance (the tutor analog of `land_priority` for fetches): the
/// payoff itself beats a tutor for it — staging Doomsday skips the tutor's extra cast.
/// Used only as the LAST tie-break, when both P(cast) and min-ttd are equal (tight
/// mana, where neither line is deterministic and the stochastic estimate can't see the
/// payoff-arrival delay). Without it the tie falls to the last candidate, which can be
/// a redundant tutor → tutor→tutor loop. Higher = acquire first.
fn payoff_rank(state: &SimState, who: PlayerId, id: ObjId) -> u8 {
    match recipe::card_role(state, who, id) {
        CardRole::Payoff => 2,
        CardRole::PayoffTutor => 1,
        _ => 0,
    }
}


/// The legal `LandDrop` whose role is in `want`, chosen by `land_priority`.
fn best_land_drop(state: &SimState, who: PlayerId, legal: &[LegalAction], want: &[CardRole]) -> Option<LegalAction> {
    legal.iter()
        .filter_map(|a| match a {
            LegalAction::LandDrop(id) => {
                let role = recipe::card_role(state, who, *id);
                want.contains(&role).then(|| (a.clone(), land_priority(role)))
            }
            _ => None,
        })
        .max_by_key(|(_, pri)| *pri)
        .map(|(a, _)| a)
}

/// The first legal `CastSpell` whose role is in `want`. No ranking — same-role
/// cards are interchangeable (any ritual makes black, any petal); the solver
/// already decided this role advances the plan.
fn first_cast(state: &SimState, who: PlayerId, legal: &[LegalAction], want: &[CardRole]) -> Option<LegalAction> {
    legal.iter()
        .find(|a| matches!(a, LegalAction::CastSpell { card_id, .. }
            if want.contains(&recipe::card_role(state, who, *card_id))))
        .cloned()
}

/// Every ordered sequence drawn from a subset of `items` (including the empty
/// sequence). For the tiny look-at-top sets (≤3 cards) the strategy enumerates
/// arrangements to score each via the objective.
fn ordered_subsets<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    let n = items.len();
    let mut out: Vec<Vec<T>> = Vec::new();
    for mask in 0u32..(1u32 << n) {
        let subset: Vec<T> = (0..n).filter(|i| mask & (1 << i) != 0).map(|i| items[i].clone()).collect();
        out.extend(permutations(&subset));
    }
    out
}

fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let head = rest.remove(i);
        for mut p in permutations(&rest) {
            p.insert(0, head.clone());
            out.push(p);
        }
    }
    out
}

/// A legal `CastSpell` for the named card (the payoff — Doomsday).
fn cast_named(state: &SimState, legal: &[LegalAction], name: &str) -> Option<LegalAction> {
    legal.iter()
        .find(|a| matches!(a, LegalAction::CastSpell { card_id, .. }
            if state.objects.get(card_id).map_or(false, |c| c.catalog_key == name)))
        .cloned()
}

/// Translate one emitted [`recipe::LineStep`] into the matching legal engine action
/// for *this* window, if it is available now. The strategy executes the first step
/// of the freshly-emitted line that is currently legal, so the engine's state
/// advance + re-emission walks the multi-turn line.
fn line_step_action(step: recipe::LineStep, legal: &[LegalAction]) -> Option<LegalAction> {
    use recipe::LineStep;
    let hits = |a: &LegalAction| match step {
        LineStep::PlayLand(id) => matches!(a, LegalAction::LandDrop(x) if *x == id),
        LineStep::CrackFetch(id) => {
            matches!(a, LegalAction::ActivateAbility { source_id, .. } if *source_id == id)
        }
        LineStep::CastPetal(id)
        | LineStep::CastRitual(id)
        | LineStep::CastTutor(id)
        | LineStep::CastDoomsday(id) => {
            matches!(a, LegalAction::CastSpell { card_id, .. } if *card_id == id)
        }
    };
    legal.iter().find(|a| hits(a)).cloned()
}

/// Follow the solved deterministic line: emit the first of its steps that is legal
/// this window. `None` only when the emitter can't build a line (exotic source it
/// doesn't model) — the caller logs + falls back so we can measure that gap.
fn follow_line(state: &SimState, who: PlayerId, legal: &[LegalAction], max_turn: u32)
    -> Option<LegalAction>
{
    let line = recipe::deterministic_line(state, who, max_turn)?;
    // A real line may have nothing to do *this* window (it matures next turn) — return
    // an explicit Pass so the caller doesn't fall back as if the emitter failed.
    Some(line.steps.iter().find_map(|&s| line_step_action(s, legal)).unwrap_or(LegalAction::Pass))
}

/// `mana value` of a cost string (digits → generic, each W/U/B/R/G/C → 1).
fn cmc_of(cost: &str) -> u32 {
    let (mut n, mut num, mut saw) = (0u32, 0u32, false);
    for ch in cost.trim().chars() {
        if let Some(d) = ch.to_digit(10) {
            num = num * 10 + d;
            saw = true;
            continue;
        }
        if saw {
            n += num;
            num = 0;
            saw = false;
        }
        if matches!(ch, 'W' | 'U' | 'B' | 'R' | 'G' | 'C') {
            n += 1;
        }
    }
    n + if saw { num } else { 0 }
}

/// The cheapest legal noncreature CastSpell that's fuel for the pop — any spell the
/// engine offers here is castable (it has already excluded counterspells with no legal
/// target), excluding Doomsday and the car itself. Cheapest-first maximizes the number
/// of casts we can pay for this turn.
fn cast_cheapest_fuel(state: &SimState, legal: &[LegalAction]) -> Option<LegalAction> {
    legal.iter()
        .filter(|a| match a {
            LegalAction::CastSpell { card_id, .. } => {
                let key = state.objects.get(card_id).map(|o| o.catalog_key.as_str()).unwrap_or("");
                key != DOOMSDAY
                    && key != FANTASTICAR
                    && state.def_of(*card_id).map_or(false, |d| !d.is_land() && !d.is_creature())
            }
            _ => false,
        })
        .min_by_key(|a| match a {
            LegalAction::CastSpell { card_id, .. } => {
                state.def_of(*card_id).map_or(99, |d| cmc_of(d.mana_cost()))
            }
            _ => 99,
        })
        .cloned()
}

/// A legal self-counter cast (Daze / Force of Will) targeting our OWN spell on the
/// stack — the held-priority 4th noncreature spell that pops the car. The engine only
/// offers it when it has a legal on-stack target (via `Who::Any`) and the free
/// alternative cost is payable; `announce` picks that alt-cost (bounce an Island / pitch
/// a blue card) so we never need {1}{U} / {3}{U}{U}.
fn cast_self_counter(state: &SimState, legal: &[LegalAction]) -> Option<LegalAction> {
    legal.iter()
        .find(|a| matches!(a, LegalAction::CastSpell { card_id, .. }
            if state.objects.get(card_id)
                .map(|o| o.catalog_key.as_str())
                .is_some_and(|k| k == DAZE || k == FORCE_OF_WILL)))
        .cloned()
}

/// A legal mana-positive noncreature cast to ramp toward the car's {3}. RITUAL FIRST:
/// a ritual nets more mana per cast, so the car becomes affordable in FEWER noncreature
/// casts — which leaves the free {0} artifacts (Petals/Baubles) to be cast AFTER the car
/// for the 4th-spell trigger. Casting a Petal here is the last resort: spent as ramp it
/// no longer counts as a post-car spell, and that mis-ordering (car ends up the 4th spell,
/// on the stack rather than in play) is exactly what dropped the realized pop rate.
fn cast_ramp(state: &SimState, legal: &[LegalAction]) -> Option<LegalAction> {
    let is_ritual = |id: &ObjId| state.def_of(*id).map_or(false, |d| d.added_mana_on_resolve().is_some());
    let is_petal = |id: &ObjId| state.def_of(*id).map_or(false, |d|
        !d.is_land() && !d.is_creature() && d.mana_cost().trim() == "0" && !d.mana_abilities().is_empty());
    legal.iter()
        .find(|a| matches!(a, LegalAction::CastSpell { card_id, .. } if is_ritual(card_id)))
        .or_else(|| legal.iter().find(|a| matches!(a, LegalAction::CastSpell { card_id, .. } if is_petal(card_id))))
        .cloned()
}


/// Pursue the car with POP DISCIPLINE: develop a land, then spend the car / ramp / pop-fuel
/// ONLY on a turn where a pop actually completes — the car can be cast as the ≤3rd
/// noncreature spell with a cast after it (`recipe::can_pop_this_turn`). On any other turn
/// it CONSERVES the pop fuel (Petals/Baubles/LED/rituals) and digs with cantrips only, then
/// pops next turn with the resources intact. The old "cast the car the instant it's
/// castable" fired it as the doomed 4th spell after a dig cantrip (on the stack, not in
/// play → no trigger) and burned the ramp — the dominant failure-to-fire.
fn pursue_car(state: &SimState, who: PlayerId, legal: &[LegalAction]) -> Option<LegalAction> {
    use CardRole::*;
    // 1) Develop first — a land drop is free progress and the car's {3} needs it.
    if let Some(a) = best_land_drop(state, who, legal, &[BlackLandUntapped, Fetch, BlackLandTapped, BlueSource]) {
        return Some(a);
    }
    if let Some(a) = fetch_activation(state, legal) {
        return Some(a);
    }
    // 2) Pop only if it COMPLETES this turn.
    if recipe::can_pop_this_turn(state, who) {
        let car_in_play = state.permanents_of(who).any(|p| p.catalog_key == FANTASTICAR);
        if car_in_play {
            if let Some(a) = cast_cheapest_fuel(state, legal) {
                return Some(a); // dump fuel — the 4th cast pops it
            }
        } else if let Some(a) = cast_named(state, legal, FANTASTICAR) {
            return Some(a); // affordable now → cast it (guaranteed ≤3rd this turn)
        } else if let Some(a) = cast_ramp(state, legal) {
            return Some(a); // ritual-first ramp toward the {3}
        }
    }
    // 3) Can't pop this turn → CONSERVE the pop fuel; dig with cantrips only.
    if let Some(a) = first_cast(state, who, legal, &[Cantrip]) {
        return Some(a);
    }
    Some(LegalAction::Pass)
}

/// A legal fetch-land activation (sac → search), if any.
fn fetch_activation(state: &SimState, legal: &[LegalAction]) -> Option<LegalAction> {
    legal.iter()
        .find(|a| matches!(a, LegalAction::ActivateAbility { source_id, ability_index }
            if state.def_of(*source_id)
                .and_then(|d| d.abilities().get(*ability_index))
                .map_or(false, |ab| ab.is_fetch_ability())))
        .cloned()
}

// ── Reference value-table policy (A/B comparison ONLY — never drives play) ─────
// The OLD heuristic choices, used solely so compare mode can diff them against the
// principled (objective-driven) policy. See `recipe::dd_card_value`.

fn heur_pick(state: &SimState, who: PlayerId, choices: &[ObjId]) -> Option<ObjId> {
    choices.iter().copied().max_by(|&a, &b| {
        recipe::dd_card_value(state, who, a)
            .partial_cmp(&recipe::dd_card_value(state, who, b))
            .unwrap_or(Ordering::Equal)
    })
}

fn heur_surveil_bin(state: &SimState, who: PlayerId, id: ObjId) -> bool {
    recipe::dd_card_value(state, who, id) < 0.3
}

fn heur_scry_keep(state: &SimState, who: PlayerId, top: &[ObjId]) -> Vec<ObjId> {
    let mut keep: Vec<ObjId> = top.iter().copied()
        .filter(|&id| recipe::dd_card_value(state, who, id) >= 0.3)
        .collect();
    keep.sort_by(|&a, &b| recipe::dd_card_value(state, who, b)
        .partial_cmp(&recipe::dd_card_value(state, who, a)).unwrap_or(Ordering::Equal));
    keep
}

fn heur_bury(state: &SimState, who: PlayerId, count: usize, candidates: &[ObjId]) -> Vec<ObjId> {
    let mut scored: Vec<(ObjId, f64)> = candidates.iter()
        .map(|&id| (id, recipe::dd_card_value(state, who, id)))
        .collect();
    scored.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
    scored.into_iter().take(count).map(|(id, _)| id).collect()
}

fn names(state: &SimState, ids: &[ObjId]) -> String {
    ids.iter().map(|&i| DDGoldfishStrategy::nm(state, i)).collect::<Vec<_>>().join(",")
}

impl Strategy for DDGoldfishStrategy {
    fn player_id(&self) -> PlayerId { self.player_id }

    fn drain_decisions(&mut self) -> Vec<String> { std::mem::take(&mut self.decisions) }

    /// Choose the FREE alternative cost (Daze bounces an Island; Force pitches a blue
    /// card) whenever the printed mana cost can't be paid from current mana — the
    /// self-counter pop path. For every normal cast the mana cost IS payable (the engine
    /// only offers castable spells), so this leaves Doomsday / rituals / petals untouched.
    fn announce(&mut self, state: &SimState, card_id: ObjId, options: &AnnounceOptions) -> AnnounceChoice {
        let who = self.player_id;
        let alt_cost_index = if options.available_alt_costs.is_empty() {
            None
        } else {
            let mana_payable = state.def_of(card_id).map_or(false, |d| {
                !d.mana_cost().is_empty()
                    && state.potential_mana(who).can_pay(&parse_mana_cost(d.mana_cost()))
            });
            if mana_payable { None } else { Some(0) }
        };
        AnnounceChoice { chosen_mode: 0, alt_cost_index, chosen_x: 3 }
    }

    fn declare_attackers(&mut self, _state: &SimState) -> Vec<(ObjId, Option<ObjId>)> { Vec::new() }
    fn declare_blockers(&mut self, _state: &SimState) -> Vec<(ObjId, ObjId)> { Vec::new() }

    fn choose_action(&mut self, state: &SimState, ap: PlayerId,
                     legal: &[LegalAction]) -> LegalAction {
        let who = self.player_id;
        // Goldfish: we act only on our own main phase with an empty stack. We never
        // interact on the opponent's turn or respond to the stack — we just pass to
        // let our own spells (rituals, cantrips) resolve, then keep assembling.
        if ap != who { return LegalAction::Pass; }
        let in_main = matches!(state.current_phase,
            Some(TurnPosition::Phase(PhaseKind::PreCombatMain))
            | Some(TurnPosition::Phase(PhaseKind::PostCombatMain)));
        if !in_main { return LegalAction::Pass; }
        // On a NON-EMPTY stack we hold priority for exactly one reason: to cast the
        // self-counter that lands the FOURTH noncreature spell on our own held spell
        // (car already in play, three casts made). That is the only line that responds
        // to its own stack; every other window passes and lets the stack resolve.
        if !state.stack.is_empty() {
            if self.car_enabled
                && state.permanents_of(who).any(|p| p.catalog_key == FANTASTICAR)
                && state.noncreature_casts_this_turn(who) == 3
                && recipe::can_pop_this_turn(state, who)
            {
                if let Some(a) = cast_self_counter(state, legal) {
                    self.dlog(format!("T{}: self-counter → 4th spell pops the car", state.current_turn));
                    return a;
                }
            }
            return LegalAction::Pass;
        }

        // 1) Cast Doomsday the instant it is castable.
        if let Some(a) = cast_named(state, legal, DOOMSDAY) {
            self.dlog(format!("T{}: cast Doomsday", state.current_turn));
            return a;
        }
        // 1b) Two-wincon: commit to The Fantasticar as a first-class plan when we hold a
        // car and Doomsday is NOT a ready, faster line (or the car is already in play).
        // The car line is STOCHASTIC — we dig toward it with cantrips + land drops while
        // conserving the pop fuel — so we do NOT gate on a deterministic car line already
        // existing; `pursue_car` digs until it assembles, then pops.
        let det = recipe::deterministic_cast_turn(state, who, self.cutoff.max(1));
        if self.car_enabled {
            let car = recipe::car_pop_turn(state, who, self.cutoff.max(1));
            let car_in_play = state.permanents_of(who).any(|p| p.catalog_key == FANTASTICAR);
            let car_in_hand = state.hand_of(who).any(|c| c.catalog_key == FANTASTICAR);
            // Pick the plan that maximises P(send), re-evaluated every window:
            //   - car already in play → finish it;
            //   - both lines guaranteed → take the faster one;
            //   - exactly one guaranteed → take it;
            //   - neither guaranteed → take whichever is stochastically more likely.
            // This subsumes "Doomsday-first, car-backup": we develop/dig toward whichever
            // combo the objective favours, and switch as new draws shift it.
            let prefer_car = match (car, det) {
                (Some(c), Some(d)) => c <= d,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => {
                    recipe::p_car_pop_by(state, who, self.cutoff.max(1))
                        >= recipe::p_cast_by(state, who, self.cutoff.max(1))
                }
            };
            if car_in_play || (car_in_hand && prefer_car) {
                self.dlog(format!("T{}: car plan (car-ttd={:?}, dd-ttd={:?}, out={})",
                    state.current_turn, car, det, car_in_play));
                if let Some(a) = pursue_car(state, who, legal) {
                    return a;
                }
            }
        }
        // 2) SETTLE vs GAMBLE — keyed on the solver's det-ttd / min-ttd (no thresholds).
        let mn = recipe::min_ttd(state, who, self.cutoff.max(1));
        if det.is_some() {
            // A GUARANTEED line lands by the cutoff → SETTLE: execute it, and ONLY it.
            // We do NOT dig here — digging could spend a resource the line needs (e.g.
            // sac a petal to cantrip, then whiff and fail to cast). If there's nothing
            // to assemble this window, pass and let the line mature next turn.
            self.dlog(format!("T{}: settle (det-ttd={:?}, min-ttd={:?})", state.current_turn, det, mn));
            // FOLLOW the solved line — execute its next legal step, nothing else.
            if let Some(a) = follow_line(state, who, legal, self.cutoff.max(1)) {
                return a;
            }
            // Emitter gap (an unmodelled source): log it so we can measure how often
            // the follow-the-line path is incomplete, then fall back.
            self.dlog(format!("T{}: settle FALLBACK — line emitter gap", state.current_turn));
            return self.assemble_step(state, who, legal).unwrap_or(LegalAction::Pass);
        }
        // 3) No guaranteed line by the cutoff → GAMBLE. Per the policy: if even the
        //    optimistic min-ttd is > cutoff (no possible win with the current cards),
        //    FILTER to actions that can pull min-ttd back under — first rip a fetch (it
        //    shuffles AND fetches a black source). There's no by-cutoff line to
        //    cannibalize, so this is free. Otherwise develop + dig to optimize E(ttd)
        //    toward the live optimistic line.
        if mn.is_none() {
            if let Some(a) = fetch_activation(state, legal) {
                self.dlog(format!("T{}: min-ttd>cutoff → rip fetch to lower it", state.current_turn));
                return a;
            }
        }
        if let Some(a) = self.develop_and_dig(state, who, legal) {
            self.dlog(format!("T{}: gamble (min-ttd={:?})", state.current_turn, mn));
            return a;
        }
        // 4) No by-cutoff line and nothing left to dig — follow a slower deterministic
        //    line if one exists (better late than never).
        if let Some(a) = follow_line(state, who, legal, FALLBACK_HORIZON) {
            return a;
        }
        LegalAction::Pass
    }

    fn take_mulligan(&mut self, state: &SimState, mulligans_taken: u32) -> bool {
        let who = self.player_id;
        let p = recipe::p_send_by(state, who, self.cutoff);
        // The keep/mull decision is the selected mulligan mode's (Keep7 / Realistic /
        // Aggressive — see `super::mull`). `p` is computed only for the logging below.
        let mull = super::mull::should_mulligan(self.mull_mode, state, who, self.cutoff, mulligans_taken);
        if !mull {
            // Calibration probe: the kept hand's predicted P(cast by cutoff), to be
            // compared against the realized outcome (see `run_goldfish_calibration`).
            self.dlog(format!("CALIB {:.4}", p));
            let det = recipe::deterministic_cast_turn(state, who, self.cutoff);
            let hand: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
            self.dlog(format!("KEPT det={:?} [{}]", det, names(state, &hand)));
            // Machine-readable per-game summary for the aggregate stats (mull level,
            // predicted P, and whether the opening hand already had a deterministic
            // line by the cutoff).
            self.dlog(format!("STATS mull={} pred={:.4} det={}",
                mulligans_taken, p, det.is_some() as u8));
        }
        self.dlog(format!("T0: mull#{} P(cast by {})={:.2} → {}",
            mulligans_taken, self.cutoff, p, if mull { "MULL" } else { "KEEP" }));
        if self.compare {
            // Reference: the baseline DoomsdayStrategy's category-rule mulligan.
            let heur_mull = crate::dd_should_mulligan(state, who, mulligans_taken);
            if heur_mull != mull {
                let hand: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
                self.dlog(format!(
                    "DIFF mulligan#{}: mode={} (P={:.2}) heuristic={} hand=[{}]",
                    mulligans_taken,
                    if mull { "MULL" } else { "KEEP" }, p,
                    if heur_mull { "MULL" } else { "KEEP" },
                    names(state, &hand)));
            }
        }
        mull
    }

    // ── Cantrip / selection resolution: driven by the solver ──────────────────

    /// Ponder (and any "put back in any order, then maybe shuffle"): the keep-vs-
    /// shuffle + ordering decision falls out of `P(cast by cutoff)`.
    fn order_top_library(&mut self, cards: &[ObjId], state: &SimState) -> Vec<ObjId> {
        let who = self.player_id;
        let revealed: Vec<String> = cards.iter()
            .filter_map(|id| state.objects.get(id).map(|c| c.catalog_key.clone()))
            .collect();
        // Ponder offers a shuffle (a `MayDo` follows this `OrderTop`). The DIG line carries a
        // card-free reason plus a ` ‖ `-delimited list of per-card fate tokens (see/top/…) that
        // the UI renders as chips; keep the card names OUT of the reason (they're in the chips).
        let (choice, p) = recipe::best_top_choice(state, who, &revealed, true, self.cutoff);
        match choice {
            recipe::TopChoice::Shuffle => {
                self.pending_shuffle = true;
                let toks: Vec<String> = revealed.iter().map(|r| format!("see:{r}"))
                    .chain(std::iter::once("shuffle".to_string())).collect();
                self.dig(state, format!("T{} · Ponder: the top scored below a fresh draw, so shuffled it away (P {:.0}%) ‖ {}",
                    state.current_turn, p * 100.0, toks.join(";")));
                cards.to_vec() // order is irrelevant — we'll shuffle it away
            }
            recipe::TopChoice::Keep(order_keys) => {
                self.pending_shuffle = false;
                let toks: Vec<String> = order_keys.iter().map(|k| format!("top:{k}")).collect();
                self.dig(state, format!("T{} · Ponder: kept the top in this order to draw into (P {:.0}%) ‖ {}",
                    state.current_turn, p * 100.0, toks.join(";")));
                // Map the chosen key ordering back onto the actual card ids.
                let mut remaining: Vec<ObjId> = cards.to_vec();
                let mut out: Vec<ObjId> = Vec::new();
                for key in &order_keys {
                    if let Some(pos) = remaining.iter().position(|id|
                        state.objects.get(id).map_or(false, |c| &c.catalog_key == key))
                    {
                        out.push(remaining.remove(pos));
                    }
                }
                out.extend(remaining); // safety: append anything unmatched
                out
            }
        }
    }

    /// The only "may" we drive is Ponder's shuffle, flagged by `order_top_library`.
    fn resolve_choice(&mut self, _source: ObjId, req: &ChoiceRequest, _state: &SimState) -> ChoiceResult {
        match req {
            ChoiceRequest::Mode(_) => {
                let yes = self.pending_shuffle;
                self.pending_shuffle = false;
                ChoiceResult::Mode(if yes { 1 } else { 0 })
            }
            ChoiceRequest::Color => ChoiceResult::Color(Color::Black),
            ChoiceRequest::CreatureType => ChoiceResult::CreatureType("Wizard".to_string()),
            ChoiceRequest::CardName => ChoiceResult::CardName(String::new()),
            ChoiceRequest::WardPayment { .. } => ChoiceResult::Bool(true),
            ChoiceRequest::MayPutOnBattlefield { .. } => ChoiceResult::OptionalObject(None),
            ChoiceRequest::MayAttach => ChoiceResult::Bool(true),
        }
    }

    /// Searches and digs, by source:
    /// - **fetch land** → pick a black source, untapped-preferred (a land ordering —
    ///   the allowed heuristic; the *colour* requirement comes from the goal).
    /// - **tutor to top** (Personal Tutor) → the candidate maximizing P(cast by
    ///   cutoff) with it staged on top (drawn next turn) — i.e. Doomsday.
    /// - **dig to hand** (Flow State) → the candidate maximizing P(cast by cutoff)
    ///   with it added to hand now.
    fn choose_for_effect(&mut self, effect_id: ObjId, choices: &[ObjId], state: &SimState) -> Option<ObjId> {
        if choices.is_empty() { return None; }
        let who = self.player_id;
        let src_def = state.def_of(effect_id)
            .or_else(|| state.objects.get(&effect_id).and_then(|o| state.catalog.get(&o.catalog_key)));
        let is_fetch = src_def.map_or(false, |d| d.abilities().iter().any(|a| a.is_fetch_ability()));
        let to_top = src_def.map_or(false, |d| d.library_top_tutor().is_some());
        let principled = if is_fetch {
            // A fetched land enters now (like a dig to hand): pick the candidate that
            // most advances the objective, NOT merely "a black source". This prefers a
            // dual that also supplies the colour the line needs next (e.g. blue for the
            // tutor's pip) over a same-priority off-colour land (Underground Sea vs a
            // plain Swamp both rank as untapped-black, but only the Sea pays the pip).
            // Ties (objective-indifferent) break on land priority — untapped first.
            choices.iter().copied().max_by(|&a, &b| {
                self.candidate_p(state, a, false)
                    .partial_cmp(&self.candidate_p(state, b, false))
                    .unwrap_or(Ordering::Equal)
                    // Tie-break on min-ttd (lower = faster): separates a Sea (pays the pip
                    // sooner) from a same-priority Swamp where P(cast) saturates equal.
                    .then_with(|| self.candidate_ttd(state, b, false)
                        .partial_cmp(&self.candidate_ttd(state, a, false)).unwrap_or(Ordering::Equal))
                    .then_with(|| land_priority(recipe::card_role(state, who, a))
                        .cmp(&land_priority(recipe::card_role(state, who, b))))
            })
        } else {
            // Tutor stages on top (drawn next turn); a dig goes to hand now. Primary key
            // is the objective P(cast by cutoff); the min-ttd tie-break then separates the
            // by-cutoff lines that probability saturates equal — staging Doomsday (payoff
            // next turn) beats staging a tutor for it (payoff a turn later), for free.
            choices.iter().copied().max_by(|&a, &b| {
                self.candidate_p(state, a, to_top)
                    .partial_cmp(&self.candidate_p(state, b, to_top))
                    .unwrap_or(Ordering::Equal)
                    .then_with(|| self.candidate_ttd(state, b, to_top)
                        .partial_cmp(&self.candidate_ttd(state, a, to_top)).unwrap_or(Ordering::Equal))
                    // Last resort (tight mana, both lines stochastic): the payoff beats a
                    // tutor for it, so we never loop tutor→tutor.
                    .then_with(|| payoff_rank(state, who, a).cmp(&payoff_rank(state, who, b)))
                    .then_with(|| keep_rank(state, who, a).cmp(&keep_rank(state, who, b)))
            })
        };
        if let Some(pick) = principled {
            let name = DDGoldfishStrategy::nm(state, pick);
            let (reason, tok) = if is_fetch {
                ("fetched the source the next spell needs", format!("play:{name}"))
            } else if to_top {
                ("tutored the payoff onto the top", format!("top:{name}"))
            } else {
                ("dug the best card to hand", format!("draw:{name}"))
            };
            self.dig(state, format!("T{} · {} ‖ {}", state.current_turn, reason, tok));
        }
        if self.compare {
            let heur = heur_pick(state, who, choices);
            if heur != principled {
                let kind = if is_fetch { "fetch" } else { "dig/tutor" };
                self.dlog(format!("DIFF {} T{}: principled={} heuristic={} from [{}]",
                    kind, state.current_turn,
                    principled.map(|i| DDGoldfishStrategy::nm(state, i)).unwrap_or_default(),
                    heur.map(|i| DDGoldfishStrategy::nm(state, i)).unwrap_or_default(),
                    names(state, choices)));
            }
        }
        principled
    }

    /// Consider's surveil = the unified policy on {keep-on-top, bin}: maximize
    /// P(cast by cutoff) (the E(ttd) proxy), which already subsumes the min-ttd
    /// feasibility filter — an infeasible keep scores P = 0, and a card that's merely
    /// useless still wastes Consider's draw, scoring below a fresh unknown draw → bin.
    /// (So "minimum above cutoff on the keep → bin" is the P = 0 corner of this rule.)
    fn surveil_choice(&mut self, id: ObjId, state: &SimState) -> bool {
        let who = self.player_id;
        let Some(key) = state.objects.get(&id).map(|o| o.catalog_key.clone()) else { return false };
        let keep_p = recipe::p_send_by_with_known_top(state, who, &[key.as_str()], true, self.cutoff);
        let base_p = recipe::p_send_by(state, who, self.cutoff);
        let principled_bin = keep_p < base_p; // a known keep worse than an unknown draw → bin
        // NOTE: this is the generic surveil handler — it fires for Consider AND for surveil
        // lands (Undercity Sewers), and isn't told which. So label it "surveil", not "Consider".
        // The decision is MARGINAL — keep iff P(send | this card on top) ≥ P(send | a fresh
        // draw); lead with that gap (the two absolute rates often round to the same %).
        let (kp, dp) = (keep_p * 100.0, base_p * 100.0);
        self.dig(state, if principled_bin {
            format!("T{} · surveil: a fresh draw wins by {:.1}pp ({:.1}% vs keeping {:.1}%) ‖ gy:{}",
                state.current_turn, dp - kp, dp, kp, key)
        } else if kp - dp < 0.05 {
            format!("T{} · surveil: about even with a fresh draw (≈{:.1}%), kept by default ‖ top:{}",
                state.current_turn, kp, key)
        } else {
            format!("T{} · surveil: keeping it beats a fresh draw by {:.1}pp ({:.1}% vs {:.1}%) ‖ top:{}",
                state.current_turn, kp - dp, kp, dp, key)
        });
        if self.compare {
            let heur_bin = heur_surveil_bin(state, who, id);
            if heur_bin != principled_bin {
                self.dlog(format!("DIFF surveil T{} {}: principled={} heuristic={} (keep_p={:.3} base_p={:.3})",
                    state.current_turn, key,
                    if principled_bin { "BIN" } else { "KEEP" },
                    if heur_bin { "BIN" } else { "KEEP" }, keep_p, base_p));
            }
        }
        principled_bin
    }

    /// Preordain's scry: choose the kept-on-top arrangement (rest to bottom) that
    /// maximizes P(cast by cutoff) — enumerated over the (≤2-card) reveal.
    fn scry(&mut self, top: &[ObjId], state: &SimState) -> (Vec<ObjId>, Vec<ObjId>) {
        let who = self.player_id;
        let keyed: Vec<(ObjId, String)> = top.iter()
            .filter_map(|&id| state.objects.get(&id).map(|o| (id, o.catalog_key.clone())))
            .collect();
        let mut best_keep: Vec<ObjId> = Vec::new();
        let mut best_p = recipe::p_send_by(state, who, self.cutoff); // keep nothing on top
        for arrangement in ordered_subsets(&keyed) {
            let keys: Vec<&str> = arrangement.iter().map(|(_, k)| k.as_str()).collect();
            let p = recipe::p_send_by_with_known_top(state, who, &keys, true, self.cutoff);
            if p > best_p {
                best_p = p;
                best_keep = arrangement.iter().map(|(id, _)| *id).collect();
            }
        }
        let bottom: Vec<ObjId> = top.iter().copied().filter(|id| !best_keep.contains(id)).collect();
        let toks: Vec<String> = best_keep.iter().map(|&id| format!("top:{}", DDGoldfishStrategy::nm(state, id)))
            .chain(bottom.iter().map(|&id| format!("bot:{}", DDGoldfishStrategy::nm(state, id)))).collect();
        self.dig(state, format!("T{} · scry: kept the highest-scoring arrangement on top, rest to the bottom (P {:.0}%) ‖ {}",
            state.current_turn, best_p * 100.0, toks.join(";")));
        if self.compare {
            let heur_keep = heur_scry_keep(state, who, top);
            if heur_keep != best_keep {
                self.dlog(format!("DIFF scry T{}: principled_keep=[{}] heuristic_keep=[{}] of [{}]",
                    state.current_turn, names(state, &best_keep), names(state, &heur_keep), names(state, top)));
            }
        }
        (best_keep, bottom)
    }

    /// Brainstorm's put-back: bury the cards the plan does not need. INTERIM — driven
    /// by the solver's gap (`mana_gap` + `payoff_in_hand`), the recomputed "sufficient
    /// cards" set, NOT a value table. ("Which cards can I afford to lose" is exactly
    /// what the visible-path requirement set will answer fully.)
    fn put_on_library(&mut self, count: usize, candidates: &[ObjId], _top: bool,
                      state: &SimState) -> Vec<ObjId> {
        let who = self.player_id;
        // Keep the combo pieces the plan needs (`card_needed`); among the rest
        // (expendable), the last-ditch trinary weight buries irrelevant bricks before
        // relevant cards before cantrips.
        let mut expendable: Vec<ObjId> = candidates.iter().copied()
            .filter(|&id| !self.card_needed(state, id))
            .collect();
        expendable.sort_by_key(|&id| keep_rank(state, who, id));
        let mut buried: Vec<ObjId> = expendable.into_iter().take(count).collect();
        // If too few are expendable, we must still bury `count` — pad with the
        // lowest-keep-rank of the remaining (needed) cards.
        if buried.len() < count {
            let mut rest: Vec<ObjId> = candidates.iter().copied().filter(|id| !buried.contains(id)).collect();
            rest.sort_by_key(|&id| keep_rank(state, who, id));
            for id in rest {
                if buried.len() >= count { break; }
                buried.push(id);
            }
        }
        buried.truncate(count);
        let toks: Vec<String> = buried.iter().map(|&id| format!("top:{}", DDGoldfishStrategy::nm(state, id))).collect();
        self.dig(state, format!("T{} · Brainstorm: put back the cards the line needs least, kept the rest ‖ {}",
            state.current_turn, toks.join(";")));
        if self.compare {
            let heur = heur_bury(state, self.player_id, count, candidates);
            let pa: std::collections::HashSet<ObjId> = buried.iter().copied().collect();
            let hb: std::collections::HashSet<ObjId> = heur.iter().copied().collect();
            if pa != hb {
                self.dlog(format!("DIFF brainstorm-bury T{}: principled=[{}] heuristic=[{}] of [{}]",
                    state.current_turn, names(state, &buried), names(state, &heur), names(state, candidates)));
            }
        }
        buried
    }

    // ── Required trait methods ─────────────────────────────────────────────────

    fn plan_gap(&self, _state: &SimState) -> TargetGap { TargetGap::default() }

    /// London-mulligan bottoming (a mulligan decision — an allowed exception):
    /// bottom the cards the plan doesn't need (`card_needed`, solver-derived); pad
    /// with extras if everything is needed.
    fn london_bottom(&self, state: &SimState, n: usize) -> Vec<ObjId> {
        let who = self.player_id;
        use super::mull::MullMode;
        // Learned policies: keep the highest-scoring (7-n)-card subset; bottom the rest. This is
        // what makes the Interactive mode preserve protection instead of shipping it.
        if let MullMode::LearnedSpeed | MullMode::LearnedInteractive = self.mull_mode {
            use super::learned_mull::LearnedObjective;
            let cards: Vec<(ObjId, &str)> =
                state.hand_of(who).map(|c| (c.id, c.catalog_key.as_str())).collect();
            let names: Vec<&str> = cards.iter().map(|&(_, nm)| nm).collect();
            let obj = if self.mull_mode == MullMode::LearnedSpeed {
                LearnedObjective::Speed
            } else {
                LearnedObjective::Interactive
            };
            return super::learned_mull::learned_bottom(&names, n as u32, state.on_play, obj)
                .iter()
                .map(|&i| cards[i].0)
                .collect();
        }
        let hand: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
        let mut bottom: Vec<ObjId> = hand.iter().copied().filter(|&id| !self.card_needed(state, id)).collect();
        if bottom.len() < n {
            for &id in &hand {
                if bottom.len() >= n { break; }
                if !bottom.contains(&id) { bottom.push(id); }
            }
        }
        bottom.truncate(n);
        bottom
    }

    fn card_fills(&self, _card_id: ObjId, _gap: &TargetGap, _state: &SimState) -> f64 { 0.0 }
}

// ── AggroMullStrategy: experiment wrapper (baseline gameplay + aggressive mull) ──
//
// Completes the 2×2 (gameplay × mulligan): runs an inner strategy's gameplay
// verbatim but swaps in the aggressive `p_cast_by`-threshold mulligan, to measure
// how much of the ASAP edge is the mulligan vs the in-game play. Debug-only.

pub struct AggroMullStrategy {
    inner: Box<dyn Strategy>,
    cutoff: u32,
}

impl AggroMullStrategy {
    pub fn new(inner: Box<dyn Strategy>, cutoff: u32) -> Self {
        Self { inner, cutoff: cutoff.max(1) }
    }
}

impl Strategy for AggroMullStrategy {
    // The one override: the aggressive P(cast by cutoff) threshold mulligan.
    fn take_mulligan(&mut self, state: &SimState, mulligans_taken: u32) -> bool {
        let who = self.inner.player_id();
        let p = recipe::p_cast_by(state, who, self.cutoff);
        let threshold = match mulligans_taken { 0 => 0.55, 1 => 0.38, _ => 0.20 };
        mulligans_taken < 3 && p < threshold
    }

    // Forward exactly the methods DoomsdayStrategy overrides — its gameplay verbatim.
    // Everything else falls through to the trait default, which is precisely what
    // DoomsdayStrategy uses for those, so the wrapper is identical there.
    fn choose_action(&mut self, s: &SimState, ap: PlayerId, l: &[LegalAction]) -> LegalAction { self.inner.choose_action(s, ap, l) }
    fn choose_mana_ability(&mut self, s: &SimState, w: PlayerId, a: &[ManaAbilityOption], m: &ManaCost) -> Option<ManaActivation> { self.inner.choose_mana_ability(s, w, a, m) }
    fn announce(&mut self, s: &SimState, c: ObjId, o: &AnnounceOptions) -> AnnounceChoice { self.inner.announce(s, c, o) }
    fn declare_attackers(&mut self, s: &SimState) -> Vec<(ObjId, Option<ObjId>)> { self.inner.declare_attackers(s) }
    fn declare_blockers(&mut self, s: &SimState) -> Vec<(ObjId, ObjId)> { self.inner.declare_blockers(s) }
    fn player_id(&self) -> PlayerId { self.inner.player_id() }
    fn plan_gap(&self, s: &SimState) -> TargetGap { self.inner.plan_gap(s) }
    fn card_fills(&self, c: ObjId, g: &TargetGap, s: &SimState) -> f64 { self.inner.card_fills(c, g, s) }
    fn drain_decisions(&mut self) -> Vec<String> { self.inner.drain_decisions() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libmtg_engine::{build_catalog, PlayerState, Zone};

    /// Personal Tutor's filter is "a sorcery", and a Doomsday deck also runs other
    /// sorceries (Thoughtseize, Edge of Autumn). The strategy MUST stage Doomsday (the
    /// payoff) — staging a brick on top wastes the whole tutor and the draw.
    #[test]
    fn personal_tutor_stages_doomsday_not_another_sorcery() {
        let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        s.catalog = build_catalog();
        // Three blue/black sources in play so the staged-Doomsday line is castable.
        for _ in 0..3 {
            s.place_card(PlayerId::Us, "Underground Sea", Zone::Battlefield);
        }
        let pt = s.place_card(PlayerId::Us, "Personal Tutor", Zone::Graveyard);
        let dd = s.place_card(PlayerId::Us, "Doomsday", Zone::Library);
        let th = s.place_card(PlayerId::Us, "Thoughtseize", Zone::Library);
        let edge = s.place_card(PlayerId::Us, "Edge of Autumn", Zone::Library);

        let mut strat = DDGoldfishStrategy::new(4);
        let pick = strat.choose_for_effect(pt, &[dd, th, edge], &s);
        assert_eq!(pick, Some(dd), "Personal Tutor must stage Doomsday, not a brick sorcery");
    }

    /// With a 4-Personal-Tutor build, PT's candidates often include BOTH Doomsday and
    /// another Personal Tutor. With tight mana (no deterministic line), the stochastic
    /// model scores staging either as "1 useful card" — a tie — so a naive `max_by`
    /// could stage the redundant tutor (looping tutor→tutor, never casting). The payoff
    /// must dominate: staging Doomsday skips the tutor's extra cast.
    #[test]
    fn personal_tutor_stages_doomsday_over_a_redundant_tutor() {
        let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        s.catalog = build_catalog();
        // One untapped Sea: enough to cast a tutor, NOT enough for a deterministic BBB
        // line — so staging DD vs another PT ties on the stochastic estimate.
        s.place_card(PlayerId::Us, "Underground Sea", Zone::Battlefield);
        let pt = s.place_card(PlayerId::Us, "Personal Tutor", Zone::Graveyard);
        let dd = s.place_card(PlayerId::Us, "Doomsday", Zone::Library);
        let other_pt = s.place_card(PlayerId::Us, "Personal Tutor", Zone::Library);

        let mut strat = DDGoldfishStrategy::new(4);
        // Doomsday FIRST, the tutor LAST — the order that made a bare `max_by` pick the
        // tutor (it returns the last of equal elements).
        let pick = strat.choose_for_effect(pt, &[dd, other_pt], &s);
        assert_eq!(pick, Some(dd), "must stage the payoff, never loop on a redundant tutor");
    }
}
