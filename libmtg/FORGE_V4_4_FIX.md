# Forge v4.4 compile fix

Fixes `E0282` in `apps/libmtg-forge/src/matchup.rs`.

`known_partial_support()` previously contained a placeholder `match` whose only arm returned `None`. With no `Some(...)` arm, Rust could not infer the `Option<T>` type. Since the current fidelity gate intentionally has no partial-support entries, the function now directly returns `Vec::new()` and keeps a comment marking where future partial-support warnings should be added.
