# Forge v4.5 search-smoke stability fix

- ForgeStrategy no longer chooses Basalt Monolith or Grim Monolith's paid untap ability while that Monolith is already untapped. The rules engine still exposes the activation as legal; this is a pilot-level anti-loop rule.
- ForgeSearchStrategy prunes the same obvious zero-progress candidate from search.
- forge-lab runs its CLI body on a 16 MiB worker-thread stack, which avoids Windows main-thread stack exhaustion in nested IR/search evaluation.
- search-smoke emits stderr stage markers so future failures identify whether they occurred during the heuristic baseline or rollout search.

Also ensure command-line flags are separated, e.g. `--seed 1 --max-turns 3`, not `--seed 1--max-turns 3`.
