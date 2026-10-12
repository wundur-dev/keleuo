# Proposal: scripting in Keleuo

Status: proposal for the owner to read, nothing built. Written 2026-10-12.

## The idea in one paragraph

Let Keleuo watch what happens in a session (what Claude says and does, what scrolls past in a
terminal, which files change) and react to it: notify you, highlight a line, run a command, send
something to Claude, or answer a permission prompt by your own rules. It's a trigger idea from another of our apps
moved to coding: "when you see X, do Y", across every session you have open.

## What it is for (examples)

These are the cases the design must handle well. If a feature doesn't serve one of them, it waits.

1. **Tell me when it matters.** Notify (and badge the session) when Claude asks for permission,
   when a long turn ends, when a subagent fails, or when context passes 80%.
2. **Close the loop on tests.** After Claude edits Rust files and the turn ends, run `cargo test`
   in a terminal; if it fails, show the failures and offer "Send to Claude" (or send them
   automatically, if you allow that script to).
3. **Permission policy.** Allow `cargo test`, `cargo check` and `git status` without asking; always
   ask for `git push`, `rm` and anything touching `.env`; deny writes outside the project folder.
4. **Terminal triggers.** In a dev-server terminal, "compiled successfully" turns the tab green;
   "error[E" lines become clickable and get a "Send to Claude" button; a stack trace path opens
   the file at that line.
5. **Watch the build.** When a long command in a terminal ends, notify with its exit status and
   duration, as long as you're looking at another session.

## Two layers: rules first, code when needed

Most of the examples above don't need a programming language. So there are two layers.

**Layer 1, rules (no code).** A small, readable file per project or for the user. Each rule is
"when this event, if this matches, do these actions". You can edit it in a form in the UI or as
text.

```toml
[[rule]]
name = "Notify on permission"
on = "permission"
do = ["notify", "badge"]

[[rule]]
name = "Rust errors are links"
on = "terminal.line"
match = 'error\[E\d+\]'
do = ["highlight:red", "offer:send-to-claude"]

[[rule]]
name = "Safe commands"
on = "permission"
when = { tool = "Bash", command = '^(cargo (test|check|clippy)|git (status|diff|log))\b' }
do = ["allow"]
```

**Layer 2, scripts (JavaScript).** For anything with state or logic: "run the tests after edits,
but only once per turn, and only if the edits touched `src/`". Scripts subscribe to the same
events and call the same actions, plus a few more.

```js
let editedRust = false;

on("tool.result", (e) => {
  if (e.fileChanged?.endsWith(".rs")) editedRust = true;
});

on("turn.end", async (e) => {
  if (!editedRust) return;
  editedRust = false;
  const run = await terminal.run("cargo test", { title: "tests" });
  if (run.exitCode !== 0) {
    notify(`Tests failed after Claude's edit (${run.failures.length})`);
    ui.offer("Send failures to Claude", () => session.send(run.tail(80)));
  }
});
```

**Why JavaScript and not Lua.** Another of our apps already runs both (Lua through mlua, JavaScript through
QuickJS), so either is cheap to embed. For Keleuo's users, developers who work with Claude
Code, JavaScript is the language almost all of them already know, editors already understand
it, and a TypeScript types file gives autocomplete in Keleuo's own editor. QuickJS is small
(about 1 MB), starts in microseconds, and can be sandboxed tightly. Lua stays a possible
second language later only if people ask for it.

## Events

Everything builds on the neutral event model the agent backends already produce (so it works the
same for any future agent, not only Claude Code), plus terminal and file events:

| Event | When | Useful fields |
|---|---|---|
| `session.start` / `session.end` | a session opens or its agent exits | folder, branch, model |
| `message` | the agent writes text | text, from (agent or subagent) |
| `tool.call` / `tool.result` | a tool starts or finishes | tool, summary, input, output, error, file changed, diff |
| `permission` | the agent asks to use a tool | tool, command or path, description |
| `turn.end` | a turn finishes | duration, tokens, context %, cost if billed per use |
| `subagent.start` / `subagent.end` | a subagent runs | description, kind, failed |
| `terminal.line` | a line appears in a terminal (colours stripped) | terminal, text, captures |
| `terminal.exit` | a command or the shell ends | terminal, exit code, duration |
| `file.change` | a project file changes on disk | path, by (agent, person, other) |
| `timer` | a script's own timer fires | |

Scripts never see your typing in a terminal before you press Enter, and never see text marked
private.

## Actions

| Action | Rules | Scripts | Needs your OK when installing |
|---|---|---|---|
| notify, badge a session, highlight a line, make text a link | yes | yes | no |
| open a file at a line, show a panel | yes | yes | no |
| offer a button ("Send to Claude") that you click | yes | yes | no |
| run a command in a terminal (visible, in its own tab) | yes | yes | yes |
| send a message to the agent | no | yes | yes |
| allow or deny a permission | yes | yes | yes |
| run a hidden shell command and read its output | no | yes | yes, and shown in the log |
| keep a small store of values between runs | no | yes | no |

Two actions are not offered at all: reading credentials or `.env` files, and making network
requests. A script that needs the network can run a visible terminal command; that keeps
everything it does in front of you.

## Safety: the part that matters most

Scripts can type into Claude and approve its tool use. That makes them powerful and dangerous.
Six rules:

1. **Declared powers.** Each script lists what it may do (`terminal`, `send`, `permissions`,
   `shell`). On first enable, Keleuo shows that list in plain words, as browser extensions do.
   Changing the list asks again.
2. **Project scripts never run on their own.** A repo can ship `.keleuo/` rules and scripts, but
   cloning someone's repo must not run their code. They stay off until you trust them for that
   folder, and they are re-checked when they change (a hash, like Claude Code's own trust
   prompt).
3. **Permission rules can only narrow, never widen silently.** A rule that auto-allows shows its
   pattern in the session ("allowed by rule: Safe commands"). Commands touching `.env`, `git push`,
   `rm -rf` and anything outside the project folder are never auto-allowed, whatever a rule says.
4. **Loop guard.** A script that sends to Claude can't do it again until Claude's turn has ended,
   and no more than a set number of times per hour (default 10). Exceeding it pauses the script
   and tells you.
5. **Time and memory limits.** Each handler gets a short time budget (for example 50 ms of work
   per event) and a memory cap. Runaway scripts are stopped and switched off, with the reason.
6. **Everything is logged.** A Scripts log shows each rule or script that fired, what it matched
   and what it did, so automation never acts invisibly.

## How it fits Claude Code's own hooks

Claude Code already has hooks: shell commands it runs on its own events. Keleuo already uses them
internally (for permissions and status), and they stay the right tool for things that must
happen inside Claude Code no matter which app runs it. Keleuo scripts are different: they see
terminals and files as well as the agent, they span several sessions, they can touch the UI, and
they work with any agent backend. The Scripts panel could also list a project's Claude Code
hooks read-only, so all automation is visible in one place.

## Where things live

- **Your own:** `~/Library/Application Support/keleuo/scripts/` (rules file plus `.js` files),
  available in every session.
- **Per project:** `.keleuo/rules.toml` and `.keleuo/scripts/` in the repo, so a team can share
  them, under the trust rule above.
- **Over SSH:** scripts run in the app on your Mac; the events arrive over the connection, and
  terminal commands run on the remote machine through the same `Host` interface as everything
  else.

## Performance

- Terminal lines are checked against all rules at once with one fast multi-pattern search
  (the same technique another of our apps uses for triggers), so hundreds of rules cost little during a flood.
- Scripts run on their own thread, never the UI thread; events are queued, and if a script falls
  behind, terminal lines are batched rather than blocking the terminal.
- A session with no rules or scripts pays nothing.

## The UI

- A **Scripts** panel (a dock panel like Files and Skills): your rules and scripts with an on/off
  switch each, last fired time, and the trust state for project ones.
- The **rule editor**: a form (event, pattern, actions) with a text view of the same rule.
- The **script editor**: Keleuo's editor with JavaScript highlighting and autocomplete from the
  types file.
- A **test bench**: replay a recorded session (its events and terminal output) through a rule or
  script and see what would fire, without touching Claude or running anything.
- The **Scripts log** described above.

## Phases

1. **Rules with safe actions:** events, terminal triggers, notify, badge, highlight, links,
   "offer" buttons, the Scripts panel and log. Delivers examples 1, 4 and 5. Low risk.
2. **Permission rules:** allow and deny by pattern, with the hard limits in rule 3. Delivers
   example 3.
3. **JavaScript scripts:** the runtime, the types file and editor support, declared powers,
   terminal commands, the store, the test bench. Delivers example 2.
4. **Project scripts and sharing:** `.keleuo/` in repos, the trust flow, maybe a small gallery
   of example packs.

Each phase is usable on its own, and you can stop after any of them.

## Questions for you

1. Is JavaScript the right second layer for you, or would you rather Lua, to match our other app?
2. Should auto-sending to Claude exist at all, or only as a button you click? (It is the most
   useful and the most dangerous action.)
3. Do you want permission rules early (phase 2), or after scripts have proved themselves?
4. Should project scripts be part of the plan, given the trust risk, or keep scripts personal
   for now?
