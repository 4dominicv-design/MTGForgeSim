# Forge v4.9 — X-cost / colorless payment fix

This patch fixes the illegal Kozilek's Command lines discovered in the v4.8 smoke trace.

## Changes

- `AnnounceOptions` now includes `max_x`, the largest currently payable X on the normal cost path.
- Default strategies choose `min(3, max_x)` instead of blindly choosing X=3.
- Forge strategies choose the largest legal X for Kozilek's Command.
- Variable X-mana is checked together with the base mana cost, so `{X}{C}{C}` with only three mana has max X=1.
- Cast legality treats an X spell as castable when X=0 is legal; the exact X is chosen during announcement.
- The cast sub-machine fills mana for base + X together.
- `cast_spell` performs a combined base+X mana preflight before moving the spell or spending mana.
- Failure to pay an additional cost now aborts the cast instead of silently continuing.
- The auto-tap planner now explicitly satisfies `{C}` pips with actual colorless-producing mana abilities.

## Regression tests added

- `test_kozileks_command_max_x_accounts_for_base_cc`
  - 2 mana -> max X=0
  - 3 mana -> max X=1
  - 4 mana -> max X=2
  - 5 mana -> max X=3
- `test_kozileks_command_rejects_unpayable_x_without_partial_cast`
- `test_kozileks_command_x1_spends_three_total_mana`
- `test_auto_tap_plan_satisfies_specific_colorless_pips`

## Next strategic step

The current Forge pilot announces the maximum legal X and mode 0 for Kozilek's Command. A future search upgrade should expose `(X, mode pair)` as distinct search choices so the teacher can compare, for example, X=1 spawn+scry against X=0/1 creature-exile lines rather than hardcoding one announcement.
