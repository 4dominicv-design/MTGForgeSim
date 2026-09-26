//! SPIKE — validate the resource model on a real (UB tempo) list.
//!
//! The model:
//! resources are *legal actions* gated by `Object(Filter)` + fungible `Mana`;
//! actions chain by lining up produces↔consumes; we reason **backward** from the
//! goal `BBB + Doomsday-in-hand`, so choices are bound by the subgoal (no
//! heuristics) and the unsatisfied frontier *is* the gap.
//!
//! This is a SPIKE, not the production engine. Faithful simplifications, all
//! flagged, that the real version removes:
//!   1. The search works over **capability counts** extracted from card IR, not
//!      `obj_matches` grounding over individual object instances.
//!   2. Mana is tracked as a **black count**, not full per-colour `ManaPool`
//!      arithmetic — exact for the all-black DD goal (`BBB`, Dark Ritual's `B`).
//!   3. A ritual is "needs a 1-black seed → yields 3" (Dark Ritual shape), read
//!      from `added_mana_on_resolve`; richer ritual costs aren't modelled.
//!   4. Fetch type-matching is assumed: a fetch reaches black iff the deck has
//!      an untapped black land. Exact for this manabase; target depletion isn't
//!      counted. The real version grounds the fetch's search filter against the
//!      library via `obj_matches`.
//!
//! Even so, every capability below is *read structurally from IR* (never a card
//! name, except the payoff's identity), and the chaining is the real backward
//! production graph — so this exercises the model, not a parallel re-derivation.

use libmtg_engine::{
    obj_matches, ActivationTiming, CardDef, Color, ObjId, PlayerId, SimState, SourceZone,
};

const DOOMSDAY: &str = "Doomsday";
const FANTASTICAR: &str = "The Fantasticar";
const DAZE: &str = "Daze";
const FORCE_OF_WILL: &str = "Force of Will";

// ── Capability projection (read from IR via the existing accessors) ──────────

/// Has a battlefield mana ability that can *unconditionally* produce black
/// (lands, Lotus Petal on the battlefield, moxen). `Default` timing excludes LED
/// (`Instant`-timed). Conditional mana is skipped — e.g. Cavern of Souls' colored
/// mana is gated on casting a creature spell (`ma.condition`), so it can't pay
/// for Doomsday. SPIKE: this conservatively drops *all* conditional sources;
/// the production version evaluates the condition (board-state gates like
/// metalcraft can still qualify, casting-spell gates like Cavern's don't).
fn produces_color(def: &CardDef, color: Color) -> bool {
    def.mana_abilities().iter().any(|ma| {
        matches!(ma.source_zone, SourceZone::Battlefield)
            && ma.timing == ActivationTiming::Default
            && ma.condition.is_none()
            && ma.produces.contains(&color)
    })
}

/// Specialization for the Doomsday goal's black requirement.
fn produces_black(def: &CardDef) -> bool {
    produces_color(def, Color::Black)
}

/// Produces `color` via a RENEWABLE battlefield mana ability — one whose cost
/// TAPS (does not sacrifice) the source. The untapped-state the cost consumes is
/// restored by every untap step, so the source is available again EACH turn
/// (lands, moxen). This is the cost-level half of the sac/tap distinction.
fn renewable_produces_color(def: &CardDef, color: Color) -> bool {
    def.mana_abilities().iter().any(|ma| {
        matches!(ma.source_zone, SourceZone::Battlefield)
            && ma.timing == ActivationTiming::Default
            && ma.condition.is_none()
            && ma.produces.contains(&color)
            && !ma.costs.requires_sac_self()
    })
}

/// Produces `color` via a ONE-SHOT battlefield mana ability — one whose cost
/// SACRIFICES the source (Lotus Petal). The cost consumes the object itself, which
/// no untap restores, so the source is spent exactly once across the whole plan.
fn oneshot_produces_color(def: &CardDef, color: Color) -> bool {
    def.mana_abilities().iter().any(|ma| {
        matches!(ma.source_zone, SourceZone::Battlefield)
            && ma.timing == ActivationTiming::Default
            && ma.condition.is_none()
            && ma.produces.contains(&color)
            && ma.costs.requires_sac_self()
    })
}

/// A spell that adds black on resolution (a "ritual", generically — Dark Ritual,
/// not by name). Read from the structured `added_mana_on_resolve`.
fn is_black_ritual(def: &CardDef) -> bool {
    match def.added_mana_on_resolve() {
        Some(out) => out.colors.as_ref().map_or(true, |cs| cs.contains(&Color::Black)),
        None => false,
    }
}

/// A ritual's GENERIC pip count (the `{N}` in its cost), clamped to the `ritual_gp`
/// histogram width. Assumes one black seed pip (every modelled ritual is `{…}{B}`);
/// any non-black colored pips are approximated as generic (rare for rituals). So
/// Dark Ritual `{B}` → 0, Cabal Ritual `{1}{B}` → 1.
fn ritual_generic_cost(def: &CardDef) -> usize {
    let (mut generic, mut other_colored, mut num) = (0usize, 0usize, 0usize);
    for ch in def.mana_cost().trim().chars() {
        if let Some(d) = ch.to_digit(10) {
            num = num * 10 + d as usize;
            continue;
        }
        generic += std::mem::take(&mut num);
        if matches!(ch, 'W' | 'U' | 'R' | 'G' | 'C') {
            other_colored += 1;
        }
    }
    (generic + num + other_colored).min(3)
}

/// A renewable mana source that makes some NON-black mana (and no black) — e.g.
/// Island. It can pay a ritual's generic pip but never a black pip.
fn produces_nonblack_mana(def: &CardDef) -> bool {
    !produces_black(def)
        && def.mana_abilities().iter().any(|ma| {
            matches!(ma.source_zone, SourceZone::Battlefield)
                && ma.timing == ActivationTiming::Default
                && ma.condition.is_none()
                && !ma.costs.requires_sac_self()
        })
}

/// A free (0-mana) artifact that taps/sacs for `color` — Lotus Petal, generically.
/// LED is excluded: its mana ability is `Instant`-timed, so `produces_color` is false.
fn is_free_artifact_of_color(def: &CardDef, color: Color) -> bool {
    !def.is_land() && def.mana_cost().trim() == "0" && produces_color(def, color)
}

fn is_free_black_artifact(def: &CardDef) -> bool {
    is_free_artifact_of_color(def, Color::Black)
}

fn is_fetch(def: &CardDef) -> bool {
    def.is_land() && def.abilities().iter().any(|a| a.is_fetch_ability())
}

// ── The available black-producing capabilities, projected from a state ───────
//
// The model is the resources → costs → effects → resources cycle. A mana source
// is a resource; spending it pays a COST that consumes a resource, and what the
// cost consumes is the whole sac/tap story:
//   • a TAP cost consumes the source's *untapped-state* — regenerated every untap
//     step, so the source is RENEWABLE (available again next turn);
//   • a SAC cost consumes the *object itself* — nothing restores it, so it's
//     ONE-SHOT (spent once across the entire plan).
// So renewable capacity is tracked per-turn (it comes back), while one-shot
// sources are a shared `pool` that depletes as costs consume it.

/// The resource frontier toward `BBB`, separated by what a source's cost consumes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Caps {
    /// RENEWABLE black capacity already online: tap sources in play (lands, moxen)
    /// + in-play fetches (each cracks into a renewable land). The untapped-state
    /// these spend is restored every untap step → available every turn.
    renew_inplay: u32,
    /// Renewable black tap-lands in hand that enter UNTAPPED (Sea/Swamp,
    /// fetch→untapped): a land drop turns them into renewable capacity, usable the
    /// turn played.
    untapped_lands: u32,
    /// Renewable black tap-lands in hand that enter TAPPED (surveil duals): usable
    /// the turn AFTER played — they untap on the next PASS.
    tapped_lands: u32,
    /// ONE-SHOT any-colour mana objects (Lotus Petal — sac cost): a shared pool
    /// whose members are CONSUMED when spent and never restored. Spending one to
    /// pay a pip removes it from BBB; this is the cost cycle, not a special case.
    pool: u32,
    /// One-shot ritual spells bucketed by GENERIC pip count (index = `{N}` in the
    /// cost, assuming a single black seed pip): index 0 = Dark Ritual `{B}`, index 1
    /// = Cabal Ritual `{1}{B}`. Each yields 3 black; generic pips draw from any mana.
    ritual_gp: [u32; 4],
    /// NON-black renewable mana (lands that don't make black — e.g. Island): pays a
    /// ritual's generic pip but never a black pip. Split like the black lands;
    /// `other_inplay` is already on the battlefield.
    other_untapped: u32,
    other_tapped: u32,
    other_inplay: u32,
    /// Is the once-per-turn land drop still available THIS turn?
    land_drop: bool,
}

fn library_has_target_color(state: &SimState, who: PlayerId, color: Color) -> bool {
    state.library_of(who).any(|c| {
        state
            .catalog
            .get(&c.catalog_key)
            .is_some_and(|d| d.is_land() && produces_color(d, color) && !d.enters_tapped())
    })
}

fn library_has_black_target(state: &SimState, who: PlayerId) -> bool {
    library_has_target_color(state, who, Color::Black)
}

/// Depth (0-based, 0 = next draw) of Doomsday within the KNOWN top of the library,
/// if it's there. Reads only `known_top_len` cards — legitimately known to the
/// controller (set by our own tutor / scry / ordering, reset on shuffle), and 0 in a
/// fresh opening hand, so this is not hidden information.
fn payoff_known_depth(state: &SimState, who: PlayerId) -> Option<u32> {
    let known = state.player(who).known_top_len;
    state
        .library_of(who)
        .take(known)
        .position(|c| c.catalog_key == DOOMSDAY)
        .map(|p| p as u32)
}

fn project(state: &SimState, who: PlayerId) -> Caps {
    project_inner(state, who, true)
}

/// Like [`project`], but `count_fetches=false` EXCLUDES fetchlands from the mana
/// capabilities. Used for a line that must not shuffle — e.g. when a known card is
/// staged on top of the library, cracking a fetch would scatter it, so that mana is
/// not available to that line.
fn project_inner(state: &SimState, who: PlayerId, count_fetches: bool) -> Caps {
    let mut c = Caps {
        land_drop: state.player(who).lands_played_this_turn < 1,
        ..Default::default()
    };
    let fetch_target = library_has_black_target(state, who);
    let use_fetch = |def: &CardDef| count_fetches && is_fetch(def) && fetch_target;

    // In play: classify each untapped black source by what its cost consumes —
    // one-shot (sac → pool) vs renewable (tap, or a fetch that becomes a land).
    for perm in state.permanents_of(who) {
        if perm.bf().is_some_and(|bf| bf.tapped) {
            continue;
        }
        let Some(def) = state.def_of(perm.id) else { continue };
        if oneshot_produces_color(def, Color::Black) {
            c.pool += 1;
        } else if renewable_produces_color(def, Color::Black) || use_fetch(def) {
            c.renew_inplay += 1;
        } else if produces_nonblack_mana(def) {
            c.other_inplay += 1;
        }
    }

    // Hand: free mana artifacts (cast for 0 this turn, then used — sac→pool,
    // tap→renewable like a Mox), black tap-lands (land-drop gated), rituals.
    for card in state.hand_of(who) {
        let Some(def) = state.catalog.get(&card.catalog_key) else { continue };
        if !def.is_land() && def.mana_cost().trim() == "0" {
            if oneshot_produces_color(def, Color::Black) {
                c.pool += 1;
            } else if renewable_produces_color(def, Color::Black) {
                c.renew_inplay += 1;
            }
        } else if def.is_land() {
            if use_fetch(def) {
                c.untapped_lands += 1;
            } else if produces_black(def) {
                // Enters-tapped honesty: a tapland makes no mana the turn it's
                // played, but untaps on the next PASS → usable a turn later.
                if def.enters_tapped() {
                    c.tapped_lands += 1;
                } else {
                    c.untapped_lands += 1;
                }
            } else if produces_nonblack_mana(def) {
                if def.enters_tapped() {
                    c.other_tapped += 1;
                } else {
                    c.other_untapped += 1;
                }
            }
        } else if is_black_ritual(def) {
            c.ritual_gp[ritual_generic_cost(def)] += 1;
        }
    }
    c
}

// ── Backward black-mana reachability ─────────────────────────────────────────

/// Can we reach `need` black this turn? Backward production graph: a flat
/// producer gives +1; the land drop gives +1 once; a ritual converts a 1-black
/// seed into 3. Sound + complete for the all-black goal (every producer is
/// dominant toward black).
/// Generic backward mana reachability: can we pay `blk` colored (black) pips and
/// `gen` generic pips from `black` black-capable units, `other` non-black units,
/// and `ritual_gp` rituals? This is the reusable core — *any* mana cost (Doomsday's
/// `BBB`, The Fantasticar's `{3}`) reduces to a `(blk, gen)` query, so the model
/// is no longer black-specific (the colored axis is just "the pips that need a
/// matching source"; for this UB deck that axis is black).
///
/// Producers: a black-capable unit pays one black pip OR one generic pip; a
/// non-black unit pays one generic pip; a ritual costs a 1-black seed + `g` generic
/// pips and yields 3 black-capable mana (may chain — its output seeds another).
fn reach_mana(blk: i32, gen: i32, black: u32, other: u32, ritual_gp: [u32; 4]) -> bool {
    if blk <= 0 && gen <= 0 {
        return true;
    }
    // Pay a colored pip from a black-capable unit.
    if blk > 0 && black > 0 && reach_mana(blk - 1, gen, black - 1, other, ritual_gp) {
        return true;
    }
    // Pay a generic pip — prefer non-black mana, then black (conserve black for pips).
    if gen > 0 && other > 0 && reach_mana(blk, gen - 1, black, other - 1, ritual_gp) {
        return true;
    }
    if gen > 0 && black > 0 && reach_mana(blk, gen - 1, black - 1, other, ritual_gp) {
        return true;
    }
    // Fire a ritual: pay `g` generic pips (non-black first, rest from black) + a
    // 1-black seed; gain 3 black-capable mana.
    if blk + gen > 0 {
        for g in 0..ritual_gp.len() {
            if ritual_gp[g] == 0 {
                continue;
            }
            let from_other = (g as u32).min(other);
            let black_cost = 1 + (g as u32 - from_other); // seed + generic-from-black
            if black >= black_cost {
                let mut rg = ritual_gp;
                rg[g] -= 1;
                if reach_mana(blk, gen, black - black_cost + 3, other - from_other, rg) {
                    return true;
                }
            }
        }
    }
    false
}

/// Black-only specialization (Doomsday's `BBB`): pay `need` black pips. Delegates
/// to the generic [`reach_mana`] — kept as a named helper for the all-black goal.
/// Equivalence with the former standalone implementation (for the combo regime,
/// `need ≤ 3`) is pinned by `reach_black_parity` in the tests.
fn reach_black(need: i32, black: u32, other: u32, ritual_gp: [u32; 4]) -> bool {
    reach_mana(need, 0, black, other, ritual_gp)
}

/// A parsed mana cost: generic pips + colored pips per `Color`. Drives the generic
/// [`cost_turn`] backward solver, so any line states its requirement as a cost
/// (Doomsday `BBB`, The Fantasticar `{3}`) rather than a bespoke black query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ManaCost {
    generic: u32,
    pips: [u32; 5], // indexed by `Color as usize`: W, U, B, R, G
}

const BLACK: usize = Color::Black as usize;

impl ManaCost {
    /// Parse "BBB", "1B", "3", etc. Digits → generic; W/U/B/R/G → colored pips;
    /// {C} → generic (colorless); X/braces/whitespace ignored.
    fn parse(cost: &str) -> ManaCost {
        let mut mc = ManaCost { generic: 0, pips: [0; 5] };
        let mut num = 0u32;
        let mut saw_digit = false;
        let flush = |mc: &mut ManaCost, num: &mut u32, saw: &mut bool| {
            if *saw {
                mc.generic += *num;
                *num = 0;
                *saw = false;
            }
        };
        for ch in cost.trim().chars() {
            if let Some(d) = ch.to_digit(10) {
                num = num * 10 + d;
                saw_digit = true;
                continue;
            }
            flush(&mut mc, &mut num, &mut saw_digit);
            match ch {
                'W' => mc.pips[Color::White as usize] += 1,
                'U' => mc.pips[Color::Blue as usize] += 1,
                'B' => mc.pips[Color::Black as usize] += 1,
                'R' => mc.pips[Color::Red as usize] += 1,
                'G' => mc.pips[Color::Green as usize] += 1,
                'C' => mc.generic += 1,
                _ => {}
            }
        }
        flush(&mut mc, &mut num, &mut saw_digit);
        mc
    }
}

/// Can `cost` be paid on turn `turn` with `pool` one-shot tokens available?
/// Colored pips are charged against black-capable mana (the only colored axis in
/// this deck); any non-black colored pips are approximated as generic (none occur
/// in the modeled costs). Generic pips draw from all mana.
fn cost_on_turn(c: &Caps, cost: &ManaCost, turn: u32, pool: u32) -> bool {
    let blk = cost.pips[BLACK];
    let non_black_colored: u32 = cost
        .pips
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != BLACK)
        .map(|(_, n)| *n)
        .sum();
    let gen = cost.generic + non_black_colored;
    reach_mana(
        blk as i32,
        gen as i32,
        renew_black_by_turn(c, turn) + pool,
        other_online_by_turn(c, turn),
        c.ritual_gp,
    )
}

/// How many lands in hand can be ONLINE (untapped, tappable) by turn `turn` —
/// PASS-as-action, deterministically. Color-agnostic: it's purely land-drop
/// timing (the caller projects `untapped`/`tapped` for whichever color it cares
/// about). `turn` is the 1-based turn number (turn 1 = now / the casting turn for
/// a "turn-1 Doomsday"). One land drop per turn (turn 1's only if `land_drop` is
/// still available), everything untaps between turns, so a tapland played on an
/// earlier turn is online by `turn`. Drops by `turn` = `(turn-1) + land_drop`; a
/// tapland needs an "early" slot (a turn before `turn`) to have untapped in time;
/// untapped lands are online the turn they're played.
fn online_lands(untapped: u32, tapped: u32, land_drop: bool, turn: u32) -> u32 {
    let drops = turn.saturating_sub(1) + land_drop as u32;
    if drops == 0 {
        return 0;
    }
    let early = drops - 1;
    (untapped + tapped.min(early)).min(drops)
}

/// Renewable black capacity online by turn `turn` (1-based, no draws): in-play tap
/// sources plus black tap-lands deployable and online by then. Excludes the
/// one-shot pool — that's a consumable threaded separately.
fn renew_black_by_turn(c: &Caps, turn: u32) -> u32 {
    c.renew_inplay + online_lands(c.untapped_lands, c.tapped_lands, c.land_drop, turn)
}

/// NON-black renewable mana online by turn `turn` (for paying ritual generic pips).
/// The land drop is SHARED with the black lands, so this is (total lands online) −
/// (black lands online): black lands win the early drops, and what's left is the
/// non-black mana available — e.g. Swamp + Island gives 1 black / 0 other on turn 1,
/// 1 black / 1 other on turn 2.
fn other_online_by_turn(c: &Caps, turn: u32) -> u32 {
    let black_lands = online_lands(c.untapped_lands, c.tapped_lands, c.land_drop, turn);
    let total_lands = online_lands(
        c.untapped_lands + c.other_untapped,
        c.tapped_lands + c.other_tapped,
        c.land_drop,
        turn,
    );
    c.other_inplay + total_lands.saturating_sub(black_lands)
}

/// Can BBB (3 black) be produced on turn `turn` with `pool` one-shot tokens still
/// available? Renewable capacity is per-turn (it returns each untap); the pool and
/// rituals are consumables. `pool` is passed explicitly so a caller that already
/// spent a token (e.g. a sac'd petal paying a pip) charges BBB the depleted pool.
fn bbb_on_turn(c: &Caps, turn: u32, pool: u32) -> bool {
    // Doomsday's BBB expressed as a generic cost over the shared solver.
    cost_on_turn(c, &ManaCost::parse("BBB"), turn, pool)
}

/// Earliest turn (≤ `max_turn`) BBB is producible with `pool` one-shot tokens, or
/// `None`. Monotonic in `turn` (renewable capacity only grows).
fn bbb_turn(c: &Caps, max_turn: u32, pool: u32) -> Option<u32> {
    (1..=max_turn).find(|&t| bbb_on_turn(c, t, pool))
}

// ── Deterministic payoff acquisition (Personal Tutor) ────────────────────────
//
// A library-top tutor (Personal Tutor) makes the PAYOFF an acquirable resource,
// symmetric to a fetch making mana acquirable: both "search library → produce an
// object", grounded by the SAME `obj_matches` question ("can it get DD?" ≡ "can
// this fetch get black?"). The one difference is the destination zone — a fetch's
// land lands in play (instantly usable), a tutor's card lands on TOP of the
// library, so it needs one more action to reach hand: next turn's natural draw.
// That draw is normally the STOCHASTIC half; the tutor's trick is that it makes
// that one draw DETERMINISTIC (you know the top card — you put it there), which is
// why the payoff line lives in this deterministic layer. Hence "+1 turn": tutor
// turn N, draw DD turn N+1.

/// The single colored pip of a mana cost, or `None` if the cost isn't exactly one
/// colored pip (e.g. it has generic/colorless mana or two colored pips). SPIKE:
/// the deterministic payoff line models only a one-pip, MV-1 tutor (Personal/
/// Mystical/Vampiric Tutor) — "1 source of its colour" is the whole cost; richer
/// tutor costs aren't modelled (production: full cost payment).
fn single_colored_pip(cost: &str) -> Option<Color> {
    let mut colors = Vec::new();
    let mut has_other = false;
    for ch in cost.trim().chars() {
        match ch {
            'W' => colors.push(Color::White),
            'U' => colors.push(Color::Blue),
            'B' => colors.push(Color::Black),
            'R' => colors.push(Color::Red),
            'G' => colors.push(Color::Green),
            _ => has_other = true, // generic digits, {C}, etc.
        }
    }
    (colors.len() == 1 && !has_other).then(|| colors[0])
}

/// If `who` holds a card that can tutor the payoff (`Doomsday`) to the TOP of the
/// library, return the tutor's single colored pip — the "1 source" requirement.
/// Grounded by `obj_matches`: the tutor's own search `Filter` must actually match
/// a Doomsday that is IN the library (so a "find an instant" tutor wouldn't
/// qualify, and a tutor can't get a Doomsday that isn't in the library). The
/// library-top shape is read structurally (`CardDef::library_top_tutor`), no
/// name-check; battlefield searchers (Green Sun's Zenith) are excluded there.
fn payoff_tutor_pip(state: &SimState, who: PlayerId) -> Option<Color> {
    let dd_in_lib: Vec<_> = state
        .library_of(who)
        .filter(|c| c.catalog_key == DOOMSDAY)
        .map(|c| c.id)
        .collect();
    if dd_in_lib.is_empty() {
        return None;
    }
    for card in state.hand_of(who) {
        let Some(def) = state.catalog.get(&card.catalog_key) else { continue };
        let Some(filter) = def.library_top_tutor() else { continue };
        if !dd_in_lib.iter().any(|&id| obj_matches(filter, id, state)) {
            continue;
        }
        if let Some(pip) = single_colored_pip(def.mana_cost()) {
            return Some(pip);
        }
    }
    None
}

/// The earliest turn (1-based, ≤ `max_turn`) a RENEWABLE source of `color` is
/// online, or `None`. Renewable means the cost taps (not sacrifices) the source,
/// so its untapped-state returns each turn — it can pay a pip AND still be free for
/// a later demand. The one-shot pool is deliberately EXCLUDED here: a pool token
/// paying the pip is consumed, so it's charged against BBB in `deterministic_cast_turn`,
/// not double-counted as a standing source.
fn renew_color_turn(
    state: &SimState,
    who: PlayerId,
    color: Color,
    max_turn: u32,
    extra: &[&str],
) -> Option<u32> {
    let land_drop = state.player(who).lands_played_this_turn < 1;
    let fetch_target = library_has_target_color(state, who, color);
    // A renewable colour source already in play → online right now (turn 1).
    let inplay = state.permanents_of(who).any(|perm| {
        !perm.bf().is_some_and(|bf| bf.tapped)
            && state
                .def_of(perm.id)
                .is_some_and(|def| renewable_produces_color(def, color) || (is_fetch(def) && fetch_target))
    });
    if inplay {
        return Some(1);
    }
    // Otherwise the earliest turn a renewable colour tap-land in hand — plus any
    // hypothetically-acquired `extra` cards (e.g. a fetch target under evaluation) —
    // comes online. Threading `extra` here is what lets the model see that a fetched
    // Underground Sea pays a {U} pip while a Swamp does not.
    let (mut untapped, mut tapped) = (0u32, 0u32);
    let mut count = |def: &CardDef| {
        if !def.is_land() {
            return;
        }
        if is_fetch(def) && fetch_target {
            untapped += 1;
        } else if renewable_produces_color(def, color) {
            if def.enters_tapped() {
                tapped += 1;
            } else {
                untapped += 1;
            }
        }
    };
    for card in state.hand_of(who) {
        if let Some(def) = state.catalog.get(&card.catalog_key) {
            count(def);
        }
    }
    for &key in extra {
        if let Some(def) = state.catalog.get(key) {
            count(def);
        }
    }
    (1..=max_turn).find(|&t| online_lands(untapped, tapped, land_drop, t) >= 1)
}

// ── The functions ────────────────────────────────────────────────────────────

/// `state → sufficient?` — can `who` cast Doomsday on turn 1 (this turn)? The
/// whole one-shot pool is available (nothing's been pre-spent this turn).
pub fn sufficient(state: &SimState, who: PlayerId) -> bool {
    let has_dd = state.hand_of(who).any(|c| c.catalog_key == DOOMSDAY);
    let c = project(state, who);
    has_dd && bbb_on_turn(&c, 1, c.pool)
}

/// What a not-yet-combo'd hand is missing RIGHT NOW: the payoff (no Doomsday in hand),
/// the mana (can't make BBB this turn), both, or neither (has both but hasn't cast — a
/// sequencing/timing gap). Used to decompose the games that miss the cutoff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MissingElement {
    Mana,
    Payoff,
    Both,
    Neither,
}

pub fn missing_element(state: &SimState, who: PlayerId) -> MissingElement {
    let dd = state.hand_of(who).any(|c| c.catalog_key == DOOMSDAY);
    let c = project(state, who);
    let mana = bbb_on_turn(&c, 1, c.pool);
    match (mana, dd) {
        (true, true) => MissingElement::Neither,
        (true, false) => MissingElement::Payoff,
        (false, true) => MissingElement::Mana,
        (false, false) => MissingElement::Both,
    }
}

/// The earliest turn (1-based; turn 1 = now) by which `who` can cast Doomsday
/// using ONLY the current hand+board — no *blind* draws relied upon. PASS untaps
/// and grants a land drop each turn (so taplands come online, and lands
/// accumulate), but its draw isn't used for unknown cards. `None` if not reachable
/// deterministically by `max_turn`. (A tight depth bound is roughly "black lands
/// in hand − black available now"; `max_turn = 3` is the cap — we don't model
/// turn 4.)
///
/// Two deterministic lines are considered and the earlier taken:
/// - **Direct** — Doomsday already in hand: the earliest turn BBB is reachable
///   (the full one-shot pool is available for BBB).
/// - **Tutor** — a library-top tutor for Doomsday in hand (see `payoff_tutor_pip`):
///   pay its pip, the *known* card is drawn the turn after, then cast once BBB is
///   up. Paying the pip is a COST that consumes a resource, so the line forks on
///   *which* source pays it:
///     * a **renewable** pip-colour source (tap) — its untapped-state returns next
///       turn, so the pool stays whole for BBB; pip on turn `s` ⇒ draw `s+1`;
///     * a **one-shot** pool token (sac, turn 1) — the pool loses one for BBB; draw
///       turn 2. No petal special-case — it's the sac cost consuming the object.
///   This is honest: the drawn card is one the player placed, not a deck peek.
pub fn deterministic_cast_turn(state: &SimState, who: PlayerId, max_turn: u32) -> Option<u32> {
    deterministic_cast_turn_ex(state, who, &[], max_turn)
}

/// [`deterministic_cast_turn`] with `extra` cards hypothetically acquired into hand
/// (a dig / fetch target under evaluation). `extra` is folded into the mana caps AND
/// threaded into the pip check, so e.g. a fetched Underground Sea is recognised as
/// paying a tutor's {U} pip (a Swamp is not) — the colour the fetch-target choice
/// turns on.
fn deterministic_cast_turn_ex(
    state: &SimState,
    who: PlayerId,
    extra: &[&str],
    max_turn: u32,
) -> Option<u32> {
    let mut c = project(state, who);
    let mut has_dd = state.hand_of(who).any(|c| c.catalog_key == DOOMSDAY);
    for &key in extra {
        if let Some(def) = state.catalog.get(key) {
            fold_known_card(&mut c, &mut has_dd, def, key);
        }
    }

    // Direct: DD in hand, earliest turn BBB is up with the full pool.
    let direct = if has_dd { bbb_turn(&c, max_turn, c.pool) } else { None };

    // Staged: Doomsday is on the KNOWN top of the library (we tutored / ordered it
    // there) at depth `d` → drawn on relative turn `d + 2`, then cast once BBB is up.
    // The mana must NOT shuffle (cracking a fetch would scatter the staged top), so
    // BBB is reckoned over non-fetch capabilities. Known-top is 0 in a fresh opening
    // hand, so this never fires for a mulligan (no hidden-info / hypergeometric leak).
    let staged = match payoff_known_depth(state, who) {
        Some(d) if !has_dd => {
            let nf = project_inner(state, who, false);
            bbb_turn(&nf, max_turn, nf.pool)
                .map(|b| b.max(d + 2))
                .filter(|&t| t <= max_turn)
        }
        _ => None,
    };

    let tutor = payoff_tutor_pip(state, who).and_then(|pip| {
        let mut best: Option<u32> = None;
        let mut consider = |t: u32| {
            if t <= max_turn {
                best = Some(best.map_or(t, |b: u32| b.min(t)));
            }
        };
        // Pay the pip with a RENEWABLE source: pool intact, but the pip (hence the
        // draw) is bounded by when that source comes online.
        if let Some(s) = renew_color_turn(state, who, pip, max_turn, extra) {
            if let Some(b) = bbb_turn(&c, max_turn, c.pool) {
                consider((s + 1).max(b));
            }
        }
        // Pay the pip with a ONE-SHOT pool token (turn 1): the pool loses one.
        if c.pool >= 1 {
            if let Some(b) = bbb_turn(&c, max_turn, c.pool - 1) {
                consider(2u32.max(b));
            }
        }
        best
    });

    [direct, tutor, staged].into_iter().flatten().min()
}

// ── The Fantasticar: backward solver for the "pop the car" line ──────────────
//
// Symmetric to `deterministic_cast_turn` but for the OTHER wincon: sacrifice The
// Fantasticar to its "fourth noncreature spell" trigger (→ four 4/4 fliers). The
// requirement is two-part: (1) The Fantasticar in play (or castable for {3} this
// turn, cast among the first three so it's out for the 4th), and (2) four
// noncreature spells cast this turn — the car's own cast counts. The cheap fuel is
// the deck's FREE noncreature spells (Mishra's Bauble / Lotus Petal / LED); a Petal
// pulls double duty (cast → +1 to the count; sacrifice → +1 mana toward {3}).
//
// SPIKE (conservative lower bound, like the rest of this module): only free `{0}`
// noncreature spells are modeled as count-fuel. Cantrips/rituals also count toward
// four in reality but pricing them into the same turn needs the per-spell mana
// solve; deferred. The deck's nine free copies make the line viable regardless, and
// the realized sim (which counts every noncreature cast via the engine trigger) sees
// any extra lines this misses.

/// Earliest turn (1-based, ≤ `max_turn`) `who` can pop The Fantasticar using only
/// the current hand+board (no blind draws), or `None`.
/// A noncreature spell we can cast *proactively* this turn for the count — i.e. it doesn't
/// need a target we can't freely supply: an opponent OBJECT (removal: Fatal Push), or a
/// spell on the STACK (counters: Force/Daze — even now they're `Who::Any`, they still need a
/// stack spell, and are handled separately as self-counters in `self_counter_free_casts`).
/// Thoughtseize (targets a player) and cantrips (no target) qualify, as do rituals and {0}s.
fn is_proactive_noncreature(def: &CardDef) -> bool {
    use libmtg_engine::{TargetSpec, Who, ZoneId};
    fn needs_unavailable_target(ts: &TargetSpec) -> bool {
        match ts {
            TargetSpec::ObjectInZone { controller, zone, .. } => {
                matches!(controller, Who::Opp) || matches!(zone, ZoneId::Stack)
            }
            TargetSpec::Union(v) => v.iter().any(needs_unavailable_target),
            _ => false,
        }
    }
    !def.is_land() && !def.is_creature() && !needs_unavailable_target(def.target_spec())
}

/// Mana value of a parsed cost.
fn mana_value(cost: &ManaCost) -> u32 {
    cost.generic + cost.pips.iter().sum::<u32>()
}

/// The car's noncreature fuel, partitioned for `car_pop_feasible`.
///   free_petal = {0} mana artifacts (Lotus Petal): +1 to the count AND +1 pool mana.
///   free_other = {0} non-mana (Mishra's Bauble, LED): +1 to the count only.
///   rituals    = Dark-Ritual-shaped: +1 to the count AND net mana.
///   paid       = other proactive noncreature spells' mana values (Thoughtseize, cantrips).
#[derive(Clone, Default)]
struct CarFuel {
    free_petal: u32,
    free_other: u32,
    rituals: u32,
    paid: Vec<u32>,
}

impl CarFuel {
    /// Fold one card into the tally. `*car_seen` skips exactly one Fantasticar (the
    /// payoff itself is not fuel); pass it already-true when a car is in play.
    fn add(&mut self, def: &CardDef, key: &str, car_seen: &mut bool) {
        if key == FANTASTICAR && !*car_seen {
            *car_seen = true;
            return;
        }
        if !is_proactive_noncreature(def) {
            return;
        }
        if is_black_ritual(def) {
            self.rituals += 1;
        } else if def.mana_cost().trim() == "0" {
            if oneshot_produces_color(def, Color::Black) {
                self.free_petal += 1;
            } else {
                self.free_other += 1;
            }
        } else {
            self.paid.push(mana_value(&ManaCost::parse(def.mana_cost())));
        }
    }
}

pub fn car_pop_turn(state: &SimState, who: PlayerId, max_turn: u32) -> Option<u32> {
    car_pop_turn_full(state, who, &[], &[], false, max_turn, 0)
}

/// How far the hand is from a DETERMINISTIC car pop by `max_turn`, measured in extra
/// Lotus-Petal-equivalents (each worth +1 noncreature cast AND +1 mana — the canonical
/// "one more piece" a cantrip digs into). 0 = deterministic (`car_pop_turn` is `Some`);
/// 1 = one piece short ("cast the car but only 3 spells", or "one more mana"); capped at
/// 3 ("far"). Drives the mulligan's "almost sends car" keep — a 1-away hand with a dig is
/// very likely to find the piece and pop.
pub fn car_pop_shortfall(state: &SimState, who: PlayerId, max_turn: u32) -> u32 {
    for k in 0..3 {
        if car_pop_turn_full(state, who, &[], &[], false, max_turn, k).is_some() {
            return k;
        }
    }
    3
}

/// [`car_pop_turn`] with `extra_hand` cards gained THIS turn (a Brainstorm burst you
/// keep) and a KNOWN top-of-library prefix that arrives over the coming turns (Ponder
/// ordering / a Brainstorm put-back left on top). Mirrors `deterministic_cast_turn_full`:
/// extras fold in immediately, prefix cards by their draw turn. This is what lets the
/// selection find a guaranteed car line in revealed pieces — e.g. Brainstorm into
/// petal/car/petal → keep them → car the next turn.
fn car_pop_turn_full(
    state: &SimState,
    who: PlayerId,
    extra_hand: &[&str],
    known_top: &[&str],
    immediate: bool,
    max_turn: u32,
    // Phantom Lotus-Petal-equivalents added to the fuel — `car_pop_shortfall` probes
    // 0/1/2 to find how many "one more piece"s the hand is short. 0 = exact line.
    extra_petals: u32,
) -> Option<u32> {
    let car_in_play = state.permanents_of(who).any(|p| p.catalog_key == FANTASTICAR);

    // Base mana caps: project() already folds the hand; add extra_hand mana on top.
    // Base fuel: classified from hand + extra_hand. (Hand mana is in project(); we only
    // need the fuel COUNT here, so hand cards go through `CarFuel::add`, not fold_known_card.)
    let mut base_caps = project(state, who);
    let mut base_fuel = CarFuel::default();
    let mut base_car = car_in_play;
    let mut base_seen = car_in_play; // a car in play already accounts for the payoff
    for card in state.hand_of(who) {
        if let Some(def) = state.catalog.get(&card.catalog_key) {
            if card.catalog_key == FANTASTICAR {
                base_car = true;
            }
            base_fuel.add(def, &card.catalog_key, &mut base_seen);
        }
    }
    for &k in extra_hand {
        if let Some(def) = state.catalog.get(k) {
            if k == FANTASTICAR {
                base_car = true;
            }
            let mut dd = false;
            fold_known_card(&mut base_caps, &mut dd, def, k);
            base_fuel.add(def, k, &mut base_seen);
        }
    }
    // Self-targetable counters (Daze/Force) we can cast on our own spell for the count.
    let hand_extra: Vec<&str> = state
        .hand_of(who)
        .map(|c| c.catalog_key.as_str())
        .chain(extra_hand.iter().copied())
        .collect();
    base_fuel.free_other += self_counter_free_casts(state, who, hand_extra.iter().copied());

    for t in 1..=max_turn {
        let seen = if immediate { t as usize } else { t.saturating_sub(1) as usize };
        let mut caps = base_caps;
        let mut fuel = base_fuel.clone();
        let mut car = base_car;
        let mut cseen = base_seen;
        for &k in known_top.iter().take(seen) {
            if let Some(def) = state.catalog.get(k) {
                if k == FANTASTICAR {
                    car = true;
                }
                let mut dd = false;
                fold_known_card(&mut caps, &mut dd, def, k);
                fuel.add(def, k, &mut cseen);
            }
        }
        if car {
            fuel.paid.sort_unstable();
            // We must CAST the car ({3}) unless it is already in play.
            if car_pop_feasible(&caps, t, !car_in_play, &fuel, extra_petals) {
                return Some(t);
            }
        }
    }
    None
}

/// Can the car pop on turn `t`? Thin per-turn wrapper over [`solve_pop`] — it just supplies
/// the resources for a FRESH turn (`spells_this_turn = 0`): `base` = generic-capable mana
/// online by turn `t` MINUS hand Petals (which `solve_pop` re-adds as fuel, so they are not
/// double-counted), i.e. lands + in-play one-shots (`c.pool` already includes hand petals).
/// This is the deterministic-solver caller; `can_pop_this_turn` is the live-execution caller
/// — same rule, same `solve_pop`. SPIKE: paid pips and ritual seeding approximated as generic
/// (not strictly black), exact enough for this mostly-black manabase.
fn car_pop_feasible(c: &Caps, t: u32, car_in_hand: bool, fuel: &CarFuel, extra_petals: u32) -> bool {
    let lands_m = renew_black_by_turn(c, t) + other_online_by_turn(c, t);
    // base_mana subtracts only the REAL hand petals (already in `c.pool`); the phantom
    // shortfall petals are added to the fuel below, so `solve_pop` credits their mana AND
    // cast-slot without the pool double-count.
    let base_mana = (lands_m + c.pool).saturating_sub(fuel.free_petal);
    let mut f = fuel.clone();
    f.free_petal += extra_petals;
    solve_pop(base_mana, 0, &f, !car_in_hand)
}

/// Can `who` COMPLETE a car pop THIS turn from the current MID-turn state? Unlike
/// `car_pop_turn` (which assumes a fresh turn and projects future land drops), this gates
/// real-time execution: it accounts for noncreature spells already cast this turn and the
/// mana available right now (floating + untapped, via `potential_mana`). The hard rule it
/// enforces is ordering: the car must be castable as the **≤3rd** noncreature spell of the
/// turn (so the FOURTH cast triggers with the car already IN PLAY) with ≥1 castable spell
/// after it. The strategy spends the car / ramp / pop-fuel only when this holds — otherwise
/// it conserves and pops next turn, never firing the car as the doomed 4th-or-later spell.
/// Extra FREE cast-slots from self-targetable counters (Daze / Force of Will) over `keys`
/// (the relevant in-hand cards), castable on YOUR OWN spell via their free ALTERNATIVE cost:
/// a Daze returns an Island land you control (Underground Sea is "Island Swamp", and a
/// tapped land still bounces — you tap it for mana, then bounce it); a Force exiles a spare
/// blue card. Each consumes one such resource. (Targeting your own stack spell is the engine's
/// `Who::Any` path. Hard-casting {1}{U} / {3}{U}{U} is conservatively omitted — the combo
/// uses the free cost.) These are pure cast-slots: they advance the per-turn noncreature count.
fn self_counter_free_casts<'a>(
    state: &SimState,
    who: PlayerId,
    keys: impl Iterator<Item = &'a str>,
) -> u32 {
    let (mut dazes, mut forces, mut hand_islands, mut blue_fodder) = (0u32, 0u32, 0u32, 0u32);
    for key in keys {
        if key == DAZE {
            dazes += 1;
        } else if key == FORCE_OF_WILL {
            forces += 1;
        } else if let Some(d) = state.catalog.get(key) {
            if d.is_island() {
                hand_islands += 1;
            } else if d.is_blue() && !d.is_land() && !is_proactive_noncreature(d) {
                // A spare blue card to pitch to Force (e.g. a blue creature). A blue CANTRIP
                // is excluded — pitching a castable spell is net-zero cast-slots.
                blue_fodder += 1;
            }
        }
    }
    let board_islands = state
        .permanents_of(who)
        .filter(|p| state.catalog.get(&p.catalog_key).is_some_and(|d| d.is_island()))
        .count() as u32;
    dazes.min(hand_islands + board_islands) + forces.min(blue_fodder)
}

/// PURE resource solver — the one place the per-turn pop rule lives. Both callers
/// (`can_pop_this_turn` for live execution, and ultimately the deterministic per-turn
/// check) fill these resources; they differ only in how:
///   - `base_mana`: generic-capable mana available WITHOUT casting fuel (lands online +
///     floating + in-play one-shots);
///   - `spells_this_turn`: noncreature casts already made this turn — the **temporal
///     cast-slot** resource already consumed;
///   - `fuel`: the noncreature spells in hand. A Petal is BOTH a cast and {1} mana; a
///     ritual a cast and net mana; `free_other`/`paid` are casts (paid ones cost mana);
///   - `car_in_play`: the car is already a permanent (else it must be CAST — paying {3}).
/// Rule: reach 4 noncreature casts this turn with the car in play before the 4th. If the
/// car must be cast, it has to occupy cast-slot ≤3 with ≥1 castable spell after it — that
/// ordering IS the temporal resource, and it is what stops the car being fired as the
/// doomed 4th spell (on the stack, not in play → the trigger whiffs).
fn solve_pop(base_mana: u32, spells_this_turn: u32, fuel: &CarFuel, car_in_play: bool) -> bool {
    if spells_this_turn >= 4 {
        return false; // the 4th cast has passed; if it was going to pop, it already did
    }
    let seedable = base_mana >= 1 || fuel.free_petal >= 1;
    let ritual_mana = if seedable { 2 * fuel.rituals } else { 0 };
    let free = fuel.free_petal + fuel.free_other; // {0} casts (Petals also sac for mana)
    let total_mana = base_mana + fuel.free_petal + ritual_mana;
    // Greedily fit the paid spells (cantrips/Thoughtseize) into `avail` mana.
    let fit_paid = |avail: u32| -> u32 {
        let mut rem = avail;
        let mut n = 0;
        for &cmc in &fuel.paid {
            if rem >= cmc { rem -= cmc; n += 1; } else { break; }
        }
        n
    };
    if car_in_play {
        // Any 4 noncreature casts this turn pop it — need (4 - spells_this_turn) more.
        let castable = free + fuel.rituals + fit_paid(total_mana);
        return spells_this_turn + castable >= 4;
    }
    // Car in hand → must afford {3} AND land as the ≤3rd noncreature spell of the turn.
    if total_mana < 3 {
        return false;
    }
    let ramp_before = min_ramp_for_three(base_mana, fuel);
    let car_pos = spells_this_turn + ramp_before + 1;
    if car_pos > 3 {
        return false; // the car would be the 4th-or-later spell → no pop this turn
    }
    let castable_non_car = free + fuel.rituals + fit_paid(total_mana.saturating_sub(3));
    let post_car = castable_non_car.saturating_sub(ramp_before);
    post_car >= 4 - car_pos
}

/// Can `who` COMPLETE a car pop THIS turn from the current MID-turn state? Fills the
/// resources for [`solve_pop`] from live state — cast-slots already used =
/// `noncreature_casts_this_turn`, base mana = `potential_mana` (floating + untapped, NOT
/// future land drops). The strategy gates spending the car/ramp/fuel on this; otherwise it
/// conserves and pops next turn. See [`solve_pop`] for the rule.
pub fn can_pop_this_turn(state: &SimState, who: PlayerId) -> bool {
    let car_in_play = state.permanents_of(who).any(|p| p.catalog_key == FANTASTICAR);
    let car_in_hand = state.hand_of(who).any(|c| c.catalog_key == FANTASTICAR);
    if !car_in_play && !car_in_hand {
        return false;
    }
    let mut fuel = CarFuel::default();
    let mut seen = car_in_play; // a car in play already accounts for the payoff
    for card in state.hand_of(who) {
        if let Some(def) = state.catalog.get(&card.catalog_key) {
            fuel.add(def, &card.catalog_key, &mut seen);
        }
    }
    let hand_keys: Vec<&str> = state.hand_of(who).map(|c| c.catalog_key.as_str()).collect();
    fuel.free_other += self_counter_free_casts(state, who, hand_keys.iter().copied());
    fuel.paid.sort_unstable();
    let base = state.potential_mana(who).total.max(0) as u32;
    solve_pop(base, state.noncreature_casts_this_turn(who), &fuel, car_in_play)
}

/// Minimum number of mana-producing spells (rituals first — fewer casts; a {0} Petal must
/// lead to seed a ritual when there is no black yet) cast BEFORE the car to fund its {3},
/// given `base` mana already available now.
fn min_ramp_for_three(base: u32, fuel: &CarFuel) -> u32 {
    if base >= 3 {
        return 0;
    }
    let (mut mana, mut spells) = (base, 0u32);
    let (mut petals, mut rituals) = (fuel.free_petal, fuel.rituals);
    if base == 0 && rituals > 0 && petals > 0 {
        mana += 1; petals -= 1; spells += 1; // a Petal seeds the first ritual
    }
    while mana < 3 && rituals > 0 { mana += 2; rituals -= 1; spells += 1; }
    while mana < 3 && petals > 0 { mana += 1; petals -= 1; spells += 1; }
    spells
}

/// Earliest turn `who` can "send" — resolve Doomsday OR pop The Fantasticar — from the
/// current hand+board with no blind draws.
pub fn deterministic_send_turn(state: &SimState, who: PlayerId, max_turn: u32) -> Option<u32> {
    deterministic_send_turn_full(state, who, &[], &[], false, max_turn)
}

/// [`deterministic_send_turn`] with the `extra_hand`/`known_top` look-ahead — the
/// min of the Doomsday and car lines, each evaluated with the same revealed pieces.
fn deterministic_send_turn_full(
    state: &SimState,
    who: PlayerId,
    extra_hand: &[&str],
    known_top: &[&str],
    immediate: bool,
    max_turn: u32,
) -> Option<u32> {
    [
        deterministic_cast_turn_full(state, who, extra_hand, known_top, immediate, max_turn),
        car_pop_turn_full(state, who, extra_hand, known_top, immediate, max_turn, 0),
    ]
    .into_iter()
    .flatten()
    .min()
}

// ── Concrete deterministic-line emission ─────────────────────────────────────
//
// `deterministic_cast_turn` says *when* a guaranteed line lands; this emits *what*
// it is, as an ordered, object-bound step list, so the strategy can FOLLOW the
// solved line instead of re-deriving assembly with a local priority list (the seam
// the missing Personal-Tutor step fell through). The line is recomputed from the
// current state every window; the strategy executes the first currently-legal step,
// so the engine's state advance + re-emission sequences the multi-turn line (one
// land drop per turn, play-then-crack a fetch, cast the tutor then draw Doomsday).

/// One concrete action of the deterministic line, bound to the object it acts on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStep {
    /// Make the once-per-turn land drop with this land (untapped black / fetch / tapland).
    PlayLand(ObjId),
    /// Activate a fetch already in play (engine picks the black target).
    CrackFetch(ObjId),
    /// Cast Lotus Petal (mana cost 0) — afterwards a one-shot black source.
    CastPetal(ObjId),
    /// Cast Dark Ritual (1 black seed → 3 black).
    CastRitual(ObjId),
    /// Cast the payoff tutor (Personal Tutor) → Doomsday on top of the library.
    CastTutor(ObjId),
    /// Cast Doomsday.
    CastDoomsday(ObjId),
}

/// The ordered, object-bound deterministic line that lands Doomsday on `turn`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetLine {
    pub turn: u32,
    pub steps: Vec<LineStep>,
}

/// Concrete black-mana sources, partitioned exactly like `project` (same IR
/// predicates) but retaining object ids so the line can name them.
#[derive(Default)]
struct SourceIds {
    renew_inplay: Vec<ObjId>,  // renewable black in play (taps; online now)
    inplay_fetch: Vec<ObjId>,  // fetch in play → crack to a renewable black land
    hand_untapped: Vec<ObjId>, // black untapped land in hand (land drop, online when played)
    hand_fetch: Vec<ObjId>,    // fetch in hand (land drop → crack → untapped black)
    hand_tapped: Vec<ObjId>,   // black tapland in hand (land drop, online next turn)
    petals_inplay: Vec<ObjId>, // one-shot pool already in play (spent at Doomsday's cost)
    petals_hand: Vec<ObjId>,   // Lotus Petal in hand (cast 0 → pool)
    rituals: Vec<ObjId>,       // Dark Ritual in hand
}

fn source_ids(state: &SimState, who: PlayerId) -> SourceIds {
    let mut s = SourceIds::default();
    let fetch_target = library_has_black_target(state, who);
    for perm in state.permanents_of(who) {
        if perm.bf().is_some_and(|bf| bf.tapped) {
            continue;
        }
        let Some(def) = state.def_of(perm.id) else { continue };
        if oneshot_produces_color(def, Color::Black) {
            s.petals_inplay.push(perm.id);
        } else if renewable_produces_color(def, Color::Black) {
            s.renew_inplay.push(perm.id);
        } else if is_fetch(def) && fetch_target {
            s.inplay_fetch.push(perm.id);
        }
    }
    for card in state.hand_of(who) {
        let Some(def) = state.catalog.get(&card.catalog_key) else { continue };
        if !def.is_land() && def.mana_cost().trim() == "0" {
            // Free one-shot black artifact (Lotus Petal). Free renewable artifacts
            // (black moxen) aren't modelled as a step here — if one is ever needed
            // the line falls short and the strategy falls back; these lists don't run them.
            if oneshot_produces_color(def, Color::Black) {
                s.petals_hand.push(card.id);
            }
        } else if def.is_land() {
            if is_fetch(def) && fetch_target {
                s.hand_fetch.push(card.id);
            } else if produces_black(def) {
                if def.enters_tapped() {
                    s.hand_tapped.push(card.id);
                } else {
                    s.hand_untapped.push(card.id);
                }
            }
        } else if is_black_ritual(def) {
            s.rituals.push(card.id);
        }
    }
    s
}

/// The payoff tutor (Personal Tutor) in hand that can reach a Doomsday in the
/// library — the object to actually cast for the tutor line.
fn payoff_tutor_id(state: &SimState, who: PlayerId) -> Option<ObjId> {
    let dd_in_lib: Vec<_> = state
        .library_of(who)
        .filter(|c| c.catalog_key == DOOMSDAY)
        .map(|c| c.id)
        .collect();
    if dd_in_lib.is_empty() {
        return None;
    }
    state.hand_of(who).find_map(|card| {
        let def = state.catalog.get(&card.catalog_key)?;
        let filter = def.library_top_tutor()?;
        let reaches = dd_in_lib.iter().any(|&id| obj_matches(filter, id, state));
        (reaches && single_colored_pip(def.mana_cost()).is_some()).then_some(card.id)
    })
}

/// Emit the guaranteed line that lands Doomsday by `max_turn` as ordered,
/// object-bound steps — or `None` when no such line exists (the same condition as
/// [`deterministic_cast_turn`]). The strategy follows this verbatim instead of
/// re-deriving assembly; minimal in consumables (a petal/ritual is emitted only
/// when renewable black can't reach BBB by the cast turn), so a dig never spends a
/// resource the line needs.
pub fn deterministic_line(state: &SimState, who: PlayerId, max_turn: u32) -> Option<DetLine> {
    let turn = deterministic_cast_turn(state, who, max_turn)?;
    let s = source_ids(state, who);
    let dd = state
        .hand_of(who)
        .find(|o| o.catalog_key == DOOMSDAY)
        .map(|o| o.id);
    let land_drop = state.player(who).lands_played_this_turn < 1;

    // Doomsday staged on the known top (we tutored it there): don't tutor again, and
    // don't crack/play a fetch — a shuffle would scatter the staged top. Develop only
    // non-shuffle mana and draw it. Mirrors det-cast-turn's non-fetch reckoning.
    let staged = dd.is_none() && payoff_known_depth(state, who).is_some();
    let inplay_fetch: &[ObjId] = if staged { &[] } else { s.inplay_fetch.as_slice() };
    let hand_fetch: &[ObjId] = if staged { &[] } else { s.hand_fetch.as_slice() };

    // Renewable black online by the cast turn: in-play renewables + in-play fetches
    // (each cracks into a renewable land) + hand lands brought online by then
    // (untapped/fetch the turn they're played, taplands need an earlier slot).
    let inplay_renew = (s.renew_inplay.len() + inplay_fetch.len()) as u32;
    let hand_untapped = (s.hand_untapped.len() + hand_fetch.len()) as u32;
    let renew_online = inplay_renew
        + online_lands(hand_untapped, s.hand_tapped.len() as u32, land_drop, turn);

    // Consumables only cover what renewable black can't reach by the cast turn.
    let need_after_lands = 3u32.saturating_sub(renew_online);
    let petals_avail = (s.petals_inplay.len() + s.petals_hand.len()) as u32;
    let petals_used = petals_avail.min(need_after_lands);
    let need_after_petals = need_after_lands - petals_used;
    // A ritual converts a 1-black seed into 3, so a single one covers any residual
    // (≤3 black goal) as long as some other source seeds its `B`.
    let ritual_used = need_after_petals > 0 && !s.rituals.is_empty();

    // The line is built by REDUCTION, not a hand-tuned ordering. Land development comes
    // first, in DEPENDENCY order: crack in-play fetches, then make the once-per-turn land
    // drop with FETCHES AHEAD of plain black lands — so the drop produces whatever colour
    // the next cast needs (a fetched Underground Sea pays Personal Tutor's {U}; a Swamp
    // can't). Fetches are skipped while a card is staged on top (a shuffle would scatter
    // it — see `staged` above). The strategy re-derives the line every window, so this
    // sequences the multi-turn line one drop at a time.
    let develop = |steps: &mut Vec<LineStep>| {
        for &f in inplay_fetch {
            steps.push(LineStep::CrackFetch(f));
        }
        for &f in hand_fetch {
            steps.push(LineStep::PlayLand(f)); // crack re-emitted once it's in play
        }
        for &l in &s.hand_untapped {
            steps.push(LineStep::PlayLand(l));
        }
        for &l in &s.hand_tapped {
            steps.push(LineStep::PlayLand(l));
        }
    };

    let mut steps = Vec::new();

    // REDUCTION A — acquire the payoff. Doomsday isn't in hand, but a library-top tutor
    // can stage it: develop the tutor's pip source, cast the tutor, and stop. Doomsday is
    // drawn next turn and the rest of the line is re-derived then. Casting the tutor only
    // AFTER developing reduces the goal to "tutor's pip is payable" — which the fetch-first
    // land development guarantees (no more Swamp-over-Sea).
    if dd.is_none() && !staged {
        if let Some(tid) = payoff_tutor_id(state, who) {
            develop(&mut steps);
            steps.push(LineStep::CastTutor(tid));
            return Some(DetLine { turn, steps });
        }
    }

    // REDUCTION B — assemble BBB, then cast. Doomsday is in hand (or staged and about to be
    // drawn). Develop lands; then petals + a single ritual cover ONLY the black the lands
    // can't reach by the cast turn (the minimal consumable witness, so a dig never spends a
    // resource the line needs). A petal is unspent mana in play (sacrificed only when the
    // cost is paid), so deploy it freely; a ritual empties if unused, so fire one only with
    // Doomsday in hand to spend it on.
    develop(&mut steps);
    for &p in &s.petals_hand {
        steps.push(LineStep::CastPetal(p));
    }
    if dd.is_some() && ritual_used {
        steps.push(LineStep::CastRitual(s.rituals[0]));
    }
    if let Some(d) = dd {
        steps.push(LineStep::CastDoomsday(d));
    }

    Some(DetLine { turn, steps })
}

/// The missing ingredients: which single producer kind, if added, flips an
/// otherwise-insufficient mana position to sufficient. Recomputed, not authored
/// — this is the "sufficient cards" set. (Assumes Doomsday itself is in hand.)
pub fn mana_gap(state: &SimState, who: PlayerId) -> Vec<&'static str> {
    let c = project(state, who);
    // Sufficiency THIS turn (turn 1, full pool) with `du` extra untapped lands /
    // `dp` extra petals (pool) / `dr` extra rituals added to hand.
    let suff = |du: u32, dp: u32, dr: u32| {
        let renew = c.renew_inplay + online_lands(c.untapped_lands + du, c.tapped_lands, c.land_drop, 1);
        // Gap heuristic treats every ritual as Dark-style (one black seed, no generic).
        reach_black(3, renew + c.pool + dp, 0, [c.ritual_gp.iter().sum::<u32>() + dr, 0, 0, 0])
    };
    let mut gap = Vec::new();
    if suff(0, 0, 0) {
        return gap;
    }
    if suff(1, 0, 0) {
        gap.push("land");
    }
    if suff(0, 1, 0) {
        gap.push("petal");
    }
    if suff(0, 0, 1) {
        gap.push("ritual");
    }
    gap
}

/// A `deck → {dd-sufficient-resources}` bundle: counts of black-source units,
/// petals, and rituals that together cast Doomsday. (`lands` are black-source
/// units available this turn, abstracting over in-play vs. land-drop.)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bundle {
    pub lands: u32,
    pub petals: u32,
    pub rituals: u32,
}

/// Derive the minimal sufficient bundles from a deck, capped by the deck's own
/// petal/ritual counts (so the absent "3 petals" bundle is a fact of the list).
pub fn bundles(deck: &[(String, i32, String)]) -> Vec<Bundle> {
    let catalog = libmtg_engine::build_catalog();
    let (mut max_petals, mut max_rituals) = (0u32, 0u32);
    for (name, qty, _) in deck {
        let Some(def) = catalog.get(name.as_str()) else { continue };
        let q = (*qty).max(0) as u32;
        if is_free_black_artifact(def) {
            max_petals += q;
        } else if is_black_ritual(def) {
            max_rituals += q;
        }
    }
    // A ritual is never worth more than one in a *minimal* bundle; ≥3 black units
    // is the most a minimal bundle needs.
    let cap_petals = max_petals.min(3);
    let cap_rituals = max_rituals.min(1);

    let suff = |b: Bundle| reach_black(3, b.lands + b.petals, 0, [b.rituals, 0, 0, 0]);
    let dominated = |b: Bundle, found: &[Bundle]| {
        found.iter().any(|s| {
            *s != b && s.lands <= b.lands && s.petals <= b.petals && s.rituals <= b.rituals && suff(*s)
        })
    };

    let mut out: Vec<Bundle> = Vec::new();
    for lands in 0..=3 {
        for petals in 0..=cap_petals {
            for rituals in 0..=cap_rituals {
                let b = Bundle { lands, petals, rituals };
                if suff(b) && !dominated(b, &out) {
                    // drop any already-found bundle this one dominates
                    out.retain(|s| !(b.lands <= s.lands && b.petals <= s.petals && b.rituals <= s.rituals));
                    out.push(b);
                }
            }
        }
    }
    out
}

// ── Stochastic gap-closing (#7b): expected turns to draw into the gap ─────────
//
// The deterministic layer says what's castable with NO blind draws. When that's
// short, the natural per-turn draw closes the gap stochastically. We model the
// draw HONESTLY as deck-level probability (you know your 60, never the order): the
// expected number of draws to hit one of K "outs" in an N-card library is the
// negative-hypergeometric mean (N+1)/(K+1). This layer only consumes the gap +
// library counts — never peeks at library order — and is kept separate from the
// deterministic reachability. v1 models AGGRESSIVE (not breakneck) play: one draw
// to find each missing piece, assume you can deploy it; it does NOT chain
// speculative lines (e.g. double-ritual Flow State), and it ignores opponent
// disruption (Wasteland) entirely — both deliberately out of scope.

/// Expected number of blind draws to hit the first of `outs` cards in an
/// `library`-card library drawn without replacement — the negative-hypergeometric
/// mean `(N+1)/(K+1)`. `∞` when there are no outs (the gap never closes by drawing).
pub fn expected_draws_to_out(library: u32, outs: u32) -> f64 {
    if outs == 0 {
        return f64::INFINITY;
    }
    (library as f64 + 1.0) / (outs as f64 + 1.0)
}

/// Whether `who` already has the payoff accessible: Doomsday in hand, or a
/// library-top tutor in hand that can fetch one (so no draw is needed to find it).
pub fn payoff_in_hand(state: &SimState, who: PlayerId) -> bool {
    state.hand_of(who).any(|c| c.catalog_key == DOOMSDAY) || payoff_tutor_pip(state, who).is_some()
}

/// The stochastic frontier as library out-counts: `(mana_outs, payoff_outs,
/// library)`. `mana_outs` = library cards whose kind is in the current `mana_gap`
/// (drawing one flips the mana position); `payoff_outs` = Doomsdays + tutors for
/// one still in the library, or 0 if the payoff is already in hand.
fn gap_outs(state: &SimState, who: PlayerId) -> (u32, u32, u32) {
    let library = state.library_of(who).count() as u32;
    let gap = mana_gap(state, who);
    let mana = state
        .library_of(who)
        .filter(|c| {
            state.catalog.get(&c.catalog_key).is_some_and(|def| {
                (gap.contains(&"land") && def.is_land() && produces_black(def) && !def.enters_tapped())
                    || (gap.contains(&"petal") && is_free_black_artifact(def))
                    || (gap.contains(&"ritual") && is_black_ritual(def))
            })
        })
        .count() as u32;
    let payoff = if payoff_in_hand(state, who) {
        0
    } else {
        let dd_ids: Vec<_> = state
            .library_of(who)
            .filter(|c| c.catalog_key == DOOMSDAY)
            .map(|c| c.id)
            .collect();
        state
            .library_of(who)
            .filter(|c| {
                c.catalog_key == DOOMSDAY
                    || state
                        .catalog
                        .get(&c.catalog_key)
                        .and_then(|d| d.library_top_tutor())
                        .is_some_and(|f| dd_ids.iter().any(|&id| obj_matches(f, id, state)))
            })
            .count() as u32
    };
    (mana, payoff, library)
}

/// Crude v1 E[turns-to-Doomsday] (1-based; `1.0` = castable now, `∞` = a brick).
/// The two halves of the resource problem run in PARALLEL (you draw toward both at
/// once), so the estimate is the MAX of their arrival turns:
/// - **mana** — the deterministic BBB-assembly turn if assemblable without draws,
///   else one draw to find a `mana_gap` out (plus deploying it);
/// - **payoff** — `1` if Doomsday/tutor is in hand, else one draw to find it.
///
/// KNOWN v1 LIMITATION (flagged): when a deterministic line exists this returns its
/// floor and does NOT credit stochastic *acceleration* — e.g. "3 lands + DD in
/// hand" reads as turn 3, even though a drawn ritual/petal would cast it sooner, so
/// cantrips are still useful here. Crediting that needs the per-turn cast-probability
/// model (the Ponder keep-vs-shuffle layer), which subsumes this scalar. For now
/// `e_ttd` is a coarse `MulliganMode`-threshold quantity, not a cantrip evaluator.
pub fn e_ttd(state: &SimState, who: PlayerId, max_turn: u32) -> f64 {
    if let Some(t) = deterministic_cast_turn(state, who, max_turn) {
        return t as f64;
    }
    let c = project(state, who);
    let (mana_outs, payoff_outs, library) = gap_outs(state, who);
    let mana_turn = bbb_turn(&c, max_turn, c.pool)
        .map(|t| t as f64)
        .unwrap_or_else(|| 1.0 + expected_draws_to_out(library, mana_outs));
    let payoff_turn = if state.hand_of(who).any(|c| c.catalog_key == DOOMSDAY) {
        1.0 // the payoff itself is in hand — castable as soon as the mana is up
    } else if payoff_tutor_pip(state, who).is_some() {
        // A tutor for the payoff in hand is ONE TURN SLOWER than the payoff itself:
        // cast the tutor (it stages the payoff), then draw it next turn. Without this
        // a tutor-in-hand was scored as a turn-1 payoff, so staging a redundant tutor
        // looked faster than staging Doomsday — the tutor→tutor loop.
        2.0
    } else {
        1.0 + expected_draws_to_out(library, payoff_outs)
    };
    mana_turn.max(payoff_turn)
}

// ── known top-of-library: the unifying cantrip/tutor primitive ────────────────
//
// Personal Tutor (put on top), Ponder (no-shuffle reorder), and Brainstorm (put
// back) are ALL the same effect: arrange KNOWN cards on top of the library. In the
// resources → costs → effects → resources cycle that effect PRODUCES the resource
// "known top-of-library = [chosen card sequence]", and the draw step CONSUMES it
// deterministically (honest — you arranged it, you know it). So the deterministic
// layer walks `known_top` as guaranteed upcoming draws, and the cantrip STRATEGY
// falls out of choosing the arrangement (or shuffling) that minimises E[TTD].

/// Fold a single known incoming card into a `(Caps, has_dd)` accumulator, by the
/// same cost-level classification `project` uses. A fetch in the prefix is treated
/// as an untapped black land (it cracks into one); a known tutor is not modelled
/// (v1: miss-OK).
fn fold_known_card(c: &mut Caps, has_dd: &mut bool, def: &CardDef, key: &str) {
    if key == DOOMSDAY {
        *has_dd = true;
    } else if !def.is_land() && def.mana_cost().trim() == "0" {
        if oneshot_produces_color(def, Color::Black) {
            c.pool += 1;
        } else if renewable_produces_color(def, Color::Black) {
            c.renew_inplay += 1;
        }
    } else if def.is_land() {
        if is_fetch(def) || (produces_black(def) && !def.enters_tapped()) {
            c.untapped_lands += 1;
        } else if produces_black(def) {
            c.tapped_lands += 1;
        } else if produces_nonblack_mana(def) {
            if def.enters_tapped() {
                c.other_tapped += 1;
            } else {
                c.other_untapped += 1;
            }
        }
    } else if is_black_ritual(def) {
        c.ritual_gp[ritual_generic_cost(def)] += 1;
    }
}

/// Earliest deterministic cast turn given `extra_hand` cards that are already in
/// hand THIS turn (Brainstorm's burst — you keep them now) plus a KNOWN
/// top-of-library prefix that arrives over the coming turns. `immediate` is the
/// prefix draw timing, the one real difference between a tutor and a cantrip: a
/// TUTOR (Personal Tutor) only stages the top, so the cards arrive on the natural
/// draws (card `i` on turn `i + 2`, no turn-1 draw); a CANTRIP (Ponder) DRAWS after
/// arranging, so the first card arrives THIS turn (card `i` on turn `i + 1`). Takes
/// the min of the no-prefix lines (`deterministic_cast_turn`, already covering
/// direct + tutor) and the extra-hand/prefix-accelerated line.
fn deterministic_cast_turn_full(
    state: &SimState,
    who: PlayerId,
    extra_hand: &[&str],
    known_top: &[&str],
    immediate: bool,
    max_turn: u32,
) -> Option<u32> {
    let mut base = project(state, who);
    let mut base_dd = state.hand_of(who).any(|c| c.catalog_key == DOOMSDAY);
    for &key in extra_hand {
        if let Some(def) = state.catalog.get(key) {
            fold_known_card(&mut base, &mut base_dd, def, key);
        }
    }
    let mut prefix_line = None;
    for t in 1..=max_turn {
        let seen = if immediate { t as usize } else { t.saturating_sub(1) as usize };
        let mut c = base;
        // Earliest turn Doomsday is in HAND: already held (turn 1); a Doomsday drawn off
        // the prefix (its draw turn); or — a turn LATER — a payoff tutor drawn off the
        // prefix, since it must be cast (paying its pip) to stage Doomsday, which is then
        // drawn the next turn. That +1 is exactly why staging Doomsday is strictly faster
        // than staging a tutor for it — it falls out of the turn count, no special rule.
        let mut payoff_by: Option<u32> = if base_dd { Some(1) } else { None };
        for (i, &key) in known_top.iter().take(seen).enumerate() {
            let Some(def) = state.catalog.get(key) else { continue };
            let draw_turn = i as u32 + if immediate { 1 } else { 2 };
            if key == DOOMSDAY {
                payoff_by = Some(payoff_by.map_or(draw_turn, |p| p.min(draw_turn)));
            } else if let Some(pip) = def.library_top_tutor().and(single_colored_pip(def.mana_cost())) {
                // The tutor is cast the turn it's drawn, so its pip must be online by then
                // (a renewable source — conservative, so we never over-claim a guaranteed
                // line). Doomsday is then staged and drawn the following turn.
                if renew_color_turn(state, who, pip, draw_turn, extra_hand).is_some_and(|s| s <= draw_turn) {
                    let dd_turn = draw_turn + 1;
                    payoff_by = Some(payoff_by.map_or(dd_turn, |p| p.min(dd_turn)));
                }
            } else {
                let mut dummy = false;
                fold_known_card(&mut c, &mut dummy, def, key);
            }
        }
        if payoff_by.is_some_and(|p| p <= t) && bbb_on_turn(&c, t, c.pool) {
            prefix_line = Some(t);
            break;
        }
    }
    match (deterministic_cast_turn_ex(state, who, extra_hand, max_turn), prefix_line) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// `deterministic_cast_turn_full` with no extra in-hand cards — the plain
/// known-top (tutor / Ponder) case.
fn deterministic_cast_turn_with_known(
    state: &SimState,
    who: PlayerId,
    known_top: &[&str],
    immediate: bool,
    max_turn: u32,
) -> Option<u32> {
    deterministic_cast_turn_full(state, who, &[], known_top, immediate, max_turn)
}

/// How many prefix cards actually advance the plan (a black mana source or the
/// payoff/a tutor) — the rest are "bricks" locked onto the top of the library.
fn prefix_useful_count(state: &SimState, who: PlayerId, known_top: &[&str]) -> u32 {
    let _ = who;
    known_top
        .iter()
        .filter(|&&key| {
            key == DOOMSDAY
                || state.catalog.get(key).is_some_and(|d| {
                    (d.is_land() && produces_black(d))
                        || is_fetch(d)
                        || is_free_black_artifact(d)
                        || is_black_ritual(d)
                        || d.library_top_tutor().is_some()
                })
        })
        .count() as u32
}

/// E[TTD] given a KNOWN top-of-library prefix. The strong model the cantrip
/// strategy minimises over: the prefix accelerates the deterministic line if it
/// completes it; otherwise it's a LIABILITY — its brick cards are dead draws
/// locked on top, so they push the stochastic estimate back by their count (the
/// "stuck" penalty that makes shuffling correct). `immediate` = the cantrip/tutor
/// draw-timing (see `deterministic_cast_turn_with_known`). v1 crude: a non-brick
/// prefix card is treated as ≈ a random draw (neutral); the stochastic library
/// size isn't reduced by the prefix.
pub fn e_ttd_with_known_top(
    state: &SimState,
    who: PlayerId,
    known_top: &[&str],
    immediate: bool,
    max_turn: u32,
) -> f64 {
    if let Some(t) = deterministic_cast_turn_with_known(state, who, known_top, immediate, max_turn) {
        return t as f64;
    }
    let base = e_ttd(state, who, max_turn);
    if !base.is_finite() {
        return base;
    }
    let bricks = known_top.len() as f64 - prefix_useful_count(state, who, known_top) as f64;
    base + bricks.max(0.0)
}

// ── P(cast by cutoff turn): the cut-off objective ────────────────────────────
//
// A combo deck doesn't want the lowest EXPECTED cast turn — it wants to cast BY a
// CUTOFF turn `T`, past which P(win) → 0 (you've been Wastelanded / disrupted /
// raced). So the objective is `P(TTD ≤ T)`, NOT `E[TTD]`. Two payoffs: (1) "stuck
// past the cutoff" becomes an automatic `P = 0` rejection (no fuzzy penalty), which
// makes cantrip decisions sharp; (2) `P(combo by T)` is the headline the goldfish
// actually reports.

/// P(at least one of `outs` successes among `draws` cards drawn without
/// replacement from a `library`-card library) — the hypergeometric tail, product
/// form (no factorials).
fn p_at_least_one(library: u32, outs: u32, draws: u32) -> f64 {
    if outs == 0 || draws == 0 {
        return 0.0;
    }
    let (n, k, d) = (library as i64, outs as i64, draws as i64);
    let mut p_none = 1.0f64;
    for i in 0..d {
        let non_out = n - k - i;
        if non_out <= 0 {
            return 1.0; // every remaining card is an out
        }
        p_none *= non_out as f64 / (n - i) as f64;
    }
    1.0 - p_none
}

/// The stochastic side of `P(cast by cutoff)` given a draw budget `draws`: the mana
/// side (deterministic by `cutoff`, else draw a `mana_gap` out) times the payoff
/// side (in hand, else draw it). Crude independence; v1.
///
/// The draw budget is enlarged by the looks our castable cantrips provide
/// (`cantrip_looks`): a cantrip lets you SEE extra cards toward the gap, so it
/// behaves like extra hypergeometric draws. This is what stops `p_cast_by` from
/// being cantrip-blind (treating "find the missing piece" as only the blind natural
/// draws). It is the principled `g`-improvement: every decision keying off
/// `p_cast_by` (mulligan + all selection) now credits a hand's digging.
fn p_cast_stochastic(state: &SimState, who: PlayerId, cutoff: u32, draws: u32) -> f64 {
    let c = project(state, who);
    let (mana_outs, payoff_outs, library) = gap_outs(state, who);
    let draws = draws + cantrip_looks(state, who, cutoff);
    let p_mana = if bbb_turn(&c, cutoff, c.pool).is_some() {
        1.0
    } else {
        p_at_least_one(library, mana_outs, draws)
    };
    let p_payoff = if payoff_in_hand(state, who) {
        1.0
    } else {
        p_at_least_one(library, payoff_outs, draws)
    };
    p_mana * p_payoff
}

/// Whether `who` can produce blue (to cast the blue cantrips) — a blue source in
/// play or playable from hand (a blue land, a fetch that can find one, or a petal).
fn has_blue_source(state: &SimState, who: PlayerId) -> bool {
    let inplay = state.permanents_of(who).any(|p| {
        !p.bf().is_some_and(|bf| bf.tapped)
            && state.def_of(p.id).is_some_and(|d| produces_color(d, Color::Blue))
    });
    let inhand = state.hand_of(who).any(|c| {
        state.catalog.get(&c.catalog_key).is_some_and(|d| {
            (d.is_land()
                && (produces_color(d, Color::Blue)
                    || (is_fetch(d) && library_has_target_color(state, who, Color::Blue))))
                || is_free_artifact_of_color(d, Color::Blue)
        })
    });
    inplay || inhand
}

/// Whether a SHUFFLE source is available (a fetch in hand/play, or a shuffle-cantrip
/// like Ponder in hand) — it refreshes the top of the library so successive cantrips
/// see new cards (otherwise a second look mostly re-sees arranged/put-back cards).
fn has_shuffle_source(state: &SimState, who: PlayerId) -> bool {
    let perm_fetch = state.permanents_of(who).any(|p| state.def_of(p.id).is_some_and(is_fetch));
    let hand = state.hand_of(who).any(|c| {
        state.catalog.get(&c.catalog_key).is_some_and(|d| is_fetch(d) || d.shuffles_on_resolve())
    });
    perm_fetch || hand
}

/// The "looks" our castable cantrips contribute to the find budget. Each cantrip's
/// sight is read structurally (`cards_seen_on_resolve`: Ponder 4 / Brainstorm 3 /
/// Consider 2 / …). NON-ADDITIVE without a shuffle: only the best cantrip counts in
/// full; each additional one nets ~1 (it re-sees arranged/put-back cards) UNLESS a
/// shuffle source (fetch / Ponder) refreshes the top, when they're additive again.
/// Capped by realizable time (~one cantrip per remaining turn before the cutoff) and
/// gated on castability (a blue source). APPROXIMATION — see `p_cast_stochastic`.
fn cantrip_looks(state: &SimState, who: PlayerId, cutoff: u32) -> u32 {
    if !has_blue_source(state, who) {
        return 0;
    }
    let mut seen: Vec<u32> = state
        .hand_of(who)
        .filter_map(|c| {
            let def = state.catalog.get(&c.catalog_key)?;
            (def.digs_on_resolve() && def.is_blue()).then(|| def.cards_seen_on_resolve())
        })
        .collect();
    if seen.is_empty() {
        return 0;
    }
    seen.sort_unstable_by(|a, b| b.cmp(a)); // best (most sight) first
    // Time cap: ~one cantrip per remaining turn before the cutoff cast.
    let elapsed = (state.current_turn as u32).max(1);
    let cap = cutoff.saturating_sub(elapsed).max(1) as usize;
    seen.truncate(cap);
    let shuffle = has_shuffle_source(state, who);
    let mut looks = seen[0];
    for &s in &seen[1..] {
        looks += if shuffle { s } else { 1 };
    }
    looks
}

/// `P(cast Doomsday by turn cutoff)` — THE objective. `1.0` if a deterministic line
/// lands by `cutoff`; else the chance the `cutoff - 1` natural draws close the gap.
pub fn p_cast_by(state: &SimState, who: PlayerId, cutoff: u32) -> f64 {
    p_cast_by_full(state, who, &[], &[], false, cutoff)
}

/// `P(cast by cutoff)` with `extra_hand` cards already in hand this turn
/// (Brainstorm's burst) and a known top-of-library prefix. A deterministic line
/// (with the extras / prefix folded in) → `1.0`; otherwise the prefix's BRICK
/// cards are wasted draws locked on top, shrinking the stochastic budget — so a
/// brick prefix that eats every pre-cutoff draw drives `P → 0` (the "stuck past the
/// cutoff is unacceptable" rejection, automatic). v1 crude: `extra_hand` helps only
/// the deterministic check, not the stochastic gap; the library size isn't reduced.
pub fn p_cast_by_full(
    state: &SimState,
    who: PlayerId,
    extra_hand: &[&str],
    known_top: &[&str],
    immediate: bool,
    cutoff: u32,
) -> f64 {
    if deterministic_cast_turn_full(state, who, extra_hand, known_top, immediate, cutoff).is_some() {
        return 1.0;
    }
    let draws = cutoff.saturating_sub(1);
    let seen = if immediate { cutoff } else { cutoff.saturating_sub(1) };
    let prefix_drawn = seen.min(known_top.len() as u32) as usize;
    let useful = prefix_useful_count(state, who, &known_top[..prefix_drawn]);
    let wasted = prefix_drawn as u32 - useful;
    p_cast_stochastic(state, who, cutoff, draws.saturating_sub(wasted))
}

/// `P(cast by cutoff)` given just a known top-of-library prefix (tutor / Ponder).
pub fn p_cast_by_with_known_top(
    state: &SimState,
    who: PlayerId,
    known_top: &[&str],
    immediate: bool,
    cutoff: u32,
) -> f64 {
    p_cast_by_full(state, who, &[], known_top, immediate, cutoff)
}

// ── The unified SEND objective: P(Doomsday OR car by cutoff) ──────────────────

/// Hypergeometric P(draw ≥ `k` of `outs` in `draws` from `library`).
fn p_at_least_k(library: u32, outs: u32, draws: u32, k: u32) -> f64 {
    if k == 0 {
        return 1.0;
    }
    if outs < k || draws < k {
        return 0.0;
    }
    let (n, m, d) = (library as i64, outs as i64, draws as i64);
    // P(X = 0): every draw is a non-out.
    let mut pj = 1.0f64;
    for i in 0..d {
        let nb = n - m - i;
        if nb <= 0 {
            return 1.0; // outs saturate the draws → certainly ≥ k
        }
        pj *= nb as f64 / (n - i) as f64;
    }
    let mut below = pj; // accumulate P(X < k)
    let mut j = 0i64;
    while j + 1 < k as i64 {
        let den = (j + 1) * (n - m - d + j + 1);
        if den <= 0 {
            break;
        }
        pj = pj * ((m - j) * (d - j)) as f64 / den as f64;
        below += pj;
        j += 1;
    }
    (1.0 - below).clamp(0.0, 1.0)
}

/// A card's contribution to the car line: (is the payoff, is cheap fuel, is a mana source).
fn car_card_roles(state: &SimState, key: &str) -> (bool, bool, bool) {
    if key == FANTASTICAR {
        return (true, false, false);
    }
    let Some(def) = state.catalog.get(key) else {
        return (false, false, false);
    };
    // Fuel: a cheap proactive noncreature — free, a ritual, or mana value ≤ 1
    // (Petal/Bauble/LED/Dark Ritual/cantrip/Thoughtseize). Counterspells are excluded
    // by `is_proactive_noncreature` (no opponent object to target in a goldfish).
    let fuel = is_proactive_noncreature(def)
        && (def.mana_cost().trim() == "0"
            || is_black_ritual(def)
            || mana_value(&ManaCost::parse(def.mana_cost())) <= 1);
    // Mana source toward the {3}: a land, a {0} mana artifact (Petal), or a ritual.
    let mana = def.is_land()
        || (def.mana_cost().trim() == "0" && !def.mana_abilities().is_empty())
        || is_black_ritual(def);
    (false, fuel, mana)
}

/// Stochastic P(pop the car by `cutoff`) given `draws` + cantrip looks: P(have a car) ×
/// P(have ≥3 fuel) × P(have {3} mana), each hypergeometric. Crude independence; v1,
/// mirroring `p_cast_stochastic`. `extra_hand` cards count as already held.
fn p_car_pop_stochastic(
    state: &SimState,
    who: PlayerId,
    extra_hand: &[&str],
    cutoff: u32,
    draws: u32,
) -> f64 {
    let library = state.library_of(who).count() as u32;
    if library == 0 {
        return 0.0;
    }
    let draws = draws + cantrip_looks(state, who, cutoff);

    let (mut have_car, mut hand_fuel, mut hand_mana) = (
        state.permanents_of(who).any(|p| p.catalog_key == FANTASTICAR),
        0u32,
        0u32,
    );
    let hand_iter = state
        .hand_of(who)
        .map(|c| c.catalog_key.clone())
        .chain(extra_hand.iter().map(|s| s.to_string()));
    for key in hand_iter {
        let (p, f, m) = car_card_roles(state, &key);
        have_car |= p;
        hand_fuel += f as u32;
        hand_mana += m as u32;
    }
    let (mut lib_car, mut lib_fuel, mut lib_mana) = (0u32, 0u32, 0u32);
    for c in state.library_of(who) {
        let (p, f, m) = car_card_roles(state, &c.catalog_key);
        lib_car += p as u32;
        lib_fuel += f as u32;
        lib_mana += m as u32;
    }

    let p_car = if have_car { 1.0 } else { p_at_least_one(library, lib_car, draws) };
    let p_fuel = p_at_least_k(library, lib_fuel, draws, 3u32.saturating_sub(hand_fuel));
    let p_mana = p_at_least_k(library, lib_mana, draws, 3u32.saturating_sub(hand_mana));
    p_car * p_fuel * p_mana
}

/// `P(SEND by cutoff)` = P(resolve Doomsday OR pop the car). 1.0 if a deterministic send
/// line exists; otherwise the (roughly independent) OR of the two stochastic estimates.
/// THIS is the single objective the strategy maximises, so digging toward, keeping, and
/// mulliganing for whichever wincon is closer all fall out of it.
pub fn p_send_by(state: &SimState, who: PlayerId, cutoff: u32) -> f64 {
    p_send_by_full(state, who, &[], &[], false, cutoff)
}

pub fn p_send_by_full(
    state: &SimState,
    who: PlayerId,
    extra_hand: &[&str],
    known_top: &[&str],
    immediate: bool,
    cutoff: u32,
) -> f64 {
    if deterministic_send_turn_full(state, who, extra_hand, known_top, immediate, cutoff).is_some() {
        return 1.0;
    }
    let p_dd = p_cast_by_full(state, who, extra_hand, known_top, immediate, cutoff);
    let draws = cutoff.saturating_sub(1);
    let p_car = p_car_pop_stochastic(state, who, extra_hand, cutoff, draws);
    1.0 - (1.0 - p_dd) * (1.0 - p_car)
}

/// `p_send_by` with a known top-of-library prefix (Ponder / a Brainstorm put-back).
pub fn p_send_by_with_known_top(
    state: &SimState,
    who: PlayerId,
    known_top: &[&str],
    immediate: bool,
    cutoff: u32,
) -> f64 {
    p_send_by_full(state, who, &[], known_top, immediate, cutoff)
}

/// `P(pop the car by cutoff)` alone — 1.0 if a deterministic car line exists, else the
/// stochastic estimate. Used to choose between the two plans when NEITHER is guaranteed.
pub fn p_car_pop_by(state: &SimState, who: PlayerId, cutoff: u32) -> f64 {
    if car_pop_turn(state, who, cutoff).is_some() {
        return 1.0;
    }
    p_car_pop_stochastic(state, who, &[], cutoff, cutoff.saturating_sub(1))
}

/// All orderings of `items` (factorial — intended for small slices, e.g. Ponder's
/// top 3).
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

/// A library-ordering decision: keep a chosen ordered prefix on top, or shuffle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TopChoice {
    /// Keep these cards on top, in this order (drawn first-to-last).
    Keep(Vec<String>),
    /// Shuffle the library — discard the known top, reset to deck-average.
    Shuffle,
}

/// The Ponder decision, FALLING OUT of `P(cast by cutoff)`: given the revealed top
/// cards you arrange (then draw) or shuffle away, return the choice that MAXIMISES
/// P(cast by `cutoff`). "Getting closer" (keep an arrangement that raises P) and
/// "getting stuck" (shuffle a brick top, whose keep-P is ≈ 0 past the cutoff) are
/// not hand-coded — they're just which branch wins. Ponder DRAWS after arranging,
/// so the kept arrangement is scored `immediate = true`; the shuffle branch resets
/// to the deck-average `p_cast_by`. `can_shuffle` reflects whether the card offers
/// it (Ponder does; a no-shuffle look does not — `Shuffle` then scores 0).
pub fn best_top_choice(
    state: &SimState,
    who: PlayerId,
    revealed: &[String],
    can_shuffle: bool,
    cutoff: u32,
) -> (TopChoice, f64) {
    let mut best = (
        TopChoice::Shuffle,
        if can_shuffle { p_send_by(state, who, cutoff) } else { 0.0 },
    );
    for perm in permutations(revealed) {
        let keys: Vec<&str> = perm.iter().map(String::as_str).collect();
        let p = p_send_by_with_known_top(state, who, &keys, true, cutoff);
        if p > best.1 {
            best = (TopChoice::Keep(perm), p);
        }
    }
    best
}

/// The 2 least-useful card keys to put back (Brainstorm/Ponder buries): a card is
/// "useful" if it advances the plan (a black mana source, the payoff, or a tutor).
/// Prefers burying real bricks from `pool` (hand ∪ drawn); pads with whatever's
/// left if fewer than 2 bricks exist.
fn worst_two_to_bury<'a>(state: &SimState, who: PlayerId, pool: &[&'a str]) -> Vec<&'a str> {
    // "Useful" for EITHER wincon: a Doomsday piece (prefix_useful_count), OR a car PIECE we
    // actually spend — the car itself, a {0} pop spell (Petal/Bauble/LED), a ritual, or any
    // land (generic mana for the {3}). NOT cmc≥1 cantrips/Thoughtseize: a Brainstorm has done
    // its dig and is buriable (so this keeps Petals/Baubles/cars without hoarding spent
    // diggers — the Doomsday bury still bins the extra Brainstorms). Reactive Force/Daze stay
    // binnable too.
    let useful = |key: &str| {
        if prefix_useful_count(state, who, &[key]) > 0 || key == FANTASTICAR {
            return true;
        }
        state.catalog.get(key).is_some_and(|d| {
            d.is_land()
                || is_black_ritual(d)
                || (!d.is_creature() && !d.is_land() && d.mana_cost().trim() == "0")
        })
    };
    let mut bricks: Vec<&str> = pool.iter().copied().filter(|k| !useful(k)).collect();
    let mut rest: Vec<&str> = pool.iter().copied().filter(|k| useful(k)).collect();
    bricks.append(&mut rest);
    bricks.into_iter().take(2).collect()
}

/// The Brainstorm decision, also falling out of `P(cast by cutoff)`. Brainstorm is
/// `+3 / -2`: draw 3 into hand NOW (the burst — `extra_hand`), then put 2 back on
/// top (`known_top`, drawn on the coming natural turns). v1 keeps all 3 in hand and
/// buries the 2 least-useful cards from hand ∪ drawn (`worst_two_to_bury`); without
/// a shuffle those 2 are dead draws you'll re-draw, so they shrink the stochastic
/// budget. Returns `(buried-in-order, P)`.
pub fn best_brainstorm(
    state: &SimState,
    who: PlayerId,
    drawn: &[String],
    cutoff: u32,
) -> (Vec<String>, f64) {
    let keep: Vec<&str> = drawn.iter().map(String::as_str).collect();
    // The buriable pool is the 3 drawn plus the current hand's cards.
    let hand: Vec<String> = state
        .hand_of(who)
        .map(|c| c.catalog_key.clone())
        .collect();
    let mut pool: Vec<&str> = keep.clone();
    pool.extend(hand.iter().map(String::as_str));
    let buried = worst_two_to_bury(state, who, &pool);
    let p = p_cast_by_full(state, who, &keep, &buried, false, cutoff);
    (buried.iter().map(|s| s.to_string()).collect(), p)
}

// ── Card valuation: the solver's role classification for the strategy ─────────
//
// The strategy (DDGoldfishStrategy) and the goldfish card-evaluator need a single
// "how useful is this card toward casting Doomsday?" verdict. `card_role` is that
// classification, read structurally from card IR (the same private helpers the
// backward planner uses) — no card names except the payoff's own identity. The
// strategy keys its proactive action choice off the role; the evaluator turns it
// into the scalar `dd_card_value` that drives the engine's evaluator-defaulted
// decisions (Brainstorm bury / scry / surveil).

/// What role a card plays toward casting Doomsday fast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CardRole {
    /// Doomsday itself — the payoff.
    Payoff,
    /// A library-top tutor that can fetch the payoff (Personal/Vampiric Tutor).
    PayoffTutor,
    /// A ritual spell that adds black on resolution (Dark Ritual).
    Ritual,
    /// A free mana artifact that makes black (Lotus Petal).
    Petal,
    /// A fetch land that can find a black source from the current library.
    Fetch,
    /// An untapped black-producing land (Underground Sea, Swamp).
    BlackLandUntapped,
    /// A black-producing land that enters tapped (surveil dual).
    BlackLandTapped,
    /// A cantrip / dig spell (Ponder, Brainstorm, Consider, Flow State, Preordain).
    Cantrip,
    /// A blue (non-black) mana source — useful only to cast cantrips/tutors.
    BlueSource,
    /// Anything else: dead weight pre-Doomsday.
    Other,
}

/// Classify `id`'s role toward casting Doomsday (structural, from IR). Fetches and
/// payoff-tutors are grounded against the current library (a fetch with no black
/// target, or a tutor that can't reach a Doomsday, is just `Other`).
pub fn card_role(state: &SimState, who: PlayerId, id: ObjId) -> CardRole {
    let Some(obj) = state.objects.get(&id) else { return CardRole::Other };
    if obj.catalog_key == DOOMSDAY {
        return CardRole::Payoff;
    }
    let Some(def) = state.catalog.get(&obj.catalog_key) else { return CardRole::Other };
    if let Some(filter) = def.library_top_tutor() {
        let finds_dd = state
            .library_of(who)
            .any(|c| c.catalog_key == DOOMSDAY && obj_matches(filter, c.id, state));
        if finds_dd {
            return CardRole::PayoffTutor;
        }
    }
    if is_black_ritual(def) {
        return CardRole::Ritual;
    }
    if is_free_black_artifact(def) {
        return CardRole::Petal;
    }
    if def.is_land() {
        if is_fetch(def) {
            return if library_has_black_target(state, who) {
                CardRole::Fetch
            } else {
                CardRole::Other
            };
        }
        if produces_black(def) {
            return if def.enters_tapped() {
                CardRole::BlackLandTapped
            } else {
                CardRole::BlackLandUntapped
            };
        }
        if produces_color(def, Color::Blue) {
            return CardRole::BlueSource;
        }
        return CardRole::Other;
    }
    if (def.is_instant() || def.is_sorcery()) && def.digs_on_resolve() {
        return CardRole::Cantrip;
    }
    // A free artifact that taps for blue but not black — still a blue source.
    if def.mana_cost().trim() == "0" && produces_color(def, Color::Blue) {
        return CardRole::BlueSource;
    }
    CardRole::Other
}

/// REFERENCE heuristic value table — retained ONLY for A/B decision comparison
/// (`DDGoldfishStrategy`'s compare mode diffs the principled policy against it).
/// The strategy's actual play decisions do **not** consult this; they use the
/// objective (`min_ttd` / `p_cast_by` / `mana_gap`). Kept so we can see exactly
/// where the tuned table and the principled policy disagree while debugging.
pub fn dd_card_value(state: &SimState, who: PlayerId, id: ObjId) -> f64 {
    match card_role(state, who, id) {
        CardRole::Payoff => {
            let secured = state.hand_of(who).any(|c| c.id != id && c.catalog_key == DOOMSDAY);
            if secured { 0.15 } else { 1.0 }
        }
        CardRole::PayoffTutor => 0.95,
        CardRole::BlackLandUntapped => 0.85,
        CardRole::Fetch => 0.82,
        CardRole::Ritual => 0.80,
        CardRole::Petal => 0.78,
        CardRole::BlackLandTapped => 0.70,
        CardRole::Cantrip => 0.50,
        CardRole::BlueSource => 0.40,
        CardRole::Other => 0.05,
    }
}

// ── min-TTD: the optimistic "if everything goes perfectly" cast turn ──────────
//
// Three distinct quantities (see the dd-goldfish-strategy charter):
//   • ttd       — the random variable (E[ttd], P(ttd ≤ K); see `p_cast_by`/`e_ttd`).
//   • det-ttd   — the GUARANTEED turn using no draws (`deterministic_cast_turn`).
//   • min-ttd   — the OPTIMISTIC turn assuming the most helpful cards the library
//                 actually contains are drawn on schedule. A scalar bound, and
//                 always ≤ det-ttd (a lucky draw only helps). 3 black lands +
//                 Doomsday is det-ttd 3 but min-ttd 2 (a drawn petal/ritual).
//
// min-ttd is the feasibility gate: `min-ttd > cutoff` ⟹ even perfect luck can't
// cast by the cutoff ⟹ P(ttd ≤ cutoff) = 0, so a keep is dead (bin/shuffle and hope
// for a hand whose min-ttd ≤ cutoff). The gap det-ttd − min-ttd is the value of luck
// (and thus of digging).

/// One representative library card per *helpful* kind, in acceleration priority
/// (payoff → ritual → petal → untapped black / fetch → tapped black). These are the
/// cards a favorable draw would supply; used to compute the optimistic min-ttd.
fn helpful_library_reps(state: &SimState, who: PlayerId) -> Vec<String> {
    let need_payoff = !payoff_in_hand(state, who);
    let (mut pay, mut rit, mut pet, mut ub, mut tb): (
        Option<String>, Option<String>, Option<String>, Option<String>, Option<String>,
    ) = (None, None, None, None, None);
    for card in state.library_of(who) {
        match card_role(state, who, card.id) {
            CardRole::Payoff | CardRole::PayoffTutor if need_payoff => {
                pay.get_or_insert_with(|| card.catalog_key.clone());
            }
            CardRole::Ritual => { rit.get_or_insert_with(|| card.catalog_key.clone()); }
            CardRole::Petal => { pet.get_or_insert_with(|| card.catalog_key.clone()); }
            CardRole::BlackLandUntapped | CardRole::Fetch => {
                ub.get_or_insert_with(|| card.catalog_key.clone());
            }
            CardRole::BlackLandTapped => { tb.get_or_insert_with(|| card.catalog_key.clone()); }
            _ => {}
        }
    }
    [pay, rit, pet, ub, tb].into_iter().flatten().collect()
}

/// The optimistic min-ttd (≤ `max_turn`, else `None`): the earliest turn Doomsday is
/// castable if the most helpful library cards arrive as natural draws. Reuses the
/// validated no-draw reachability with those cards staged as a known top
/// (`deterministic_cast_turn_with_known` already mins against the no-draw line).
/// The staging order is the acceleration priority (an internal bound computation,
/// not a play decision); if imperfect it only over-estimates min-ttd — still a valid
/// optimistic bound.
pub fn min_ttd(state: &SimState, who: PlayerId, max_turn: u32) -> Option<u32> {
    let reps = helpful_library_reps(state, who);
    let seq: Vec<&str> = reps.iter().map(String::as_str).collect();
    deterministic_cast_turn_with_known(state, who, &seq, false, max_turn)
}

const TAMIYO: &str = "Tamiyo, Inquisitive Student";

/// Earliest turn (1-based, on the play) by which the hand can DETERMINISTICALLY flip
/// Tamiyo, Inquisitive Student — a separate "plan" the mulligan can recognize even though
/// the pilot never plays it out. The flip triggers on drawing your **3rd card in a turn**
/// while she's in play (`catalog::tamiyo_check`). Only the draw COUNT matters, not card
/// identity, so the natural draw step and free cyclers (Street Wraith) count toward it and
/// no random draw is relied upon. `None` if Tamiyo isn't in hand or no flip lands by
/// `max_turn`.
///
/// Mana model (on the play, one land drop per turn, blue lands played first): a blue land
/// gives renewable {U} every turn it is online — untapped lands the same turn, taplands the
/// next; a fetch is treated as an untapped blue source; Lotus Petal is a one-shot {U}.
/// Each cast-to-draw cantrip costs one blue pip (Brainstorm draws 3, Ponder/Consider 1);
/// free cyclers cost no mana.
///
/// METHOD NOTE: unlike [`deterministic_cast_turn`], this is NOT a principled backward
/// plan-builder over the resource model — it forward-counts draws against a hard-coded
/// blue-mana/turn model (the assumptions above). That shortcut is acceptable because the
/// pilot never *executes* the Tamiyo plan (it always races Doomsday); the mulligan only
/// needs to know the plan exists and roughly how fast, not produce a playable line. If we
/// ever wanted to play Tamiyo out, this would have to be rebuilt the DD way. Known
/// imprecisions: a fetch is assumed to reach a blue source, and Lion's Eye Diamond's
/// hand-discard cost is ignored (it reads as a one-shot {U}).
pub fn tamiyo_flip_turn(state: &SimState, who: PlayerId, max_turn: u32) -> Option<u32> {
    if !state.hand_of(who).any(|c| c.catalog_key == TAMIYO) {
        return None;
    }
    let (mut fast_blue, mut tap_blue, mut petals, mut free_draws) = (0u32, 0u32, 0u32, 0u32);
    let mut cantrip_draws: Vec<u32> = Vec::new();
    for card in state.hand_of(who) {
        let Some(def) = state.catalog.get(&card.catalog_key) else { continue };
        if def.is_land() {
            if is_fetch(def) {
                fast_blue += 1; // a fetch finds an untapped blue dual (assumed available)
            } else if renewable_produces_color(def, Color::Blue) {
                if def.enters_tapped() { tap_blue += 1; } else { fast_blue += 1; }
            }
        } else if oneshot_produces_color(def, Color::Blue) {
            petals += 1; // Lotus Petal: one-shot {U}
        } else if def.cards_drawn_on_resolve() > 0 {
            cantrip_draws.push(def.cards_drawn_on_resolve()); // a {U} cast-to-draw cantrip
        }
        if def.has_cycling_draw() { free_draws += 1; } // Street Wraith: a free draw
    }
    cantrip_draws.sort_unstable_by(|a, b| b.cmp(a)); // biggest draw (Brainstorm) first

    // Renewable blue from lands available on turn t (untapped count from t, taplands from t-1).
    let land_blue = |t: u32| fast_blue.min(t) + tap_blue.min(t.saturating_sub(1));
    // Earliest turn we can pay {U} for Tamiyo — off a land if one is online, else a petal.
    let deploy = (1..=max_turn).find(|&t| land_blue(t) >= 1 || petals >= 1)?;
    let deploy_off_land = land_blue(deploy) >= 1;
    // A petal is permanently spent on deploy when no land is available to cast her.
    let petal_pool = petals.saturating_sub(if deploy_off_land { 0 } else { 1 });

    for f in deploy..=max_turn {
        let natural = if f >= 2 { 1 } else { 0 }; // on the play: no turn-1 draw step
        // Tamiyo's land pip is consumed only on the turn she is deployed; afterward the
        // land untaps free for cantrips.
        let land_pip = if f == deploy && deploy_off_land { 1 } else { 0 };
        let blue = land_blue(f).saturating_sub(land_pip) + petal_pool;
        let paid: u32 = cantrip_draws.iter().take(blue as usize).sum();
        if natural + paid + free_draws >= 3 {
            return Some(f);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use libmtg_engine::{build_catalog, PlayerState, Zone};

    /// Build an opening-hand state (hand in hand, the rest of the sample deck in library)
    /// and return the earliest deterministic Tamiyo-flip turn.
    fn flip_turn(hand: &[&str]) -> Option<u32> {
        let names: Vec<String> = crate::sample_doomsday_deck().iter()
            .flat_map(|(n, q, _)| std::iter::repeat(n.clone()).take((*q).max(0) as usize))
            .collect();
        let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        s.catalog = build_catalog();
        let mut rest = names.clone();
        for &h in hand { if let Some(p) = rest.iter().position(|n| n == h) { rest.remove(p); } }
        for &h in hand { s.place_card(PlayerId::Us, h, Zone::Hand { known: false }); }
        for n in &rest { s.place_card(PlayerId::Us, n, Zone::Library); }
        tamiyo_flip_turn(&s, PlayerId::Us, 4)
    }

    /// The former standalone black-only reachability, kept here verbatim to pin
    /// equivalence with the generic `reach_mana` it was replaced by. For the combo
    /// regime (need ≤ 3, Dark-Ritual-shaped rituals) the two must agree exactly.
    fn reach_black_old(need: i32, black: u32, other: u32, ritual_gp: [u32; 4]) -> bool {
        if need <= 0 {
            return true;
        }
        if black > 0 && reach_black_old(need - 1, black - 1, other, ritual_gp) {
            return true;
        }
        if need <= 3 {
            for g in 0..ritual_gp.len() {
                if ritual_gp[g] == 0 {
                    continue;
                }
                let g = g as u32;
                let from_other = g.min(other);
                let seed = 1 + (g - from_other);
                let mut rg = ritual_gp;
                rg[g as usize] -= 1;
                if reach_black_old(seed as i32, black, other - from_other, rg) {
                    return true;
                }
            }
        }
        false
    }

    #[test]
    fn reach_black_parity_with_generic_solver() {
        // Exhaustive over the BBB combo regime: need ≤ 3, black/other ≤ 8, Dark
        // Ritual (g=0) ≤ 4 and a Cabal-shaped g=1 ritual ≤ 2. The new black helper
        // (→ reach_mana) must equal the old standalone implementation everywhere.
        for need in 0..=3i32 {
            for black in 0..=8u32 {
                for other in 0..=6u32 {
                    for d in 0..=4u32 {
                        for c1 in 0..=2u32 {
                            let rg = [d, c1, 0, 0];
                            assert_eq!(
                                reach_black(need, black, other, rg),
                                reach_black_old(need, black, other, rg),
                                "mismatch at need={need} black={black} other={other} rg={rg:?}",
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn mana_cost_parse_and_generic_reach() {
        assert_eq!(ManaCost::parse("BBB"), ManaCost { generic: 0, pips: [0, 0, 3, 0, 0] });
        assert_eq!(ManaCost::parse("3"), ManaCost { generic: 3, pips: [0; 5] });
        assert_eq!(ManaCost::parse("1B"), ManaCost { generic: 1, pips: [0, 0, 1, 0, 0] });
        // {3} generic: any 3 mana pays it — 3 non-black sources suffice, 2 don't.
        assert!(reach_mana(0, 3, 0, 3, [0; 4]));
        assert!(!reach_mana(0, 3, 0, 2, [0; 4]));
        // {3} via a Dark Ritual off one black source; unseedable with no black.
        assert!(reach_mana(0, 3, 1, 0, [1, 0, 0, 0]));
        assert!(!reach_mana(0, 3, 0, 0, [1, 0, 0, 0]));
    }

    /// Build a state with `hand` in hand (rest of the library empty — these tests
    /// only exercise the hand+board solver, no draws).
    fn hand_state(hand: &[&str]) -> SimState {
        let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        s.catalog = build_catalog();
        for &h in hand {
            s.place_card(PlayerId::Us, h, Zone::Hand { known: false });
        }
        s
    }

    fn abbrev(name: &str) -> &str {
        match name {
            "The Fantasticar" => "car", "Doomsday" => "dd", "Dark Ritual" => "ritual",
            "Lotus Petal" => "petal", "Mishra's Bauble" => "bauble", "Lion's Eye Diamond" => "led",
            "Thoughtseize" => "ts", "Brainstorm" => "bs", "Ponder" => "ponder",
            "Consider" => "consider", "Edge of Autumn" => "edge", "Street Wraith" => "wraith",
            "Force of Will" => "fow", "Daze" => "daze", "Thassa's Oracle" => "oracle",
            "Underground Sea" => "sea", "Island" => "island", "Swamp" => "swamp",
            "Undercity Sewers" => "sewers", "Cavern of Souls" => "cavern",
            "Polluted Delta" | "Verdant Catacombs" | "Flooded Strand" | "Misty Rainforest"
            | "Scalding Tarn" => "fetch",
            other => other,
        }
    }

    /// DEBUG (`cargo test debug_sample_car_hands -- --ignored --nocapture`): deal vroomsday
    /// opening 7s and report the per-payoff pop cadence + a sample of car hands, to see
    /// where the car line is (or isn't) found.
    #[test]
    #[ignore]
    fn debug_sample_car_hands() {
        use rand::seq::SliceRandom;
        use rand::SeedableRng;
        let names: Vec<String> = crate::vroomsday_deck().iter()
            .flat_map(|(n, q, _)| std::iter::repeat(n.clone()).take((*q).max(0) as usize))
            .collect();
        let mut rng = rand::rngs::SmallRng::seed_from_u64(7);
        let cat = build_catalog();

        // `car_pop_turn`/`deterministic_cast_turn` over the first `sz` cards of `cards`
        // (as if all `sz` were in hand) — a cheap proxy for "by T3 after drawing `sz-7`
        // more cards while making land drops". Not a real sim (it doesn't gate drawn
        // cards by turn), but an upper bound on what a cantrip strategy could reach.
        let pop_by = |cards: &[String], sz: usize| -> bool {
            let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
            s.catalog = cat.clone();
            for n in &cards[..sz] { s.place_card(PlayerId::Us, n, Zone::Hand { known: false }); }
            car_pop_turn(&s, PlayerId::Us, 3).is_some()
        };
        let cast_by = |cards: &[String], sz: usize| -> bool {
            let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
            s.catalog = cat.clone();
            for n in &cards[..sz] { s.place_card(PlayerId::Us, n, Zone::Hand { known: false }); }
            deterministic_cast_turn(&s, PlayerId::Us, 3).is_some()
        };

        const SIZES: [usize; 3] = [7, 9, 11]; // opening 7, +2 natural draws, +cantrips
        let mut car_pop = [0u32; 3];
        let mut dd_cast = [0u32; 3];
        let (mut car_hands, mut dd_hands) = (0u32, 0u32);
        let mut shown = 0;

        println!("\n=== sample car hands (car @ 7|9|11 cards = poppable by T3?) ===");
        for _ in 0..30000 {
            let mut sh = names.clone();
            sh.shuffle(&mut rng);
            let open7 = &sh[..7];
            let car_in_open = open7.iter().any(|n| n == "The Fantasticar");
            let dd_in_open = open7.iter().any(|n| n == "Doomsday");
            if car_in_open {
                car_hands += 1;
                for (i, &sz) in SIZES.iter().enumerate() {
                    if pop_by(&sh, sz) { car_pop[i] += 1; }
                }
                if shown < 30 {
                    let flag = |b: bool| if b { 'Y' } else { '·' };
                    let mut h: Vec<&str> = open7.iter().map(|n| abbrev(n)).collect();
                    h.sort();
                    println!("[{}{}{}] {}",
                        flag(pop_by(&sh, 7)), flag(pop_by(&sh, 9)), flag(pop_by(&sh, 11)), h.join(" "));
                    shown += 1;
                }
            }
            if dd_in_open {
                dd_hands += 1;
                for (i, &sz) in SIZES.iter().enumerate() {
                    if cast_by(&sh, sz) { dd_cast[i] += 1; }
                }
            }
        }
        let pct = |n: u32, d: u32| 100.0 * n as f64 / d.max(1) as f64;
        println!("\n=== P(send by T3 | payoff in opening 7), as the effective hand grows ===");
        println!("            7 cards   9 cards  11 cards   (proxy for draws/cantrips by T3)");
        println!("car  pop:  {:6.1}%  {:6.1}%  {:6.1}%   (n={})",
            pct(car_pop[0], car_hands), pct(car_pop[1], car_hands), pct(car_pop[2], car_hands), car_hands);
        println!("DD   cast: {:6.1}%  {:6.1}%  {:6.1}%   (n={})",
            pct(dd_cast[0], dd_hands), pct(dd_cast[1], dd_hands), pct(dd_cast[2], dd_hands), dd_hands);
    }

    #[test]
    fn car_pop_turn_with_car_in_play() {
        // Car already out + four free noncreature spells → pop this turn (no mana).
        let mut s = hand_state(&["Mishra's Bauble", "Mishra's Bauble", "Lotus Petal", "Lotus Petal"]);
        s.place_card(PlayerId::Us, "The Fantasticar", Zone::Battlefield);
        assert_eq!(car_pop_turn(&s, PlayerId::Us, 4), Some(1));
    }

    #[test]
    fn car_pop_turn_car_in_hand_t1_off_petals() {
        // Car + 3 Petals + 1 black land: land + two sacked petals = {3} for the car,
        // then the third petal is the 4th noncreature spell → pop on T1.
        let s = hand_state(&[
            "The Fantasticar", "Lotus Petal", "Lotus Petal", "Lotus Petal", "Underground Sea",
        ]);
        assert_eq!(car_pop_turn(&s, PlayerId::Us, 4), Some(1));
    }

    #[test]
    fn car_pop_turn_car_in_hand_three_baubles_t3() {
        // Car + 3 Baubles (no mana of their own) + 3 lands: cast the car off three land
        // drops on T3, the three baubles fill the count → pop T3 (on the play).
        let s = hand_state(&[
            "The Fantasticar", "Mishra's Bauble", "Mishra's Bauble", "Mishra's Bauble",
            "Underground Sea", "Swamp", "Island",
        ]);
        assert_eq!(car_pop_turn(&s, PlayerId::Us, 4), Some(3));
    }

    #[test]
    fn car_pop_turn_three_petals_no_land_fails() {
        // Car + 3 Petals, no land: making {3} sacks all three petals, which forces the
        // car to be the 4th spell (on the stack → no pop); reserving one leaves only {2}.
        let s = hand_state(&["The Fantasticar", "Lotus Petal", "Lotus Petal", "Lotus Petal"]);
        assert_eq!(car_pop_turn(&s, PlayerId::Us, 4), None);
    }

    #[test]
    fn car_pop_turn_needs_car_and_enough_free() {
        // No car at all → None.
        assert_eq!(
            car_pop_turn(&hand_state(&["Lotus Petal", "Lotus Petal", "Lotus Petal", "Lotus Petal"]), PlayerId::Us, 4),
            None
        );
        // Car but only two free spells → None.
        assert_eq!(
            car_pop_turn(&hand_state(&["The Fantasticar", "Lotus Petal", "Mishra's Bauble", "Underground Sea"]), PlayerId::Us, 4),
            None
        );
    }

    /// Concrete vroomsday hands with a car, the pop turn worked by hand. These pin the
    /// richer fuel model: the four noncreature spells can be a ritual, Thoughtseize, a
    /// cantrip, Baubles, Petals — not just {0} spells. (On the play, no draws.)
    #[test]
    fn car_pop_sample_hands() {
        let car = |h: &[&str]| car_pop_turn(&hand_state(h), PlayerId::Us, 4);

        // Ritual → BBB, Car, Petal, Petal — four casts, car in play for the 4th. T1.
        assert_eq!(car(&["Underground Sea", "Dark Ritual", "The Fantasticar",
                         "Lotus Petal", "Lotus Petal", "Mishra's Bauble", "Mishra's Bauble"]), Some(1));

        // Cast 2 Petals for mana, Car, then a Petal as the 4th. T1.
        assert_eq!(car(&["Underground Sea", "The Fantasticar", "Lotus Petal", "Lotus Petal",
                         "Lotus Petal", "Mishra's Bauble", "Brainstorm"]), Some(1));

        // 2 lands + Ritual fund Car{3} + Thoughtseize{B}; Bauble is the 4th. T2 (needs the
        // 2nd land for Thoughtseize). Force of Will is NOT castable fuel (needs a target).
        assert_eq!(car(&["Underground Sea", "Underground Sea", "Dark Ritual", "Thoughtseize",
                         "The Fantasticar", "Mishra's Bauble", "Force of Will"]), Some(2));

        // Same shape with a cantrip ({U}) as the paid filler instead of Thoughtseize. T2.
        assert_eq!(car(&["Underground Sea", "Underground Sea", "Dark Ritual", "Brainstorm",
                         "The Fantasticar", "Mishra's Bauble", "Force of Will"]), Some(2));

        // No ritual/petal: cast Car off three land drops, three Baubles fill the count. T3.
        assert_eq!(car(&["Underground Sea", "Swamp", "Island", "The Fantasticar",
                         "Mishra's Bauble", "Mishra's Bauble", "Mishra's Bauble"]), Some(3));

        // Car + one Bauble + Force (no target) + 4 lands → only two castable noncreature
        // spells (Car, Bauble); can never reach four. None.
        assert_eq!(car(&["Underground Sea", "Underground Sea", "Underground Sea", "Underground Sea",
                         "The Fantasticar", "Mishra's Bauble", "Force of Will"]), None);
    }

    // The user's two acceptance examples for the unified, re-planned deterministic engine.
    #[test]
    fn replan_finds_faster_line_on_new_info() {
        // "3 useas dd, draw ritual for t2 → cast doomsday t2." Three Seas alone cast
        // Doomsday on T3 (three land drops → BBB); drawing a Dark Ritual for T2 re-plans
        // to a T2 cast — found deterministically by folding the drawn card in.
        let s = hand_state(&["Underground Sea", "Underground Sea", "Underground Sea", "Doomsday"]);
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), Some(3));
        assert_eq!(
            deterministic_cast_turn_full(&s, PlayerId::Us, &[], &["Dark Ritual"], false, 3),
            Some(2)
        );
    }

    #[test]
    fn put_back_finds_car_in_revealed_pieces() {
        // "land ritual brainstorm X Y Z W, brainstorm into petal car petal → car the turn
        // after." Keeping the revealed petal/car/petal (extra_hand) must DETERMINISTICALLY
        // find the car line: ritual → BBB, car, petal, petal = four casts.
        let s = hand_state(&["Underground Sea", "Dark Ritual", "Force of Will", "Daze",
                             "Force of Will", "Daze"]);
        assert_eq!(car_pop_turn(&s, PlayerId::Us, 3), None, "no car yet → nothing");
        let kept = ["Lotus Petal", "The Fantasticar", "Lotus Petal"];
        assert_eq!(car_pop_turn_full(&s, PlayerId::Us, &kept, &[], false, 3, 0), Some(1));
        assert_eq!(deterministic_send_turn_full(&s, PlayerId::Us, &kept, &[], false, 3), Some(1));
    }

    /// The SOLVER now finds the self-Daze car line (engine `Who::Any` + the self-counter fuel
    /// model). T1: Sea→B, ritual(1)→BBB, car(2), petal(3, hold priority), Daze(4) on the petal
    /// — paid by bouncing the Underground Sea. KEY: Underground Sea is "Land — Island Swamp",
    /// so it IS an Island, legal to bounce for Daze's free alt-cost (bounce works on the tapped
    /// Sea; we no longer need the land). (Strategy EXECUTION of this line is Part 3, separate.)
    #[test]
    fn car_line_via_self_daze() {
        let s = hand_state(&["Underground Sea", "Dark Ritual", "The Fantasticar",
                             "Lotus Petal", "Daze"]);
        assert_eq!(car_pop_turn(&s, PlayerId::Us, 3), Some(1),
            "Daze on our own petal, paid by bouncing the Sea (an Island), is the 4th cast");
    }

    /// The "almost sends car" signal: `car_pop_shortfall` = how many extra Lotus-Petal-
    /// equivalents (each +1 cast AND +1 mana) the hand needs to make a deterministic pop by
    /// the cutoff. 0 = deterministic; 1 = one piece away (a cantrip very likely digs it); 2+ = far.
    #[test]
    fn car_pop_shortfall_one_away_for_ponder_hand() {
        // Deterministic self-Daze hand → shortfall 0 (agrees with `car_pop_turn`).
        let det = hand_state(&["Underground Sea", "Dark Ritual", "The Fantasticar",
                               "Lotus Petal", "Daze"]);
        assert_eq!(car_pop_shortfall(&det, PlayerId::Us, 3), 0);
        // Swap the Petal for a Ponder: ritual+car gets the car out, but post-car there's only
        // the Daze (the Ponder is uncastable with the mana spent) — ONE cast/mana short. A
        // single Petal/Bauble/land (which Ponder digs) closes it → shortfall 1.
        let almost = hand_state(&["Underground Sea", "Dark Ritual", "The Fantasticar",
                                  "Ponder", "Daze"]);
        assert_eq!(car_pop_turn(&almost, PlayerId::Us, 3), None, "not deterministic");
        assert_eq!(car_pop_shortfall(&almost, PlayerId::Us, 3), 1, "exactly one piece short");
        // A bare car with no fuel/mana is far from a pop.
        let far = hand_state(&["The Fantasticar"]);
        assert!(car_pop_shortfall(&far, PlayerId::Us, 3) >= 2);
    }

    /// Boundary: a Daze is the deciding 4th cast ONLY if there's an Island to bounce for its
    /// free cost. Island in play → pop; a lone Swamp (not an Island, no {1}{U}) → one short.
    #[test]
    fn daze_self_counter_needs_an_island() {
        let with_island = setup(&["The Fantasticar", "Island"],
            &["Mishra's Bauble", "Mishra's Bauble", "Mishra's Bauble", "Daze"], &[]);
        assert!(can_pop_this_turn(&with_island, PlayerId::Us),
            "Daze bounces the Island → four casts");
        let only_swamp = setup(&["The Fantasticar", "Swamp"],
            &["Mishra's Bauble", "Mishra's Bauble", "Mishra's Bauble", "Daze"], &[]);
        assert!(!can_pop_this_turn(&only_swamp, PlayerId::Us),
            "no Island to bounce → Daze uncastable, one cast short");
    }

    /// Boundary: a Force of Will is the deciding 4th cast ONLY if there's a spare blue card to
    /// pitch (a blue creature is pitch fodder but not itself a noncreature cast).
    #[test]
    fn force_self_counter_needs_blue_to_pitch() {
        let with_pitch = setup(&["The Fantasticar"], &["Mishra's Bauble", "Mishra's Bauble",
            "Mishra's Bauble", "Force of Will", "Tamiyo, Inquisitive Student"], &[]);
        assert!(can_pop_this_turn(&with_pitch, PlayerId::Us),
            "Force pitches the blue card → four casts");
        let no_pitch = setup(&["The Fantasticar"],
            &["Mishra's Bauble", "Mishra's Bauble", "Mishra's Bauble", "Force of Will"], &[]);
        assert!(!can_pop_this_turn(&no_pitch, PlayerId::Us),
            "no blue card to pitch → Force uncastable, one cast short");
    }

    /// BASELINE ORACLE for the pop-discipline rule (`solve_pop`): explicit resource tuples →
    /// expected feasibility. Pins the behavior validated end-to-end at P(send|only car)=99%,
    /// so the resource-solver refactor cannot silently change it. Each case names the line.
    #[test]
    fn solve_pop_contract() {
        let fuel = |p: u32, o: u32, r: u32, paid: &[u32]| CarFuel {
            free_petal: p, free_other: o, rituals: r, paid: paid.to_vec(),
        };
        // --- car in hand ---
        // 3 lands fund the {3}: car is the 1st cast, then 3 baubles → pop.
        assert!(solve_pop(3, 0, &fuel(0, 3, 0, &[]), false));
        // THE BUG it prevents: a dig cantrip already cast (s=1), 0 base, petal+ritual ramp
        // (2 casts) → the car would be the 4th spell → must NOT fire this turn.
        assert!(!solve_pop(0, 1, &fuel(1, 1, 1, &[]), false));
        // Same pieces on a FRESH turn: petal(1), ritual(2), car(3), bauble(4) → pop.
        assert!(solve_pop(0, 0, &fuel(1, 1, 1, &[]), false));
        // Only {1} of mana → can't even cast the {3} car.
        assert!(!solve_pop(0, 0, &fuel(1, 0, 0, &[]), false));
        // 3 noncreature spells already cast → the car can only be the 4th → no pop.
        assert!(!solve_pop(5, 3, &fuel(0, 4, 0, &[]), false));
        // --- car already in play ---
        assert!(solve_pop(0, 0, &fuel(0, 4, 0, &[]), true)); // 4 free casts → pop
        assert!(!solve_pop(0, 0, &fuel(0, 3, 0, &[]), true)); // only 3 → one short
        assert!(solve_pop(0, 2, &fuel(0, 2, 0, &[]), true)); // 2 already + 2 more → pop
        assert!(!solve_pop(0, 4, &fuel(0, 5, 0, &[]), true)); // the 4th cast already happened
    }

    #[test]
    fn deterministic_send_turn_takes_the_faster_wincon() {
        // Doomsday line, no car → send == the Doomsday turn.
        let dd = hand_state(&["Doomsday", "Dark Ritual", "Underground Sea", "Lotus Petal"]);
        assert!(car_pop_turn(&dd, PlayerId::Us, 4).is_none());
        assert_eq!(
            deterministic_send_turn(&dd, PlayerId::Us, 4),
            deterministic_cast_turn(&dd, PlayerId::Us, 4)
        );
        // Car line, no Doomsday → send == the car turn.
        let car = hand_state(&[
            "The Fantasticar", "Lotus Petal", "Lotus Petal", "Lotus Petal", "Underground Sea",
        ]);
        assert!(deterministic_cast_turn(&car, PlayerId::Us, 4).is_none());
        assert_eq!(
            deterministic_send_turn(&car, PlayerId::Us, 4),
            car_pop_turn(&car, PlayerId::Us, 4)
        );
    }

    #[test]
    fn tamiyo_flip_turn_recognizes_fast_lines() {
        let t = "Tamiyo, Inquisitive Student";
        // No Tamiyo in hand → no flip plan.
        assert_eq!(flip_turn(&["Island", "Brainstorm", "Ponder", "Swamp", "Doomsday", "Daze", "Consider"]), None);
        // Tamiyo + one blue land + Brainstorm → T2: draw step (1) + Brainstorm (3) = 4.
        assert_eq!(flip_turn(&[t, "Island", "Brainstorm", "Swamp", "Daze", "Force of Will", "Thoughtseize"]), Some(2));
        // Tamiyo + one blue land + a single 1-draw cantrip → no fast flip (reaches only 2).
        assert_eq!(flip_turn(&[t, "Island", "Ponder", "Swamp", "Daze", "Force of Will", "Thoughtseize"]), None);
        // Tamiyo + blue land + Ponder + Street Wraith → T2: step (1) + Ponder (1) + free (1) = 3.
        assert_eq!(flip_turn(&[t, "Island", "Ponder", "Street Wraith", "Daze", "Force of Will", "Thoughtseize"]), Some(2));
        // Tamiyo + blue land + Lotus Petal + Brainstorm → T1: Tamiyo off land, Brainstorm off petal, draws 3.
        assert_eq!(flip_turn(&[t, "Island", "Lotus Petal", "Brainstorm", "Daze", "Force of Will", "Thoughtseize"]), Some(1));
        // Tamiyo + two blue lands + two 1-draw cantrips → T2: step (1) + 1 + 1 = 3.
        assert_eq!(flip_turn(&[t, "Island", "Underground Sea", "Ponder", "Consider", "Force of Will", "Daze"]), Some(2));
    }

    /// PROBE (cantrip-aware `g` sanity): a hand with HALF the combo + cantrips to find
    /// the rest must score high; cantrips ALONE (no combo half) must score low — the
    /// cantrips amplify existing pieces, they aren't a combo by themselves.
    #[test]
    fn cantrips_plus_half_score_high_cantrips_alone_low() {
        let names: Vec<String> = crate::sample_doomsday_deck()
            .iter()
            .flat_map(|(n, q, _)| std::iter::repeat(n.clone()).take((*q).max(0) as usize))
            .collect();
        let g = |hand: &[&str]| -> f64 {
            let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
            s.catalog = build_catalog();
            let mut rest = names.clone();
            for &h in hand {
                if let Some(p) = rest.iter().position(|n| n == h) { rest.remove(p); }
            }
            for &h in hand { s.place_card(PlayerId::Us, h, Zone::Hand { known: false }); }
            for n in &rest { s.place_card(PlayerId::Us, n, Zone::Library); }
            p_cast_by(&s, PlayerId::Us, 4)
        };
        let bricks = ["Wasteland", "Daze", "Murktide Regent", "Force of Will"];
        let two_cantrips = g(&["Island", "Ponder", "Brainstorm", bricks[0], bricks[1], bricks[2], bricks[3]]);
        let rit_only     = g(&["Underground Sea", "Dark Ritual", bricks[0], bricks[1], bricks[2], bricks[3], "Thoughtseize"]);
        let rit_cantrips = g(&["Underground Sea", "Dark Ritual", "Ponder", "Brainstorm", bricks[0], bricks[1], bricks[2]]);
        let dd_cantrips  = g(&["Underground Sea", "Doomsday", "Ponder", "Brainstorm", bricks[0], bricks[1], bricks[2]]);
        eprintln!(
            "PROBE p_cast_by(T4): 2-cantrips-only={two_cantrips:.3}  rit-only={rit_only:.3}  \
             rit+cantrips={rit_cantrips:.3}  dd+cantrips={dd_cantrips:.3}"
        );
        assert!(rit_cantrips > two_cantrips + 0.1,
            "ritual+cantrips ({rit_cantrips:.3}) should clearly beat 2-cantrips-only ({two_cantrips:.3})");
        assert!(dd_cantrips > two_cantrips + 0.1,
            "dd+cantrips ({dd_cantrips:.3}) should clearly beat 2-cantrips-only ({two_cantrips:.3})");
        assert!(rit_cantrips >= rit_only,
            "cantrips should not hurt a ritual hand ({rit_cantrips:.3} vs {rit_only:.3})");
    }
    use rand::{rngs::SmallRng, seq::SliceRandom, SeedableRng};
    use std::collections::HashMap;

    fn choose(n: i64, k: i64) -> f64 {
        if k < 0 || k > n {
            return 0.0;
        }
        let k = k.min(n - k);
        let mut r = 1.0f64;
        for i in 0..k {
            r = r * (n - i) as f64 / (i + 1) as f64;
        }
        r
    }

    /// Categorize a deck into `[DD, untapped-black-land, tapped-black-land,
    /// ritual, petal, other]` counts via the IR helpers (engine-unimplemented →
    /// other). Same classification the solver uses, so analytical and empirical
    /// agree on card categories.
    fn categorize(deck: &[(String, i32, String)], catalog: &HashMap<String, CardDef>) -> [i64; 6] {
        let (mut dd, mut u, mut tl, mut ritual, mut petal, mut other) = (0i64, 0, 0, 0, 0, 0);
        for (name, qty, _) in deck {
            let q = *qty as i64;
            if name == "Doomsday" {
                dd += q;
            } else if let Some(def) = catalog.get(name.as_str()) {
                if is_free_black_artifact(def) {
                    petal += q;
                } else if is_black_ritual(def) {
                    ritual += q;
                } else if is_fetch(def) {
                    u += q;
                } else if def.is_land() && produces_black(def) {
                    if def.enters_tapped() {
                        tl += q;
                    } else {
                        u += q;
                    }
                } else {
                    other += q;
                }
            } else {
                other += q;
            }
        }
        [dd, u, tl, ritual, petal, other]
    }

    /// Exact P(deterministically cast Doomsday by `turn`) for a deck with the
    /// given `[DD,U,T,ritual,petal,other]` counts — multivariate hypergeometric
    /// over the INDEPENDENT `schedule_cast_by` oracle (no Monte-Carlo, no solver).
    fn analytical_cast_by(c: [i64; 6], turn: i64) -> f64 {
        let [dd, u, tl, ritual, petal, other] = c;
        let (total, hsize) = (60i64, 7i64);
        let denom = choose(total, hsize);
        let mut p = 0.0;
        for d in 1..=dd.min(hsize) {
            for uu in 0..=u.min(hsize) {
                for tt in 0..=tl.min(hsize) {
                    for pp in 0..=petal.min(hsize) {
                        for rr in 0..=ritual.min(hsize) {
                            let o = hsize - d - uu - tt - pp - rr;
                            if o < 0 || o > other {
                                continue;
                            }
                            if !schedule_cast_by(pp, uu, tt, rr, turn) {
                                continue;
                            }
                            p += choose(dd, d) * choose(u, uu) * choose(tl, tt)
                                * choose(petal, pp) * choose(ritual, rr) * choose(other, o)
                                / denom;
                        }
                    }
                }
            }
        }
        p
    }

    /// Finer categorisation for the PT-aware oracle: the black-mana split of
    /// `categorize` plus the extra axes the Personal-Tutor line needs —
    /// `[DD, ub, sw, tl, ritual, petal, il, pt, other]` where:
    /// - `ub` = untapped lands that make BOTH black and blue (Underground Sea,
    ///   fetches → a UB dual) — black mana AND a turn-1 blue source for `{U}`,
    /// - `sw` = untapped black-ONLY land (Swamp) — black mana, no blue,
    /// - `il` = untapped blue-ONLY land (Island) — a blue source, no black,
    /// - `tl` = tapped black land (Undercity Sewers),
    /// - `pt` = a blue library-top tutor for the payoff (Personal Tutor).
    /// `ub + sw` is exactly the old `u` (untapped black); `il`/`pt` come out of
    /// `other`. Classification uses the same IR helpers as the solver.
    fn categorize_pt(deck: &[(String, i32, String)], catalog: &HashMap<String, CardDef>) -> [i64; 9] {
        let (mut dd, mut ub, mut sw, mut tl) = (0i64, 0, 0, 0);
        let (mut ritual, mut petal, mut il, mut pt, mut other) = (0i64, 0, 0, 0, 0);
        for (name, qty, _) in deck {
            let q = *qty as i64;
            let Some(def) = (if name == "Doomsday" { None } else { catalog.get(name.as_str()) }) else {
                if name == "Doomsday" { dd += q } else { other += q }
                continue;
            };
            if def.library_top_tutor().is_some() && single_colored_pip(def.mana_cost()) == Some(Color::Blue) {
                pt += q;
            } else if is_free_black_artifact(def) {
                petal += q; // Lotus Petal: free, any colour (so a blue source too)
            } else if is_black_ritual(def) {
                ritual += q;
            } else if is_fetch(def) {
                ub += q; // a fetch reaches a UB dual → both colours, untapped
            } else if def.is_land() {
                let (blk, blu) = (produces_black(def), produces_color(def, Color::Blue));
                match (def.enters_tapped(), blk, blu) {
                    (true, true, _) => tl += q,   // tapped black land
                    (false, true, true) => ub += q,
                    (false, true, false) => sw += q,
                    (false, false, true) => il += q,
                    _ => other += q,
                }
            } else {
                other += q;
            }
        }
        [dd, ub, sw, tl, ritual, petal, il, pt, other]
    }

    /// Exact P(deterministically cast Doomsday by `turn`) WITH the Personal-Tutor
    /// line, as a multivariate hypergeometric over the 9 categories — independent
    /// of `deterministic_cast_turn` (it reuses only the validated `schedule_cast_by`
    /// BBB enumerator). A hand casts by `turn` iff BBB is reachable by then AND the
    /// payoff is in hand by then:
    /// - **direct** — a Doomsday in the opening hand, or
    /// - **tutor** — a Personal Tutor + a turn-1 blue source, one turn later
    ///   (`turn ≥ 2`). The blue source is a blue land (`ub`/`il`) or a petal.
    ///
    /// `correct_petal` toggles the petal's one-mana honesty: when the ONLY blue
    /// source is a petal, paying `{U}` sacrifices it, so BBB must hold with one
    /// fewer petal. `false` mirrors the solver (which double-counts the petal — as
    /// both the blue source and a black source); the `true`/`false` gap bounds that
    /// over-count.
    fn analytical_cast_by_pt(c: [i64; 9], turn: i64, correct_petal: bool) -> f64 {
        // Opening hand only (7 cards, no draws): validates the solver's opening-hand
        // deterministic formula.
        analytical_cast_by_pt_seen(c, turn, correct_petal, 7)
    }

    /// Ground truth with DRAWS: `hsize` = cards SEEN by the cast turn (on the play,
    /// `7 + (turn - 1)` — the "opening 9-10"). An idealised ceiling — it asks whether
    /// a deterministic line exists among the cards seen, with optimal play (the mana is
    /// still land-drop-scheduled by `schedule_cast_by`; the tutor line is allowed from
    /// turn 2, slightly optimistic on a late-drawn tutor). Excludes stochastic cantrip
    /// digging, so it's the deterministic-given-draws ceiling, not the absolute one.
    fn analytical_cast_by_pt_seen(c: [i64; 9], turn: i64, correct_petal: bool, hsize: i64) -> f64 {
        let [dd, ub, sw, tl, ritual, petal, il, pt, other] = c;
        let total = 60i64;
        let denom = choose(total, hsize);
        let mut p = 0.0;
        for d in 0..=dd.min(hsize) {
        for a in 0..=ub.min(hsize) {
        for s in 0..=sw.min(hsize) {
        for tt in 0..=tl.min(hsize) {
        for r in 0..=ritual.min(hsize) {
        for pp in 0..=petal.min(hsize) {
        for ii in 0..=il.min(hsize) {
        for q in 0..=pt.min(hsize) {
            let o = hsize - d - a - s - tt - r - pp - ii - q;
            if o < 0 || o > other {
                continue;
            }
            let black_untapped = a + s;
            let direct = d >= 1 && schedule_cast_by(pp, black_untapped, tt, r, turn);
            let tutor = pt >= 1 && q >= 1 && turn >= 2 && {
                if a >= 1 || ii >= 1 {
                    // A blue LAND pays {U}; the petal stays free for BBB.
                    schedule_cast_by(pp, black_untapped, tt, r, turn)
                } else if pp >= 1 {
                    // Only a petal can pay {U}: spend one (if honest), BBB from the rest.
                    let pp_black = if correct_petal { pp - 1 } else { pp };
                    schedule_cast_by(pp_black, black_untapped, tt, r, turn)
                } else {
                    false
                }
            };
            if direct || tutor {
                p += choose(dd, d) * choose(ub, a) * choose(sw, s) * choose(tl, tt)
                    * choose(ritual, r) * choose(petal, pp) * choose(il, ii)
                    * choose(pt, q) * choose(other, o)
                    / denom;
            }
        }}}}}}}}
        p
    }

    // ── deterministic multi-turn lookahead (PASS-as-action) ──────────────────

    #[test]
    fn sewers_comes_online_after_a_pass() {
        // Two Seas in play + a surveil dual in hand: tapped on turn 1 (can't go
        // off), but play it now, PASS, and it untaps → BBB on turn 2.
        let s = setup(&["Underground Sea", "Underground Sea"], &["Undercity Sewers", "Doomsday"], &[]);
        assert!(!sufficient(&s, PlayerId::Us));
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), Some(2));
    }

    #[test]
    fn three_taplands_are_a_turn_four_line_so_out_of_range() {
        // Three surveil duals, one land drop per turn: all three untapped only by
        // turn 4 (play T1/T2/T3, they untap T2/T3/T4). We cap at turn 3 → None.
        let s = setup(&[], &["Undercity Sewers", "Undercity Sewers", "Undercity Sewers", "Doomsday"], &[]);
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), None);
    }

    #[test]
    fn three_untapped_lands_in_hand_cast_by_turn_three() {
        // One land drop per turn → three untapped lands online by turn 3.
        let s = setup(&[], &["Underground Sea", "Underground Sea", "Underground Sea", "Doomsday"], &[]);
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), Some(3));
    }

    #[test]
    fn in_play_source_plus_ritual_is_turn_one() {
        let s = setup(&["Underground Sea"], &["Underground Sea", "Dark Ritual", "Doomsday"], &[]);
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), Some(1));
    }

    #[test]
    fn one_source_never_reaches_bbb_deterministically() {
        // A single black land and nothing else can't make BBB, however many turns.
        let s = setup(&[], &["Underground Sea", "Doomsday"], &[]);
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), None);
    }

    #[test]
    fn ritual_without_a_seed_cannot_cast() {
        // Dark Ritual costs B — it needs a 1-black seed to cast. With no land or
        // petal there's nothing to produce it, and rituals can't bootstrap each
        // other, so even two rituals + Doomsday is never castable.
        let s = setup(&[], &["Dark Ritual", "Dark Ritual", "Doomsday"], &[]);
        assert!(!sufficient(&s, PlayerId::Us));
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), None);
    }

    #[test]
    fn cabal_ritual_swamp_island_is_turn_two() {
        // Swamp + Island + Cabal Ritual ({1}{B}) + Doomsday. Cabal's generic pip needs
        // a SECOND mana source, so this is a turn-2 line, not turn-1: T1 play a land,
        // T2 play the other, tap both for {1}{B} → Cabal adds BBB → cast Doomsday.
        let s = setup(&[], &["Swamp", "Island", "Cabal Ritual", "Doomsday"], &[]);
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), Some(2));
    }

    // ── deterministic payoff acquisition (Personal Tutor) ─────────────────────

    #[test]
    fn personal_tutor_acquires_doomsday_one_turn_late() {
        // BBB online now (3 Seas) but no Doomsday in hand — Personal Tutor in hand
        // and DD in library: pay {U} this turn (a Sea), draw the seeded DD next
        // turn → cast turn 2. Not castable turn 1 (no DD in hand yet).
        let s = setup(
            &["Underground Sea", "Underground Sea", "Underground Sea"],
            &["Personal Tutor"],
            &["Doomsday"],
        );
        assert!(!sufficient(&s, PlayerId::Us));
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), Some(2));
    }

    #[test]
    fn personal_tutor_needs_a_source_of_its_pip_colour() {
        // Three Swamps make BBB, but Personal Tutor costs {U} and a Swamp makes no
        // blue → the tutor is uncastable, so no payoff line exists.
        let s = setup(
            &["Swamp", "Swamp", "Swamp"],
            &["Personal Tutor"],
            &["Doomsday"],
        );
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), None);
    }

    #[test]
    fn personal_tutor_needs_doomsday_in_the_library() {
        // PT can only acquire a Doomsday that's actually in the library (grounded
        // by obj_matches). With none there, it's not a payoff line.
        let s = setup(
            &["Underground Sea", "Underground Sea", "Underground Sea"],
            &["Personal Tutor"],
            &["Island"],
        );
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), None);
    }

    // ── Fast tutor lines: a turn-1 Personal Tutor that stages Doomsday for a turn-2
    //    cast, powered by petals / rituals / lands. These are the class of T2 kills a
    //    high-Personal-Tutor build is supposed to add. ──

    #[test]
    fn petal_petal_ritual_tutor_is_t2() {
        // T1: petal→{U}, cast Personal Tutor (Doomsday to top). T2: draw it,
        // petal→{B}, Dark Ritual → BBB, cast Doomsday.
        let s = setup(
            &[],
            &["Lotus Petal", "Lotus Petal", "Dark Ritual", "Personal Tutor"],
            &["Doomsday"],
        );
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 4), Some(2));
    }

    #[test]
    fn land_land_petal_tutor_is_t2() {
        // T1: Underground Sea, cast Personal Tutor. T2: draw Doomsday, second Sea +
        // first Sea + petal = BBB, cast.
        let s = setup(
            &[],
            &["Underground Sea", "Underground Sea", "Lotus Petal", "Personal Tutor"],
            &["Doomsday"],
        );
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 4), Some(2));
    }

    #[test]
    fn land_land_land_petal_tutor_is_t2_not_t3() {
        // The petal makes this T2 (two Seas online by T2 + petal = BBB), NOT the
        // non-minimal three-lands-by-T3 line. A minimal-line solver must find the 2.
        let s = setup(
            &[],
            &["Underground Sea", "Underground Sea", "Underground Sea", "Lotus Petal", "Personal Tutor"],
            &["Doomsday"],
        );
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 4), Some(2));
    }

    #[test]
    fn battlefield_searcher_is_not_a_payoff_tutor() {
        // Green Sun's Zenith searches the library but puts the find onto the
        // BATTLEFIELD — it can't seed the top of the library for a draw, so it's
        // structurally excluded as a payoff tutor (and couldn't get a sorcery anyway).
        let s = setup(
            &["Underground Sea", "Underground Sea", "Underground Sea"],
            &["Green Sun's Zenith"],
            &["Doomsday"],
        );
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), None);
    }

    #[test]
    fn doomsday_in_hand_beats_the_tutor_line() {
        // With both DD in hand and a tutor, the direct line (cast now) wins over
        // the one-turn-late tutor line.
        let s = setup(
            &["Underground Sea", "Underground Sea", "Underground Sea"],
            &["Doomsday", "Personal Tutor"],
            &["Doomsday"],
        );
        assert!(sufficient(&s, PlayerId::Us));
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), Some(1));
    }

    #[test]
    fn baseline_oracle_requires_a_seed_for_rituals() {
        // The independent hypergeometric oracle must also reject seedless rituals.
        assert!(!schedule_cast_by(0, 0, 0, 2, 3)); // 2 rituals, no land/petal, by turn 3 → no
        assert!(schedule_cast_by(0, 1, 0, 1, 1)); // land seeds the ritual → turn 1
        assert!(schedule_cast_by(1, 0, 0, 1, 1)); // petal seeds the ritual → turn 1
    }

    /// The canonical "tempo dd tami waste" list (Moxfield M_yMyiTfg0eoKTkaqCkJ_Q),
    /// 60-card mainboard. Cards the engine doesn't implement are still listed —
    /// they classify as "other"; only the mana package + Doomsday drive T1
    /// sufficiency. (Tamiyo uses its front-face catalog key.)
    fn canonical_tempo_dd() -> Vec<(String, i32, String)> {
        [
            ("Underground Sea", 4), ("Polluted Delta", 4), ("Flooded Strand", 1),
            ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Bloodstained Mire", 1),
            ("Swamp", 1), ("Island", 1), ("Undercity Sewers", 1), ("Wasteland", 2),
            ("Cavern of Souls", 1),
            ("Lotus Petal", 2), ("Lion's Eye Diamond", 1), ("Dark Ritual", 4),
            ("Doomsday", 4),
            ("Brainstorm", 4), ("Ponder", 4), ("Consider", 1), ("Flow State", 4),
            ("Edge of Autumn", 1), ("Street Wraith", 1),
            ("Force of Will", 4), ("Daze", 2), ("Thoughtseize", 2),
            ("Thassa's Oracle", 1), ("Jace, Wielder of Mysteries", 1),
            ("Tamiyo, Inquisitive Student", 4), ("Murktide Regent", 2),
        ]
        .iter().map(|(n, q)| (n.to_string(), *q, "main".to_string())).collect()
    }

    /// The "tempo dd tami pt" variant (Moxfield k9KeAU6pl3C-qDNSWOqzxg): vs the
    /// `waste` list, +1 Lotus Petal (3 total), +Personal Tutor, +1 Thassa,
    /// −2 Wasteland, −Jace. Still 60.
    fn canonical_tempo_dd_pt() -> Vec<(String, i32, String)> {
        [
            ("Underground Sea", 4), ("Polluted Delta", 4), ("Flooded Strand", 1),
            ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Bloodstained Mire", 1),
            ("Swamp", 1), ("Island", 1), ("Undercity Sewers", 1),
            ("Cavern of Souls", 1),
            ("Lotus Petal", 3), ("Lion's Eye Diamond", 1), ("Dark Ritual", 4),
            ("Doomsday", 4), ("Personal Tutor", 1),
            ("Brainstorm", 4), ("Ponder", 4), ("Consider", 1), ("Flow State", 4),
            ("Edge of Autumn", 1), ("Street Wraith", 1),
            ("Force of Will", 4), ("Daze", 2), ("Thoughtseize", 2),
            ("Thassa's Oracle", 2),
            ("Tamiyo, Inquisitive Student", 4), ("Murktide Regent", 2),
        ]
        .iter().map(|(n, q)| (n.to_string(), *q, "main".to_string())).collect()
    }

    /// Compare two builds on the deterministic cast-by-turn metric. The `pt`
    /// list's extra Lotus Petal (3 vs 2) is the only DETERMINISTIC difference
    /// (Personal Tutor finds DD via a draw, Street Wraith is a free redraw — both
    /// land in the stochastic-draw model, not here). Quantifies the petal's speed.
    #[test]
    fn extra_petal_speeds_up_deterministic_doomsday() {
        let catalog = build_catalog();
        let waste = categorize(&canonical_tempo_dd(), &catalog);
        let pt = categorize(&canonical_tempo_dd_pt(), &catalog);
        println!("waste [DD,U,T,R,P,other] = {waste:?}");
        println!("pt    [DD,U,T,R,P,other] = {pt:?}");
        for turn in 1..=3i64 {
            let a = analytical_cast_by(waste, turn);
            let b = analytical_cast_by(pt, turn);
            println!("turn {turn}: waste = {a:.4}, +petal = {b:.4}, Δ = +{:.4}", b - a);
            assert!(b >= a - 1e-9, "an extra petal can't slow deterministic DD");
        }
    }

    /// Independent oracle: can the opening hand reach 3 black by `turn`, decided
    /// by EXPLICITLY enumerating which lands take which land-drop slot (a tapland
    /// is online only if played before `turn`, i.e. it untaps in time). This is a
    /// different algorithm from the solver's closed-form `online_black_lands`, so
    /// agreement cross-checks the multi-turn scheduling.
    fn schedule_cast_by(petals: i64, untapped: i64, tapped: i64, rituals: i64, turn: i64) -> bool {
        let slots = turn; // one land drop per turn, turns 1..=turn (turn 1 = now)
        let early = turn - 1; // slots before `turn`, where a tapland still untaps by then
        for tp in 0..=tapped.min(early) {
            let up = untapped.min(slots - tp); // untapped lands fill remaining slots
            let black = petals + up + tp;
            if black >= 3 || (rituals >= 1 && black >= 1) {
                return true;
            }
        }
        false
    }

    /// Empirical "can deterministically cast Doomsday by turn t" rate (t = 1,2,3)
    /// over `k` random opening hands, via `deterministic_cast_turn` (so PT lines
    /// are included). Returns `[_, p1, p2, p3]` (index 0 unused). The harness
    /// mirrors a real draw: 7 cards to hand, the rest to library — so a tutor can
    /// reach a Doomsday sitting in the library.
    fn empirical_cast_by(deck: &[(String, i32, String)], k: u32, seed: u64) -> [f64; 4] {
        let names: Vec<&str> = deck
            .iter()
            .flat_map(|(n, q, _)| std::iter::repeat(n.as_str()).take((*q).max(0) as usize))
            .collect();
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut state = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        state.catalog = build_catalog();
        let (us_pid, opp_pid) = (state.player_id(PlayerId::Us), state.player_id(PlayerId::Opp));
        let mut shuffled = names.clone();
        let mut by = [0u32; 4]; // games castable by turn 0,1,2,3
        for _ in 0..k {
            shuffled.shuffle(&mut rng);
            state.objects.retain(|id, _| *id == us_pid || *id == opp_pid);
            state.player_mut(PlayerId::Us).library_order.clear();
            for (i, name) in shuffled.iter().enumerate() {
                let zone = if i < 7 { Zone::Hand { known: false } } else { Zone::Library };
                state.place_card(PlayerId::Us, name, zone);
            }
            if let Some(s) = deterministic_cast_turn(&state, PlayerId::Us, 3) {
                for slot in by.iter_mut().skip(s as usize) {
                    *slot += 1;
                }
            }
        }
        [0.0, by[1] as f64 / k as f64, by[2] as f64 / k as f64, by[3] as f64 / k as f64]
    }

    /// Validate the deterministic multi-turn solver: for each turn t∈1..3 the
    /// empirical "can cast by turn t" rate over K random opening hands (via
    /// `deterministic_cast_turn`) must match the exact hypergeometric probability
    /// of the same event, where sufficiency is decided by the INDEPENDENT
    /// `schedule_cast_by` enumeration (not the solver's formula). The canonical
    /// list has NO Personal Tutor, so PT lines never fire and the agreement still
    /// cross-checks the scheduling math AND the harness, turn by turn.
    #[test]
    fn deterministic_cast_rate_matches_hypergeometric() {
        let deck = canonical_tempo_dd();
        let catalog = build_catalog();

        // Categorize via the shared IR-based helper (engine-unimplemented →
        // "other"). by-hand: 4 DD / 13 untapped / 1 tapped / 4 ritual / 2 petal / 36 other.
        let counts = categorize(&deck, &catalog);
        let [dd, u, tl, ritual, petal, other] = counts;
        assert_eq!(counts.iter().sum::<i64>(), 60);
        println!("categories: DD={dd} untapped={u} tapped={tl} ritual={ritual} petal={petal} other={other}");

        let rates = empirical_cast_by(&deck, 30_000, 0xD00D_5DA7);
        for turn in 1..=3usize {
            let p = analytical_cast_by(counts, turn as i64);
            let emp = rates[turn];
            println!("cast by turn {turn}: analytical = {p:.4}, empirical = {emp:.4} (|Δ| = {:.4})", (emp - p).abs());
            assert!(
                (emp - p).abs() < 0.012,
                "turn {turn}: empirical {emp:.4} diverges from hypergeometric {p:.4}"
            );
        }
    }

    /// A very fast 4×Personal-Tutor Doomsday list (MTGGoldfish deck 7805658),
    /// 60-card mainboard. vs the canonical tempo lists: 4 Personal Tutor (not 0/1),
    /// 4 Lotus Petal, 4 Daze, no Tamiyo/Murktide/Wasteland. Engine-unimplemented
    /// cards classify as "other".
    fn fast_pt_doomsday() -> Vec<(String, i32, String)> {
        [
            ("Underground Sea", 4), ("Polluted Delta", 4), ("Flooded Strand", 3),
            ("Bloodstained Mire", 1), ("Swamp", 1), ("Island", 1),
            ("Undercity Sewers", 1), ("Cavern of Souls", 1),
            ("Lotus Petal", 4), ("Lion's Eye Diamond", 1), ("Dark Ritual", 4),
            ("Doomsday", 4), ("Personal Tutor", 4),
            ("Brainstorm", 4), ("Ponder", 4), ("Consider", 1),
            ("Edge of Autumn", 2), ("Street Wraith", 2),
            ("Force of Will", 4), ("Daze", 4), ("Thoughtseize", 4),
            ("Thassa's Oracle", 1), ("Jace, Wielder of Mysteries", 1),
        ]
        .iter().map(|(n, q)| (n.to_string(), *q, "main".to_string())).collect()
    }

    /// The 4×Personal-Tutor list quantified. Personal Tutor turns a no-Doomsday
    /// hand into a one-turn-late cast (seed the payoff on top, draw it). We compare
    /// the PT-AWARE deterministic empirical rate against the PT-BLIND hypergeometric
    /// (which counts Personal Tutor as "other") — the gap is exactly what 4 tutors
    /// buy in deterministic speed — and print the 0-PT / 1-PT canonical lists
    /// alongside for scale. (Cross-list ordering isn't asserted: the lists differ
    /// in more than PT, so only the self-comparison PT-aware vs PT-blind is causal.)
    #[test]
    fn four_personal_tutors_speed_up_deterministic_doomsday() {
        let catalog = build_catalog();
        let fast = fast_pt_doomsday();
        let counts = categorize(&fast, &catalog);
        let [dd, u, tl, ritual, petal, other] = counts;
        assert_eq!(counts.iter().sum::<i64>(), 60);
        println!("fast-PT categories: DD={dd} untapped={u} tapped={tl} ritual={ritual} petal={petal} other={other} (PT∈other)");

        let k = 20_000u32;
        let waste = empirical_cast_by(&canonical_tempo_dd(), k, 0xFA57_0001);
        let pt1 = empirical_cast_by(&canonical_tempo_dd_pt(), k, 0xFA57_0002);
        let fast_e = empirical_cast_by(&fast, k, 0xFA57_0003);

        println!("deterministic cast-by-turn (PT-aware empirical):");
        for turn in 1..=3usize {
            let blind = analytical_cast_by(counts, turn as i64); // PT counted as "other"
            println!(
                "  turn {turn}: waste(0PT)={:.4}  pt(1PT)={:.4}  fast(4PT)={:.4}  | fast PT-blind={:.4}  Δ(PT)=+{:.4}",
                waste[turn], pt1[turn], fast_e[turn], blind, fast_e[turn] - blind
            );
            // PT lines only ADD deterministic castability — never remove it.
            assert!(
                fast_e[turn] >= blind - 5e-3,
                "PT lines can't slow the 4-PT list at turn {turn} (PT-aware {:.4} < PT-blind {blind:.4})",
                fast_e[turn]
            );
        }
        // Turn 1: PT can't help (its line is ≥ turn 2) → PT-aware ≈ PT-blind.
        assert!(
            (fast_e[1] - analytical_cast_by(counts, 1)).abs() < 6e-3,
            "Personal Tutor must not change the turn-1 rate"
        );
        // By turn 2, four tutors must add measurable deterministic castability.
        assert!(
            fast_e[2] > analytical_cast_by(counts, 2) + 0.01,
            "4 Personal Tutors should add >1% deterministic castability by turn 2"
        );
    }

    /// Cross-check the PT-FACTORING math: the empirical PT-aware cast-by-turn rate
    /// (Monte-Carlo over `deterministic_cast_turn`) must match an INDEPENDENT
    /// closed-form hypergeometric that includes the tutor line
    /// (`analytical_cast_by_pt`, which never calls the solver — it reuses only the
    /// validated `schedule_cast_by` BBB enumerator and an independently-derived PT
    /// factor). Agreement validates the tutor line's implementation AND the MC
    /// harness. Also reports the petal one-mana honesty gap (mirror vs correct).
    #[test]
    fn pt_aware_cast_rate_matches_hypergeometric() {
        let catalog = build_catalog();
        let deck = fast_pt_doomsday();
        let c9 = categorize_pt(&deck, &catalog);
        assert_eq!(c9.iter().sum::<i64>(), 60);
        let [dd, ub, sw, tl, ritual, petal, il, pt, other] = c9;
        println!("9-way: DD={dd} ub={ub} sw={sw} tl={tl} ritual={ritual} petal={petal} il={il} pt={pt} other={other}");

        let emp = empirical_cast_by(&deck, 40_000, 0x9A57_C0DE);
        for turn in 1..=3usize {
            let correct = analytical_cast_by_pt(c9, turn as i64, true); // petal = one mana (honest)
            let mirror = analytical_cast_by_pt(c9, turn as i64, false); // petal double-use (the old bug)
            println!(
                "turn {turn}: empirical={:.4}  analytical(honest)={correct:.4}  (|Δ|={:.4})   would-be-double-use={mirror:.4}  (cost-cycle removed {:.4})",
                emp[turn], (emp[turn] - correct).abs(), mirror - correct
            );
            // The solver now models the sac cost as consuming the petal-object, so
            // it must match the HONEST closed form (petal = one mana) within noise.
            assert!(
                (emp[turn] - correct).abs() < 0.012,
                "turn {turn}: empirical {:.4} diverges from honest PT-aware hypergeometric {correct:.4}",
                emp[turn]
            );
            // And it must NOT match the double-use model: the cost-cycle fix put it
            // on the honest side of the gap. (No assertion at turn 1, where the gap is 0.)
            if mirror - correct > 0.003 {
                assert!(
                    (emp[turn] - correct).abs() < (emp[turn] - mirror).abs(),
                    "turn {turn}: solver is closer to the double-use model than the honest one"
                );
            }
        }
    }

    /// INTUITION HARNESS (run: `cargo test -p dd-goldfish ground_truth -- --ignored --nocapture`).
    /// Prints, per deck and per turn, the deterministic-given-draws CEILING (the
    /// PT-aware hypergeometric over the cards seen by turn T = 7 + (T-1), idealised
    /// optimal play, no mulligan) next to the actual STRATEGY rate with KEEP7 (no
    /// mulligan — apples to apples) and with the real mulligan. The ceiling excludes
    /// stochastic cantrip digging, so a strategy ABOVE it is getting real value from
    /// cantrips; a strategy BELOW it is leaving deterministic lines uncast.
    #[test]
    #[ignore = "intuition harness; prints ground-truth vs strategy, run with --ignored --nocapture"]
    fn ground_truth_vs_strategy() {
        let catalog = build_catalog();
        let decks = [
            ("waste(0pt)", canonical_tempo_dd()),
            ("1pt", canonical_tempo_dd_pt()),
            ("4pt", fast_pt_doomsday()),
        ];
        for (name, deck) in decks {
            let counts = categorize_pt(&deck, &catalog);
            std::env::set_var("KEEP7", "1");
            let k7 = crate::run_goldfish_asap(&deck, 2_000, crate::DEFAULT_PROTECTION, 4);
            std::env::remove_var("KEEP7");
            println!("\n=== {name} === (open-7 deterministic floor; strategy keeps every 7)");
            for t in 1..=4i64 {
                // EXACT opening-7 deterministic rate (no draws) — the floor a kept 7 is
                // already worth. The strategy keeps the same 7 and then DRAWS + cantrips,
                // so `keep7` should sit ABOVE this; `keep7 − open7` is the value of play.
                let open7 = analytical_cast_by_pt_seen(counts, t, true, 7);
                // For reference: the deterministic-given-draws ceiling (cards seen by T).
                let seen = 7 + (t - 1);
                let ceil = analytical_cast_by_pt_seen(counts, t, true, seen);
                let k = k7.cast_by(t as u8);
                println!(
                    "  T{t}: open7(det)={open7:.3}   strategy keep7={k:.3} ({:+.3} from play)   [draws-ceiling(seen {seen})={ceil:.3}]",
                    k - open7
                );
            }
        }
    }

    /// INTUITION: how the DETERMINISTIC cast rate moves with Personal-Tutor count,
    /// holding the rest fixed (PT ↔ a brick). Pure combinatorics, no sim — directly
    /// tests "more PTs should raise deterministic T2-3 the most".
    /// Run: `cargo test -p dd-goldfish pt_deterministic_curve -- --ignored --nocapture`.
    #[test]
    #[ignore = "intuition: deterministic cast rate vs PT count"]
    fn pt_deterministic_curve() {
        let base = categorize_pt(&fast_pt_doomsday(), &build_catalog());
        let [dd, ub, sw, tl, ritual, petal, il, pt0, other0] = base;
        println!("\n-- deterministic OPENING-7 (no draws) --");
        println!("PT   T1     T2     T3     T4");
        for k in 0..=pt0.min(4) {
            let counts = [dd, ub, sw, tl, ritual, petal, il, k, other0 + (pt0 - k)];
            print!("{k} ");
            for t in 1..=4 {
                print!("  {:.3}", analytical_cast_by_pt_seen(counts, t, true, 7));
            }
            println!();
        }
        println!("\n-- deterministic WITH NATURAL DRAWS (seen = 7 + T-1) --");
        println!("PT   T1     T2     T3     T4");
        for k in 0..=pt0.min(4) {
            let counts = [dd, ub, sw, tl, ritual, petal, il, k, other0 + (pt0 - k)];
            print!("{k} ");
            for t in 1..=4i64 {
                print!("  {:.3}", analytical_cast_by_pt_seen(counts, t, true, 7 + (t - 1)));
            }
            println!();
        }
    }

    fn setup(bf: &[&str], hand: &[&str], library: &[&str]) -> SimState {
        let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        s.catalog = build_catalog();
        for n in bf {
            s.place_card(PlayerId::Us, n, Zone::Battlefield);
        }
        for n in hand {
            s.place_card(PlayerId::Us, n, Zone::Hand { known: false });
        }
        for n in library {
            s.place_card(PlayerId::Us, n, Zone::Library);
        }
        s
    }

    fn ub_mana_deck() -> Vec<(String, i32, String)> {
        [
            ("Underground Sea", 4), ("Polluted Delta", 4), ("Flooded Strand", 2),
            ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Bloodstained Mire", 1),
            ("Swamp", 1), ("Undercity Sewers", 1),
            ("Lotus Petal", 2), ("Dark Ritual", 4), ("Doomsday", 4),
        ]
        .iter().map(|(n, q)| (n.to_string(), *q, "main".to_string())).collect()
    }

    // ── stochastic gap-closing (#7b) ──────────────────────────────────────────

    #[test]
    fn expected_draws_to_out_matches_negative_hypergeometric() {
        // (N+1)/(K+1) is the mean position of the first "out" when K outs are
        // uniformly placed in N cards. Cross-check by shuffling and recording the
        // first hit — an independent confirmation of the closed form.
        let (n, k) = (53u32, 13u32);
        let mut deck: Vec<bool> = (0..n).map(|i| i < k).collect();
        let mut rng = SmallRng::seed_from_u64(0x0177_5EED);
        let trials = 200_000u32;
        let mut total = 0u64;
        for _ in 0..trials {
            deck.shuffle(&mut rng);
            total += deck.iter().position(|&x| x).unwrap() as u64 + 1; // 1-based
        }
        let empirical = total as f64 / trials as f64;
        let analytical = expected_draws_to_out(n, k);
        println!("E[draws to first out] N={n} K={k}: analytical={analytical:.4} empirical={empirical:.4}");
        assert!((empirical - analytical).abs() < 0.05, "negative-hypergeometric mean mismatch");
        assert!(expected_draws_to_out(40, 0).is_infinite(), "no outs ⇒ never closes");
    }

    /// Mean finite E[TTD] over `k` random opening hands, plus the brick rate
    /// (fraction with no out at all). Shares the opening-hand harness.
    fn mean_e_ttd(deck: &[(String, i32, String)], k: u32, seed: u64) -> (f64, f64) {
        let names: Vec<&str> = deck
            .iter()
            .flat_map(|(n, q, _)| std::iter::repeat(n.as_str()).take((*q).max(0) as usize))
            .collect();
        let mut rng = SmallRng::seed_from_u64(seed);
        let mut state = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        state.catalog = build_catalog();
        let (us_pid, opp_pid) = (state.player_id(PlayerId::Us), state.player_id(PlayerId::Opp));
        let mut shuffled = names.clone();
        let (mut sum, mut finite, mut bricks) = (0.0f64, 0u32, 0u32);
        for _ in 0..k {
            shuffled.shuffle(&mut rng);
            state.objects.retain(|id, _| *id == us_pid || *id == opp_pid);
            state.player_mut(PlayerId::Us).library_order.clear();
            for (i, name) in shuffled.iter().enumerate() {
                let zone = if i < 7 { Zone::Hand { known: false } } else { Zone::Library };
                state.place_card(PlayerId::Us, name, zone);
            }
            let v = e_ttd(&state, PlayerId::Us, 3);
            if v.is_finite() {
                sum += v;
                finite += 1;
            } else {
                bricks += 1;
            }
        }
        (sum / finite as f64, bricks as f64 / k as f64)
    }

    #[test]
    fn e_ttd_ranks_fast_pt_below_waste() {
        let k = 8_000;
        let (fast_mean, fast_brick) = mean_e_ttd(&fast_pt_doomsday(), k, 0xE77D_0001);
        let (waste_mean, waste_brick) = mean_e_ttd(&canonical_tempo_dd(), k, 0xE77D_0002);
        println!(
            "E[TTD] fast(4PT)={fast_mean:.3} (brick {fast_brick:.3})  waste(0PT)={waste_mean:.3} (brick {waste_brick:.3})"
        );
        // The faster, tutor-rich list resolves Doomsday sooner in expectation.
        assert!(fast_mean < waste_mean, "the 4-PT list should have lower mean E[TTD]");
    }

    // ── known top-of-library (cantrip/tutor unification) ──────────────────────

    #[test]
    fn known_card_on_top_accelerates_by_draw_timing() {
        // 3 lands + DD is a deterministic turn-3 cast. A known Dark Ritual coming
        // off the top finishes it sooner — and HOW soon is the tutor/cantrip draw
        // difference: a TUTOR only stages the top (drawn turn 2) → cast turn 2; a
        // CANTRIP draws after arranging (drawn now) → cast turn 1.
        let s = setup(&[], &["Underground Sea", "Underground Sea", "Underground Sea", "Doomsday"], &[]);
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), Some(3));
        assert_eq!(e_ttd_with_known_top(&s, PlayerId::Us, &["Dark Ritual"], false, 3), 2.0); // tutor
        assert_eq!(e_ttd_with_known_top(&s, PlayerId::Us, &["Dark Ritual"], true, 3), 1.0); // cantrip
    }

    #[test]
    fn known_doomsday_on_top_makes_a_no_dd_hand_castable() {
        // 3 lands, no Doomsday in hand: a Doomsday tutored to the top is drawn turn
        // 2, BBB comes fully online turn 3 → cast turn 3.
        let s = setup(&[], &["Underground Sea", "Underground Sea", "Underground Sea"], &[]);
        assert_eq!(e_ttd_with_known_top(&s, PlayerId::Us, &["Doomsday"], false, 3), 3.0);
    }

    #[test]
    fn p_at_least_one_matches_hypergeometric() {
        // P(≥1 of K outs in d draws from N), product form, vs an empirical shuffle.
        let (n, k, d) = (53u32, 8u32, 3u32);
        let mut deck: Vec<bool> = (0..n).map(|i| i < k).collect();
        let mut rng = SmallRng::seed_from_u64(0xB175_0001);
        let (trials, mut hits) = (200_000u32, 0u32);
        for _ in 0..trials {
            deck.shuffle(&mut rng);
            if deck[..d as usize].iter().any(|&x| x) {
                hits += 1;
            }
        }
        let empirical = hits as f64 / trials as f64;
        let analytical = p_at_least_one(n, k, d);
        println!("P(>=1 out) N={n} K={k} d={d}: analytical={analytical:.4} empirical={empirical:.4}");
        assert!((empirical - analytical).abs() < 0.01);
        assert_eq!(p_at_least_one(53, 0, 5), 0.0); // no outs
        assert_eq!(p_at_least_one(53, 8, 0), 0.0); // no draws
    }

    #[test]
    fn ponder_keeps_a_useful_top_and_shuffles_a_brick() {
        // A draw-reliant hand: 2 lands + DD can't make BBB alone, so it leans on
        // what comes off the top. Objective is P(cast by the cutoff turn 4).
        let s = setup(
            &[],
            &["Underground Sea", "Underground Sea", "Doomsday"],
            &["Dark Ritual", "Dark Ritual", "Underground Sea", "Lotus Petal", "Brainstorm"],
        );
        // Ponder draws after arranging: a top led by a ritual casts this turn →
        // P(cast by 4) = 1.0 → keep, ritual first.
        let good = vec!["Dark Ritual".to_string(), "Brainstorm".to_string(), "Brainstorm".to_string()];
        let (choice, p) = best_top_choice(&s, PlayerId::Us, &good, true, 4);
        assert!(matches!(&choice, TopChoice::Keep(v) if v[0] == "Dark Ritual"));
        assert_eq!(p, 1.0);
        // An all-brick top locks 3 dead draws past the cutoff (keep-P → 0) → shuffle.
        let brick = vec!["Brainstorm".to_string(), "Brainstorm".to_string(), "Brainstorm".to_string()];
        let (choice, _) = best_top_choice(&s, PlayerId::Us, &brick, true, 4);
        assert_eq!(choice, TopChoice::Shuffle);
    }

    #[test]
    fn brainstorm_burst_casts_when_a_piece_arrives() {
        // 2 lands + DD is a piece short. Brainstorm draws 3 INTO HAND now: a Dark
        // Ritual among them completes the line this turn → P(cast by 4) = 1.0, and
        // the 2 bricks are buried (re-drawn later, but we've already won).
        let s = setup(&[], &["Underground Sea", "Underground Sea", "Doomsday"], &["Underground Sea", "Lotus Petal"]);
        let drawn = vec!["Dark Ritual".to_string(), "Brainstorm".to_string(), "Brainstorm".to_string()];
        let (buried, p) = best_brainstorm(&s, PlayerId::Us, &drawn, 4);
        assert_eq!(p, 1.0);
        assert!(buried.iter().all(|c| c == "Brainstorm"), "buries the bricks, keeps the ritual");
    }

    // ── sufficient? over real states ─────────────────────────────────────────

    #[test]
    fn three_black_in_play() {
        assert!(sufficient(&setup(&["Underground Sea", "Underground Sea", "Swamp"], &["Doomsday"], &[]), PlayerId::Us));
    }

    #[test]
    fn two_seas_plus_ritual() {
        assert!(sufficient(&setup(&["Underground Sea", "Underground Sea"], &["Dark Ritual", "Doomsday"], &[]), PlayerId::Us));
    }

    #[test]
    fn two_seas_plus_petal() {
        assert!(sufficient(&setup(&["Underground Sea", "Underground Sea"], &["Lotus Petal", "Doomsday"], &[]), PlayerId::Us));
    }

    #[test]
    fn fetch_in_hand_to_ritual() {
        // Play a fetch (untapped) → it can get a Sea (in library) → tap → seed Dark Ritual.
        let s = setup(&[], &["Polluted Delta", "Dark Ritual", "Doomsday"], &["Underground Sea"]);
        assert!(sufficient(&s, PlayerId::Us));
    }

    #[test]
    fn surveil_dual_land_drop_makes_no_mana_this_turn() {
        // Two black in play; the one land drop is a surveil dual (enters tapped) →
        // no third black this turn → insufficient. An untapped dual instead → fine.
        let tapped = setup(&["Underground Sea", "Underground Sea"], &["Undercity Sewers", "Doomsday"], &[]);
        assert!(!sufficient(&tapped, PlayerId::Us));
        let untapped = setup(&["Underground Sea", "Underground Sea"], &["Underground Sea", "Doomsday"], &[]);
        assert!(sufficient(&untapped, PlayerId::Us));
    }

    #[test]
    fn one_land_drop_per_turn() {
        // Three black lands in hand, none in play: only one is playable → short.
        assert!(!sufficient(&setup(&[], &["Swamp", "Swamp", "Swamp", "Doomsday"], &[]), PlayerId::Us));
    }

    #[test]
    fn no_doomsday_is_insufficient() {
        assert!(!sufficient(&setup(&["Underground Sea", "Underground Sea", "Swamp"], &["Dark Ritual"], &[]), PlayerId::Us));
    }

    // ── the gap is the missing ingredients ───────────────────────────────────

    #[test]
    fn two_seas_gap_is_land_petal_or_ritual() {
        let s = setup(&["Underground Sea", "Underground Sea"], &["Doomsday"], &[]);
        assert_eq!(mana_gap(&s, PlayerId::Us), vec!["land", "petal", "ritual"]);
    }

    // ── deck → {sufficient} reproduces the five bundles ──────────────────────

    #[test]
    fn ub_deck_yields_the_five_bundles() {
        let mut got = bundles(&ub_mana_deck());
        got.sort_by_key(|b| (b.lands, b.petals, b.rituals));
        let mut want = vec![
            Bundle { lands: 3, petals: 0, rituals: 0 }, // 3 untapped lands
            Bundle { lands: 2, petals: 1, rituals: 0 }, // 2 lands + petal
            Bundle { lands: 1, petals: 2, rituals: 0 }, // 1 land + 2 petals
            Bundle { lands: 1, petals: 0, rituals: 1 }, // 1 land + ritual
            Bundle { lands: 0, petals: 1, rituals: 1 }, // petal + ritual
        ];
        want.sort_by_key(|b| (b.lands, b.petals, b.rituals));
        assert_eq!(got, want);
    }

    // ── reduction ladder: the EMITTED LINE (the witness), not just the cast turn ──
    //
    // Each test pins one reduction the deterministic solver makes: casting Doomsday
    // reduces to assembling BBB, which reduces (ritual / fetch-crack / land-drop /
    // petal) to a simpler mana position, and acquiring the payoff (a library-top
    // tutor) reduces across a turn. We assert the ordered `deterministic_line` steps,
    // so a future change that produces the wrong line — e.g. playing a Swamp over a
    // Sea/fetch and stranding a tutor (the T2→T3 bug) — fails here, not in production.

    /// The emitted deterministic line as readable `Tag(Card)` steps (`[]` if none).
    fn line(bf: &[&str], hand: &[&str], library: &[&str], max_turn: u32) -> Vec<String> {
        let s = setup(bf, hand, library);
        let nm = |id: ObjId| s.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_default();
        deterministic_line(&s, PlayerId::Us, max_turn)
            .map(|l| {
                l.steps.iter().map(|step| match step {
                    LineStep::PlayLand(i) => format!("Play({})", nm(*i)),
                    LineStep::CrackFetch(i) => format!("Crack({})", nm(*i)),
                    LineStep::CastPetal(i) => format!("Petal({})", nm(*i)),
                    LineStep::CastRitual(i) => format!("Ritual({})", nm(*i)),
                    LineStep::CastTutor(i) => format!("Tutor({})", nm(*i)),
                    LineStep::CastDoomsday(i) => format!("DD({})", nm(*i)),
                }).collect()
            })
            .unwrap_or_default()
    }

    /// Base case: BBB already up (3 black sources in play) + Doomsday in hand → cast it.
    #[test]
    fn line_base_bbb_dd_in_hand() {
        assert_eq!(
            line(&["Underground Sea", "Underground Sea", "Underground Sea"], &["Doomsday"], &[], 3),
            ["DD(Doomsday)"]
        );
    }

    /// Ritual reduces to the base: one black source + Dark Ritual + Doomsday → cast the
    /// ritual (B → BBB), then we're back to "BBB up + DD in hand".
    #[test]
    fn line_ritual_reduces_to_base() {
        assert_eq!(
            line(&["Underground Sea"], &["Dark Ritual", "Doomsday"], &[], 3),
            ["Ritual(Dark Ritual)", "DD(Doomsday)"]
        );
    }

    /// Land drop reduces: a black land in hand → play it (the once-per-turn drop), then
    /// reduce as before (ritual → base).
    #[test]
    fn line_land_drop_reduces() {
        assert_eq!(
            line(&[], &["Underground Sea", "Dark Ritual", "Doomsday"], &[], 3),
            ["Play(Underground Sea)", "Ritual(Dark Ritual)", "DD(Doomsday)"]
        );
    }

    /// Fetch-crack reduces: a fetch already in play (black target in library) → crack it
    /// into a black source, then reduce.
    #[test]
    fn line_crack_inplay_fetch_reduces() {
        assert_eq!(
            line(&["Polluted Delta"], &["Dark Ritual", "Doomsday"], &["Underground Sea"], 3),
            ["Crack(Polluted Delta)", "Ritual(Dark Ritual)", "DD(Doomsday)"]
        );
    }

    /// Tutor reduces across a turn AND develops its pip source first. Doomsday is in the
    /// LIBRARY; Personal Tutor stages it on top. The tutor's {U} must be developed (fetch
    /// → Underground Sea) BEFORE casting it — and we must NOT spend the land drop on a
    /// Swamp. This is the exact T2→T3 regression guard.
    #[test]
    fn line_tutor_develops_pip_source_not_swamp() {
        let steps = line(
            &[],
            &["Personal Tutor", "Polluted Delta", "Swamp", "Dark Ritual"],
            &["Doomsday", "Underground Sea"],
            3,
        );
        let pos = |t: &str| steps.iter().position(|s| s == t);
        assert_eq!(steps.first().map(String::as_str), Some("Play(Polluted Delta)"),
            "develop the fetch (pip source) first, not a Swamp — got {steps:?}");
        let f = pos("Play(Polluted Delta)").expect("fetch played");
        let tut = pos("Tutor(Personal Tutor)").expect("tutor cast");
        assert!(f < tut, "pip source must precede the tutor — got {steps:?}");
        if let Some(sw) = pos("Play(Swamp)") {
            assert!(f < sw, "fetch must precede a Swamp — got {steps:?}");
        }
    }

    /// Cross-check: the same tutor hand is a deterministic TURN 2 (tutor T1, draw + cast
    /// T2) — the line above is what realizes it.
    #[test]
    fn line_tutor_is_turn_two() {
        let s = setup(
            &[],
            &["Personal Tutor", "Polluted Delta", "Swamp", "Dark Ritual"],
            &["Doomsday", "Underground Sea"],
        );
        assert_eq!(deterministic_cast_turn(&s, PlayerId::Us, 3), Some(2));
    }

    /// Petal reduces: BB in play + Lotus Petal + Doomsday → crack the petal for the last
    /// black pip, then cast.
    #[test]
    fn line_petal_reduces() {
        assert_eq!(
            line(&["Underground Sea", "Underground Sea"], &["Lotus Petal", "Doomsday"], &[], 3),
            ["Petal(Lotus Petal)", "DD(Doomsday)"]
        );
    }

    /// Fetch in hand reduces: the emitted line opens by developing it (it cracks into a
    /// black source on the next window).
    #[test]
    fn line_fetch_in_hand_developed_first() {
        let steps = line(&[], &["Polluted Delta", "Dark Ritual", "Doomsday"], &["Underground Sea"], 3);
        assert_eq!(steps.first().map(String::as_str), Some("Play(Polluted Delta)"),
            "open by developing the fetch — got {steps:?}");
    }
}
