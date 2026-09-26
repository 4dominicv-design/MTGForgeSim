# Forge agent-ready v5

Built on the v4.9.1 X-cost-correctness baseline.

## Added

- hidden-information-safe `AgentObservation`
- canonical serialized `AgentAction` and opaque action ids
- shared action id/semantic helpers used by both search teacher and agent API
- `ForgeAgent` trait
- structured announcement decision for modal/X/alternate costs
- structured Karn wish decision
- `AgentBackedStrategy` with strict validation and heuristic fallback
- `HeuristicForgeAgent`, `PassForgeAgent`, and deliberately invalid test agent
- `forge-lab agent-smoke`
- `AGENT_OBS` JSON decision logging
- engine `known_hand_of()` safe accessor
- public `AnnounceOptions.available_modes` for validated agent adapters
- regression tests for invalid-action fallback and opponent-hand secrecy

The rollout search remains an internal teacher because it intentionally needs
state forks. It now emits the same external observation schema/action ids, so
its labels can train an observation-only policy without exposing `SimState`.
