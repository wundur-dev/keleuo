# Proposal: making Keleuo light on the machine

Status (2026-10-16): much of it built and measured: idle and hidden-work frames (part 1), the
working line drawn apart and its spinner turned by Core Animation, one frame per streamed piece,
batching of a session's messages, long replies split into rows, one shared file watcher, an
event-driven session follower. Open: the display link pause, copying items per frame, linkify
caching, startup loading, memory items. Originally: a proposal for the owner to read. Written 2026-10-15 from a read-through of
the code in four parts (chat, agents and background work, terminal and editor, window and idle),
with the main claims checked against the code. Every change keeps every feature.

## The one thing that explains most of it

GPUI redraws a view when it, or anything inside it, says it changed, and it redraws every view on
the way up to the window. Only the panels' contents are cached (`dock_skin.rs:558`); the shell,
the workspace, the dock and every tab strip are rebuilt on every frame. And almost everything says
it changed:

- the shell observes every workspace and every session (`shell.rs:839-840`), each workspace its
  session (`workspace.rs:290`) and every terminal (`workspace.rs:990`);
- so a token streamed in **any** session, or Claude Code's spinner in a **hidden** session's
  terminal (10-30 times a second), rebuilds the session list, the visible workspace, the dock and
  its tab strips, and repaints the window.

What runs on each of those frames is then heavier than it needs to be (below). Fixing both halves
is most of the gain: fewer frames, and cheaper frames.

## Measured now

The running build, this session working: about 316 MB (66 MB of it the window's drawing buffers),
9-15% CPU while streaming. `KELEUO_PERF=1` already times terminal and chat paints and startup;
step 0 extends it so each change below is measured before and after, not guessed.

## The plan, in order

### 0. Measure (small)

Count frames per second and renders per view under `KELEUO_PERF=1`, and log CPU, memory and idle
wakeups every few seconds. A fixed scene to compare: one session streaming, three idle, one busy
terminal in a hidden tab, the app in the background.

### 1. Stop the needless frames (small changes, the biggest win when idle)

1. **The text cursor blinks forever in the background.** Blinking starts when the window becomes
   active and never stops when it goes inactive (`vendor/gpui-base/src/input/base/state.rs:698`):
   with the message box focused, the whole window is rebuilt twice a second while you use another
   app. Stop it on deactivation; optionally stop after a minute without typing.
2. **Hidden sessions and terminals redraw the window.** Remove the blanket observers (the
   workspace's render reads neither its session nor its terminals); terminals send a "title
   changed" event only when the program, unseen mark or exit changes, not on every byte; the
   session list listens for "its row changed" (dot, title, activity, cost).
3. **The chat's 100 ms ticker rebuilds the whole window** for a spinner step and a seconds
   counter, for the whole of a turn and while background agents run (`chat_panel/mod.rs:128,174`).
   Keep the spinner's rate but cache everything around it (step 2 of part 2), and pause the ticker
   for chats not on screen. Also stop a background agent's tool clocks when it ends
   (`conversation.rs:847`): today a stopped agent can leave the ticker running at idle.
4. **Hover and drags call `window.refresh()`** (tabs `sheet_tabs.rs:375`, resize handles, the diff
   overview `doc_panel.rs:668`), which redraws even cached panels. Notify the one view instead.
5. Smaller: the terminal's startup spinner can run forever if a program never prints
   (`terminal_view.rs:943`, add a timeout); the folder picker polls `ps` every 5 s and redraws even
   when nothing changed (`browser.rs:425`).

### 2. Make each frame cheap

1. **Every visible chat entry deep-copies its item every frame** (`chat_panel/mod.rs:254`
   `item_at(..).cloned()`): a Read's output, a Write's whole file, a subagent with its transcript
   (`tools.rs:307`, `agents.rs:133`). Collapsed tool groups render their members too, so 100
   folded Reads cost 100 copies per frame. Fix: items behind `Arc` (`Vec<Arc<Item>>`, only the
   tail mutates), render from borrows, tool runs worked out once per change, Copy buttons read the
   text when clicked.
2. **Cache the window's frame:** the session list becomes its own cached view; the workspace and
   dock are cached; the chat's composer and its finished messages stop rebuilding on a tick.
3. **Markdown is re-linked and often re-parsed every frame** (`items.rs:499` linkifies the whole
   message per frame; a path completing mid-stream makes the parser start over: quadratic over a
   long reply). Cache the linked text per message by revision; link only complete lines while
   streaming; keep each message's parsed state by a stable id (today keyed by list position, so
   loading an earlier part re-parses everything visible).
4. **The terminal shapes every row every frame** (`terminal_view.rs:1006-1158`, one `shape_line`
   per row, per-cell colour lookups) while holding the terminal's lock. Redraw only the rows that
   changed (alacritty's damage tracking), shape outside the lock.
5. **Diff cards build up to 400 rows of elements per frame** (`tools.rs:147-228`); pre-build the
   rows once per diff (and fix the shared `d0-…` ids, a small bug).
6. **The session list scans whole conversations per row per frame** (`needs_input`, titles from a
   large first paste, menus built for every row: `shell.rs:1326-1500`); and on every token of any
   session, `attend()` scans all sessions and `save_open()` rebuilds the saved list. Keep counters
   on the conversation; cache rows by revision; build menus when opened.
7. **Side panels copy on every token:** the Agent panel clones every subagent's transcript
   (`agent_side.rs:38`), Changes clones every diff of the session (`changes_panel.rs:118`).
8. **The vendored markdown parser copies the whole document on each append** (`text/state.rs:1271,
   1288`) and takes two frames per delta. Parse small appends in place.
9. Small: per-frame syscalls for terminal tab names (`terminal_view.rs:839`); the composer clones
   its model list each frame and re-highlights every code fence per keystroke; the workspace
   formats an unused string per frame.

### 3. Background work: do it once, and only when needed

1. **After every turn each session rebuilds its whole project index**: a full walk plus re-reading
   up to 8,000 files for symbols (`workspace.rs:283,245,1183`). Per session, so three sessions on
   one folder do it three times; over SSH each file is a separate `ssh … cat`. Fix: one tree,
   index and watcher per (machine, folder), shared; update symbols for the files that changed;
   skip the turn-end refresh when a watcher covers it; build nothing for a session until it is
   first shown.
2. **Claude Code's session file is checked 10 times a second, forever** (`history.rs:623`), even
   idle; over SSH each check starts an ssh process (3 a second, per session), plus subagent
   transcripts every third round, never pruned. Fix: kqueue/FSEvents locally; fast polling only
   between a turn's start and end (hooks tell us), slow otherwise; one long-lived `tail -f` over
   SSH; stop tailing ended subagents.
3. **Cursor's chat is re-read whole on every change** (`cursor/mod.rs:127`, every 1.5 s), and over
   SSH its whole database (up to 64 MB) is copied several times per turn. Read only new rows.
4. **Startup loads every saved session in full before showing a tab**, builds each one's
   workspace, starts its shells and web views, and parses `~/.claude.json` once per session
   (`shell.rs:205`, `workspace.rs:107`, `web_panel.rs:122`). Fix: show the last session first;
   load the rest when first shown.
5. **SSH blocks the window**: starting a session or switching views fetches whole session files on
   the UI thread to find one line (`agent_session.rs:596,1502`); stream and stop early, off the UI
   thread.
6. The folder picker re-reads up to 30 whole session files per folder chosen, and stacks scans
   when arrowing through folders (`browser.rs:249`): debounce, cache by size and date.
7. Settings are written whole, synchronously, on every session switch and each keystroke in the
   Settings window: debounce, write in the background, atomically.
8. The terminal history snapshot builds 3,000 lines of text on the UI thread every 2 s during
   output (`terminal_view.rs:348`): save after a quiet spell and on close, build it off-thread.
9. Images are base64-decoded again on every load with a hand-written decoder (`images.rs`): skip
   when the file exists; use the `base64` crate already in the tree.

### 4. Memory

1. **Images are kept decoded at full size for the app's life** (GPUI's asset store never evicts):
   a Retina screenshot is 20-60 MB, decoded at full size for a 160×110 thumbnail. Decode
   thumbnails small in the background into a bounded cache; drop the full image when its view
   closes.
2. **Every terminal keeps 10,000 lines of scrollback** (`pty.rs:127`), 30-50 MB when full, never
   given back, including the agents' own interfaces whose record is the chat. 2-3k for agent
   interfaces; a setting for shells.
3. **Tool output is held several times:** the stream parser deep-clones every user line's content
   and Claude's `tool_use_result` (the whole file again for a Read); the mapper keeps every tool's
   input (a Write's whole file) for the process's life (`stream.rs:329`, `map.rs:105`). Subagent
   results are never trimmed (`history.rs:302`). Take instead of clone; keep only what edit tools
   need; trim older turns' results as history already does.
4. An open file's saved copy is a second full `String` and every keystroke copies the whole file
   to compare (`doc_panel.rs:247`): keep the saved version as a rope. Agent edits replace the
   whole buffer (re-wrap, re-highlight, lost scroll and undo): apply only the changed span.

### 5. Editor and resizing

1. **Soft-wrapped files are re-measured line by line on every resize frame**, each line shaped up
   to ~7 times with a fresh text system (`text_wrapper.rs:365`): shape once, break from glyph
   positions, debounce during a drag.
2. The terminal reflows its whole scrollback and signals the program on every frame of a resize
   (`terminal_view.rs:966`): resize when the drag settles.
3. The markdown preview and the diff view redo whole-file work on every scroll event
   (`doc_panel.rs:472,543`): cache by document revision.
4. GPU odds and ends: the rounded corners rebuild a path every frame (`dock_skin.rs:84`), tool
   headers draw a blurred inset shadow, and 5-6 full-size backgrounds are stacked under each
   panel. Small each; check visually.

## Found while measuring

Parts 0 and 1 are done (0d2566b). With nothing drawn at all, the idle app still wakes about 128
times a second: GPUI's display link (`gpui-pre-macos` `window.rs:834`) fires on every refresh of
the screen while the window is at least partly visible, and finds nothing to draw. It stops only
when the window is fully covered or minimised. Pausing it after a few idle frames and starting it
again on the next change would take idle wakeups to near zero, but it means keeping our own copy
of GPUI's macOS layer (as `vendor/gpui-base` is for the components). Worth it for battery; a
separate decision.

## What is already right

Chat is a virtual list; terminal output is batched into one redraw per frame; the editor lays out
and highlights only visible lines and parses incrementally; layout saving is debounced and off
the UI thread; animations (scroll, fades) stop by themselves; closed sessions release everything.

## Risks to watch

- Cached views only redraw when told: anything that changes without an event goes stale. Each
  change gets a test that the view updates.
- Shared per-folder indexes change who owns the file tree; sessions over SSH and in worktrees are
  separate folders and stay separate.
- Changing `Item` to `Arc<Item>` touches much of `keleuo-core`; done in one step, by the tests.

## Order of work, as pull requests

1. Measuring (part 0) and the frame fixes (part 1): small, mostly local, the largest idle win.
2. Cheap frames (part 2), the chat first, then the session list and the terminal.
3. Background work (part 3): the shared index and the follower first.
4. Memory (part 4) and the editor (part 5).

Each is measured against the scene in part 0, before and after.
