//! `dd-goldfish` CLI: run the Doomsday goldfish simulator and render the result.
//! Goldfishes a deck loaded from a text file or a Moxfield/MTGGoldfish URL, or
//! a built-in sample deck when `--deck` is omitted.

use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use libmtg_doomsday::{
    run_goldfish, run_goldfish_asap_mode, sample_doomsday_deck, GoldfishStats, MullMode,
    DEFAULT_CUTOFF, DEFAULT_PROTECTION,
};
use libmtg_engine::{build_catalog, warn_unimplemented_cards};

#[derive(Clone, Copy, ValueEnum)]
enum StrategyKind {
    /// Baseline DoomsdayStrategy (mana-development oriented).
    Baseline,
    /// Aggressive cast-Doomsday-ASAP, following the recipe solver toward a cutoff.
    Asap,
    /// 2×2 probe: baseline gameplay + the aggressive p_cast_by mulligan.
    BaselineAggro,
}

/// Opening-hand mulligan discipline for the cast-ASAP pilot.
#[derive(Clone, Copy, ValueEnum)]
enum MullArg {
    /// Never mulligan — keep the opening 7.
    Keep7,
    /// "Keep a real plan" player heuristic (default).
    Realistic,
    /// High p_cast_by bar, loosening as you mulligan.
    Aggressive,
    /// Learned P(cast)-GBDT policy, raw-speed objective (the fastest mulligan).
    LearnedSpeed,
    /// Learned policy scoring P(cast) x resolve(resources): same speed as Aggressive, more interaction.
    LearnedInteractive,
}

impl From<MullArg> for MullMode {
    fn from(m: MullArg) -> Self {
        match m {
            MullArg::Keep7 => MullMode::Keep7,
            MullArg::Realistic => MullMode::Realistic,
            MullArg::Aggressive => MullMode::Aggressive,
            MullArg::LearnedSpeed => MullMode::LearnedSpeed,
            MullArg::LearnedInteractive => MullMode::LearnedInteractive,
        }
    }
}

#[derive(Parser)]
#[command(about = "Goldfish Monte-Carlo simulator for Doomsday (race to cast DD)")]
struct Args {
    /// Deck to goldfish: a text decklist file, or a Moxfield/MTGGoldfish URL.
    /// Omit to use the built-in sample Doomsday list.
    #[arg(long)]
    deck: Option<String>,
    /// Number of games to simulate.
    #[arg(long, default_value_t = 10_000)]
    games: u32,
    /// Which pilot to simulate.
    #[arg(long, value_enum, default_value_t = StrategyKind::Asap)]
    strategy: StrategyKind,
    /// Opening-hand mulligan discipline for the cast-ASAP pilot.
    #[arg(long, value_enum, default_value_t = MullArg::Realistic)]
    mull_mode: MullArg,
    /// Cutoff turn for the cast-ASAP objective `P(cast by cutoff)`.
    #[arg(long, default_value_t = DEFAULT_CUTOFF)]
    cutoff: u32,
    /// Force on the draw (draw a card on turn 1). Default randomizes play/draw 50/50.
    #[arg(long)]
    draw: bool,
    /// Force on the play (no turn-1 draw). Default randomizes play/draw 50/50.
    #[arg(long)]
    play: bool,
    /// Emit a labeled keep-all-7 CSV (card-name counts + solver signals + realized win)
    /// to stdout for the mulligan-learning bake-off. Uses Keep7 + on-the-play.
    #[arg(long)]
    dump_keep_data: bool,
    /// Estimate per-hand P(cast by cutoff): read a file of opening hands (one per line,
    /// cards `|`-separated), replay each `--games` times on the play, print `rate<TAB>hand`.
    #[arg(long)]
    sim_hands: Option<String>,
    /// With --sim-hands: output E[time-to-Doomsday] (censored at --cutoff as the horizon, e.g.
    /// --cutoff 10) instead of P(cast by cutoff). Lower is better; never-cast counts as the horizon.
    #[arg(long)]
    ttd: bool,
    /// With --sim-hands: output `cards<TAB>protection<TAB>hand` = mean cards & protection in hand
    /// at the moment DD is cast (conditional on casting by --cutoff). A resources-retained proxy.
    #[arg(long)]
    resources: bool,
    /// Trace one fixed opening 7 (cards `|`-separated): print play-by-play for a few
    /// winning and losing games, to see how the hand actually plays out.
    #[arg(long)]
    trace_hand: Option<String>,
    /// A/B debug: print, for a few SEEDED games, every decision where the principled
    /// policy disagrees with the reference value-table heuristic.
    #[arg(long)]
    compare: bool,
    /// Seed for `--compare` (reproducible games).
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Calibration: bucket the kept-hand P(cast by cutoff) prediction vs the realized
    /// success rate (is our estimate accurate?).
    #[arg(long)]
    calibrate: bool,
    /// Deterministic two-wincon "send" report: over `--games` opening hands (Keep7),
    /// the share that can GUARANTEE a send by `--cutoff` via Doomsday, via The
    /// Fantasticar, or either — with the either-wincon sends split protected/naked.
    /// Uses the built-in `vroomsday` list unless `--deck` is given. Honors `--play`
    /// (default) / `--draw`.
    #[arg(long)]
    send_report: bool,
    /// Realized two-wincon send simulation: goldfish the deck twice (Doomsday-only vs
    /// Doomsday+Fantasticar) and print P(send by `--cutoff`) for each + the speedup and
    /// the protected/naked split. Uses the built-in `vroomsday` list unless `--deck` is
    /// given, the `--mull-mode`, and `--play` (default) / `--draw`.
    #[arg(long)]
    send_sim: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();

    let deck = match &args.deck {
        Some(spec) => match load_deck(spec) {
            Ok(deck) => deck,
            Err(e) => {
                eprintln!("error: {e}");
                return ExitCode::FAILURE;
            }
        },
        None => sample_doomsday_deck(),
    };

    if args.send_report {
        // Default to the built-in vroomsday (Doomsday + Fantasticar) list when no
        // --deck was given; otherwise report on the loaded deck.
        let deck = if args.deck.is_none() { libmtg_doomsday::vroomsday_deck() } else { deck };
        let on_draw = args.draw;
        let cutoff = args.cutoff.min(u8::MAX as u32) as u8;
        let on = if on_draw { "draw" } else { "play" };
        let total: i32 = deck.iter().filter(|(_, _, b)| b == "main").map(|(_, q, _)| *q).sum();
        eprintln!(
            "Deterministic two-wincon send report: {} hands, on the {on}, cutoff T{cutoff}…",
            args.games
        );
        let r = libmtg_doomsday::deterministic_send_report(
            &deck, args.games, DEFAULT_PROTECTION, cutoff, on_draw,
        );
        let share = |n: u32| if r.send_by > 0 { 100.0 * n as f64 / r.send_by as f64 } else { 0.0 };
        println!("Deck: {total} mainboard | {} opening hands (Keep7, on the {on}) | cutoff T{cutoff}", r.games);
        println!();
        println!("  P(send by T{cutoff})             = {:6.2}%   ({} / {})", r.pct(r.send_by), r.send_by, r.games);
        println!("    via Doomsday   (cast by T{cutoff}) = {:6.2}%", r.pct(r.dd_by));
        println!("    via Fantasticar (pop by T{cutoff}) = {:6.2}%", r.pct(r.car_by));
        println!("    two-wincon speedup vs DD-only  = +{:.2} pts", r.pct(r.send_by) - r.pct(r.dd_by));
        println!();
        println!("  of those T{cutoff} sends:");
        println!("    protected (≥1 disruption held) = {:6.2}%   ({} / {})", share(r.send_protected), r.send_protected, r.send_by);
        println!("    naked     (0 disruption held)  = {:6.2}%   ({} / {})", share(r.send_naked), r.send_naked, r.send_by);
        return ExitCode::SUCCESS;
    }

    if args.send_sim {
        let deck = if args.deck.is_none() { libmtg_doomsday::vroomsday_deck() } else { deck };
        let on_play = if args.draw { Some(false) } else { Some(true) };
        let on = if args.draw { "draw" } else { "play" };
        let cutoff = args.cutoff;
        let mode: MullMode = args.mull_mode.into();
        eprintln!(
            "Realized two-wincon send sim: {} games x2 (DD-only vs +car), on the {on}, cutoff T{cutoff}, mull={:?}…",
            args.games, mode
        );
        let dd = libmtg_doomsday::run_goldfish_send(&deck, args.games, DEFAULT_PROTECTION, cutoff, mode, on_play, false);
        let both = libmtg_doomsday::run_goldfish_send(&deck, args.games, DEFAULT_PROTECTION, cutoff, mode, on_play, true);
        let c = cutoff.min(u8::MAX as u32) as u8;
        let dd_pct = 100.0 * dd.cast_by(c);
        let both_pct = 100.0 * both.cast_by(c);
        // Protected/naked split of the +car sends (protection held in hand at the send).
        let protected: u32 = both.protection.iter().filter(|(&p, _)| p >= 1).map(|(_, &n)| n).sum();
        let naked: u32 = both.protection.get(&0).copied().unwrap_or(0);
        let sends = protected + naked;
        let share = |n: u32| if sends > 0 { 100.0 * n as f64 / sends as f64 } else { 0.0 };
        println!("Realized P(send by T{c}) on the {on}  ({} games, mull={:?}):", args.games, mode);
        println!();
        println!("  Doomsday only          = {dd_pct:6.2}%");
        println!("  Doomsday + Fantasticar = {both_pct:6.2}%");
        println!("  two-wincon speedup     = +{:.2} pts  (x{:.2})", both_pct - dd_pct,
            if dd_pct > 0.0 { both_pct / dd_pct } else { 0.0 });
        println!();
        println!("  of the +car sends (≈{} games): protected {:.1}%, naked {:.1}%",
            sends, share(protected), share(naked));
        return ExitCode::SUCCESS;
    }

    // Surface cards the engine can't simulate (dropped / inert) before running.
    // Skip for machine-readable stdout modes (--dump-keep-data, --sim-hands).
    if !args.dump_keep_data && args.sim_hands.is_none() && args.trace_hand.is_none() {
        warn_unimplemented_cards(&deck, "deck", &build_catalog());
    }

    if let Some(spec) = &args.trace_hand {
        let hand: Vec<String> = spec.split('|').map(|s| s.trim().to_string()).collect();
        eprintln!("Tracing hand ({} cards), cutoff T{}…", hand.len(), args.cutoff);
        for line in libmtg_doomsday::run_goldfish_fixed_hand_trace(&deck, &hand, args.cutoff, 2, 2, !args.draw) {
            println!("{line}");
        }
        return ExitCode::SUCCESS;
    }

    if std::env::var("AUDIT_DET").is_ok() {
        eprintln!("Auditing deterministic-but-failed games (seed {}, cutoff T{})…", args.seed, args.cutoff);
        for line in libmtg_doomsday::run_goldfish_audit_det(&deck, args.cutoff, args.seed, args.games, 3) {
            println!("{line}");
        }
        return ExitCode::SUCCESS;
    }

    if args.dump_keep_data {
        eprintln!("Dumping {} keep-all-7 rows (Keep7, on the play, cutoff T{})…", args.games, args.cutoff);
        print!("{}", libmtg_doomsday::run_goldfish_dump(&deck, args.games, args.cutoff));
        return ExitCode::SUCCESS;
    }

    if let Some(path) = &args.sim_hands {
        let text = std::fs::read_to_string(path).expect("read --sim-hands file");
        let hands: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        let on_play = !args.draw;
        let metric = if args.ttd { "E[TTD]" } else { "P(cast)" };
        eprintln!("Per-hand {}: {} hands x {} games (on the {}, horizon/cutoff T{})…",
            metric, hands.len(), args.games, if on_play { "play" } else { "draw" }, args.cutoff);
        for line in hands {
            let hand: Vec<String> = line.split('|').map(|s| s.trim().to_string()).collect();
            if args.resources {
                let (cards, prot) = libmtg_doomsday::run_goldfish_fixed_hand_resources(&deck, &hand, args.cutoff, args.games, on_play);
                println!("{cards:.4}\t{prot:.4}\t{line}");
            } else {
                let v = if args.ttd {
                    libmtg_doomsday::run_goldfish_fixed_hand_ttd(&deck, &hand, args.cutoff, args.games, on_play)
                } else {
                    libmtg_doomsday::run_goldfish_fixed_hand(&deck, &hand, args.cutoff, args.games, on_play)
                };
                println!("{v:.4}\t{line}");
            }
        }
        return ExitCode::SUCCESS;
    }

    if args.calibrate {
        eprintln!("Calibration: {} games, cutoff T{} — predicted P(cast by cutoff) vs observed…",
            args.games, args.cutoff);
        println!("  bucket        n     predicted   observed");
        for (b, (pred, obs, n)) in libmtg_doomsday::run_goldfish_calibration(&deck, args.cutoff, args.games)
            .into_iter().enumerate()
        {
            if n == 0 { continue; }
            println!("  [{:.1},{:.1})  {:>6}    {:>6.3}     {:>6.3}",
                b as f64 / 10.0, (b + 1) as f64 / 10.0, n, pred, obs);
        }
        return ExitCode::SUCCESS;
    }

    if args.compare {
        let n = args.games.min(500);
        eprintln!("A/B decision comparison: {n} seeded game(s), seed {}, cutoff T{}…", args.seed, args.cutoff);
        for line in libmtg_doomsday::run_goldfish_compare(&deck, args.cutoff, args.seed, n) {
            println!("{line}");
        }
        return ExitCode::SUCCESS;
    }

    let label = match args.strategy {
        StrategyKind::Baseline => "baseline",
        StrategyKind::Asap => "cast-ASAP",
        StrategyKind::BaselineAggro => "baseline+aggro-mull",
    };
    eprintln!(
        "Goldfishing {} games ({label}, cutoff T{})…",
        args.games, args.cutoff
    );
    let stats = match args.strategy {
        StrategyKind::Baseline => run_goldfish(&deck, args.games, DEFAULT_PROTECTION, args.cutoff),
        StrategyKind::Asap => run_goldfish_asap_mode(
            &deck, args.games, DEFAULT_PROTECTION, args.cutoff, args.mull_mode.into(),
            if args.play { Some(true) } else if args.draw { Some(false) } else { None },
        ),
        StrategyKind::BaselineAggro => libmtg_doomsday::run_goldfish_baseline_aggro(
            &deck, args.games, DEFAULT_PROTECTION, args.cutoff,
        ),
    };
    print_report(&stats, args.cutoff);
    ExitCode::SUCCESS
}

/// Resolve a `--deck` argument (URL or file path) to the engine deck format.
fn load_deck(spec: &str) -> Result<Vec<(String, i32, String)>, String> {
    let deck = if spec.starts_with("http://") || spec.starts_with("https://") {
        libmtg_decklist::from_url(spec).map_err(|e| e.to_string())?
    } else {
        let content = std::fs::read_to_string(spec)
            .map_err(|e| format!("reading deck file {spec}: {e}"))?;
        libmtg_decklist::Decklist::parse_text(&content)
    };
    if deck.main.is_empty() {
        return Err(format!("no mainboard cards parsed from {spec}"));
    }
    Ok(deck.to_engine_deck())
}

fn bar(c: u32, max: u32, width: usize) -> String {
    let n = if max == 0 {
        0
    } else {
        (c as usize * width) / max as usize
    };
    "█".repeat(n)
}

fn print_report(s: &GoldfishStats, cutoff: u32) {
    println!("\n══ Doomsday goldfish — {} games ══", s.games);
    let cutoff_t = cutoff.min(u8::MAX as u32) as u8;
    println!(
        "  P(cast by T{}): {:.1}%   ← cut-off objective",
        cutoff_t,
        100.0 * s.cast_by(cutoff_t)
    );
    println!(
        "  cast DD:         {} ({:.1}%)",
        s.successes(),
        100.0 * (1.0 - s.fail_rate())
    );
    println!("  failed:          {} ({:.1}%)", s.fails, 100.0 * s.fail_rate());
    println!("  mean cast turn:  {:.2}", s.mean_cast_turn());
    println!("  mean protection: {:.2}", s.mean_protection());

    if !s.mull_count.is_empty() {
        let kept: u32 = s.mull_count.values().sum();
        println!("\n  mulligans (kept at each hand size) + predicted vs realized P(cast by T{cutoff_t}):");
        for k in 0u8..=3 {
            let n = s.mull_count.get(&k).copied().unwrap_or(0);
            if n == 0 { continue; }
            let avg = s.mull_pred_sum.get(&k).copied().unwrap_or(0.0) / n as f64;
            let realized = s.mull_cast.get(&k).copied().unwrap_or(0) as f64 / n as f64;
            println!("    {} cards: {:>6} ({:>4.1}% of keeps)   pred {:.3}  realized {:.3}",
                7 - k, n, 100.0 * n as f64 / kept.max(1) as f64, avg, realized);
        }
        if s.kept7_count > 0 && s.mull7_count > 0 {
            println!("    air in opening 7 — kept-at-7: {:.2}   mulliganed: {:.2}",
                s.kept7_air_sum as f64 / s.kept7_count as f64,
                s.mull7_air_sum as f64 / s.mull7_count as f64);
        }
        let det = s.deterministic_cast;
        let sto = s.stochastic_cast;
        let g = s.games.max(1) as f64;
        println!("\n  how we got there (by T{cutoff_t}):");
        println!("    deterministic line in opening hand: {:>6} ({:.1}%)", det, 100.0 * det as f64 / g);
        println!("    drew / cantripped into it:          {:>6} ({:.1}%)", sto, 100.0 * sto as f64 / g);
        println!("    not by cutoff:                      {:>6} ({:.1}%)",
            s.games - det - sto, 100.0 * (s.games - det - sto) as f64 / g);
        let (mm, mp, mb, mn) = (s.miss_mana, s.miss_payoff, s.miss_both, s.miss_neither);
        if mm + mp + mb + mn > 0 {
            println!("      \u{21b3} missing mana (no BBB):  {:>6} ({:.1}%)", mm, 100.0 * mm as f64 / g);
            println!("      \u{21b3} missing payoff (no DD): {:>6} ({:.1}%)", mp, 100.0 * mp as f64 / g);
            println!("      \u{21b3} missing both:           {:>6} ({:.1}%)", mb, 100.0 * mb as f64 / g);
            if mn > 0 {
                println!("      \u{21b3} had both (timing gap):  {:>6} ({:.1}%)", mn, 100.0 * mn as f64 / g);
            }
        }
    }

    if !s.samples.is_empty() {
        println!("\n  sample games (mulligans + kept opening hand → outcome):");
        for g in &s.samples {
            for m in &g.mulligans {
                println!("    [ mull ] {}", m.join(", "));
            }
            let outcome = g.cast_turn.map_or("no cast".to_string(), |t| format!("cast T{t}"));
            println!("    [keep {}] {}  →  {}", 7 - g.mulls, g.hand.join(", "), outcome);
        }
    }

    println!("\n  cast-turn distribution:");
    let max_c = s.cast_turn.values().copied().max().unwrap_or(1);
    for t in 1..=cutoff_t {
        let c = s.cast_turn.get(&t).copied().unwrap_or(0);
        println!(
            "    T{:<2} {:>7} {:>5.1}% │{}",
            t,
            c,
            100.0 * c as f64 / s.games.max(1) as f64,
            bar(c, max_c, 40)
        );
    }

    println!("\n  protection in hand at cast (of {} casts):", s.successes());
    let max_p = s.protection.values().copied().max().unwrap_or(1);
    let maxk = s.protection.keys().copied().max().unwrap_or(0);
    for k in 0..=maxk {
        let c = s.protection.get(&k).copied().unwrap_or(0);
        println!(
            "    {} {:>7} {:>5.1}% │{}",
            k,
            c,
            100.0 * c as f64 / s.successes().max(1) as f64,
            bar(c, max_p, 40)
        );
    }

    // Cumulative "cast by turn N" curve.
    use textplots::{Chart, Plot, Shape};
    let pts: Vec<(f32, f32)> = (1..=cutoff_t)
        .map(|t| (t as f32, (100.0 * s.cast_by(t)) as f32))
        .collect();
    println!("\n  P(cast by turn) %:");
    Chart::new(100, 50, 1.0, cutoff_t as f32)
        .lineplot(&Shape::Lines(&pts))
        .display();
}
