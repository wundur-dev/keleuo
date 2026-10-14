# Keleuo

Instructions for coding agents (Claude Code, Codex and others) working on this repo.

A native macOS window for Claude Code, in Rust with GPUI. Crates: `keleuo-core` (sessions,
backends, files; no UI) and `keleuo-app` (the window).

## Build and check

```sh
CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target" cargo build -p keleuo-app --release
CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target" cargo test -p keleuo-core -p keleuo-app
CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target" cargo clippy -p keleuo-core -p keleuo-app
```

On an exFAT drive, macOS leaves `._*` files that break builds and git; delete them first:
`find . -name '._*' -type f -not -path './target/*' -delete`.

## After a change: offer the restart

When a change to the app is done (built, tests and clippy pass), end the reply by offering a
restart onto the new build, as its own block:

```
! ./scripts/restart
```

Keleuo shows a Run button on that block. The script builds the release binary, quits the
running Keleuo and starts the new build in the same folder with the same arguments; open
sessions come back. Never run it yourself: it quits the app this session runs in.

## Changes go through pull requests

`main` changes only by merging a pull request; the owner reviews and merges.

- Start each change on a new branch off an up-to-date `main` (`git pull origin main`).
- Commit there, push the branch, and open a PR with `gh pr create`: what changed and why,
  and how it was checked.
- Never push to `main`, merge a PR, or delete a branch someone else made.
- After the owner merges, switch back to `main` and pull.
