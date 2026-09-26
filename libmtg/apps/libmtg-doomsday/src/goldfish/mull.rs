//! Goldfish mulligan modes.
//!
//! Three pluggable opening-hand policies for the cast-Doomsday-ASAP pilot, so the
//! goldfish can show a deck under different mulligan disciplines (the headline
//! P(cast by cutoff) shifts a lot with how hard you mulligan):
//!
//! - [`MullMode::Keep7`] — never mulligan. Pure raw deck speed, no mulligan dynamics.
//! - [`MullMode::Realistic`] — a *player's-thought-process* heuristic: keep a hand
//!   with real mana and an actual plan to cast Doomsday; mulligan no-mana / flooded /
//!   do-nothing hands. Built and validated card-by-card against human judgement (see
//!   the `sample_hands` example). It deliberately does NOT consult `p_cast_by` — every
//!   keep/mull names an inspectable reason.
//! - [`MullMode::Aggressive`] — the fanatical race bar: keep only a guaranteed-or-nearly
//!   guaranteed fast Doomsday — a DETERMINISTIC line by the cutoff, or Doomsday + BB +
//!   a castable cantrip to dig the last black. No `p_cast_by`; speculative cantrip hands
//!   are thrown back, so it rewards decks with more fast enablers (rituals/petals/tutors).
//!
//! The `Realistic` rule is the load-bearing one. Its logic (the "G1 / blind" checklist):
//!
//! ```text
//! KEEP if any fires (the solver does the deterministic checks; heuristics do the rest):
//!   - at 4 cards (mulligans_taken >= 3);
//!   - a DETERMINISTIC Doomsday line by the cutoff   (this is "DD + a way to reach BBB");
//!   - Tamiyo plan:  Tamiyo + a blue land to cast it + (a look or a fetch to fuel);
//!   - combo-in-progress:  Doomsday in hand + a black source + a castable look;
//!   - dig:  two castable looks.
//! else MULL — i.e. no colored land, flooded (>= 4 lands), or just sources + filler.
//! ```
//!
//! A "look" is a card-selection effect you can actually pay for: a cast-to-dig cantrip
//! (Ponder/Brainstorm/Consider — needs blue) or a free cycling cantrip (Street Wraith —
//! always castable). Sources are counted the way the *game* sees them: only
//! unconditional colored lands are a mana base (Cavern of Souls, colored only for
//! creatures, and one-shots like Lotus Petal are not).

use libmtg_engine::{Color, PlayerId, SimState, SourceZone, ActivationTiming, CardDef};

use super::recipe::{self, CardRole};

/// Opening-hand discipline for the goldfish pilot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MullMode {
    /// Never mulligan — keep the opening 7.
    Keep7,
    /// Player-heuristic "keep a real plan" rule (the default).
    Realistic,
    /// Keep only a deterministic Doomsday line by the cutoff (the fanatical race bar).
    Aggressive,
    /// The learned P(cast)-GBDT policy, raw-speed objective (the fastest mulligan).
    LearnedSpeed,
    /// The learned policy scoring P(cast) x resolve(resources): ties Aggressive on speed but keeps
    /// far more interaction in hand.
    LearnedInteractive,
}

impl Default for MullMode {
    fn default() -> Self { MullMode::Realistic }
}

impl MullMode {
    /// Parse a web/CLI token; unknown → default.
    pub fn from_str_or_default(s: &str) -> MullMode {
        match s {
            "keep7" => MullMode::Keep7,
            "aggressive" => MullMode::Aggressive,
            "learned-speed" | "learned_speed" => MullMode::LearnedSpeed,
            "learned-interactive" | "learned_interactive" => MullMode::LearnedInteractive,
            _ => MullMode::Realistic,
        }
    }
}

/// Cards that contribute to no realistic plan in a Doomsday hand — they're read as if
/// they weren't in the hand (a junk-heavy hand thus has too few real cards to form a
/// plan, and mulligans). A curated list because they're context-free dead weight here:
/// the alt win-con and tutor-target (Thassa's Oracle), reanimation with no target
/// (Unearth), the hand-discard mana rock (LED), a marginal cantrip (Edge of Autumn), the
/// secondary win-con (Jace), and the utility lands (Wasteland, Cavern of Souls). Extend
/// as imported lists surface more. (Excess *real* lands are handled separately, as flood.)
const AIR: &[&str] = &[
    "Thassa's Oracle", "Unearth", "Lion's Eye Diamond", "Edge of Autumn",
    "Jace, Wielder of Mysteries", "Wasteland", "Cavern of Souls",
];

pub(crate) fn is_air(name: &str) -> bool { AIR.contains(&name) }

/// Explicit, inspectable signals over the opening hand — plain card counts plus one
/// trustworthy deterministic-solver fact. No `p_cast_by`. Named-air cards (see [`AIR`])
/// are excluded from every count, as if they weren't drawn.
#[derive(Clone, Copy, Debug, Default)]
pub struct HandSignals {
    /// Lands (a fetch counts) that tap blue UNCONDITIONALLY.
    pub blue_lands: u32,
    /// Lands (a fetch counts) that tap black UNCONDITIONALLY.
    pub black_lands: u32,
    /// Distinct lands tapping U or B (a dual counts once) — the real mana base.
    pub colored_lands: u32,
    /// All lands incl. colorless (Wasteland) — for the flood check.
    pub lands: u32,
    /// One-shot any-colour mana (Lotus Petal): not a land, not a base.
    pub petals: u32,
    /// Dark Ritual: needs a black seed, so not a base source either.
    pub rituals: u32,
    /// DEEP looks — cast-to-dig cantrips with a blue land available (Ponder, Brainstorm,
    /// Consider, Flow State): real selection.
    pub deep_looks: u32,
    /// MARGINAL looks — shallow selection: a free cycler (Street Wraith), a surveil land
    /// (Undercity Sewers), or a fetch while holding >= 2 lands. Worth at most 1 (no time
    /// to dig shallow twice by the cutoff).
    pub marginal_looks: u32,
    /// Named-air cards in hand (see [`AIR`]) — bottomable junk; feeds the post-bottom
    /// flood check (air competes with excess lands for the London bottoms).
    pub named_air: u32,
    /// Doomsday (or a tutor that finds it) in hand.
    pub has_dd: bool,
    /// Tamiyo, Inquisitive Student in hand.
    pub has_tamiyo: bool,
    /// A fetch in hand (fixes colours / fuels the Tamiyo flip).
    pub fetch: bool,
    /// Real non-mana, non-dig, non-payoff cards — interaction (Force/Daze/Thoughtseize)
    /// and threat bodies (Tamiyo/Murktide/Bowmasters). The "support" that carries a
    /// thin-dig hand over the top.
    pub supporters: u32,
    /// The solver finds a guaranteed send (Doomsday OR car) by the cutoff.
    pub det_line: bool,
    /// The car is exactly ONE Lotus-Petal-equivalent short of a deterministic pop by the
    /// cutoff (`recipe::car_pop_shortfall == 1`) — "almost sends": one mana or one cast
    /// away, which a single cantrip very likely digs into. (`false` for car-less decks, so
    /// the tempo list is untouched.) The deterministic case (shortfall 0) is `det_line`.
    pub car_almost: bool,
    /// The hand can deterministically flip Tamiyo by turn 2 (the fast-Tamiyo plan).
    pub tami_fast_flip: bool,
}

impl HandSignals {
    /// A real mana base: at least one colored LAND (one-shots don't count).
    fn mana_base(&self) -> bool { self.colored_lands >= 1 }
    /// A real black source toward BBB (land or Petal — a bare Ritual needs a seed).
    fn black_src(&self) -> bool { self.black_lands >= 1 || self.petals >= 1 }
    /// BB — two black units (lands and/or Petals).
    fn bb(&self) -> bool { self.black_lands + self.petals >= 2 }
    /// Effective look count: deep looks plus AT MOST one marginal look (no time to dig
    /// shallow twice before the cutoff).
    fn looks(&self) -> u32 { self.deep_looks + self.marginal_looks.min(1) }
    /// Flooded AFTER the London bottom: you bottom named-air first, then excess lands, so
    /// you're only truly clogged if >= 4 lands remain once you've shed what you can.
    fn flooded(&self, mulligans_taken: u32) -> bool {
        let land_bottoms = mulligans_taken.saturating_sub(self.named_air);
        self.colored_lands.saturating_sub(land_bottoms) >= 4
    }
}

/// Does `def` tap for `color` from the battlefield UNCONDITIONALLY (default timing,
/// no condition) — mirrors the solver's source rule. Lotus Petal (any colour) and
/// duals count; Cavern of Souls (colored only for creature spells) does not.
fn taps_for(def: &CardDef, color: Color) -> bool {
    def.mana_abilities().iter().any(|ma| {
        matches!(ma.source_zone, SourceZone::Battlefield)
            && ma.timing == ActivationTiming::Default
            && ma.condition.is_none()
            && ma.produces.contains(&color)
    })
}

/// Castability coverage: can the hand pay `def`'s mana cost off STABLE sources — lands
/// only, not one-shot Petals? Generic pips come from any land; a coloured pip needs a
/// land of that colour. So Ponder `{U}` wants one blue land, but Flow State `{1}{U}`
/// wants two lands (one blue) — a lone Sea can't cast it, and an uncastable cantrip is
/// dead, not a look.
fn stably_castable(s: &HandSignals, def: &CardDef) -> bool {
    let (mut blue, mut black, mut generic, mut num) = (0u32, 0u32, 0u32, 0u32);
    for ch in def.mana_cost().trim().chars() {
        if let Some(d) = ch.to_digit(10) {
            num = num * 10 + d;
            continue;
        }
        generic += std::mem::take(&mut num);
        match ch {
            'U' => blue += 1,
            'B' => black += 1,
            'W' | 'R' | 'G' | 'C' => generic += 1,
            _ => {}
        }
    }
    generic += num;
    s.colored_lands >= blue + black + generic && s.blue_lands >= blue && s.black_lands >= black
}

/// Compute the [`HandSignals`] for `who`'s current hand at the given cutoff. Named-air
/// cards (see [`AIR`]) are skipped in every count, as if they weren't drawn.
pub fn hand_signals(state: &SimState, who: PlayerId, cutoff: u32) -> HandSignals {
    let mut s = HandSignals {
        // A guaranteed line by the cutoff via EITHER wincon (Doomsday or the car) — the engine
        // detects both, so a deterministic car hand is kept, not thrown back for lacking a
        // Doomsday line. (No bespoke car rule beyond this — the solver does the work.)
        det_line: recipe::deterministic_send_turn(state, who, cutoff).is_some(),
        // One Petal-equivalent short of a guaranteed car pop — the "almost sends" keep.
        car_almost: recipe::car_pop_shortfall(state, who, cutoff) == 1,
        // Fast Tamiyo plan: a deterministic flip by turn 2 (see `recipe::tamiyo_flip_turn`).
        tami_fast_flip: recipe::tamiyo_flip_turn(state, who, 2).is_some(),
        ..Default::default()
    };
    // Pass 1: mana base (so colours are known before classifying looks). Count + skip air.
    for card in state.hand_of(who) {
        if is_air(&card.catalog_key) { s.named_air += 1; continue; }
        let role = recipe::card_role(state, who, card.id);
        let Some(def) = state.catalog.get(&card.catalog_key) else { continue };
        if def.is_land() {
            s.lands += 1;
            let is_fetch = matches!(role, CardRole::Fetch);
            let u = is_fetch || taps_for(def, Color::Blue);
            let b = is_fetch || taps_for(def, Color::Black);
            if u { s.blue_lands += 1; }
            if b { s.black_lands += 1; }
            if u || b { s.colored_lands += 1; }
        }
        match role {
            CardRole::Payoff | CardRole::PayoffTutor => s.has_dd = true,
            CardRole::Petal => s.petals += 1,
            CardRole::Ritual => s.rituals += 1,
            CardRole::Fetch => s.fetch = true,
            _ => {}
        }
        if card.catalog_key == "Tamiyo, Inquisitive Student" { s.has_tamiyo = true; }
        // A "supporter" = a real NON-LAND, non-dig, non-payoff card: interaction
        // (Force/Daze/Thoughtseize), a threat body (Tamiyo/Murktide/…), OR an accelerant
        // (Ritual/Petal) — going faster dodges Wasteland / the counter window, so speed
        // is soft protection, the same category of resilience as hard interaction.
        let is_dig = def.digs_on_resolve() || def.has_cycling_draw();
        let is_payoff = matches!(role, CardRole::Payoff | CardRole::PayoffTutor);
        let is_accelerant = matches!(role, CardRole::Ritual | CardRole::Petal);
        if !is_dig && !def.is_land() && !is_payoff && !is_accelerant { s.supporters += 1; }
    }
    // Accelerants don't scale (ritual+ritual < ritual): one is a weak second piece for
    // the dig plan (handled in `realistic_keep`); any BEYOND the first are AIR — they
    // join the bottomable pool. (Interaction is the opposite: daze+daze > daze, so real
    // supporters are counted in full above.)
    s.named_air += (s.petals + s.rituals).saturating_sub(1);
    // Pass 2: classify looks (needs pass-1 mana base). DEEP = a cast-to-dig cantrip you
    // can stably pay for; MARGINAL = a free cycler (Street Wraith), a surveil land
    // (Undercity Sewers), or a fetch backed by a second land.
    let have_2_lands = s.lands >= 2;
    for card in state.hand_of(who) {
        if is_air(&card.catalog_key) { continue; }
        let Some(def) = state.catalog.get(&card.catalog_key) else { continue };
        if def.digs_on_resolve() {
            if stably_castable(&s, def) { s.deep_looks += 1; }
        } else if def.has_cycling_draw() {
            s.marginal_looks += 1;
        } else {
            let role = recipe::card_role(state, who, card.id);
            if matches!(role, CardRole::BlackLandTapped)
                || (matches!(role, CardRole::Fetch) && have_2_lands)
            {
                s.marginal_looks += 1;
            }
        }
    }
    s
}

/// The "Realistic" keep decision at mulligan depth `mulligans_taken` — keep iff the best
/// `7 − k` cards (named-air + excess lands bottomed) hold a viable plan. Returns true to
/// KEEP; the always-keep-at-4 floor is applied by [`should_mulligan`]. The plans:
/// - **deterministic line** — the solver guarantees Doomsday by the cutoff;
/// - **fast dd** — Doomsday + a black source + a deep look (or BB + any look);
/// - **tami flip** — a deterministic Tamiyo flip by turn 2;
/// - **dig** — a deep look + a second piece (another look, or a supporter / accelerant),
///   unless flooded once the excess lands are bottomed.
pub fn realistic_keep(s: &HandSignals, mulligans_taken: u32) -> bool {
    if s.det_line { return true; }
    if !s.mana_base() { return false; } // no real colored land → nothing to cast off
    // Fast DD: have the payoff + a route to BBB. A lone black source needs a deep (real)
    // look; BB lets a marginal look finish the third black.
    if s.has_dd && s.black_src() && (s.deep_looks >= 1 || (s.bb() && s.looks() >= 1)) {
        return true;
    }
    if s.tami_fast_flip { return true; }
    // Almost-car: one Petal-equivalent short of a guaranteed pop, WITH a real cantrip to dig
    // for that piece — the user's "sea, rit, car, ponder, daze → ponder into one more mana".
    // The solver says you're a single mana/cast away; a deep look very likely closes it by
    // the cutoff. (`car_almost` is false without a car in hand, so this only fires for the
    // two-wincon list.)
    if s.car_almost && s.deep_looks >= 1 { return true; }
    // Dig: a deep look + a SECOND PIECE, scaled by hand size. At the opening 7 the second
    // piece must be STRONG — a 2nd deep look, or a real supporter (interaction / threat).
    // After a mulligan the bar relaxes (you bottom the air, so the kept hand is leaner):
    // a marginal look or a single accelerant is enough. Only if not flooded post-bottom.
    if s.black_src() && !s.flooded(mulligans_taken) && s.deep_looks >= 1 {
        let strong = s.deep_looks >= 2 || s.supporters >= 1;
        let relaxed = strong || s.marginal_looks >= 1 || (s.petals + s.rituals) >= 1;
        let second_piece = if mulligans_taken == 0 { strong } else { relaxed };
        if second_piece {
            return true;
        }
    }
    false
}

/// Aggressive: the fanatical race bar. Keep only a hand that is a guaranteed-or-nearly
/// guaranteed fast Doomsday, mulliganing anything speculative. Two ways in:
///   1. a DETERMINISTIC line by the cutoff (the solver guarantees it); or
///   2. near-deterministic: Doomsday in hand + **BB** (two black units — lands/petals)
///      + a castable cantrip to dig the third black / the finisher.
/// No `p_cast_by`. (`KEEP7` env retained for the apples-to-apples gameplay experiment.)
pub fn aggressive_keep(state: &SimState, who: PlayerId, cutoff: u32) -> bool {
    if std::env::var("KEEP7").is_ok() { return true; }
    let s = hand_signals(state, who, cutoff);
    if s.det_line { return true; }
    s.has_dd && s.bb() && (s.deep_looks + s.marginal_looks) >= 1
}

/// Whether to MULLIGAN this hand under `mode`. Always keep at 4 cards
/// (`mulligans_taken >= 3`); otherwise dispatch to the mode's keep rule.
pub fn should_mulligan(
    mode: MullMode,
    state: &SimState,
    who: PlayerId,
    cutoff: u32,
    mulligans_taken: u32,
) -> bool {
    use super::learned_mull::LearnedObjective;
    // The learned policies carry their own floor (they keep a 1-card hand), so they bypass the
    // heuristic 4-card cap and follow the backward-induction bars all the way down.
    if let MullMode::LearnedSpeed | MullMode::LearnedInteractive = mode {
        let hand: Vec<&str> = state.hand_of(who).map(|c| c.catalog_key.as_str()).collect();
        let obj = if mode == MullMode::LearnedSpeed {
            LearnedObjective::Speed
        } else {
            LearnedObjective::Interactive
        };
        return !super::learned_mull::learned_keep(&hand, mulligans_taken, state.on_play, obj);
    }
    if mulligans_taken >= 3 { return false; } // always keep the 4-card hand
    let keep = match mode {
        MullMode::Keep7 => true,
        MullMode::Realistic => realistic_keep(&hand_signals(state, who, cutoff), mulligans_taken),
        MullMode::Aggressive => aggressive_keep(state, who, cutoff),
        MullMode::LearnedSpeed | MullMode::LearnedInteractive => unreachable!(),
    };
    !keep
}

#[cfg(test)]
mod tests {
    use super::*;
    use libmtg_engine::{build_catalog, PlayerState, SimState, Zone};

    // The sample tempo DD list — fills the library so fetch targets / deterministic
    // lines resolve. (Same list the `sample_hands` example deals from.)
    const TEMPO: &[(&str, i32)] = &[
        ("Underground Sea", 3), ("Polluted Delta", 4), ("Flooded Strand", 1),
        ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Marsh Flats", 1),
        ("Island", 1), ("Swamp", 1), ("Undercity Sewers", 2), ("Wasteland", 3),
        ("Cavern of Souls", 1), ("Lotus Petal", 2), ("Lion's Eye Diamond", 1),
        ("Dark Ritual", 4), ("Doomsday", 4), ("Brainstorm", 4), ("Ponder", 4),
        ("Consider", 1), ("Edge of Autumn", 1), ("Force of Will", 4), ("Daze", 3),
        ("Thoughtseize", 2), ("Street Wraith", 1), ("Thassa's Oracle", 1),
        ("Unearth", 1), ("Tamiyo, Inquisitive Student", 4), ("Orcish Bowmasters", 2),
        ("Murktide Regent", 2),
    ];

    /// Build an opening-hand state (hand in hand, the rest of the tempo deck in
    /// library) and return the `Realistic` keep decision (at the opening 7).
    fn realistic_keeps(hand: &[&str]) -> bool { realistic_keeps_at(hand, 0) }
    /// As above, but at mulligan depth `mulls` (keeping `7 − mulls` after the London bottom).
    fn realistic_keeps_at(hand: &[&str], mulls: u32) -> bool {
        let catalog = build_catalog();
        let mut pool: Vec<String> = Vec::new();
        for (n, q) in TEMPO { for _ in 0..*q { pool.push((*n).to_string()); } }
        for h in hand {
            if let Some(i) = pool.iter().position(|c| c == h) { pool.remove(i); }
        }
        let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        s.catalog = catalog;
        for h in hand { s.place_card(PlayerId::Us, h, Zone::Hand { known: false }); }
        for n in &pool { s.place_card(PlayerId::Us, n, Zone::Library); }
        realistic_keep(&hand_signals(&s, PlayerId::Us, 4), mulls)
    }

    /// Every opening hand we hand-judged during design, with the human "blind" verdict.
    /// The Realistic rule must reproduce all of them. (true = KEEP)
    #[test]
    fn realistic_matches_human_blind_calls() {
        let cases: &[(bool, &[&str])] = &[
            // ── keeps ──
            (true,  &["Tamiyo, Inquisitive Student", "Doomsday", "Scalding Tarn", "Daze", "Ponder", "Murktide Regent", "Polluted Delta"]), // A
            (true,  &["Force of Will", "Misty Rainforest", "Brainstorm", "Doomsday", "Ponder", "Undercity Sewers", "Dark Ritual"]), // E (det line)
            (true,  &["Brainstorm", "Doomsday", "Misty Rainforest", "Orcish Bowmasters", "Polluted Delta", "Tamiyo, Inquisitive Student", "Murktide Regent"]), // F
            (true,  &["Misty Rainforest", "Dark Ritual", "Dark Ritual", "Doomsday", "Daze", "Tamiyo, Inquisitive Student", "Orcish Bowmasters"]), // G (det line)
            (true,  &["Tamiyo, Inquisitive Student", "Lion's Eye Diamond", "Ponder", "Flooded Strand", "Doomsday", "Street Wraith", "Force of Will"]), // I
            (true,  &["Dark Ritual", "Undercity Sewers", "Scalding Tarn", "Dark Ritual", "Ponder", "Swamp", "Tamiyo, Inquisitive Student"]), // J
            (true,  &["Tamiyo, Inquisitive Student", "Ponder", "Scalding Tarn", "Force of Will", "Polluted Delta", "Murktide Regent", "Orcish Bowmasters"]), // K
            (true,  &["Brainstorm", "Scalding Tarn", "Edge of Autumn", "Polluted Delta", "Ponder", "Cavern of Souls", "Thoughtseize"]), // L
            (true,  &["Ponder", "Underground Sea", "Brainstorm", "Edge of Autumn", "Unearth", "Brainstorm", "Polluted Delta"]), // M
            (true,  &["Swamp", "Thoughtseize", "Unearth", "Misty Rainforest", "Consider", "Brainstorm", "Wasteland"]), // N
            (true,  &["Daze", "Consider", "Tamiyo, Inquisitive Student", "Underground Sea", "Ponder", "Dark Ritual", "Ponder"]), // O
            (true,  &["Marsh Flats", "Street Wraith", "Wasteland", "Daze", "Edge of Autumn", "Wasteland", "Ponder"]), // P
            // one land but a dig + a wall of interaction/threats (low air) — a keep.
            (true,  &["Underground Sea", "Thoughtseize", "Force of Will", "Murktide Regent", "Murktide Regent", "Ponder", "Force of Will"]), // batch #12
            // H but the land is a blue Sea, so Consider IS castable off stable mana — a
            // terrible-but-lowest-end keep (dig + support).
            (true,  &["Consider", "Underground Sea", "Force of Will", "Orcish Bowmasters", "Edge of Autumn", "Lotus Petal", "Lotus Petal"]), // H-with-Sea
            // ── mulls ──
            (false, &["Lotus Petal", "Ponder", "Murktide Regent", "Tamiyo, Inquisitive Student", "Wasteland", "Dark Ritual", "Dark Ritual"]), // B (no colored land)
            (false, &["Tamiyo, Inquisitive Student", "Force of Will", "Daze", "Ponder", "Edge of Autumn", "Brainstorm", "Thoughtseize"]), // C (no land)
            (false, &["Cavern of Souls", "Force of Will", "Murktide Regent", "Ponder", "Wasteland", "Swamp", "Orcish Bowmasters"]), // D (Ponder uncastable, no blue)
            // H: its only land is a black Swamp, so Consider is castable only by burning a
            // one-shot Petal — not a real dig. No stable look ⇒ no plan ⇒ mull.
            (false, &["Consider", "Swamp", "Force of Will", "Orcish Bowmasters", "Edge of Autumn", "Lotus Petal", "Lotus Petal"]), // H
            // round 4: two Ponders but NO black source — digs toward an uncastable DD.
            (false, &["Thoughtseize", "Ponder", "Daze", "Unearth", "Island", "Ponder", "Wasteland"]), // r4-#12
            // round 4: explosive mana but only one look and no DD/Tamiyo — speculative.
            (false, &["Scalding Tarn", "Ponder", "Lotus Petal", "Wasteland", "Misty Rainforest", "Lotus Petal", "Dark Ritual"]), // r4-#13
        ];
        let mut wrong = Vec::new();
        for (i, (want, hand)) in cases.iter().enumerate() {
            let got = realistic_keeps(hand);
            if got != *want {
                wrong.push(format!(
                    "  case {i}: want {} got {} — [{}]",
                    if *want { "KEEP" } else { "MULL" },
                    if got { "KEEP" } else { "MULL" },
                    hand.join(", ")));
            }
        }
        assert!(wrong.is_empty(), "Realistic rule disagreed with human calls:\n{}", wrong.join("\n"));
    }

    /// Hand-size scaling: a thin dig (deep look backed only by marginal looks / a single
    /// accelerant) is bad at 7 but a keep once the air is bottomed. And no deep look is
    /// never a dig, regardless of marginal looks or support.
    #[test]
    fn realistic_scales_with_hand_size() {
        // r4-#13: Ponder (deep) + 2 fetches (marginal) + 2 Petals + Dark Ritual — no
        // real supporter, no second deep look. Mull at 7, keep at 5.
        let thin = &["Scalding Tarn", "Ponder", "Lotus Petal", "Wasteland",
                     "Misty Rainforest", "Lotus Petal", "Dark Ritual"];
        assert!(!realistic_keeps_at(thin, 0), "thin dig is a mull at the 7");
        assert!(realistic_keeps_at(thin, 2), "thin dig is a keep at the 5 (air bottomed)");
        // Marginal looks (Sewers + Street Wraith) + interaction, but NO deep look — not a
        // dig at any size; we wouldn't keep sewers+wraith as if it were ponder+brainstorm.
        let no_deep = &["Undercity Sewers", "Street Wraith", "Underground Sea",
                        "Force of Will", "Daze", "Thoughtseize", "Murktide Regent"];
        assert!(!realistic_keeps_at(no_deep, 0), "no deep look ⇒ no dig at 7");
        assert!(!realistic_keeps_at(no_deep, 2), "no deep look ⇒ no dig at 5");
    }

    /// The "almost sends car" keep (vroomsday only). A hand one Petal-equivalent short of a
    /// guaranteed pop, with a real cantrip to dig for it, is a keep the Doomsday-centric
    /// branches (fast-DD, dig's second-piece) all miss. Tested on synthetic signals so it's
    /// isolated from card classification.
    #[test]
    fn realistic_keeps_almost_car_with_a_dig() {
        // A Sea (mana base + black source) + a castable Ponder (deep look). No Doomsday, no
        // Tamiyo, no dig "second piece" (no 2nd deep look, no supporter, no accelerant).
        let base = HandSignals {
            colored_lands: 1, black_lands: 1, blue_lands: 1, deep_looks: 1,
            ..Default::default()
        };
        // Without the almost-car signal this skeleton has no plan → mull.
        assert!(!realistic_keep(&base, 0), "no payoff, no dig second-piece → mull");
        // One piece short of a car pop → the almost-car branch keeps it.
        assert!(realistic_keep(&HandSignals { car_almost: true, ..base }, 0),
            "almost-car + a dig → keep");
        // The dig must be REAL — almost-car with no cantrip to find the piece is not a keep.
        assert!(!realistic_keep(&HandSignals { car_almost: true, deep_looks: 0, ..base }, 0),
            "almost-car but no cantrip to dig → mull");
    }

    #[test]
    fn keep7_never_mulligans_and_floor_holds() {
        // Keep7 always keeps; the 4-card hand is always kept in every mode.
        let catalog = build_catalog();
        let mut s = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        s.catalog = catalog;
        for n in ["Island", "Wasteland", "Force of Will"] { // a junk 3-ish hand
            s.place_card(PlayerId::Us, n, Zone::Hand { known: false });
        }
        assert!(!should_mulligan(MullMode::Keep7, &s, PlayerId::Us, 4, 0));
        assert!(!should_mulligan(MullMode::Realistic, &s, PlayerId::Us, 4, 3)); // 4-card: keep
    }
}
