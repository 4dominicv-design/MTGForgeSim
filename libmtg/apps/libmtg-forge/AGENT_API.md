# Forge Agent API v1

The agent boundary intentionally does **not** expose `SimState`. External/learned
agents receive a JSON `AgentObservation` containing only player-visible
information plus engine-generated legal actions.

Schema version: `forge_agent_obs_v1`

## Priority decision

The engine emits an observation containing:

- turn / phase / active player
- both life totals
- your complete hand
- opponent hand **size** plus only identities explicitly marked known
- both battlefields, graveyards, face-up exile, and the stack
- your known/Forge-visible top card only
- floating and immediately available mana
- `legal_actions`

Each legal action has:

- `id`: opaque per-game execution id; return this exact string
- `semantic`: cross-game learning label
- `kind`
- card/source metadata
- a short human-readable description

An agent returns:

```json
{"action_id":"cast:Mystic Forge@ObjId(28):Main"}
```

The adapter matches that id against the legal list for the exact priority
window. Unknown ids are rejected, logged, and delegated to the bootstrap Forge
heuristic. The engine never executes an action merely because an agent invented
an id.

## Announcement decision

`ForgeAgent::choose_announcement` can optionally control modal/X decisions. It
receives the legal modes, maximum payable X, and alternate-cost count. Invalid
mode/X/alternate-cost selections are rejected and fall back to the bootstrap
Forge announcement policy.

This is the seam an eventual Jev adapter should use for Kozilek's Command rather
than encoding an illegal X into a priority action.

## Karn wish decision

`ForgeAgent::choose_wish` receives stable choice ids spanning the unified
wishboard/exile pool. Returning an unknown id falls back to the existing Karn
heuristic.

## Smoke tests

```powershell
cargo run --release -p libmtg-forge --bin forge-lab -- agent-smoke apps/libmtg-forge/examples/forge-trinisphere.txt --agent heuristic --seed 1 --max-turns 3
```

Validation/fallback test:

```powershell
cargo run --release -p libmtg-forge --bin forge-lab -- agent-smoke apps/libmtg-forge/examples/forge-trinisphere.txt --agent invalid --seed 1 --max-turns 1
```

The second command should report `invalid_decisions > 0` and still complete.

Every priority request is also written to `decision_log` as:

```text
AGENT_OBS\t{...JSON...}
```

The rollout-search teacher emits the same observation schema and uses the same
`id` / `semantic` action helpers, so search labels and future learned/cloud
agents share one vocabulary.

## Hidden-information guarantee in v1

The observation builder never serializes the opponent's hidden hand identities
or a hidden library top. A regression test checks this using the inert-opponent
smoke scenario.

## Still delegated to engine/bootstrap policy

Agent API v1 focuses on the decision surfaces needed to start model integration:
priority actions, spell announcement (mode/X/alternate cost), and Karn wishes.
Some lower-level choices such as generic target selection, scry ordering, combat
damage ordering, and London-bottom selection still use the engine/Forge
bootstrap policy. These can be promoted to structured agent observations after
the priority/X/wish interface is validated.
