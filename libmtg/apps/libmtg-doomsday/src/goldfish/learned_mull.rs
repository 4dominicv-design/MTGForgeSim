//! Learned mulligan policy + per-hand model estimates. Two GBDTs (each a tree-walk over an
//! embedded ~156 KB blob, bit-exact to the sklearn model) share one tag representation:
//!   * `pcast` — P(cast Doomsday by T3) from the hand's properties.
//!   * `ettd`  — E[turns-to-Doomsday] (censored), the negative-result speed model.
//! On top of `pcast` sit the backward-induction keep-bars, exposed as two policies:
//!   * `Speed`       — keep iff best-subset P(cast) >= bar. The raw-speed optimum (78% by T3).
//!   * `Interactive` — keep iff best-subset [P(cast) * resolve(R)] >= bar, where R is the
//!                     Doomsday-utility of resources retained (protection + clock).
//!
//! Everything runs in WASM: just arithmetic over `const` blobs, no Python, no float surprises.

use std::sync::OnceLock;

use serde::Serialize;

pub use super::learned_gen::{
    add_card_tags, is_blue, BARS_DRAW_SPEED, BARS_DRAW_WIN, BARS_PLAY_SPEED, BARS_PLAY_WIN, N_TAGS,
};

/// Which learned objective to optimize.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LearnedObjective {
    Speed,
    Interactive,
}

struct Model {
    baseline: f32,
    tree_start: Vec<u32>,
    feat: Vec<u8>,
    thr: Vec<f32>,
    left: Vec<u32>,
    right: Vec<u32>,
    val: Vec<f32>,
}

static PCAST_BIN: &[u8] = include_bytes!("pcast_model.bin");
static ETTD_BIN: &[u8] = include_bytes!("ettd_model.bin");

fn pcast_model() -> &'static Model {
    static M: OnceLock<Model> = OnceLock::new();
    M.get_or_init(|| parse_model(PCAST_BIN))
}

fn ettd_model() -> &'static Model {
    static M: OnceLock<Model> = OnceLock::new();
    M.get_or_init(|| parse_model(ETTD_BIN))
}

fn parse_model(b: &[u8]) -> Model {
    let mut o = 0usize;
    let f32_at = |b: &[u8], o: &mut usize| -> f32 {
        let v = f32::from_le_bytes(b[*o..*o + 4].try_into().unwrap());
        *o += 4;
        v
    };
    let u32_at = |b: &[u8], o: &mut usize| -> u32 {
        let v = u32::from_le_bytes(b[*o..*o + 4].try_into().unwrap());
        *o += 4;
        v
    };
    let baseline = f32_at(b, &mut o);
    let n_trees = u32_at(b, &mut o) as usize;
    let tree_start: Vec<u32> = (0..n_trees).map(|_| u32_at(b, &mut o)).collect();
    let n_nodes = u32_at(b, &mut o) as usize;
    let feat: Vec<u8> = b[o..o + n_nodes].to_vec();
    o += n_nodes;
    let thr: Vec<f32> = (0..n_nodes).map(|_| f32_at(b, &mut o)).collect();
    let left: Vec<u32> = (0..n_nodes).map(|_| u32_at(b, &mut o)).collect();
    let right: Vec<u32> = (0..n_nodes).map(|_| u32_at(b, &mut o)).collect();
    let val: Vec<f32> = (0..n_nodes).map(|_| f32_at(b, &mut o)).collect();
    Model { baseline, tree_start, feat, thr, left, right, val }
}

/// Tree-walk inference: sum each tree's leaf value (+ baseline). `x` = [tags(N_TAGS), size, on_play].
fn predict(m: &Model, x: &[f32]) -> f32 {
    let mut sum = m.baseline;
    for &ts in &m.tree_start {
        let mut n = ts as usize;
        loop {
            let f = m.feat[n];
            if f == 255 {
                sum += m.val[n];
                break;
            }
            n = if x[f as usize] <= m.thr[n] {
                m.left[n] as usize
            } else {
                m.right[n] as usize
            };
        }
    }
    sum
}

fn features(cards: &[&str], on_play: bool) -> Vec<f32> {
    let mut x = vec![0.0f32; N_TAGS + 2];
    for c in cards {
        add_card_tags(c, &mut x);
    }
    x[N_TAGS] = cards.len() as f32;
    x[N_TAGS + 1] = if on_play { 1.0 } else { 0.0 };
    x
}

/// Model estimate of P(cast Doomsday by T3) for a hand.
pub fn pcast(cards: &[&str], on_play: bool) -> f32 {
    predict(pcast_model(), &features(cards, on_play))
}

/// Model estimate of E[turns-to-Doomsday] (censored) for a hand.
pub fn ettd(cards: &[&str], on_play: bool) -> f32 {
    predict(ettd_model(), &features(cards, on_play))
}

/// R = Doomsday-utility of resources retained: deployable protection + clock/backup.
pub fn resources(cards: &[&str]) -> f32 {
    let (mut fow, mut daze, mut ts, mut waste, mut clock, mut blue) = (0, 0, 0, 0, 0, 0);
    let mut dd = 0;
    for &c in cards {
        if is_blue(c) {
            blue += 1;
        }
        match c {
            "Force of Will" => fow += 1,
            "Daze" => daze += 1,
            "Thoughtseize" => ts += 1,
            "Wasteland" => waste += 1,
            "Tamiyo, Inquisitive Student" | "Murktide Regent" => clock += 1,
            "Doomsday" => dd += 1,
            _ => {}
        }
    }
    let depl = if blue > fow { 1.0 } else { 0.3 }; // FoW needs a non-FoW blue pitch
    fow as f32 * depl
        + 0.7 * daze as f32
        + 0.6 * ts as f32
        + 0.3 * waste as f32
        + 0.4 * (clock + (dd - 1).max(0)) as f32
}

/// resolve(R) in [0.30, 0.90]: how well resources let the Doomsday actually resolve.
pub fn resolve(cards: &[&str]) -> f32 {
    0.30 + 0.60 * resources(cards).tanh()
}

fn score(cards: &[&str], on_play: bool, obj: LearnedObjective) -> f32 {
    let p = pcast(cards, on_play);
    match obj {
        LearnedObjective::Speed => p,
        LearnedObjective::Interactive => p * resolve(cards),
    }
}

fn bars(on_play: bool, obj: LearnedObjective) -> [f32; 6] {
    match (on_play, obj) {
        (true, LearnedObjective::Speed) => BARS_PLAY_SPEED,
        (false, LearnedObjective::Speed) => BARS_DRAW_SPEED,
        (true, LearnedObjective::Interactive) => BARS_PLAY_WIN,
        (false, LearnedObjective::Interactive) => BARS_DRAW_WIN,
    }
}

/// Best `keep_size`-card subset of the drawn hand (max score). Returns (best score, kept bitmask).
fn best_subset(hand: &[&str], keep_size: usize, on_play: bool, obj: LearnedObjective) -> (f32, u32) {
    let n = hand.len();
    let (mut best, mut best_mask) = (f32::MIN, 0u32);
    for mask in 0u32..(1 << n) {
        if mask.count_ones() as usize != keep_size {
            continue;
        }
        let cards: Vec<&str> = (0..n).filter(|&i| mask & (1 << i) != 0).map(|i| hand[i]).collect();
        let s = score(&cards, on_play, obj);
        if s > best {
            best = s;
            best_mask = mask;
        }
    }
    (best, best_mask)
}

/// KEEP decision: at `mulls` mulligans (London: you hold 7, would bottom `mulls`), keep iff the
/// best `(7-mulls)`-subset's score clears the bar for that size.
pub fn learned_keep(hand: &[&str], mulls: u32, on_play: bool, obj: LearnedObjective) -> bool {
    let keep_size = 7usize.saturating_sub(mulls as usize);
    if keep_size <= 1 {
        return true; // forced keep — never mulligan to 0
    }
    let bar = bars(on_play, obj)[mulls as usize];
    best_subset(hand, keep_size, on_play, obj).0 >= bar
}

/// Which `mulls` cards to put on the bottom: everything NOT in the best `(7-mulls)`-subset.
/// Returns indices into `hand`.
pub fn learned_bottom(hand: &[&str], mulls: u32, on_play: bool, obj: LearnedObjective) -> Vec<usize> {
    let n = hand.len();
    let keep_size = n.saturating_sub(mulls as usize);
    let (_, keep_mask) = best_subset(hand, keep_size, on_play, obj);
    (0..n).filter(|&i| keep_mask & (1 << i) == 0).collect()
}

/// Instant model read on a hand: both GBDTs + the resource score. No simulation, and deliberately
/// no binary keep/mull verdict — the speed policy mulligans most opening 7s by design, so a bare
/// "MULL" misleads. The keep/bottom decision lives in [`keep_suggestion`], shown transparently as
/// score-vs-bar.
#[derive(Serialize)]
pub struct HandEstimates {
    pub p_cast: f32,
    pub e_ttd: f32,
    pub resources: f32,
    pub resolve: f32,
    pub interactive_score: f32,
}

pub fn hand_estimates(hand: &[&str], on_play: bool) -> HandEstimates {
    HandEstimates {
        // The GBDT is an unclamped regressor: clamp the *displayed* probabilities to [0,1]
        // (the policy keeps the raw scores internally, where only the ordering matters).
        p_cast: pcast(hand, on_play).clamp(0.0, 1.0),
        e_ttd: ettd(hand, on_play).max(0.0),
        resources: resources(hand),
        resolve: resolve(hand),
        interactive_score: score(hand, on_play, LearnedObjective::Interactive).clamp(0.0, 1.0),
    }
}

/// One objective's suggested split of a hand into `keep_size` cards to keep + the rest to bottom,
/// with the best-subset score against that size's keep-bar. We show the score AND the bar (not a
/// binary verdict) so the steep speed bar reads as "this hand is below threshold", not "bad hand".
#[derive(Serialize)]
pub struct KeepArm {
    pub keep: Vec<String>,
    pub bottom: Vec<String>,
    pub score: f32,
    pub bar: f32,
    pub keeps: bool,
}

/// Both policies' keep/bottom suggestion at a chosen hand size — the legitimate home of bottoming
/// (you only bottom when keeping fewer than 7). Instant; pure model arithmetic.
#[derive(Serialize)]
pub struct KeepSuggestion {
    pub keep_size: usize,
    pub speed: KeepArm,
    pub interactive: KeepArm,
}

fn keep_arm(hand: &[&str], on_play: bool, keep_size: usize, obj: LearnedObjective) -> KeepArm {
    let n = hand.len();
    let ks = keep_size.clamp(1, n.max(1));
    let (score, mask) = best_subset(hand, ks, on_play, obj);
    let keep = (0..n).filter(|&i| mask & (1 << i) != 0).map(|i| hand[i].to_string()).collect();
    let bottom = (0..n).filter(|&i| mask & (1 << i) == 0).map(|i| hand[i].to_string()).collect();
    // Keep-bar for size s is D_{8-s} = bars[7-s]; size 1 is a forced keep (bar 0).
    let bar = if ks <= 1 { 0.0 } else { bars(on_play, obj)[(n - ks).min(5)] };
    KeepArm { keep, bottom, score, bar, keeps: score >= bar }
}

pub fn keep_suggestion(hand: &[&str], on_play: bool, keep_size: usize) -> KeepSuggestion {
    KeepSuggestion {
        keep_size: keep_size.clamp(1, hand.len().max(1)),
        speed: keep_arm(hand, on_play, keep_size, LearnedObjective::Speed),
        interactive: keep_arm(hand, on_play, keep_size, LearnedObjective::Interactive),
    }
}
