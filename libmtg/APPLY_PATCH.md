# Apply v5.2 overlay

Copy the contents of this folder over the root of your existing `libmtg`
workspace, preserving the paths and allowing the four Rust source files to be
replaced.

Then run:

```powershell
cargo test -p libmtg-engine
cargo test -p libmtg-forge
```

Recommended targeted checks:

```powershell
cargo test -p libmtg-engine residual_mana -- --nocapture
cargo test -p libmtg-engine forge_spells_offered -- --nocapture
cargo test -p libmtg-engine failed_paid_mana -- --nocapture
```

Finally rerun the Jev comparison and confirm there are no `[priority] BUG: cast failed` lines.
