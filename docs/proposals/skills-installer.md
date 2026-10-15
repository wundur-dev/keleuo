# Proposal: installing skills, and finding new ones

Status: proposal for the owner to read, nothing built. Written 2026-10-15.

## The idea in one paragraph

A skill is a folder with a `SKILL.md` (a name and a description up top, then instructions) and
any files it needs. Claude Code, Cursor and Codex all read the same format; they differ only in
which folders they look in. Today installing one means a terminal command per agent and knowing
where each one looks. This proposal adds an installer to the Skills tab: paste a git URL (or
`owner/repo`), see the skills in it and read them, choose which agents and which scope, and
Keleuo puts each one where every chosen agent natively looks, remembering where it came from so
it can be updated or removed later. A second part adds a short, reviewed list of skills worth
trying, published on keleuo.com and shown in the Skills tab, each one a click from the installer.

## What it is for

1. **Install a skill without knowing where it goes.** One action, every agent you use.
2. **Read it before it runs.** A skill is instructions an agent follows, and some come with
   scripts it may run. Nothing is installed unseen.
3. **Keep skills current, or take them out.** Update from the source, see what changed first;
   remove every copy Keleuo made.
4. **Find good skills.** A few worth knowing about, with why, ready to try.

## What exists today

- The Skills tab lists what each agent reads (`backends/claude/skills.rs`,
  `backends/cursor/skills.rs`), including AGENTS.md and Cursor's rules since 0985a30, and creates
  a new empty skill in the project (`create_skill_in`).
- Each backend already knows its own folders; nothing installs from elsewhere, and nothing
  records where a skill came from.

## Where each agent looks

Checked against the installed tools (Claude Code 2.1.300, cursor-agent 2026.10.14, the Codex CLI
in ChatGPT.app), by reading their code. A name found twice is the first one, for each agent.

| Agent | All projects | One project |
|---|---|---|
| Claude Code | `~/.claude/skills/` (and installed plugins' skills) | `.claude/skills/` |
| Cursor | `~/.cursor/skills/`, then `~/.claude/skills/`, `~/.codex/skills/`, `~/.grok/skills/`, `~/.agents/skills/` | the same five under the project |
| Codex | `$CODEX_HOME/skills/` (`~/.codex/skills/`; its own under `.system/`) | not confirmed: likely `.agents/skills/` or `.codex/skills/` |

So one copy in `~/.claude/skills/` serves Claude Code and Cursor; Codex needs its own.

## Part 1: the installer

### How it goes

1. **Skills tab, "+" › "Install from git…".** A field takes a URL (`https://github.com/…`,
   `git@…`, any git remote) or `owner/repo` (GitHub). A subfolder or branch can follow:
   `owner/repo/path/to/skill`, `…@branch`.
2. **Keleuo fetches it** (a shallow `git clone` into a temporary folder, with the person's own git
   credentials, so private repositories work) and finds every `SKILL.md` in it. Nothing is
   installed yet.
3. **A sheet lists them:** each skill's name and description, its files, and its SKILL.md in full
   (the files it would run marked as such). Each has a checkbox.
4. **Where:** this project or all projects; which agents (those Keleuo runs, each ticked when
   its folder is not already covered: Claude Code's user folder covers Cursor too).
5. **Install.** Keleuo copies each chosen skill into each agent's folder and writes its record.
   A skill already there under the same name is not overwritten without asking (and says which
   one is there and where it came from, if Keleuo knows).
6. The Skills tab shows it at once, marked with its source; agents pick it up in their next
   session (say so).

### Copies, not links

Each agent gets a plain copy in its own folder. A link into one shared folder would save space,
but Cursor refuses linked configuration in places, and a copy is what each agent would have if it
were installed by hand: the native place, nothing for an agent to trip on. Updating writes the
copies again.

### The record

One file, `installed-skills.json` in Keleuo's settings folder:

```json
{
  "version": 1,
  "skills": [{
    "name": "humanizer",
    "source": "https://github.com/blader/humanizer",
    "path": "",
    "commit": "3f2c…",
    "installed": "2026-10-15",
    "copies": [
      {"agent": "claude-code", "dir": "~/.claude/skills/humanizer", "hash": "sha256 of the files"},
      {"agent": "codex", "dir": "~/.codex/skills/humanizer", "hash": "…"}
    ]
  }]
}
```

The hash is of what was installed, so an update can tell when someone edited a copy since (and
ask before replacing it). A project install records the project too.

### Updating and removing

- **Update** (per skill, or all): fetch the source again; if the commit moved, show the
  SKILL.md's changes and any changed files, then replace the copies. Edited copies are named
  and kept unless the person says otherwise.
- **Remove:** delete the copies the record lists, after showing them. Folders Keleuo did not
  make are never touched.
- **Skills not from Keleuo** (installed by hand, by `npx skills`, by plugins) show as they do
  today, without update or remove.

### Agents as the source of the folders

A backend says where its skills go: `skill_dirs(scope) -> Vec<PathBuf>`, the folder it reads
first for each scope, and which other agents' folders it also reads (so the installer can skip a
copy that would only be a duplicate). Claude Code and Cursor have this knowledge already in
their `skills.rs`; Codex adds its own when it is a backend. Over SSH the copies go through the
session's host (`Host`), like every other file: the clone is local, the copies land on the
remote machine, the record says which machine.

### Trust

- Nothing runs during install: copying files is all it does. The skill's scripts run only if an
  agent later decides to, under that agent's own permissions.
- The sheet shows everything that will be installed, and the source and commit.
- A skill from a repository that is not on a well-known host, or that ships executables, gets a
  plain warning line, not a block.

## Part 2: finding skills

### On keleuo.com

A page, `keleuo.com/skills`, with a few skills worth knowing about: what each does, why it is
worth a try, who made it, which agents it works with, and a "Try it in Keleuo" button. Entries
are added by hand after a read-through, a handful at a time (a log of picks, not a catalogue).

The same list as `keleuo.com/skills/feed.json`:

```json
{
  "version": 1,
  "updated": "2026-10-15",
  "skills": [{
    "id": "blader/humanizer",
    "name": "humanizer",
    "summary": "Rewrites text to drop the tells of machine writing.",
    "why": "Useful before anything you send under your own name.",
    "author": "blader",
    "source": "https://github.com/blader/humanizer",
    "path": "",
    "commit": "3f2c…",
    "sha256": "of the reviewed files",
    "agents": ["claude-code", "cursor", "codex"],
    "tags": ["writing"],
    "added": "2026-10-15"
  }]
}
```

**Pinned to what was reviewed.** An entry names the exact commit, and the hash of the files as
reviewed; the installer fetches that commit and refuses if the files differ. A recommendation is
a vouch, and a skill tells an agent what to do: an entry pointing at whatever a repository holds
today could change after it was read.

### In Keleuo

A "Discover" group at the bottom of the Skills tab: the latest few from the feed, new ones
marked, each opening the installer filled in (with the pinned commit). The feed is fetched at
most once a day, only when the Skills tab is open, and can be turned off in Settings (fetching it
tells keleuo.com an address; say so there).

"Try it in Keleuo" on the site opens Keleuo with a `keleuo://install-skill?…` link, which needs
Keleuo to be a real `.app` that registers the scheme (on the open queue already). Until then the
button copies an `owner/repo@commit` line to paste into the installer.

### Where entries come from

Picked by hand, from what is already out there: [skills.sh](https://skills.sh) (`npx skills`),
Claude Code plugin marketplaces, GitHub repositories with SKILL.md files, things people share.
A short review note per entry: what it does, what it runs, anything to watch. The daily site log
could mention new picks.

## Open questions

1. **Codex's project folder.** Confirm where Codex reads project skills before Codex is a
   backend (its docs or a test).
2. **Plugins.** Claude Code plugins bundle skills with commands and hooks and have their own
   installer (`/plugin install`). Leave plugins to Claude Code (show them, as today), and keep
   this to plain skills?
3. **Project skills in git.** A skill installed into a project lands in its repository. Leave it
   to be committed (shared with the team), or offer to add it to `.git/info/exclude`?
4. **Names that clash.** Two sources with the same skill name: install under another folder
   name? Each agent takes the first by its own order, so the name the agent sees is the
   `name:` in SKILL.md, not the folder.

## Order of work

1. The installer for Claude Code and Cursor, user and project scope, with the record, update and
   remove (most of the work: the sheet, git fetch, copies, the record).
2. The site page and the feed, with the first few reviewed entries.
3. Discover in the Skills tab.
4. Codex as a target when Codex is a backend; `keleuo://` when Keleuo is an `.app`.
