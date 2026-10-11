# Workbench

A native desktop app for working with Claude Code on one project folder: skills on the
left, code in the centre, Claude on the right, a real terminal along the bottom. Written in
Rust with gpui (the UI framework behind Zed) and gpui-component. First prototype.

MIT licence, copyright Wundur AI Learning, LLC.

## Run it

Needs Rust 1.99 (pinned in `rust-toolchain.toml`) and Claude Code (`claude`) on the PATH.
Keep the build output on the internal disk:

```sh
export CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target"
cargo run -p workbench-app -- /path/to/project
cargo run -p workbench-app --release -- /path/to/project   # for speed measurements
cargo test -p workbench-core
```

With no folder it opens the current directory. File > Open Folder (Cmd+O) switches folder.

Remote over SSH (first steps, see below): `cargo run -p workbench-app -- --ssh host:/path/on/host`.

## Layout and keys

STATUS_TABLE_PLACEHOLDER

## Layout of the code

- `crates/workbench-core`: no UI. Skills discovery and frontmatter, the gitignore-aware file
  tree, the Claude Code `stream-json` parser and chat state (tool calls, permission prompts,
  diffs, cost), the `claude` subprocess, `path:line` link detection, tab names, and the
  `Host` interface every project operation goes through. Tested with recorded streams in
  `tests/fixtures`.
- `crates/workbench-app`: the gpui window.

## How Claude runs

`claude -p --output-format stream-json --input-format stream-json --verbose
--include-partial-messages --permission-prompt-tool stdio`, in the project folder. Text
streams in as it is written. Permission prompts arrive as `control_request` /
`can_use_tool` lines and are answered from the chat with Allow or Deny, so Claude's normal
"ask first" mode works without falling back to `acceptEdits`. The session id is kept so a
restarted process continues with `--resume`.

## Remote over SSH: the plan

Every project operation goes through `workbench_core::host::Host`: list and read files, read
and write skills, watch for changes, the terminal's shell, and the `claude` command.

- `LocalHost`: this machine.
- `SshHost` (first steps, in the prototype): one `ssh` command per operation over a shared
  control connection (`ControlMaster`). The terminal is `ssh -t host 'cd dir && exec $SHELL -l'`
  in the local PTY; Claude is `ssh -T host claude -p ...` with the same stream over the ssh
  pipes. Files are read with `cat`, listed with `git ls-files`. No file watching.
- Next: a small headless helper binary (`workbench-remote`), copied to the host on first
  connect and started over `ssh` (the approach Zed uses for remote development). It speaks a
  compact length-prefixed protocol over its stdin and stdout: list, read, write, watch
  (notify on the remote), spawn PTY, spawn `claude`, and later spawn language servers. One
  connection carries everything; the `Host` trait stays the same, with an `RemoteHost`
  implementation talking to the helper.

## Code intelligence: the plan

For reading and navigating code in the editor (not for the Claude panel).

- Now: every `path:line` or file name in Claude's messages, tool cards and diffs is a link
  that opens the file at that line.
- Next: a tree-sitter symbol index (definitions per file, outline, go to definition by name),
  built on a background thread through `Host`.
- Then LSP: hover for type and docs, Cmd+click go to definition, find references, back and
  forward through jumps; completions last. gpui-component's code editor already has hooks for
  LSP hover, completion and diagnostics. Language servers run where the code is: on the remote
  host through the helper binary, with the protocol forwarded over the same connection.

## What is missing

MISSING_PLACEHOLDER
