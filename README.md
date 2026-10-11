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

With no folder (or one that does not exist) a folder picker with recent folders opens.
File > Open Folder (Cmd+O) opens another folder in a new window.

Remote over SSH (first steps, see below): `cargo run -p workbench-app -- --ssh host:/path/on/host`.

## Layout and keys

    [ session tab ][ session tab ][ + New session ]              (title bar)
    +------------+------------------------+----------------------------+
    | Files      | Claude                 | terminals, files, skills   |
    | Skills     | (this session's        | (tab groups; split, drag)  |
    | Terminals  |  conversation)         |                            |
    | Subagents  |                        |                            |
    +------------+------------------------+----------------------------+

- **Sessions are the top level.** Each tab in the title bar is a whole workspace: a folder
  (local or over SSH), one Claude conversation with its subagents, its own terminals, open
  files, skills and dock layout. Switching a tab switches everything; hidden sessions keep
  running and their tab shows a dot (blue working, amber needs permission, white unseen).
- **New session** (Cmd+N or "+"): the same folder, the same folder in a new git worktree (a
  new branch and folder next to the project, made with `git worktree add`, shown on the tab),
  a recent folder, or any folder. Without a worktree, when Claude in one session edits a file
  another session in the same folder has open, that file shows "also edited by session ...".
- **Docking inside a session** (gpui-component's DockArea): the lists, Claude, every terminal
  and every file are tabs that drag between tab groups or onto an edge to split. The default
  is lists on the left, Claude, then code; "Reset layout" (title bar or View menu) restores
  it, and the arrangement is saved per folder.
- **Subagents** run live: a strip on top of the Claude panel while they run, a list in the
  sidebar's Claude panel, and a card per subagent in the chat; each expands to its own
  messages and tool calls.
- **Earlier sessions** of the folder (from `~/.claude/projects`, read-only) are listed in the
  sidebar and open as a new session tab with their history; one still running in another
  terminal offers "Fork here" or "Take over when it stops" (never signalled).
- Tabs rename in place: double-click a session tab, F2 or the tab menu for other tabs.

| Keys | What |
| --- | --- |
| Cmd+N / Cmd+Shift+W | New session / close the session |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous session |
| Cmd+T | New terminal tab |
| Cmd+B | Fold or unfold the sidebar |
| Cmd+\\ / Cmd+Shift+\\ | Move the shown file or terminal into a new group beside / below |
| Cmd+Shift+C / Cmd+Shift+E | Zoom Claude / zoom the code group (again to unzoom) |
| Cmd+O | New session in a chosen folder |
| Cmd+S, Cmd+W | Save, close tab |
| Cmd+E | Markdown: rendered or editor |
| F2 | Rename the focused tab |
| Cmd+click in code | Go to definition (quick symbol index) |
| Ctrl+- / Ctrl+Shift+- | Back / forward through jumps |
| Cmd+L | Focus the message box |

Environment: `WORKBENCH_MODEL=haiku` picks the model; `WORKBENCH_PERF=1` prints timings;
`WORKBENCH_CONTROL=<fifo>` accepts test commands (`open`, `term`, `type`, `split`, `send`,
`allow`, `new`, `new-worktree`, `tab`, `tabs`, `earlier`, `resume`, `fork`, `takeover`, `key`,
`state`, ...; see `control.rs`, `shell.rs`, `workspace.rs`); `WORKBENCH_BACKGROUND=1` opens
windows without taking focus; `WORKBENCH_CONFIG_DIR` moves the settings folder.

## Layout of the code

- `crates/workbench-core`: no UI. Skills discovery and frontmatter, the gitignore-aware file
  tree, the Claude Code `stream-json` parser and chat state (tool calls, permission prompts,
  diffs, cost), the `claude` subprocess, `path:line` link detection, tab names, and the
  `Host` interface every project operation goes through. Tested with recorded streams in
  `tests/fixtures`.
- `crates/workbench-core` also holds the `Agent` trait (start, send, answer permission,
  interrupt, stop) that a chat session talks to; `ClaudeCode` is the only implementation.
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
- Claude: images in tool results show as `[image]`; the chat renders the last 150 items
  and older ones on request, but is not virtualized.
- Sessions: the open session tabs are not restored after a restart (each folder's dock
  layout is); worktree sessions are local only; the shared-folder warning covers edits by
  Claude, not edits typed in another session's editor.
- Docking: dock tabs cannot be double-clicked to rename (F2 or the tab menu instead).
- Terminal: no mouse reporting, no kitty keyboard protocol, no IME composition (dead keys
  and CJK input), no cursor blink, fixed colours and font (Menlo 12.5).
- Skills: plugin skills are found from `installed_plugins.json` and their `skills/` folder
  only (custom skill paths in a plugin manifest are not read). No delete or rename.
- Idle CPU is about 1 percent: gpui wakes the main thread on every display refresh even
  when nothing is drawn.
- Windows and Linux: nothing Mac-only in the core, but the app was only built and run on
  macOS.
