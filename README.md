# Keleion

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
cargo run -p keleion-app --release -- /path/to/project      # the browser at that folder
cargo run -p keleion-app --release -- --new /path/to/project # a session there at once
cargo test --workspace
```

The window opens on the **session browser** at the folder given (or the current folder, like
`code .`; started from the Finder, the last folder). Remote: `--ssh host:/path/on/host`.

### The `keleion` command

`scripts/keleion` opens the browser at the current folder, or hands the request to the
window that is already open (one window for everything; the first one listens on a socket in
`$TMPDIR`). Install it once, after a release build:

```sh
CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target" cargo build -p keleion-app --release
ln -s "$PWD/scripts/keleion" "$HOME/.local/bin/keleion"   # any folder on your PATH
```

Then `keleion`, `keleion ~/code/app`, `keleion --new .` or `keleion --ssh dev:/srv/app`.
`KELEION_BIN` points it at another binary.

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
- **Chat and Terminal views** of one session, the tabs along the bottom of the chat. A
  Claude Code session runs the real `claude` interface once, in a pseudo-terminal, for the
  session's whole life. Terminal shows that terminal; Chat draws the same conversation from
  the session's file (followed as it grows) and its message box types into that same process
  (pasted, then Enter; Stop sends Escape). Switching only changes which view is shown, so it
  is instant at any length. Permission prompts show in Chat with Allow and Deny and in the
  terminal at the same time; whichever answers first counts. Each session remembers its
  view; "New sessions open in" on the strip sets the default. Hidden for backends without a
  terminal interface, and for SSH folders (those run the structured stream, Chat only).
- **Cost or context.** On a subscription plan the session shows how full its context is
  ("22% of context", from the last turn's tokens against the model's window); billed per use
  (an API key or token in the environment, a cloud provider, or an `apiKeyHelper` in Claude
  Code's settings; no credential is read) it shows the cost. Click it to switch Auto, Always
  (cost) or Never (context).
- **Session ids**: the first 8 characters next to each session in the list and the browser;
  the whole id in the tooltip, a click copies it.
- **New terminals** (Cmd+T, "+" in Terminals) open as a tab next to the agent's tab.
- **A session open elsewhere is never continued here as it is.** It counts as open when a
  `claude` process names it on its command line (`--resume`, `-r`, `--session-id`; found
  wherever that process runs), when a `claude` without one works in the folder (its folder
  from `proc_pidinfo`, else `lsof`), or when its file changed in the last 10 seconds. Such a
  session offers only Fork here and (with a process) Take over when it stops, and a plain
  Continue is checked again at the moment of opening and refused if it has become live.
- **Long sessions**: the file is read as a stream, never whole. Chat builds only the context
  after the last compaction; each earlier stretch is a line that is read when it is scrolled
  to, under a "Context compacted here" divider whose summary opens on a click. Tool results
  in a history keep their first 16 KB. A 150 MB scratch session with five compactions opens
  in about 0.15 s.
- **Questions before Claude is up**: the folder trust question shows in Chat with Trust and
  Don't trust (answered in the terminal as a person would, with the arrow keys and Enter);
  anything else it asks before it is up (no SessionStart hook within 6 seconds, or a list to
  choose from) shows "is asking something in the terminal" with Show terminal, and the
  session's dot turns amber.
- **Chat** reads like the chat apps people know: a centred column (760 px at most), the
  person's messages as rounded bubbles on the right, Claude's replies as plain text across
  the column, code blocks with their language and a copy button, Copy under each message on
  hover. The composer floats at the bottom of the column: the text, images to send as
  thumbnails (paste, drop, or the paperclip; click one to see it full size, x to remove), the
  model, the permission mode and a send button that turns into stop while Claude works.
- **Model and permission mode** from the composer. A new model restarts Claude Code on the
  same conversation with `--model` between turns (Claude Code's own `/model` would also save
  it as the default for every new session in the person's settings). The permission mode is
  switched as a person would, with Shift+Tab until the status line shows it; the hooks
  report the mode it is in. A message starting with `/` is typed, not pasted, so Claude Code
  takes it as a command.
- **Images**: images sent from Chat are pasted into Claude Code as file paths, which it turns
  into its own `[Image #N]` attachments; in the Terminal view Cmd+V with an image on the
  clipboard sends Ctrl+V, Claude Code's own image paste, and a copy is kept. Pointing at an
  `[Image #N]` on the terminal screen shows that image. Images in history show as
  thumbnails. They are kept in `~/Library/Caches/keleion/images/<session>` and removed after
  30 days; nothing is uploaded anywhere else.
- **Worktrees** are explicit: the browser's right side is titled with the folder's repository
  and branch, and "New session in a git worktree..." first shows what it will make ("Copy of
  proj (main) into ../proj-2, branch keleion/proj-2") with the name editable.
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

Environment: `KELEION_MODEL=haiku` picks the model; `KELEION_PERMISSION_MODE=default`
starts sessions in that permission mode; `KELEION_PERF=1` prints timings;
`KELEION_CONTROL=<fifo>` accepts test commands (`open`, `term`, `type`, `split`, `send`,
`allow`, `new`, `browse`, `tab`, `tabs`, `move-session`, `fold-sessions`, `view`,
`term-type`, `term-screen`, `chat`, `chat-top`, `chat-scroll`, `panel`, `earlier`, `resume`, `fork`, `takeover`, `key`, `state`,
...; see `control.rs`, `shell.rs`, `workspace.rs`); `KELEION_BACKGROUND=1` opens windows
without taking focus; `KELEION_CONFIG_DIR` moves the settings folder (and the window's
socket with it); `KELEION_TEST_BACKENDS=1` adds the scripted test agent to the browser.

## Layout of the code

- `crates/keleion-core`: no UI. The gitignore-aware file tree, skills and frontmatter, the
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
- `crates/keleion-app`: the gpui window. It only knows `Backend`, `Agent` and `Event`;
  `src/backends.rs` is the one file that names a backend, and a test fails if another file
  does.

## Adding an agent backend

1. Add `crates/keleion-core/src/backends/<name>/` (or one file) and list it in
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
4. Register it in `crates/keleion-app/src/backends.rs`. The new-session browser then offers
   it.
5. Test it the way `backends/scripted.rs` and `backends/claude/mod.rs` do: drive the `Agent`
   through the trait and apply its events to a `Conversation`. Recorded output belongs in
   `crates/keleion-core/tests/fixtures`.

## How Claude runs

Each session runs `claude` (`--session-id <new id>`, or `--resume <id>`, with
`--fork-session` for a fork) in a pseudo-terminal in the project folder, plus
`--settings <file>`: a settings file of the window's own (in a `keleion-<pid>` folder in
the system's temporary folder, removed when the window closes) that adds hooks. Nothing in
`~/.claude` is changed. Each hook runs `keleion-hook <socket> <token> <event>` (built next
to `keleion`; standard library only), which passes the hook's input to the window over a
local socket. Otherwise it is started as in a person's terminal: no stream flags, TERM
`xterm-256color`, LANG set when the window has none, and the variables a running Claude Code
gives its children unset (when the window was itself started from one).

The hooks:

- `SessionStart`: the interface is up; messages typed before it wait until then.
- `UserPromptSubmit`, `PreToolUse`, `Stop`, `Notification`: the status dot (working, which
  tool, idle, unseen) from what the interface says rather than guessed.
- `PermissionRequest`: the hook waits; Chat shows Allow and Deny, and the answer goes back as
  the hook's decision. The interface shows its own prompt at the same moment; answered there,
  `PostToolUse` ends the wait.

The chat itself comes from the session file under `~/.claude/projects`, read from where the
last read stopped every 100 ms: messages, tool calls and results, diffs, subagents and token
counts. The interface writes whole messages, not a stream, so a reply shows when its block is
done (the status line says what is running meanwhile). The cost is an estimate, marked `~`,
from the token counts and the published per-token prices (`backends/claude/pricing.rs`).

The structured route (`claude -p --output-format stream-json --input-format stream-json
--verbose --include-partial-messages --permission-prompt-tool stdio`, permission prompts as
`control_request` lines) stays for backends without a terminal interface and for SSH
folders, whose hooks could not reach the window.

## Remote over SSH: the plan

Every project operation goes through `keleion_core::host::Host`: list and read files, read
and write skills, watch for changes, the terminal's shell, and the `claude` command.

- `LocalHost`: this machine.
- `SshHost` (first steps, in the prototype): one `ssh` command per operation over a shared
  control connection (`ControlMaster`). The terminal is `ssh -t host 'cd dir && exec $SHELL -l'`
  in the local PTY; Claude is `ssh -T host claude -p ...` with the same stream over the ssh
  pipes. Files are read with `cat`, listed with `git ls-files`. No file watching.
- Next: a small headless helper binary (`keleion-remote`), copied to the host on first
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
- Claude: images in tool results show as `[image]`.
- Sessions: the open session tabs are not restored after a restart (each folder's dock
  layout is); worktree sessions are local only; the shared-folder warning covers edits by
  Claude, not edits typed in another session's editor.
- Docking: dock tabs cannot be double-clicked to rename (F2 or the tab menu instead). Tabs
  that do not fit scroll rather than shrink; there is no overflow menu.
- Session list and browser: the list order and the open sessions are not saved; the browser
  has no remote recent folders and lists folders only (not files). The live dot sees agents
  on this machine only.
- Claude Code sessions: the cost is an estimate from token counts; text shows a block at a
  time (the file has no partial messages); a slash command typed in Chat is pasted as text.
  A permission answered in the terminal shows as allowed in Chat even when it was refused
  there.
- Terminal: no mouse reporting, no kitty keyboard protocol, no IME composition (dead keys
  and CJK input), no cursor blink, fixed colours and font (Menlo 12.5).
- Skills: plugin skills are found from `installed_plugins.json` and their `skills/` folder
  only (custom skill paths in a plugin manifest are not read). No delete or rename.
- Idle CPU is about 1 percent: gpui wakes the main thread on every display refresh even
  when nothing is drawn.
- Windows and Linux: nothing Mac-only in the core, but the app was only built and run on
  macOS.
