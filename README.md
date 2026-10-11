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

One thin title bar carries the panel switches, the editor tabs, the folder and git branch.
Below it: skills | files | code | Claude, over the terminal. Panels resize with splitters.

| Keys | What |
| --- | --- |
| Cmd+1 / Cmd+2 / Cmd+3 / Cmd+J | Show or hide skills / files / Claude / terminal (sizes are kept) |
| Cmd+Shift+E / Cmd+Shift+C | Focus mode: only the editor / only Claude (again to leave) |
| Cmd+O | Open a folder in a new window |
| Cmd+S, Cmd+W | Save, close tab |
| Cmd+E | Markdown: rendered or editor |
| F2, double-click, right-click | Rename a tab in place (Enter saves, Escape cancels); "Reset name" in the menu |
| Cmd+click (or F12) in code | Go to definition (quick symbol index) |
| Ctrl+- / Ctrl+Shift+- | Back / forward through jumps |
| Cmd+L | Ask Claude (focus the message box) |
| Cmd+T | New terminal tab |

`WORKBENCH_MODEL=haiku` picks the model for Claude; `WORKBENCH_PERF=1` prints paint times,
key-to-glyph latency and file open times to stderr.

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

- Remote: `--ssh host:/path` is wired (terminal and Claude over ssh, files over one `ssh`
  command each) but untested, since no SSH host was available. No remote file watching.
- Code intelligence: definitions come from line patterns, not tree-sitter queries or LSP. No
  hover, no find references, no outline view yet.
- Editor: no "save changes?" prompt on close; a file changed on disk while it has unsaved
  edits is not reloaded and not flagged.
- Claude: one conversation per window; no session picker for `--resume`; subagent tool calls
  are shown inline (marked) rather than nested; images in tool results show as `[image]`;
  the chat is not virtualized, so very long sessions will get slower.
- Terminal: no mouse reporting, no kitty keyboard protocol, no IME composition (dead keys
  and CJK input), no cursor blink, fixed colours and font (Menlo 12.5).
- Skills: plugin skills are found from `installed_plugins.json` and their `skills/` folder
  only (custom skill paths in a plugin manifest are not read). No delete or rename.
- Idle CPU is about 1 percent: gpui wakes the main thread on every display refresh even
  when nothing is drawn.
- Windows and Linux: nothing Mac-only in the core, but the app was only built and run on
  macOS.
