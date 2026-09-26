use std::collections::{BTreeMap, HashMap};

use libmtg_decklist::Decklist;
use rand::{rngs::SmallRng, seq::SliceRandom, SeedableRng};
use serde::Serialize;

/// Coarse roles used only for opening-hand experiments. These are deliberately
/// separate from the rules engine: they let us measure composition/consistency
/// before every Forge card has a full rules implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeRole {
    Land,
    FastMana,
    Engine,
    Tutor,
    Lock,
    Payoff,
    Utility,
}

pub fn role_for(name: &str) -> ForgeRole {
    match name {
        "Ancient Tomb" | "Urza's Workshop" | "Urza's Tower" | "Urza's Saga" | "Planar Nexus" => ForgeRole::Land,
        "Lotus Petal" | "Mox Opal" | "Grim Monolith" | "Basalt Monolith" => ForgeRole::FastMana,
        "Mystic Forge" | "The One Ring" | "Tezzeret, Cruel Captain" | "Karn, the Great Creator" => ForgeRole::Engine,
        "Transmute Artifact" => ForgeRole::Tutor,
        "Trinisphere" | "Disruptor Flute" => ForgeRole::Lock,
        "Paradox Engine" | "Relic of Sauron" | "Giant's Boulder" | "Kozilek's Command" => ForgeRole::Payoff,
        _ => ForgeRole::Utility,
    }
}

fn expanded_main(deck: &Decklist) -> Vec<String> {
    let mut out = Vec::with_capacity(deck.main_count() as usize);
    for e in &deck.main {
        for _ in 0..e.qty {
            out.push(e.name.clone());
        }
    }
    // Stable sorting makes common-random-number A/B trials more correlated for
    // two nearly-identical 60s. The simulator shuffles immediately afterwards.
    out.sort();
    out
}

fn role_count(hand: &[String], role: ForgeRole) -> usize {
    hand.iter().filter(|c| role_for(c) == role).count()
}

fn contains(hand: &[String], card: &str) -> bool {
    hand.iter().any(|c| c == card)
}

/// Baseline mulligan policy, intentionally simple and named "v0" in output.
/// It is a starting point for learning/hand-labeling, not a claim of optimality.
pub fn keep_v0(hand: &[String], mulligans_taken: u32) -> bool {
    if mulligans_taken >= 2 {
        return true; // never go below five in the baseline policy
    }
    let lands = role_count(hand, ForgeRole::Land);
    let fast = role_count(hand, ForgeRole::FastMana);
    let engines = role_count(hand, ForgeRole::Engine);
    let tutors = role_count(hand, ForgeRole::Tutor);
    let locks = role_count(hand, ForgeRole::Lock);
    let mana_pieces = lands + fast;
    let business = engines + tutors + locks;

    // Reject obvious no-mana/no-business hands. This is deliberately permissive:
    // exact castability depends on card rules we are implementing next.
    mana_pieces >= 2 && business >= 1
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct OpeningStats {
    pub trials: u64,
    pub keep_7_pct: f64,
    pub keep_6_pct: f64,
    pub keep_5_pct: f64,
    pub avg_mulligans: f64,
    pub engine_access_pct: f64,
    pub tutor_access_pct: f64,
    pub lock_piece_pct: f64,
    pub fast_mana_pct: f64,
    pub engine_plus_fast_mana_pct: f64,
    pub mystic_forge_pct: f64,
    pub karn_pct: f64,
    pub ring_pct: f64,
    pub transmute_pct: f64,
    pub trinisphere_pct: f64,
    pub card_in_kept_hand_pct: BTreeMap<String, f64>,
}

#[derive(Debug, Default, Clone)]
struct OpeningAccum {
    trials: u64,
    kept_at: [u64; 3],
    mulligans: u64,
    engine: u64,
    tutor: u64,
    lock: u64,
    fast: u64,
    engine_fast: u64,
    forge: u64,
    karn: u64,
    ring: u64,
    transmute: u64,
    trinisphere: u64,
    cards: HashMap<String, u64>,
}

impl OpeningAccum {
    fn observe(&mut self, hand: &[String], mulligans: u32) {
        self.trials += 1;
        self.mulligans += mulligans as u64;
        self.kept_at[mulligans.min(2) as usize] += 1;
        let engine = role_count(hand, ForgeRole::Engine) > 0;
        let tutor = role_count(hand, ForgeRole::Tutor) > 0;
        let lock = role_count(hand, ForgeRole::Lock) > 0;
        let fast = role_count(hand, ForgeRole::FastMana) > 0;
        self.engine += engine as u64;
        self.tutor += tutor as u64;
        self.lock += lock as u64;
        self.fast += fast as u64;
        self.engine_fast += (engine && fast) as u64;
        self.forge += contains(hand, "Mystic Forge") as u64;
        self.karn += contains(hand, "Karn, the Great Creator") as u64;
        self.ring += contains(hand, "The One Ring") as u64;
        self.transmute += contains(hand, "Transmute Artifact") as u64;
        self.trinisphere += contains(hand, "Trinisphere") as u64;
        for card in hand {
            *self.cards.entry(card.clone()).or_default() += 1;
        }
    }

    fn finish(self) -> OpeningStats {
        let n = self.trials.max(1) as f64;
        let pct = |x: u64| 100.0 * x as f64 / n;
        let mut per_card = BTreeMap::new();
        for (name, count) in self.cards {
            per_card.insert(name, pct(count));
        }
        OpeningStats {
            trials: self.trials,
            keep_7_pct: pct(self.kept_at[0]),
            keep_6_pct: pct(self.kept_at[1]),
            keep_5_pct: pct(self.kept_at[2]),
            avg_mulligans: self.mulligans as f64 / n,
            engine_access_pct: pct(self.engine),
            tutor_access_pct: pct(self.tutor),
            lock_piece_pct: pct(self.lock),
            fast_mana_pct: pct(self.fast),
            engine_plus_fast_mana_pct: pct(self.engine_fast),
            mystic_forge_pct: pct(self.forge),
            karn_pct: pct(self.karn),
            ring_pct: pct(self.ring),
            transmute_pct: pct(self.transmute),
            trinisphere_pct: pct(self.trinisphere),
            card_in_kept_hand_pct: per_card,
        }
    }
}

fn draw_london_hand(deck: &[String], trial_seed: u64) -> (Vec<String>, u32) {
    let mut mulligans = 0u32;
    loop {
        // A London mulligan is a fresh shuffle each time. Derive a deterministic
        // sub-seed so every trial is reproducible and A/B comparisons use the same
        // random-number schedule.
        let sub_seed = trial_seed ^ ((mulligans as u64 + 1).wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut rng = SmallRng::seed_from_u64(sub_seed);
        let mut shuffled = deck.to_vec();
        shuffled.shuffle(&mut rng);
        let seven: Vec<String> = shuffled.into_iter().take(7).collect();
        if keep_v0(&seven, mulligans) {
            if mulligans == 0 {
                return (seven, 0);
            }
            // Approximate London bottom using role/card value. Exact bottoming will
            // move into the engine strategy once all relevant cards are implemented.
            let mut scored: Vec<(String, i32)> = seven.into_iter().map(|c| {
                let score = match role_for(&c) {
                    ForgeRole::Engine => 90,
                    ForgeRole::Tutor => 80,
                    ForgeRole::Land => 70,
                    ForgeRole::FastMana => 65,
                    ForgeRole::Lock => 60,
                    ForgeRole::Payoff => 45,
                    ForgeRole::Utility => 30,
                } + match c.as_str() {
                    "Mystic Forge" => 20,
                    "Karn, the Great Creator" => 15,
                    "The One Ring" => 12,
                    _ => 0,
                };
                (c, score)
            }).collect();
            scored.sort_by_key(|(_, score)| -*score);
            let keep_n = 7usize.saturating_sub(mulligans as usize);
            return (scored.into_iter().take(keep_n).map(|(c, _)| c).collect(), mulligans);
        }
        mulligans += 1;
    }
}

pub fn simulate_openings(deck: &Decklist, trials: u64, seed: u64) -> OpeningStats {
    let expanded = expanded_main(deck);
    assert!(expanded.len() >= 7, "deck must contain at least seven mainboard cards");
    let mut acc = OpeningAccum::default();
    for i in 0..trials {
        let trial_seed = splitmix64(seed.wrapping_add(i));
        let (hand, mulls) = draw_london_hand(&expanded, trial_seed);
        acc.observe(&hand, mulls);
    }
    acc.finish()
}

#[derive(Debug, Clone, Serialize)]
pub struct CompareStats {
    pub seed: u64,
    pub policy: &'static str,
    pub a: OpeningStats,
    pub b: OpeningStats,
    pub delta_b_minus_a: CompareDelta,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompareDelta {
    pub keep_7_pp: f64,
    pub avg_mulligans: f64,
    pub engine_access_pp: f64,
    pub engine_plus_fast_mana_pp: f64,
    pub mystic_forge_pp: f64,
    pub trinisphere_pp: f64,
}

pub fn compare_openings(a: &Decklist, b: &Decklist, trials: u64, seed: u64) -> CompareStats {
    let sa = simulate_openings(a, trials, seed);
    let sb = simulate_openings(b, trials, seed);
    CompareStats {
        seed,
        policy: "forge_keep_v0",
        delta_b_minus_a: CompareDelta {
            keep_7_pp: sb.keep_7_pct - sa.keep_7_pct,
            avg_mulligans: sb.avg_mulligans - sa.avg_mulligans,
            engine_access_pp: sb.engine_access_pct - sa.engine_access_pct,
            engine_plus_fast_mana_pp: sb.engine_plus_fast_mana_pct - sa.engine_plus_fast_mana_pct,
            mystic_forge_pp: sb.mystic_forge_pct - sa.mystic_forge_pct,
            trinisphere_pp: sb.trinisphere_pct - sa.trinisphere_pct,
        },
        a: sa,
        b: sb,
    }
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_seed_is_reproducible() {
        let d = Decklist::parse_text("4 Ancient Tomb\n4 Urza's Workshop\n4 Lotus Petal\n4 Grim Monolith\n4 Mystic Forge\n4 Karn, the Great Creator\n4 Transmute Artifact\n4 The One Ring\n4 Manifold Key\n4 Urza's Saga\n4 Urza's Tower\n4 Planar Nexus\n4 Giant's Boulder\n4 Kozilek's Command\n4 Relic of Sauron\n");
        let a = simulate_openings(&d, 1000, 42);
        let b = simulate_openings(&d, 1000, 42);
        assert_eq!(a.keep_7_pct, b.keep_7_pct);
        assert_eq!(a.avg_mulligans, b.avg_mulligans);
        assert_eq!(a.mystic_forge_pct, b.mystic_forge_pct);
    }
}
