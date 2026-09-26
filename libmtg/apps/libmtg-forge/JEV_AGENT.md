# Jev Forge Agent (v5.1)

This build connects the hidden-information-safe `ForgeAgent` API to TypeSafe's Jev System One API.
The Rust rules engine remains authoritative for legality. Jev receives the existing `AgentObservation` and a finite set of engine-approved choices, then returns only one choice.

## Environment variables

PowerShell (current terminal only):

```powershell
$env:JEV_API_KEY="YOUR_KEY"
$env:JEV_MODEL="jev-latest"        # optional
$env:JEV_BASE_URL="https://api.typesafe.ai" # optional
$env:JEV_TIMEOUT_SECONDS="20"      # optional
```

Do not put the API key in a deck file or commit it to git.

## 1. Verify access/model

```powershell
cargo run --release -p libmtg-forge --bin forge-lab -- jev-check
```

This calls `GET /v1/models` and reports whether `JEV_MODEL` is available to the account.

## 2. Let Jev pilot one cheap smoke game

Start small because Jev is called at live decision points:

```powershell
cargo run --release -p libmtg-forge --bin forge-lab -- agent-smoke apps/libmtg-forge/examples/forge-trinisphere.txt --agent jev --seed 1 --max-turns 1
```

The decision log contains `JEV_METRIC` JSON records with model, confidence, probabilities, latency and tokens. The API key is never logged.

Jev also receives structured decisions for spell announcement (including Kozilek's Command X/mode combinations) and Karn wish choices. Generic targets/lower-level engine choices still use the bootstrap heuristic in v5.1.

## 3. Compare Jev to the rollout teacher

The most useful first evaluation is *not* separate Jev/search games. `agent-compare` runs the search teacher, captures each teacher observation, then asks Jev and the cheap heuristic what they would do from that exact state and exact candidate set.

Very cheap first pass:

```powershell
cargo run --release -p libmtg-forge --bin forge-lab -- agent-compare apps/libmtg-forge/examples/forge-trinisphere.txt --games 1 --seed 1 --max-turns 2 --rollouts 1
```

Then increase slowly:

```powershell
cargo run --release -p libmtg-forge --bin forge-lab -- agent-compare apps/libmtg-forge/examples/forge-trinisphere.txt --games 10 --seed 1 --max-turns 3 --rollouts 1
```

Output includes:

- Jev vs rollout-teacher agreement
- heuristic vs rollout-teacher agreement
- average Jev confidence
- average Jev latency
- total input/output tokens
- estimated Jev input cost (override price with `JEV_INPUT_COST_PER_MTOK`)
- concrete disagreement positions with legal actions and probability distribution

Do not treat search agreement as ground-truth win rate. The rollout teacher/evaluator is still a bootstrap policy. Disagreements are the most useful positions to review manually.

## API shape

The adapter sends `POST /v1/systemone` with:

- `state`: the safe observation, excluding the duplicated legal-action array
- `model`: `JEV_MODEL`
- `questions.decision.type`: `choice`
- `criteria`: short keys (`a0`, `a1`, ...) mapped to legal engine action descriptions

Jev's returned choice is mapped back to the original opaque `action_id`, which is validated again by `AgentBackedStrategy` before execution.

## Recommended sequence

1. `cargo test -p libmtg-forge`
2. `jev-check`
3. one-turn `agent-smoke --agent jev`
4. one-game `agent-compare`
5. inspect disagreements
6. run ~10 games with `--rollouts 1`
7. only then decide whether Jev is strong enough to bootstrap a larger dataset
