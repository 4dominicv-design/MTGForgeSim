# Forge v5.1 — Jev adapter + comparison harness

Adds:

- `JevForgeAgent` using TypeSafe `POST /v1/systemone` Choice questions
- Bearer API key from `JEV_API_KEY` (never persisted/logged)
- `jev-check` (`GET /v1/models`)
- `agent-smoke --agent jev`
- Jev priority-action, spell-announcement/X, and Karn-wish choice adapters
- structured `JEV_METRIC` logs (confidence, probabilities, latency, tokens)
- `agent-compare`, evaluating Jev and heuristic on the exact observations/candidates labeled by rollout search
- token/cost/latency/agreement summaries and disagreement samples
- offline tests for choice mapping, compact state, and announcement IDs

The rules engine still validates every action. API/network errors return an invalid sentinel that triggers the existing heuristic fallback rather than executing an unvalidated action.
