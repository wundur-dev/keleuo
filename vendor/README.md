# Vendored crates

## gpui-base

`gpui-base/` is [gpui-base](https://crates.io/crates/gpui-base) 0.9.1 (part of gpui-kit, from
Longbridge) with Wundur's changes, patched in through `[patch.crates-io]` in the workspace
`Cargo.toml`. Its version must match the `gpui-kit` version in `crates/wundur-app/Cargo.toml`.

`gpui-base.patch` is the whole of Wundur's changes against the published 0.9.1, so the next
upgrade starts from it:

1. Download the new version: `curl -sL -o gb.crate https://static.crates.io/crates/gpui-base/gpui-base-X.Y.Z.crate && tar xzf gb.crate`.
2. In the new copy, `patch -p1 < gpui-base.patch` and resolve what doesn't apply: check first
   whether upstream now does the same thing (in 0.9.1 it had fixed the right-click crash and
   added range decorations to the code editor itself).
3. Put the result in `gpui-base/`, bump `gpui-kit` to the same version, build, and fix the
   app for the release's breaking changes (its notes list them).
4. Write a new `gpui-base.patch` (`diff -ruN` against the published copy, without
   `Cargo.lock` and `.cargo_vcs_info.json`) and update this file.

Run its own tests with the repo's toolchain:
`cargo test --manifest-path vendor/gpui-base/Cargo.toml --lib --features test-support`.
Nine tests fail on the published 0.9.1 too (dock drag frames, cursor visibility, streaming
parse order); they are not ours.

### What the changes are for

Each is marked `Wundur patch` (or `Keleuo patch`) in the code.

- **Dock** (`dock/`): docking with a compass (targets to split a panel off or add it as a tab),
  `DockArea::set_panel_min_width` (the chat is never narrower than 460).
- **Message box** (`input/`): text decorations for ordinary multi-line text, not only the code
  editor (the Markdown marks while typing); range decorations there too, with a `Dots` style
  (the red dots under a misspelled word).
- **Resize handles** (`resizable/resize_handle.rs`): dragging a splitter redraws only the view
  it is in, not the whole window (which painted every cached panel again).
- **Markdown** (`text/`): space between list items (also above a nested list); find marks on
  the words found in the chat; streaming text drawn in the frame that brings it, at most ten
  times a second, the list keeping its place; idle work not redrawing the window.
