# Forge pilot: cheapest path from heuristic to learned agent

## 1. Validate the search teacher first

Compile/test:

```bash
cargo test -p libmtg-engine
cargo test -p libmtg-forge
```

Run several seeds:

```bash
cargo run --release -p libmtg-forge --bin forge-lab -- search-smoke \
  apps/libmtg-forge/examples/forge-tezzeret.txt --seed 1 --max-turns 3 --rollouts 4
```

Repeat with seeds 2, 3, 4, ... and read `search.decision_log`. Each searched choice prints every candidate and its rollout score.

Do **not** generate a huge training set until those choices look sensible. If search makes an obviously bad choice, fix `evaluate_forge_state` or the heuristic continuation policy first; otherwise the model will faithfully learn the bad teacher.

## 2. Generate a small dataset

Start with hundreds of games, not millions:

```bash
cargo run --release -p libmtg-forge --bin forge-lab -- generate-training \
  apps/libmtg-forge/examples/forge-tezzeret.txt \
  forge-training-small.jsonl \
  --games 500 --seed 20260925 --max-turns 3 --rollouts 4
```

Each JSONL row is one decision and contains:

- visible Forge hand/battlefield state
- turn/phase/life/mana
- Mystic Forge-visible top card when applicable
- every searched legal candidate
- rollout score for each candidate
- chosen action
- final diagnostic game score/outcome

## 3. Train the $0 CPU baseline

```bash
python -m venv .venv
# Linux/macOS
source .venv/bin/activate
# Windows PowerShell: .venv\Scripts\Activate.ps1

pip install -r apps/libmtg-forge/requirements-training.txt
python apps/libmtg-forge/scripts/train_action_ranker.py \
  forge-training-small.jsonl --out-dir models/forge-v1
```

The important first metric is `top_action_agreement`: on held-out games, how often does the cheap model rank the same action first as the rollout teacher?

The model should initially be used to **prune/rank candidates**, not replace search. For example: model ranks 15 legal actions -> keep top 4 -> rollout-search those 4.

## 4. Scale only after the pipeline is sane

Then generate 10k-100k games with more rollouts. All of this can run locally; there is no API/model-training bill. CPU simulation time is the main cost.

## 5. Matchups come after goldfish search

The current rollout pilot intentionally uses an `AlwaysPass` opponent inside candidate evaluation. That prevents hidden-information cheating and makes it useful for Forge sequencing, but it is not yet a matchup-strength agent.

The next engine feature should be **determinization**:

1. expose only information the Forge player is entitled to know;
2. sample plausible opponent hidden hands/library positions from the opponent decklist;
3. evaluate each Forge action across several sampled hidden states;
4. average the rollout values;
5. train on those information-set decisions.

That is the point where the pilot becomes suitable for Dimir/Reanimator/Doomsday matchup work.
