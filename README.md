# Keleuo

A native desktop app for working with coding agents (Claude Code so far): a list of
sessions, each a whole workspace with its chat or the agent's own terminal, files, skills
and terminals. Written in Rust with gpui (the UI framework behind Zed) and gpui-component.
Prototype.

Keleuo started as a tool for our own work. Running many projects and many agent sessions at
once, we needed something native, fast and easy to work with that fit how we actually work.
It is a desktop app, not a harness: it runs Claude Code and stays out of its way, while
keeping each session's folder, conversation, terminals and files together so nothing gets lost.

Apache-2.0 licence, copyright Wundur AI Learning, LLC.

## Run it

Needs Rust 1.99 (pinned in `rust-toolchain.toml`) and Claude Code (`claude`) on the PATH.
Keep the build output on the internal disk:

```sh
export CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target"
cargo run -p keleuo-app --release -- /path/to/project      # the browser at that folder
cargo run -p keleuo-app --release -- --new /path/to/project # a session there at once
cargo test --workspace
```

The window opens on the **session browser** at the folder given (or the current folder, like
`code .`; started from the Finder, the last folder). Remote: `--ssh host:/path/on/host`.

### The `keleuo` command

`scripts/keleuo` opens the browser at the current folder, or hands the request to the
window that is already open (one window for everything; the first one listens on a socket in
`$TMPDIR`). Install it once, after a release build:

```sh
CARGO_TARGET_DIR="$HOME/Library/Caches/workbench-target" cargo build -p keleuo-app --release
ln -s "$PWD/scripts/keleuo" "$HOME/.local/bin/keleuo"   # any folder on your PATH
```

Then `keleuo`, `keleuo ~/code/app`, `keleuo --new .` or `keleuo --ssh dev:/srv/app`.
`KELEUO_BIN` points it at another binary.

### Restarting onto a new build

`scripts/restart` builds the release binary, quits the running Keleuo and starts the new
build in the same folder with the same arguments; the open sessions come back. It can run
from a terminal inside Keleuo (the quit-and-start half is detached, so it outlives the app),
which is what the Run button on a `! ./scripts/restart` block in the chat does. `--no-build`
restarts without building; debug test copies are left running. Keleuo is asked to quit
(`keleuo --quit`), not stopped, so the quit question below applies.

### Closing with work in it

Closing asks first only when something would be lost, like any Mac app. A file with unsaved
changes asks Save, Don't Save or Cancel when its tab closes (Cmd+W, its ×, a middle click) or
when Keleuo quits (Save All for several). Closing a session (Cmd+Shift+W, its menu) that is
mid-turn, or whose terminals run a command, asks Close or Cancel.

Quitting (Cmd+Q, the menu, the window's close button, or a restart) while a session is mid-turn
asks, with how many sessions are working and how many terminals are running a command: **Yes** quits and has each
working session told to carry on when Keleuo starts again (it is sent a short message saying
so, once); **No** just quits; **Cancel** keeps Keleuo open. With only terminal commands
running it asks whether to quit anyway.

## Layout and keys

    +-----------+----------------------------------------------------------------+
    | + New     | [grip] Files          ...  | [Claude Code][greet.py][zsh]       |
    | ● session | (file tree)                |  (chat or the agent's own terminal) |
    |   folder  |                            |                                     |
    | ○ session | [Files][Skills]            |                                     |
    |   ...     | [grip] Agent               |                                     |
    |           | (subagents)                |                                     |
    |           | [Agent]                    | [Chat][Terminal]                    |
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
  UI thread and only for the rows on screen. Any folder opens from the system picker (the
  folder button beside the path, File > Open Folder…, Cmd+O), a path typed or pasted in the
  filter (`/…` or `~/…`, then Enter), or a folder dropped in from the Finder. Paths show as
  `~/…`.
- **Sessions come back.** The open sessions (folder, agent, conversation, number) and the
  shown one are saved whenever they change, so a restart or a crash loses nothing. On the
  next start they open again in their order, each dormant: the conversation shows at once
  from its file and the agent starts only when needed (a message, or its Terminal shown).
  Closing a session takes it off the list; quitting does not.
- **Rename a folder** (File > Rename Folder…, or a session's context menu): its sessions
  close, the folder is renamed, Claude Code's project folder under `~/.claude/projects`
  (sessions and memory) moves with it, and the same sessions open again in the new place.
- **Chat and Terminal views** of one session, the tabs along the bottom of the chat. A
  Claude Code session runs the real `claude` interface once, in a pseudo-terminal, for the
  session's whole life. Terminal shows that terminal; Chat draws the same conversation from
  the session's file (followed as it grows) and its message box types into that same process
  (pasted, then Enter; Stop sends Escape). Switching only changes which view is shown, so it
  is instant at any length. Permission prompts show in Chat with Allow and Deny and in the
  terminal at the same time; whichever answers first counts. Each session remembers its
  view; "New sessions open in" on the strip sets the default. Hidden for backends without a
  terminal interface, and for SSH folders (those run the structured stream, Chat only).
  A message whose Enter the interface lost (busy starting on a long conversation, say) is
  seen still sitting in its input box and Enter is pressed again, with a note in the chat.
- **Cost or context.** On a subscription plan the session shows how full its context is
  ("22% of context", from the last turn's tokens against the model's window); billed per use
  (an API key or token in the environment, a cloud provider, or an `apiKeyHelper` in Claude
  Code's settings; no credential is read) it shows the cost. Click it to switch Auto, Always
  (cost) or Never (context).
- **Session ids**: the first 8 characters next to each session in the list and the browser;
  the whole id in the tooltip, a click copies it.
- **New terminals** (Cmd+T, or the button in the titlebar) open as a tab next to the agent's tab.
- **Web pages** (File > Open Web Page…): a tab among the files and terminals showing a page in
  the system web view (WKWebView, the Safari engine; no bundled browser). Back, Forward,
  Reload, the address (Enter loads; `3000` or `localhost:3000` is `http://localhost:3000`, a
  name without a scheme gets `https://`) and Open in the browser. Cmd+L goes to the address;
  a click on the page gives it the keys (Cmd+C, V, X, A, Z work in it). The page is a native
  view over the window: it follows its tab and hides when the tab is not shown, but gpui's
  menus, tooltips and notifications cannot draw over it (the folder rename dialog and the
  peeking session list hide it meanwhile). Right-click > Inspect Element opens Web Inspector.
  WebKit runs pages in its own processes; the About window counts them under Web pages.
  An http or https link in the chat, a Markdown preview or a terminal (Cmd+click) opens here;
  Cmd+click in the chat or a preview hands it to the default browser instead.
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
  The reply streams as it is written: while Claude works, the text is read off its own
  terminal and shown as a preview, replaced by the formatted reply from the session file
  when it is done. The thinking line shows how long the turn has run. Copy conversation
  (Cmd+Opt+C, the chat's ... menu, File menu) copies the whole thread as Markdown.
- **Long pastes** (10 lines or 1,000 characters) go into the message box as one pill,
  "Pasted text · 40 lines", where the cursor is: one unit (the cursor steps over it, one
  Backspace takes it), with a card above the box to show it in the text field or remove it.
  Sending puts the whole text back in its place. In the chat a paste folds to a line that
  opens it, Keleuo's own and Claude Code's (its `<pasted_content>` markers), so older
  conversations fold too.
- **Run** on a `!` command in the chat (`! git push`): the command, without the `!`, is
  typed into a terminal of the session's folder and run, so a password or login prompt has
  somewhere to be answered.
- **Tool and agent cards.** A running Bash command's card shows its latest output live;
  running cards spin and show their elapsed time. A subagent's report (or a teammate's
  message) is a one-line card that opens to the whole report. When a tool call fails in a
  known, avoidable way (so far: zsh refusing an unquoted `?`, `*` or `[`, as in a URL), its
  card says so and offers "Fix so it doesn't happen again": a Claude Code hook, a CLAUDE.md
  line or a shell setting, each saying what it writes where before anything is written.
  Keleuo never changes or retries the call itself.
- **Diffs**: a file the session edited can show everything the session changed in it, as one
  diff (rebuilt by undoing its edits): Open diff on an edit card, the Edited list, or
  Cmd+click on a path in a terminal.
- **Files**: the tree follows the folder as it changes (a file made in a terminal shows at
  once; git-ignored files stay out). New File… and New Folder… from the buttons in the
  panel's header (at the project root) or a right-click (in that folder): the name is typed
  in place, `a/b.rs` makes the folders between, and a new file opens.
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
  thumbnails. They are kept in `~/Library/Caches/keleuo/images/<session>` and removed after
  30 days; nothing is uploaded anywhere else.
- **Light and dark** follow the macOS appearance, live; the System | Light | Dark buttons in
  the titlebar (or View > Appearance) set it (saved as `appearance`). Both carry the round 4 hierarchy; the terminal has a light
  palette too and answers programs that ask for its colours (OSC 10/11), so those that adapt
  can. The accent is the icon's periwinkle (#6084E8); crane yellow marks what wants
  attention. Claude Code draws its own interface with its own theme setting, so in the light
  terminal its theme may need to be set to light in Claude Code.
- **The icon** (`crates/keleuo-app/assets/icon`): the owner's art at 128 px and up, the
  character alone for 16 to 64 px, in `Keleuo.icns`; the Dock shows it while the app runs.
- **Worktrees** are explicit: the browser's right side is titled with the folder's repository
  and branch, and "New session in a git worktree..." first shows what it will make ("Copy of
  proj (main) into ../proj-2, branch keleuo/proj-2") with the name editable.
- **Panel headers and sheet tabs** (after an earlier app): a tool panel (Files, Skills,
  Agent) has a title bar with a grip to drag it by, its title, its actions and
  options on hover; tool panels sharing a spot put their tabs along the bottom, the shown one
  joined to the content above with an accent line. Rows carry icons: files by type and coloured
  by language (folders in one warm tone), skills by kind and coloured by source (green
  project, cyan user, violet plugin; the description is the row's tooltip), terminals green
  while running, subagents by status. Blue is kept for focus. Documents (the chat, files, terminals) keep
  their tabs on top in the same style. Everything drags between groups or onto an edge to
  split (gpui-component's DockArea under a custom renderer, `dock_skin.rs`); "Reset layout"
  restores the default, and the arrangement is saved per folder.
- **Subagents** run live: a strip on top of the chat while they run, a list in the Agent
  panel (finished ones folded under Completed), and a card per subagent in the chat. A
  restored session shows only the subagents of its loaded part (after the last compaction).
- **About Keleuo** (the app menu): what it is and what it is built with; its memory (the
  figure Activity Monitor shows, with a chart of the last minute) apart from the agents' own
  programs and web pages; and the machine. A session is one conversation: earlier and
  running sessions of a folder are found in the session browser (Cmd+N), not inside a session.
- When the agent in one session edits a file another session in the same folder has open,
  that file shows "also edited by session ...".

| Keys | What |
| --- | --- |
| Cmd+N / Cmd+Shift+W | New session (the browser) / close the session |
| Cmd+O | Open a folder (the system picker) |
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
| Cmd+Opt+C | Copy the conversation as Markdown |
| Cmd+, | Settings: appearance, sessions, the chat, the message box, terminals, alerts |

Environment: `KELEUO_MODEL=haiku` picks the model; `KELEUO_PERMISSION_MODE=default`
starts sessions in that permission mode; `KELEUO_PERF=1` prints timings;
`KELEUO_CONTROL=<fifo>` accepts test commands (`open`, `term`, `type`, `split`, `send`,
`allow`, `new`, `browse`, `tab`, `tabs`, `move-session`, `fold-sessions`, `view`,
`term-type`, `term-screen`, `chat`, `chat-top`, `chat-scroll`, `panel`, `earlier`, `resume`, `fork`, `takeover`, `key`, `state`,
...; see `control.rs`, `shell.rs`, `workspace.rs`); `KELEUO_BACKGROUND=1` opens windows
without taking focus; `KELEUO_CONFIG_DIR` moves the settings folder (and the window's
socket with it); `KELEUO_TEST_BACKENDS=1` adds the scripted test agent to the browser.

## Layout of the code

- `crates/keleuo-core`: no UI. The gitignore-aware file tree, skills and frontmatter, the
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
- `crates/keleuo-app`: the gpui window. It only knows `Backend`, `Agent` and `Event`;
  `src/backends.rs` is the one file that names a backend, and a test fails if another file
  does.

## Adding an agent backend

1. Add `crates/keleuo-core/src/backends/<name>/` (or one file) and list it in
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
4. Register it in `crates/keleuo-app/src/backends.rs`. The new-session browser then offers
   it.
5. Test it the way `backends/scripted.rs` and `backends/claude/mod.rs` do: drive the `Agent`
   through the trait and apply its events to a `Conversation`. Recorded output belongs in
   `crates/keleuo-core/tests/fixtures`.

## How Claude runs

Each session runs `claude` (`--session-id <new id>`, or `--resume <id>`, with
`--fork-session` for a fork) in a pseudo-terminal in the project folder, plus
`--settings <file>`: a settings file of the window's own (in a `keleuo-<pid>` folder in
the system's temporary folder, removed when the window closes) that adds hooks. Nothing in
`~/.claude` is changed. Each hook runs `keleuo-hook <socket> <token> <event>` (built next
to `keleuo`; standard library only), which passes the hook's input to the window over a
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
counts. The interface writes whole messages, not a stream, so the reply being written is read
off the agent's terminal meanwhile (`backends/claude/screen.rs`) and shown as a preview until
the finished one arrives. The cost is an estimate, marked `~`,
from the token counts and the published per-token prices (`backends/claude/pricing.rs`).

The structured route (`claude -p --output-format stream-json --input-format stream-json
--verbose --include-partial-messages --permission-prompt-tool stdio`, permission prompts as
`control_request` lines) stays for backends without a terminal interface and for SSH
folders, whose hooks could not reach the window.

## Remote over SSH: the plan

Every project operation goes through `keleuo_core::host::Host`: list and read files, read
and write skills, watch for changes, the terminal's shell, and the `claude` command.

- `LocalHost`: this machine.
- `SshHost` (first steps, in the prototype): one `ssh` command per operation over a shared
  control connection (`ControlMaster`). The terminal is `ssh -t host 'cd dir && exec $SHELL -l'`
  in the local PTY; Claude is `ssh -T host claude -p ...` with the same stream over the ssh
  pipes. Files are read with `cat`, listed with `git ls-files`. No file watching.
- Next: a small headless helper binary (`keleuo-remote`), copied to the host on first
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
- Editor: a file changed on disk while it has unsaved edits is not reloaded and not flagged.
- Claude: images in tool results show as `[image]`.
- Sessions: sessions on other machines (SSH) are not restored after a restart; worktree
  sessions are local only; the shared-folder warning covers edits by
  Claude, not edits typed in another session's editor.
- Docking: dock tabs cannot be double-clicked to rename (F2 or the tab menu instead). Tabs
  that do not fit scroll rather than shrink; there is no overflow menu.
- Session list and browser: the browser has no remote recent folders and lists folders only (not files). The live dot sees agents
  on this machine only.
- Claude Code sessions: the cost is an estimate from token counts; the streaming preview is
  plain text until the reply is done. A permission answered in the terminal shows as allowed in Chat even when it was refused
  there.
- Terminal: no mouse reporting, no kitty keyboard protocol, no IME composition (dead keys
  and CJK input), no cursor blink, fixed colours and font (Menlo 12.5).
- Skills: plugin skills are found from `installed_plugins.json` and their `skills/` folder
  only (custom skill paths in a plugin manifest are not read). No delete or rename.
- Idle CPU is about 1 percent: gpui wakes the main thread on every display refresh even
  when nothing is drawn.
- Windows and Linux: nothing Mac-only in the core, but the app was only built and run on
  macOS.
