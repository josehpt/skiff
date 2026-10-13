---
name: skiff-cli
description: Manage Skiff sessions and coordinate agent tasks with the skiff CLI. Use to list or report sessions, start agents, arrange panes, send tasks, monitor work, collect results, or stop requested sessions. Works from Skiff panes and external shells such as Termius.
---

# Skiff CLI

`skiff` talks to `skiffd`, the daemon that owns every Skiff terminal. The app and the CLI see the same sessions and groups. A change from one shows in the other at once.

Full reference: https://github.com/solutionscay/skiff/wiki/CLI

## Boot sequence

Do these steps before the first command in a task.

1. Run `skiff status`. If the command is not found, stop. Tell the user to run Help › Install skiff command… in Skiff. The app ships the CLI and keeps it up to date. If it says skiffd is not running, stop and tell the user to open Skiff.
2. Check `$SKIFF_SESSION`. If it is set, you run inside a Skiff pane, and `.` means your own session. `--group .` means the group your pane is in.
3. Run `skiff ls --json` and `skiff group ls --json` to see what exists. Read JSON, not the tables.

Run this sequence once per task. Refresh the session list before a later send if its state may have changed.
From an external shell, use session and group IDs. Do not use `.` or `--group .` when `$SKIFF_SESSION` is unset.

## Session report

For a terminal report of all sessions, run the bundled tool:

```sh
python3 ~/.agents/skills/skiff-cli/scripts/session_report.py
```

If that installation path is absent, use `scripts/session_report.py` relative to this
skill's directory. The script reads session and group JSON from the local daemon.
It uses Python 3 and Rich to draw a table for each project, with session names,
states, unread output and titles. Rich must be available to that Python interpreter.
Use `--details` to include full paths, IDs, programs and groups, including empty groups.
Output wraps to the terminal width. Use `--width 80` for a fixed layout.
For a phone or mobile chat, use `--mobile`. This uses a single-column table at
most 38 characters wide. Do not paste the desktop table into a mobile chat.
If terminal width is unavailable, the report defaults to 38 columns.
The script does not send messages or mark output as seen.

Use the report when the user asks what is open or wants a session overview.
Use the JSON commands for selecting a target or making a state decision.

## Coordinate agent tasks

Keep task handoffs and result collection in this skill. No separate orchestration
skill is needed. The calling agent can run in Termius or any shell that can reach
the same `skiffd`.

The Skiff agent is the normal coordinator. It is one agent session in its own folder,
default `~/skiff-agent`, set up with File › Set up Skiff agent… in Skiff. Its
`AGENTS.md` is the user's standing authorization to send tasks to sessions in Skiff
projects. Its `ledger.md` lists the open and pending tasks, with pane ids and result
paths. Read the ledger before you send a task or report on pending work, and keep it
current.

### Send a task

1. Find the requested pane in the JSON list. Use its ID and confirm its working directory.
2. Read `skiff peek S -n 40` before sending. Check the current result and input box.
   If the requested task is already running or complete, report that state instead of sending it again.
   If the input box holds a draft, do not append to it or submit it. Report the draft
   and pane to the user so they can clear or send it.
3. Send a clear task: the requested result, scope, relevant context, authorized actions,
   and what the agent must report. An investigation request authorizes an investigation.
   Pass on existing permission to fix or deploy when the user has given it.
4. For long prompts, write a temporary file and use `skiff send S - --enter < prompt.md`.
   Keep shell syntax and user text separate.

Skiff has no message queue in this workflow. A refused send is not queued.
If the pane is working, report that the task was not delivered. Do not wait for it
and retry the send. Do not interrupt the pane to make room for the message.

### Monitor and collect the result

When the user asks for the result or says to report when the agent is done, keep
monitoring after a successful send. A request only to deliver a message can end
after delivery is confirmed.

- Use one `skiff wait S --for idle --timeout 45` at a time. Keep each wait below
  the shell tool's time limit and at most 60 seconds. Resume an outstanding shell
  call before starting another wait. Give a short progress update when needed.
- For several children, use `skiff wait S1 S2 S3 --any --for idle --timeout 45`.
  It returns when the first one stops and prints that session's id. Read that
  screen, then wait again on the rest.
- To keep your own turn free for the user, run `skiff events --json [S...]` as a
  background job. It prints one line for each state change, exit or removal, and
  no screen text. After any line, read that screen with `peek`.
- After a successful wait, read the screen. `idle` can occur before the agent
  starts, during a tool call, or while it thinks. If the task still runs, wait again.
- On timeout, read the screen and state. Continue when the task is still running.
  On `waiting`, report the pane and requested approval. Follow the approval rules below.
- Collect the task result, checks, and any remaining blocker. For a deployment,
  require the agent's deployment result before reporting it as live. Distinguish
  local checks from checks on production and state any material limitation.
- A child should leave its result on screen. Use a result file only when the
  report is too long for `peek`; tell the child where to write it. Do not claim
  completion from the pane state alone.

If the user asks for multiple agents, give each a distinct task and working
directory. State which files each can change when they share a repository.
Collect each result before work that depends on it. Start agents only within
the user's requested scope; a named existing pane can handle a single task.

## Names

- A session is a full id, a unique id prefix, or the name the sidebar shows. A group is an id, a prefix or a name.
- An argument that matches more than one session is an error that lists them. Pick the id from the list and run again.
- Use ids in scripts. Names can change.

## Commands

| Task | Command |
|---|---|
| List sessions | `skiff ls [--json]` |
| Start a shell | `skiff new [--name N] [--cwd DIR]` |
| Start an agent | `skiff new --agent claude [--name N]` (`skiff agents` lists ids) |
| Start a command | `skiff new -- npm run dev` |
| Start next to me | `skiff new --agent codex --group . --split right` |
| Type into an idle session | `skiff send S "text" --enter` |
| Type a command into an idle shell | `skiff send S "make test" --enter --shell` |
| Send a file or long prompt | `skiff send S - --enter < prompt.md` |
| Read the screen | `skiff peek S [-n 40]` |
| Wait for it to stop | `skiff wait S --for idle [--timeout 600]` |
| Wait for the first of several to stop | `skiff wait S1 S2 --any --for idle [--timeout 600]` |
| Follow state changes | `skiff events [--json] [S...] [--timeout N]` |
| Wait for an approval prompt | `skiff wait S --for waiting` |
| Rename | `skiff rename S "name"` |
| Mark seen | `skiff seen S` |
| Stop | `skiff kill S` |
| Themes | `skiff theme ls`, `theme session set S ID`, `theme project set NAME ID`, `theme app set ID`, and `clear` in place of `set ID` |
| Open with | `skiff open-with ls`, `open-with set KEY "CMD"`, `open-with clear KEY`. Keys: `diff`, `text`, `markdown`, `html` |
| Groups | `skiff g ls`, `g new [--name N] S1 S2 [--dir row\|col]`, `g add G S [--split right\|down\|left\|up] [--of S]`, `g rm G S`, `g rename G N`, `g delete G` |

`skiff new` prints the new session id on stdout. `--json` prints the full session.

States: `working`, `waiting`, `idle`, `done`. `waiting` means an approval prompt or a bell. Only a person answers it.

`send` checks the session before it types any bytes. It refuses in this order:

| Exit | Condition | What to do |
|---|---|---|
| 7 | The session is your own (`$SKIFF_SESSION`), by any name, id, prefix or `.` | Send to another session. |
| 3 | The session is `waiting` | Tell the user which pane needs them. End the turn. |
| 4 | The session is `working` | Do not wait for it and do not retry. See the rules. |
| 6 | The session is `done` | Its program exited. Start a new session. |
| 5 | A shell, or a program Skiff cannot identify, is in front | Add `--shell` only if you mean to type a shell command. A program started with `skiff new -- CMD` or a custom agent shows its launch shell in front, so a send to it needs `--shell`. |
| 0 | Sent | |
| 1 | Error, for example no such session | |

`--shell` relaxes only the exit 5 check. No flag overrides a waiting, working, done or own-session refusal. `--raw` and `--shell` do not either.

After the bytes go in, one more check applies:

| Exit | Condition | What to do |
|---|---|---|
| 8 | `--enter` went to a known agent (Claude Code, Codex, Gemini, Grok, opencode), but the session stayed `idle` for 5 seconds after Enter | Run `skiff peek S`. If the text is in the input box, tell the user which pane. Do not send it again. |

`send` never presses Enter a second time. It skips this check without `--enter`, with `--raw`, and when the program in front is not a known agent. A turn that starts and ends in under 5 seconds with no output can also give exit 8.

`wait` exit codes:

| Exit | Meaning |
|---|---|
| 0 | Every session reached the state. With `--any`: one session reached it. Its id, name and state print on stdout. |
| 1 | Timeout. |
| 2 | A session closed or exited before it reached the state. With `--any`: every session did. |
| 3 | A session is `waiting`. Check its pane. |

With `--for idle`, `done` or `any-not-working`, `wait` stops at once with exit 3 when any session is `waiting`, at the start or later. `--for waiting` does not change.

`events` prints one line for each state change, exit or removal. With `--json`, each line is one object: `{"event":"state","session":ID,"name":N,"state":"idle"}`, `{"event":"exit",...,"code":0}` or `{"event":"removed",...}`. It runs until Ctrl+C or `--timeout`.

| Exit | Meaning |
|---|---|
| 0 | `--timeout` ran out, or every named session was removed. |
| 1 | Error. |
| 2 | A name matches no session. |

`idle` in an event means quiet, not done. Read the screen with `peek` before you act on it.

## Recipes

Hand a task to a new agent and collect the result:

```sh
id=$(skiff new --agent claude --name reviewer --cwd /path/to/repo)
skiff wait "$id" --for idle --timeout 45     # let it boot
skiff peek "$id" -n 40                     # check the agent and input box
skiff send "$id" "Review the diff on this branch. List bugs only." --enter
skiff wait "$id" --for idle --timeout 45
skiff peek "$id" -n 80
```

If a `wait` times out and the child still works, run `wait` again. If it exits 3, stop. See the rules.

Put two agents side by side in a new group:

```sh
a=$(skiff new --agent claude) ; b=$(skiff new --agent codex)
skiff g new --name pair "$a" "$b" --dir row
```

## Rules

- Never run `skiff attach`. It takes over the terminal and needs a person at the keys.
- Do not `kill` a session, or remove a session from its group, unless the user asked for that session by name or you started it in this task.
- Do not `send` into a session you did not start unless the user asked you to. A request in the current conversation counts. A standing authorization in the instructions file of your own folder also counts, for example the Skiff agent's `AGENTS.md`. With it, you can send to any session in a Skiff project. All other checks stay: peek first, do not add to or submit a draft, do not retry, and obey every `send` and `wait` refusal.
- A standing authorization covers `send` only. It does not cover `kill`, removal from a group, approvals or theme changes.
- On exit 8 from `send`, run `skiff peek S`. If the text is in the input box, tell the user which pane holds it, with its name and id. Do not send the text again.
- Only a person answers an approval. On exit 3 from `send` or `wait`, tell the user which pane needs them, with its name and id. End the turn. Do not answer the prompt. Do not retry until the user says to continue.
- Give each `wait` a `--timeout` shorter than your shell tool's time limit.
- A child reports to its parent when it ends its turn with the result on screen or in a file. The parent waits, then reads the result with `peek`.
- Never wait for the session that started you. A send to a busy parent is refused with exit 4. Then leave the result on screen or in a file and end your turn. Do not retry. A send to an idle parent is permitted.
- If a child's report names another pane that needs a person, pass that report to the user and stop.
- Change a project or app theme, or an Open with command, only when the user asks. Those edit `projects.toml` and apply to every pane.
- A group holds 4 panes at most. A session is in one group at most. Adding it to a group takes it out of its old one.
- `idle` means no output for a while. It does not prove the agent is done. After each successful `wait`, read the screen with `peek` before you report a result.
- An agent that exits leaves a shell in its pane. `done` means the shell exited too.
