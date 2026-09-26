# Forge v5.2 — mana legality cleanup

This pass fixes the mismatch exposed by `agent-compare`, where a spell could be
listed in `legal_actions` and then fail during real payment.

## Root cause

The cost executor returns `ManaShortage` as an **already-computed residual**
after applying the floating mana pool. The mana strategy then passed that
residual into `auto_tap_plan`, which subtracted the floating pool a second time.
For example, with 2 mana floating and a 3-mana spell, the executor correctly
reported `{1}` remaining; the planner could then incorrectly conclude that the
existing 2 mana had already covered that residual and activate nothing.

## Changes

- Added `auto_tap_plan_remaining`, whose input is explicitly an unpaid residual.
- `Strategy::choose_mana_ability` now receives that residual.
- The full-cost mana loop recomputes the residual from the real pool before each
  activation.
- IR cost payment activates one mana source per `ManaShortage` retry, allowing
  the cost executor to recompute the next residual from the actual pool.
- `execute_mana_activation` now returns failure and does **not** produce mana if
  the activation cost cannot be paid.
- Updated the Doomsday strategy to the same residual-mana contract.

## Regression coverage

- 2 floating mana + an untapped one-mana source correctly plans the final mana.
- Grim Monolith (1 floating + Wastes), Trinisphere (2 + Wastes), and The One Ring
  (3 + Wastes) are both offered as legal and successfully cast.
- Giant's Boulder's paid mana-filter ability cannot produce mana when its `{1}`
  activation cost cannot be paid.
