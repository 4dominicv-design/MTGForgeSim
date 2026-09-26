# Mystic Forge simulator build notes

This branch adds the Forge-specific experiment layer and a rules implementation pass for the current Mystic Forge 75, plus Basalt Monolith for candidate-list exploration.

## Added

- `apps/libmtg-forge/`
  - `forge-lab audit`: deck-size + implementation-fidelity gate.
  - `forge-lab openings`: deterministic London-mulligan/opening-hand composition experiment.
  - `forge-lab compare`: paired-seed A/B opening-hand comparison.
  - `forge-lab matchup`: full-engine game runner.
  - `forge-lab matchup-compare`: paired-seed full-game A/B harness against one opponent.
  - `ForgeStrategy`: baseline strategy seam for later rollout/search logic.
  - `BaselineOpponentStrategy`: placeholder opponent seam; replace with archetype-specific policies before trusting matchup results.
- `libmtg-engine::run_game` derives its internal simulation RNG from the caller RNG, making shuffles and random effects replayable from a seed.
- Forge sideboards are materialized as owned face-up exile objects. This is an intentional simulator shortcut: Karn can retrieve both wishboard artifacts and artifacts exiled during the game (for example a One Ring exiled by Force of Negation).

## Forge cards implemented

Main-deck / candidate cards:

- Planar Nexus
- Urza's Tower
- Urza's Workshop
- Ancient Tomb (existing engine implementation)
- Lotus Petal (existing)
- Mox Opal (existing)
- Grim Monolith
- Basalt Monolith
- Manifold Key
- Mystic Forge
- The One Ring
- Relic of Sauron
- Giant's Boulder
- Paradox Engine
- Transmute Artifact
- Kozilek's Command
- Trinisphere
- Tezzeret, Cruel Captain
- Karn, the Great Creator (+1, static, and -2 retrieval)
- Urza's Saga chapters I/II/III and Construct token

Wishboard / sideboard cards:

- Disruptor Flute (existing engine implementation)
- Soul-Guide Lantern
- Tormod's Crypt
- Mycosynth Lattice (battlefield artifact typing + global colorless behavior needed for Forge/Karn)
- Ensnaring Bridge
- Portable Hole, including the linked return when Hole leaves the battlefield
- Summon: Bahamut, including targeted chapters I/II and Mega Flare's damage to each opponent
- Mystic Forge / Paradox Engine copies use the implementations above

## Engine primitives added or extended

- Card-level "doesn't untap during your untap step" support for Grim/Basalt Monolith.
- Explicit paid untap activations for Grim and Basalt.
- Minimum spell-mana floor used by Trinisphere after other cost modifications.
- Generic indestructible handling used by The One Ring.
- Burden counters.
- Temporary unblockable keyword handling for Manifold Key.
- Saga chapter target specifications, so Bahamut chapters I/II use real targets.
- `ForEach` over player sets, used by Bahamut's Mega Flare.
- `SumManaValue` IR expression for Mega Flare.
- Linked exile bookkeeping, used by Portable Hole.
- Sideboard-as-exile setup and Karn retrieval from the unified owned exile pool.
- Dynamic IR-granted activated abilities, used by Urza's Saga chapter II.

## Fidelity notes

The current Forge examples have catalog implementations for every listed card, but a few deliberate simulator abstractions remain:

1. **Sideboard-as-exile is intentional.** It makes Karn's wishboard and ordinary face-up exile one pool. This is correct for the Karn decisions we care about, but another card that interacts with exile could technically see wishboard cards.
2. **Mycosynth Lattice's mana-spending permission is not yet generalized.** Permanents correctly become artifacts only on the battlefield, and cards/spells/permanents become colorless; the engine does not yet globally implement "players may spend mana as though it were mana of any color." This usually does not affect the Karn/Lattice lock, but can affect colored spells cast while Lattice is in play.
3. **Kozilek's Command graveyard cards are selected at resolution** rather than announced as a variable number of targets. The creature mode also checks the X/mana-value condition on resolution rather than in the target selector. The resulting Forge lines are normally the same, but interaction around targeting can differ.
4. **"Up to one" target choices** are represented by the engine's ordinary single-target machinery in a few places (notably Bahamut/Karn +1). In normal Forge play the policy wants a target when a useful one exists, but choosing zero is not yet a distinct action everywhere.
5. **Urza's Saga III** uses the engine's practical `artifact + colorless + MV <= 1` search representation; this matches the current Forge target suite, though the paper wording is specifically printed mana cost `{0}` or `{1}`.

These are now narrow fidelity issues rather than missing-card placeholders. Keep them in mind before treating matchup percentages as tournament-ground-truth numbers.

## Current A/B question represented by the examples

Build A: 3 Tezzeret, 3 Mystic Forge, 1 Relic of Sauron.
Build B: 3 Trinisphere, 2 Mystic Forge, 2 Relic of Sauron.
All other maindeck slots and the 13-card sideboard are the same.

## Validation performed in this environment

- Both Forge example lists parse to 60-card mains + 13-card sideboards.
- Every card name in both examples is present in the engine catalog source.
- Focused regression tests are present for Basalt Monolith, Relic of Sauron, Karn retrieving an artifact exiled during the game, and Forge/Karn interactions.
- Source-level delimiter/static checks were run on the modified engine and Forge-app files.

This execution environment does **not** contain `cargo`/`rustc`, so the Rust test suite could not be compiled or executed here. Run `cargo test -p libmtg-engine` and `cargo test -p libmtg-forge` in a Rust environment before relying on the branch.

## Next work

1. Compile and run the focused Forge rules tests; fix any compiler/test failures.
2. Add focused behavioral tests for Bahamut resolution, Portable Hole linked return, Mystic Forge top casting, Transmute payment branches, Trinisphere cost floors, and Ring protection/burden draws.
3. Improve the Forge policy so it understands mana sequencing, Ring/Key loops, Forge top-card decisions, Karn targets, Transmute lines, and lock-piece timing.
4. Add a high-fidelity Dimir Tempo opponent first, then Reanimator/Doomsday/D&T/Lands/etc.
5. Add best-of-three sideboarding policies.
6. Add rollout/search and deck-configuration optimization only after gameplay policies are validated.

## Validation rule

Do not trust matchup win rates merely because every card is present in the catalog. Card fidelity and pilot quality both matter; replay seeds and decision traces should be retained for every serious experiment.

## Search / training starter (v4)

This pass adds the first search-teacher pipeline for improving Forge pilot quality without paying for an LLM or training a large reinforcement-learning system.

### Engine search support

- `SimState::fork_for_search(seed)` clones all rules-relevant state while deliberately dropping strategy/objective trait objects and reseeding future randomness.
- `simulate_priority_action(...)` forces one legal action on a fork, then lets caller-supplied continuation strategies finish the current priority window.
- Stable ObjIds are preserved across a fork, so a legal action chosen in the parent state points at the same card/permanent in the branch.

### Forge rollout pilot

`ForgeSearchStrategy`:

1. receives the engine's legal actions,
2. cheaply pre-ranks/caps the candidate set,
3. forks the position for each action,
4. uses the original `ForgeStrategy` as the continuation policy,
5. evaluates the resulting position with an explicit bootstrap value function,
6. chooses the highest average action across deterministic rollouts.

The first version is deliberately **goldfish-only**: the rollout opponent is `AlwaysPass`. This avoids accidentally using the opponent's hidden hand as information. Matchup search should be added only after opponent-hand determinization/sampling exists.

### New commands

```bash
# Compare old priority-table pilot vs rollout pilot on the same seed.
cargo run --release -p libmtg-forge --bin forge-lab -- search-smoke \
  apps/libmtg-forge/examples/forge-tezzeret.txt \
  --seed 20260925 --max-turns 3 --rollouts 4

# Generate search-labeled JSONL training data.
cargo run --release -p libmtg-forge --bin forge-lab -- generate-training \
  apps/libmtg-forge/examples/forge-tezzeret.txt \
  forge-training.jsonl \
  --games 10000 --seed 20260925 --max-turns 3 --rollouts 4
```

Each search decision emits a structured `FORGE_TRAIN` record containing visible Forge state, all searched candidate actions, rollout scores, and the chosen action. The dataset generator appends the final position score/outcome after each game.

### Cheap model-training baseline

`apps/libmtg-forge/scripts/train_action_ranker.py` flattens the JSONL decisions and trains a LightGBM regressor to imitate the rollout teacher's action scores. It reports validation MAE and, more importantly, top-action agreement on held-out games.

```bash
python -m venv .venv
source .venv/bin/activate        # Windows: .venv\\Scripts\\activate
pip install -r apps/libmtg-forge/requirements-training.txt
python apps/libmtg-forge/scripts/train_action_ranker.py \
  forge-training.jsonl --out-dir models/forge-v1
```

The learned model is not wired into Rust inference yet. First validate that search choices are sensible and that held-out top-action agreement is strong. The next step is to use the learned model only to prune/rank actions while rollout search remains the final decision-maker.
