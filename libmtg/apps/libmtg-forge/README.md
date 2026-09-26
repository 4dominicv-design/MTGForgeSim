# libmtg-forge

A Mystic Forge experiment harness built on the shared `libmtg-engine`.

## Diagnosing a stalled priority window

Priority windows have a default limit of 10,000 actions. Set
`LIBMTG_PRIORITY_LIMIT` to a positive action count to override it before running
a rebuilt executable. The engine aborts a window that exceeds that count and
prints its turn, phase, and last 16 selected actions (including card, stack size,
mana, hand size, and life). This is a safety budget, not cycle detection:
a long legal sequence may also hit the limit. An aborted run is not a valid
simulation result. It cannot interrupt a stall inside an individual action.

For example, from the workspace's `libmtg` directory in PowerShell:

```powershell
$env:LIBMTG_PRIORITY_LIMIT = '1000'
cargo run --release -p libmtg-forge --bin forge-lab -- search-smoke apps/libmtg-forge/examples/forge-trinisphere.txt --seed 1 --max-turns 3 --rollouts 1
Remove-Item Env:LIBMTG_PRIORITY_LIMIT
```

Without the environment variable, the default 10,000-action limit applies.

## What is usable now

`forge-lab audit` reports which cards still need rules implementations. `forge-lab openings`
runs deterministic London-mulligan/opening-hand composition experiments. `forge-lab compare`
uses the same seed schedule for A/B comparisons between near-identical lists.

The `matchup` command is already wired to `Scenario`/`run_game`, but deliberately refuses to
produce a win-rate result until both mainboards have full implementation coverage. This prevents
missing cards from becoming inert blanks and corrupting results.

## Commands

```bash
cargo run --release -p libmtg-forge --bin forge-lab -- audit \
  apps/libmtg-forge/examples/forge-tezzeret.txt

cargo run --release -p libmtg-forge --bin forge-lab -- openings \
  apps/libmtg-forge/examples/forge-tezzeret.txt --games 100000 --seed 20260925

cargo run --release -p libmtg-forge --bin forge-lab -- compare \
  apps/libmtg-forge/examples/forge-tezzeret.txt \
  apps/libmtg-forge/examples/forge-trinisphere.txt \
  --games 100000 --seed 20260925
```

## Important interpretation

Opening-hand metrics are *composition diagnostics*, not game win rates. The current `forge_keep_v0`
mulligan rule is an explicit baseline heuristic. It should later be replaced/compared against labeled
hands and search/rollout policies.

## Current engine status

The Forge 75 and its current 13-card wishboard now have first-pass rules implementations, including Karn retrieval from the unified exile/wishboard pool and Urza's Saga Construct creation. Basalt Monolith is also implemented for candidate-list exploration. `audit` should still be run before every matchup because opponent lists may contain missing or inert cards.

The next bottleneck is no longer card registration; it is **pilot quality**. Improve Forge sequencing and matchup-specific opponent policies before interpreting full-game win percentages as deck-strength estimates.

Every matchup run should record seed, play/draw, deck hashes/config versions, result, turn, and
policy version so regressions and surprising games can be replayed.

Once both builds and an opponent pass `audit`, the paired full-game runner is:

```bash
cargo run --release -p libmtg-forge --bin forge-lab -- matchup-compare \
  apps/libmtg-forge/examples/forge-tezzeret.txt \
  apps/libmtg-forge/examples/forge-trinisphere.txt \
  path/to/opponent.txt --games 10000 --seed 20260925
```

The shared engine has also been changed so the caller-provided seed controls the internal game RNG,
including library shuffles and random in-game effects. This is required for reproducible replays and
paired A/B trials.

## Search pilot and training data

The first rollout-search pilot is intentionally goldfish-only so it cannot cheat by observing an opponent's hidden hand.

```bash
cargo run --release -p libmtg-forge --bin forge-lab -- search-smoke \
  apps/libmtg-forge/examples/forge-tezzeret.txt --seed 20260925 --max-turns 3 --rollouts 4

cargo run --release -p libmtg-forge --bin forge-lab -- generate-training \
  apps/libmtg-forge/examples/forge-tezzeret.txt forge-training.jsonl \
  --games 10000 --seed 20260925 --max-turns 3 --rollouts 4
```

Then train the cheap action-value baseline:

```bash
pip install -r apps/libmtg-forge/requirements-training.txt
python apps/libmtg-forge/scripts/train_action_ranker.py \
  forge-training.jsonl --out-dir models/forge-v1
```

Do not use this search policy for real matchup percentages yet. The matchup-capable version needs hidden-information determinization: sample opponent hands consistent with the decklist and information revealed so far, then average action values across those samples.

## Agent API

See `AGENT_API.md`. `agent-smoke` exercises the hidden-information-safe agent
boundary; the search teacher and external agents now share the same action IDs
and observation schema.
