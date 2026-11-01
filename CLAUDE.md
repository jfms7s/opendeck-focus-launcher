# CLAUDE.md

OpenDeck plugin (`com.jfms7s.focuslauncher`, Rust on the `openaction` crate; Linux
x86_64/aarch64 and Apple Silicon macOS) with one action, "Focus or Launch": a key that
focuses an app's window, cycles through its windows, or launches it. Design docs,
plans, reviews and issue write-ups live in the Obsidian vault at
`~/git/obsidian-vault/personal/projects/opendeck-focus-launcher/`, not in this repo.
`README.md` is the user guide: "Development" has the checks, "Releasing" the
release steps, and the smoke-test checklist the live-desktop rows.

## Checks

```bash
cargo fmt --check
cargo clippy --all-targets --locked -- -D warnings
cargo clippy --target aarch64-apple-darwin --all-targets --locked -- -D warnings  # macOS code, type-checked from Linux
cargo test --locked
node --test tests/*.test.mjs       # property inspector
node build.mjs --check-only        # Cargo/manifest version, action UUID, CodePaths
```

## Scope (frozen surfaces)

Ruled 2026-11-01 after the 2026-10-07 feature-fit review. Frozen means code and
tests stay and bugs get fixed, but nothing is added: no new options, PI rows,
settings or platform behaviour on a frozen surface until the owner lifts the
freeze here first. Nothing is cut for being unused on the owner's deck: the plugin
is public.

- **macOS port** (`src/backend/macos.rs`, `src/backend/mac_ids.rs`, `src/bundle.rs`,
  the `cfg(target_os = "macos")` branches, the CI and release macOS jobs): no new
  macOS behaviour. Tests, "never run on a Mac" labels, docs and bug fixes are
  allowed. A cross-platform feature that would need new Accessibility code ships
  Linux-only (the macOS backend keeps the trait default). Lifted when README smoke
  rows 1-5 and 16-19 are recorded on a real Mac and a release carrying that record
  is published; the owner then rules Keep or Cut.
- **The two opt-out checkboxes** ("Cycle to the next window", "Minimize when
  already focused"): no further work. Lifted by owner decision.
- **`name_override`, `exec_override`, `custom_args`**: no new options around them.
  Other features may use them (e.g. Extra arguments to open a specific window).
  Lifted by owner decision.
- **Dial support** (`Encoder` in `assets/manifest.json`, `dial_down`/`dial_up` in
  `src/action.rs`): no new dial modes. One owner-approved exception: the dial
  window switcher; after it lands, dials are frozen again. Lifted by owner decision.

Not frozen: Flatpak host launch (the owner chose full Flatpak support on
2026-10-09).
