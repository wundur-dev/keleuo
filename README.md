# Workbench

A native desktop app for working with coding agents (Claude Code so far): a list of
sessions, each a whole workspace with its chat or the agent's own terminal, files, skills
and terminals. Written in Rust with gpui (the UI framework behind Zed) and gpui-component.
Prototype.

MIT licence, copyright Wundur AI Learning, LLC.

## Run it

Needs Rust 1.99 (pinned in `rust-toolchain.toml`) and Claude Code (`claude`) on the PATH.
Keep the build output on the internal disk:

```sh
export CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target"
cargo run -p workbench-app --release -- /path/to/project      # the browser at that folder
cargo run -p workbench-app --release -- --new /path/to/project # a session there at once
cargo test --workspace
```

The window opens on the **session browser** at the folder given (or the current folder, like
`code .`; started from the Finder, the last folder). Remote: `--ssh host:/path/on/host`.

### The `workbench` command

`scripts/workbench` opens the browser at the current folder, or hands the request to the
window that is already open (one window for everything; the first one listens on a socket in
`$TMPDIR`). Install it once, after a release build:

```sh
CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target" cargo build -p workbench-app --release
ln -s "$PWD/scripts/workbench" "$HOME/.local/bin/workbench"   # any folder on your PATH
```

Then `workbench`, `workbench ~/code/app`, `workbench --new .` or `workbench --ssh dev:/srv/app`.
`WORKBENCH_BIN` points it at another binary.

## Layout and keys

    +-----------+----------------------------------------------------------------+
    | + New     | [grip] Files          ...  | [Claude Code][greet.py][zsh]       |
    | ● session | (file tree)                |  (chat or the agent's own terminal) |
    |   folder  |                            |                                     |
    | ○ session | [Files][Skills]            |                                     |
    |   ...     | [grip] Agent               |                                     |
    |           | (subagents)                |                                     |
    |           | [Agent][Terminals]         | [Chat][Terminal]                    |
    +-----------+----------------------------+-------------------------------------+

- **Sessions are the top level**, listed down the left. Each is a whole workspace: a folder
  (local or over SSH), one agent conversation with its subagents, its own terminals, open
  files, skills and dock layout. A row shows a dot (blue working, amber needs permission,
  white unseen, hollow idle), the name (double-click to rename), folder and branch, a worktree
  badge, the last thing that happened and the cost so far. Drag rows to reorder; Cmd+Shift+B
  folds the list to a rail of dots and initials (names in tooltips).
- **New session** (Cmd+N, "+ New session"): the browser. Folders on the left (this folder,
  recent ones, then the folders here; type to filter, Up/Down to move, Cmd+Down to go into a
  folder, Cmd+Up for the parent), each with a green dot when an agent runs there and a count
  of its sessions. The selected folder's sessions on the right: running ones first ("running
  in another terminal since 10:42", Fork here, Take over when it stops), then recent ones
  (Continue); plus "Start a new session here" (Enter) and "New session in a git worktree"
  (Cmd+Enter). A session already open in the window just shows. Folder discovery runs off the
  UI thread and only for the rows on screen.
- **Chat and Terminal views** of one session, the tabs along the bottom of the chat: Chat is
  the native view; Terminal runs the agent's own interface (`claude --resume <id>`) in a
  pseudo-terminal on the same conversation. Only one process ever drives a session: switching
  waits for the turn to end, then ends one process before starting the other. The view
  switches at once: the terminal shows the last lines of the conversation and "Starting..."
  until the interface has drawn (about 0.6 s), and Chat shows while the interface exits
  behind it (Ctrl+C, then `/exit`); a message sent meanwhile goes once it has, and the next
  stream process starts as soon as the terminal has closed. `WORKBENCH_PERF=1` prints each
  step of a switch. In Terminal view the
  window follows the session's file, so subagents, edits (a strip of changed files), tokens
  and the status dot keep working. Each session remembers its view; "New sessions open in"
  on the strip sets the default. Hidden for backends without a terminal interface.
- **Panel headers and sheet tabs** (after an earlier app): a tool panel (Files, Skills,
  Terminals, Agent) has a title bar with a grip to drag it by, its title, its actions and
  options on hover; tool panels sharing a spot put their tabs along the bottom, the shown one
  joined to the content above with an accent line. Rows carry icons: files by type and coloured
  by language (folders in one warm tone), skills by kind and coloured by source (green
  project, cyan user, violet plugin; the description is the row's tooltip), terminals green
  while running, subagents by status. Blue is kept for focus. Documents (the chat, files, terminals) keep
  their tabs on top in the same style. Everything drags between groups or onto an edge to
  split (gpui-component's DockArea under a custom renderer, `dock_skin.rs`); "Reset layout"
  restores the default, and the arrangement is saved per folder.
- **Subagents** run live: a strip on top of the chat while they run, a list in the Agent
  panel, and a card per subagent in the chat. A session is one conversation: earlier and
  running sessions of a folder are found in the session browser (Cmd+N), not inside a session.
- When the agent in one session edits a file another session in the same folder has open,
  that file shows "also edited by session ...".

| Keys | What |
| --- | --- |
| Cmd+N / Cmd+Shift+W | New session (the browser) / close the session |
| Cmd+Shift+B | Fold or unfold the session list |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous session |
| Cmd+T | New terminal tab |
| Cmd+B | Fold or unfold the tool panels |
| Cmd+\\ / Cmd+Shift+\\ | Move the shown file or terminal into a new group beside / below |
| Cmd+Shift+C / Cmd+Shift+E | Zoom the chat / zoom the code group (again to unzoom) |
| Cmd+S, Cmd+W | Save, close tab |
| Cmd+E | Markdown: rendered or editor |
| F2 | Rename the focused tab |
| Cmd+click in code | Go to definition (quick symbol index) |
| Ctrl+- / Ctrl+Shift+- | Back / forward through jumps |
| Cmd+L | Focus the message box |

Environment: `WORKBENCH_MODEL=haiku` picks the model; `WORKBENCH_PERF=1` prints timings;
`WORKBENCH_CONTROL=<fifo>` accepts test commands (`open`, `term`, `type`, `split`, `send`,
`allow`, `new`, `browse`, `tab`, `tabs`, `move-session`, `fold-sessions`, `view`,
`term-type`, `term-screen`, `chat`, `panel`, `earlier`, `resume`, `fork`, `takeover`, `key`, `state`,
...; see `control.rs`, `shell.rs`, `workspace.rs`); `WORKBENCH_BACKGROUND=1` opens windows
without taking focus; `WORKBENCH_CONFIG_DIR` moves the settings folder (and the window's
socket with it); `WORKBENCH_TEST_BACKENDS=1` adds the scripted test agent to the browser.

## Layout of the code

- `crates/workbench-core`: no UI. The gitignore-aware file tree, skills and frontmatter, the
  chat state built from agent events (tool calls, permission prompts, diffs, subagents,
  cost), `path:line` link detection, tab names, and the `Host` interface every project
  operation and every program goes through.
  - `agent`: the neutral model: `Event` (text, tool call and result with an optional diff,
    permission request, subagent start, progress and end, usage and cost, status), the
    `Agent` trait (start, send, answer a permission prompt, interrupt, stop), `Backend`
    (what a kind of agent offers) with `Capabilities`, and the `Registry`.
  - `backends::claude`: Claude Code. Its `stream-json` format, session files under
    `~/.claude/projects`, skill folders and process discovery live here and nowhere else.
  - `backends::scripted`: a fake agent for tests that plays a fixed turn of neutral events.
- `crates/workbench-app`: the gpui window. It only knows `Backend`, `Agent` and `Event`;
  `src/backends.rs` is the one file that names a backend, and a test fails if another file
  does.

## Adding an agent backend

1. Add `crates/workbench-core/src/backends/<name>/` (or one file) and list it in
   `backends/mod.rs`.
2. Implement `Agent` for one running conversation: build a `ProgramSpec` (program, arguments,
   environment, folder) and start it with `host.program_command(&spec)`, so it runs locally or
   over SSH the same way. Read its output on a thread, map each line onto `agent::Event`s and
   pass them to the `Sink` as `Message::Event`; stderr lines as `Message::Log`; `Message::Closed`
   when it ends.
3. Implement `Backend`: an `id` (saved with sessions), a `name` (shown in the window),
   `capabilities()`, and `new_agent()`. Implement only the optional parts it has: `skills` and
   `create_skill`, `earlier_sessions`, `session_count`, `load_session`, `running` and
   `running_elsewhere`. The window hides what `capabilities()` says is missing (skills, earlier
   sessions, forking, subagents, permission prompts, the accept-edits toggle, models).
4. Register it in `crates/workbench-app/src/backends.rs`. The new-session browser then offers
   it.
5. Test it the way `backends/scripted.rs` and `backends/claude/mod.rs` do: drive the `Agent`
   through the trait and apply its events to a `Conversation`. Recorded output belongs in
   `crates/workbench-core/tests/fixtures`.

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
- Docking: dock tabs cannot be double-clicked to rename (F2 or the tab menu instead). Tabs
  that do not fit scroll rather than shrink; there is no overflow menu.
- Session list and browser: the list order and the open sessions are not saved; the browser
  has no remote recent folders and lists folders only (not files). The live dot sees agents
  on this machine only.
- Terminal view: the agent's own interface does not write its cost to the session file, so
  the cost shown is what Chat counted; a permission prompt inside the terminal interface does
  not turn the dot amber (it is not in the file). Leaving the terminal types Ctrl+C and
  `/exit`; if the person has the interface in another mode, they finish there.
- Terminal: no mouse reporting, no kitty keyboard protocol, no IME composition (dead keys
  and CJK input), no cursor blink, fixed colours and font (Menlo 12.5).
- Skills: plugin skills are found from `installed_plugins.json` and their `skills/` folder
  only (custom skill paths in a plugin manifest are not read). No delete or rename.
- Idle CPU is about 1 percent: gpui wakes the main thread on every display refresh even
  when nothing is drawn.
- Windows and Linux: nothing Mac-only in the core, but the app was only built and run on
  macOS.
