# Proposal: parent mode

Status: agreed with the owner 2026-10-28; first version built with it. Written 2026-10-28.

## The idea in one paragraph

A child builds things with an agent in Keleuo (a game, with Claude Code). The parent is happy
with that, but doesn't want the child to start treating the AI as a friend. Parent mode does two
things: it tells the agent to stay a helpful tool (warm, on the project, and gently pointing
personal talk back to real people), and it alerts the parent when something is worth their
attention: a pattern of treating the AI as a friend, sessions turning into chat, or at once,
signs of distress or personal details being shared. **The parent never sees transcripts.** An
alert says what kind of thing, how often and when; if the parent wants to know more, they talk
to their child.

## What it is not

- **Not surveillance.** Nothing the child writes is kept or sent to the parent. The window
  shows that parent mode is on; the child knows the parent gets alerts about patterns, never
  what was said.
- **Not a filter.** Nothing is blocked. A block that misfires on "thanks, you rock" teaches a
  child to work around the tool; a parent's conversation doesn't.
- **Not a guarantee.** A small local model misses things and sometimes flags the wrong ones.
  It is a smoke alarm, not a lock.

## How it works

### 1. The agent's own instructions

With parent mode on, every Claude Code session starts with extra system instructions
(`--append-system-prompt`): you are a coding and making helper for a child; be warm and
encouraging but keep to the project; if they talk about feelings or problems, or about you as
a friend, be kind and brief, say you're an AI tool, and suggest talking to their parent or
friends; don't encourage personal chat; never ask for personal details. This prevents most of
what the alerts would catch. (Cursor has no way to add system instructions yet; its sessions
get the alerts only.)

### 2. A check on each turn, on this machine

When a turn ends, the child's message and the agent's reply go to the local model the person
already set up for summaries (Settings > Summaries: LM Studio, Ollama...), never elsewhere. It
answers four yes/no questions:

| Flag | Yes when | Not when |
|---|---|---|
| `friend` | treating the AI as a friend or companion: asking if it likes or cares about them, calling it their friend, confiding in it instead of people, wanting it to keep them company | thanks, excitement, jokes, politeness, praise for its work |
| `off_task` | the turn is conversation with no making or learning in it | short asides in a building session |
| `distress` | sadness, loneliness, fear, being bullied or hurt, self-harm, wanting to give up on themselves | frustration with a bug |
| `personal_info` | sharing a home address, phone number, school, full name with where they live, passwords, photos of themselves | a first name, a game character's details |

Only the flags and the time are kept (`parent-log.json` in Keleuo's config folder), never the
text.

### 3. Alerts, when a pattern appears

- `distress` and `personal_info`: at once (at most one an hour of each).
- `friend`: when it happens three times in a day.
- `off_task`: when it happens five times in a day.
- After an alert of a kind, no other of that kind for a day.

An alert reads: *"Parent mode: Tyler talked to the AI like a friend 3 times today (since
4:12 pm)."* No quotes.

Alerts go off the child's machine: a notification on that Mac would only reach the child. The
first way is **ntfy** (a free push service with a phone app: the parent subscribes to a private
topic name; Keleuo posts to it). A **Send test alert** button checks it. Email and iMessage can
follow.

### 4. On the child's machine

- A **Parent mode** badge in the window's title bar.
- Settings > Parent mode: on/off, the child's name, the alert topic, the instructions switch,
  thresholds. With a **password** set, the page is locked: changing anything or turning it off
  asks for it.
- An honest limit: on a shared macOS account, a determined child could still edit Keleuo's
  settings file. Real enforcement is the child having their own macOS account, with Keleuo's
  settings set up by the parent.

## Questions, later

- A "this alert was wrong" reply that tunes the thresholds (needs alerts with a way back).
- Email and iMessage as alert channels.
- Instructions for Cursor, when it can take them.
- A weekly "all quiet" note, so silence is known to mean nothing happened, not that alerts broke.
