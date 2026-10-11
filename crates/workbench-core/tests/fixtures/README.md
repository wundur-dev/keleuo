# Recorded streams

Real `claude -p --output-format stream-json --input-format stream-json --verbose
--include-partial-messages` output, recorded in a throwaway folder with the haiku model.

- `edit_turn.jsonl`: Read then Edit of `greet.py`, allowed by a settings rule.
- `permission_bash_turn.jsonl`: a Bash call that needs approval, run with
  `--permission-prompt-tool stdio`, so it arrives as a `control_request` (`can_use_tool`).
- `write_turn.jsonl`: a Write that creates `notes.md`, also approved through a prompt.

Cleaned before checking in: the folder path is `/tmp/wb-proj`; thinking signatures,
partial tool input deltas, hook and rate limit events, and the long lists in `init`
(skills, plugins, commands) are removed.
