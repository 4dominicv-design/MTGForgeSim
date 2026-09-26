# Forge v4.7 search/training fixes

This pass fixes issues exposed by the first successful `search-smoke` trace.

## Rules correctness

- Land drops are now generated only during the active player's precombat or postcombat main phase with an empty stack.
- A regression test covers upkeep, own-main-phase, and opponent-turn land timing.

## Stable action identity

Training records now contain both:

- `action`: semantic identity used by the learned model, e.g. `activate:Manifold Key#0`.
- `action_id`: per-game identity containing the source `ObjId`, e.g. `activate:Manifold Key@ObjId(42)#0`.

This distinguishes two copies of the same permanent in one position without exploding the model's action vocabulary across games. `chosen_action_id` is recorded alongside `chosen` for the selected line.

## Search pruning / speed

The goldfish rollout pilot now spends search compute only during its own empty-stack main phases. Outside those windows it passes rather than re-evaluating the same Key activation during upkeep, draw, combat, and end steps.

It also prunes:

- Basalt/Grim paid untap when the Monolith is already untapped.
- Manifold Key's unblockable ability in the current non-attacking goldfish pilot.
- Manifold Key untap when there is no other tapped artifact.
- malformed rollout scores outside the legitimate +/-1,000,000 terminal-score range.

Invalid branches are not written as training candidates. If every non-pass branch is invalid, no training row is emitted for that decision.

## Expected smoke-test change

The first search decision should now appear in a main phase, not upkeep. Search logs should also be much shorter because combat-step Key/pass decisions are skipped.
