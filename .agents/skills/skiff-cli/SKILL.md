---
name: skiff-cli
description: Manage Skiff sessions and coordinate agent tasks with the skiff CLI. Use to list or report sessions, start agents, arrange panes, send tasks, monitor work, collect results, or stop requested sessions. Works from Skiff panes and external shells such as Termius.
---

# Skiff CLI

`skiff` talks to `skiffd`, the daemon that owns every Skiff terminal. The app and the CLI see the same sessions and groups. A change from one shows in the other at once. `skiff COMMAND --help` gives the full flags and exit codes.

## Boot sequence

Do these steps once per task, before the first other command.

1. Run `skiff status`. If the command is not found, stop. Tell the user to run Help › Install skiff command… in Skiff. If skiffd does not run, stop and tell the user to open Skiff. If it prints a warning, tell the user.
2. Check `$SKIFF_SESSION`. If it is set, you run in a Skiff pane. Then `.` is your own session, and `--group .` is the group of your pane. If it is not set, use ids. Do not use `.`.
3. Run `skiff ls --json` and `skiff group ls --json`. Read the JSON, not the tables. `"coordinator": true` marks the Skiff agent's session.

## Names

- A session is a full id, a unique id prefix, or the name the sidebar shows. A group is an id, a prefix or a name.
- An argument that matches more than one session is an error that lists them. Pick the id and run again.
- `skiff new --agent` with no `--name` gives the agent a random call sign, such as `Corsair Cutlass`. `skiff new --type T` names it after the type: `review`, then `review 2`. A shell has no name. The sidebar shows its title or program.
- Names change and can repeat. Use ids in scripts.

## Session report

For an overview of all sessions, run the bundled script from this skill's folder:

```sh
python3 <skill folder>/scripts/session_report.py --width 100
```

- It needs Python 3 and Rich. It changes nothing. It does not mark output as seen.
- It draws one table for each working directory, with names, states, unread output and titles.
- Always give `--width`. With no terminal on stdout, as from an agent's shell, it falls back to 38 columns.
- For a phone or a mobile chat, use `--mobile`. Do not paste a wide table into a mobile chat.
- `--details` adds paths, ids, programs, groups and empty groups.

Use the report to show the user what is open. Use the JSON to pick a target or make a decision.

## Commands

| Task | Command |
|---|---|
| List sessions, groups, projects | `skiff ls --json`, `skiff g ls --json`, `skiff projects --json` |
| Start a shell | `skiff new [--name N] [--cwd DIR]` |
| Start an agent | `skiff new --agent ID [--name N] [--cwd DIR]` (`skiff agents --json` lists ids) |
| Start a task type | `skiff new --type T [--model M] [--cwd DIR]` |
| Start a command | `skiff new -- npm run dev` |
| Start next to me | `skiff new --agent codex --group . --split right` |
| Type into an idle agent | `skiff send S "text" --enter` |
| Type a command into an idle shell | `skiff send S "make test" --enter --shell` |
| Send a long prompt | `skiff send S - --enter < prompt.md` |
| Read the screen | `skiff peek S [-n 40]` |
| Wait for a stop | `skiff wait S --for idle` |
| Wait for the first of several | `skiff wait S1 S2 --any --for idle` |
| Follow state changes | `skiff events --json [S...] [--timeout SECS]` |
| Rename, clear the name | `skiff rename S "name"`, `skiff rename S ""` |
| Mark seen | `skiff seen S` |
| Stop | `skiff kill S...` |
| Groups | `g new [--name N] [--cwd DIR] [--dir row\|col] [S...]`, `g add G S [--split right\|down\|left\|up] [--of S]`, `g rm G S`, `g rename G N`, `g delete G...` |
| Themes | `skiff theme ls`, `theme session\|project\|app set ... ID`, `clear` |
| Open with | `skiff open-with ls`, `set KEY "CMD"`, `clear KEY`. Keys: `diff`, `text`, `markdown`, `html` |

- `skiff new` prints the new id. `--json` prints the full session.
- `--cwd` is relative to your current folder. With `--group` and no `--cwd`, the session starts in the group's folder.
- `--split` needs `--group`. A full group refuses before anything starts.
- A group holds 4 panes at most. A session is in one group at most. `g add` takes it out of its old group.

States: `working`, `waiting`, `idle`, `done`.

- `waiting` is an approval prompt or a bell. Only a person answers it.
- `idle` means no output for a while. It does not prove that the agent is done.
- An agent that exits leaves a shell in its pane. `done` means the shell exited too.

## Task types

`skiff agents --json` prints `{"agents": [...], "tasks": [...]}`. Each task row has `type`, `agent`, `model` (null: the agent's default), `command` and `error`. The person sets the table in Settings › Skiff agent › Task types.

1. Pick the type yourself. Skiff reads no task text.
2. Do not use a row whose `error` is not null.
3. If a task fits a type, start it with `skiff new --type T --cwd DIR`.
4. If the person names a model, add `--model M`. A custom agent (`"custom": true`) takes its model in its command and refuses `--model`.
5. If no type fits, start the project's agent with `--agent`.

`--type` with no row exits 1 and starts nothing. `--type` with `--agent` or `-- CMD` is an error.

## Send a task

1. Find the pane in the JSON. Use its id. Confirm its working directory.
2. Run `skiff peek S -n 40`. Check the current result and the input box. If the task already runs or is complete, report that. Do not send it again.
3. Write a clear task: the result, the scope, the context, the permitted actions, and what to report. An investigation request permits an investigation only. Pass on a permission to fix or deploy only when the user gave it.
4. For a long prompt, write a file and send it with `skiff send S - --enter < file`. Keep shell syntax and user text apart.

`send` types the text as a paste. Newlines stay in the text. `--enter` presses Enter once, after the text.

Skiff has no message queue. A refused send types nothing. Do not wait and retry. Do not interrupt the pane to make room. Report that the task was not delivered.

### Send exit codes

`send` checks the session before it types. The checks run in this order:

| Exit | Condition | What to do |
|---|---|---|
| 7 | The session is your own (`$SKIFF_SESSION`), by any name | Send to another session. |
| 3 | The session is `waiting` | Tell the user which pane needs them. End the turn. |
| 4 | The session is `working` | Do not wait and do not retry. |
| 6 | The session is `done` | Its program exited. Start a new session. |
| 5 | A shell or an unknown program is in front | Add `--shell` only to type a shell command. A `skiff new -- CMD` session or a custom agent shows its launch shell, so it needs `--shell`. |
| 9 | A known agent shows a draft or its busy line, or the session took input after the check | For a draft, tell the user which pane holds it. For the busy line, do as for exit 4. |
| 0 | Sent | |
| 1 | Error, for example no match or more than one | |

`--shell` relaxes only exit 5. After the text goes in, one more check applies:

| Exit | Condition | What to do |
|---|---|---|
| 8 | `--enter` went to a known agent (Claude Code, Codex, Gemini, Grok, opencode), but no turn started in 5 seconds | Run `skiff peek S`. If the text is in the input box, tell the user which pane, with its name and id. |

- `send` never presses Enter a second time. A second send while the text is in the box gets exit 9.
- There is no exit 8 without `--enter`, with `--raw`, or when the program in front is not a known agent.
- A turn that starts and ends in under 5 seconds with no output can also give exit 8.

## Monitor and collect the result

Keep monitoring after a send when the user wants the result. A request only to deliver a message ends when the send exits 0.

- Run one `skiff wait S --for idle` at a time. Finish an open shell call before you start another wait.
- In a Skiff pane, or with no terminal on stdout, `wait` stops after 45 seconds. A `--timeout` above 60 becomes 60. On a timeout, read the screen and wait again while the task runs.
- For several children, use `skiff wait S1 S2 S3 --any --for idle`. It prints the id, name and state of the first one. Read that screen, then wait on the rest.
- To keep your turn free, run `skiff events --json [S...]` as a background job. It has no time limit. In the foreground, give `--timeout`. With no names, it follows every session, new ones too.
- After each `wait` exit 0 and each event, run `peek`. `idle` means quiet, not done.
- Collect the result, the checks and any blocker. Do not report a task as done from the state alone.
- For a deployment, get the agent's deployment result before you call it live. Keep local checks apart from production checks.
- A child leaves its result on screen. For a long result, give the child a file path.

`wait` exit codes:

| Exit | Meaning |
|---|---|
| 0 | Every session reached the state. With `--any`: one did. |
| 1 | Timeout. |
| 2 | A session closed or exited first. With `--any`: every session did. |
| 3 | A session is `waiting`. With `--for idle`, `done` or `any-not-working`, this stops the wait at once. |

With `--for idle`, a known agent that shows its busy line ("esc to interrupt") is not idle yet.

`events --json` lines:

```
{"event":"state","session":ID,"name":N,"state":"idle"}
{"event":"exit","session":ID,"name":N,"code":0}
{"event":"removed","session":ID,"name":N}
```

`events` exits 0 when `--timeout` ends or every named session is gone, 1 on an error, and 2 when a name matches no session.

For several agents, give each a different task and folder. If they share a repository, say which files each one can change. Collect each result before work that depends on it. Start agents only in the requested scope. A named existing pane can do a single task.

## The Skiff agent and its ledger

The Skiff agent (default name Skipper) is the normal coordinator. It is one agent session in its own folder, `~/skiff-agent` by default. Its `AGENTS.md` is the user's standing permission to send tasks to sessions in Skiff projects. A person opens it from its row in the app, or with `skiff skipper` in any terminal. `skiff skipper --no-attach` prints its id.

The ledger is `ledger.db` in that folder. Use only `skiff ledger` to read or change it. Never open or edit the file.

- Only the session that owns the ledger changes rows: `add`, `send`, `update`, `report`, `close`, `reopen`, `prune`, `rm`. `skiff skipper` records the owner. Another agent pane gets a refusal that names the owner. A shell outside Skiff is refused too, unless a person passes `--person`.
- `ls` and `show` work from anywhere.
- Each row has an id and an 8-character uid. Neither is used again. Every ID argument takes both. Use the uid when you name a task.

Task states are `pending`, `sent`, `blocked` and `done`. `ls` and `show` also print:

- `interrupted`: the pane restarted after the send. A restore runs a new shell under the old id, so the task does not run.
- `gone`: the pane no longer exists.
- `elsewhere` in LIVE: the task went out on another daemon. Its socket shows. It is not gone. Leave it to that daemon's Skiff agent.

A plan shows `done` when every task in it is done, else `sent`.

Work is a plan with its tasks, or a task with no plan. It is `working` while an agent is on one of its tasks (the task is `sent`, and its pane works or ended its turn), `open` otherwise, and `finished` once closed. Only `ledger close` closes work. Done is not closed.

### Ledger commands

| Command | Use |
|---|---|
| `ledger add --pane S --task "..." --title "..." --send --enter [--parent UID] [--type T] [--ref TEXT]` | Add a task and send it. |
| `ledger add --plan --task "..." --title "..." [--ref TEXT]` | Add a plan for work with more than one task. |
| `ledger send UID --enter` | Send a `pending` or `interrupted` task. |
| `ledger update UID [--state S] [--pane S] [--title T] [--result PATH] [--parent UID] ...` | Change only the given fields. |
| `ledger ls [--state S] [--all \| --closed \| --unreported] [--json]` | Show the work that is not closed: working, then open. |
| `ledger show UID [--json]` | Show one row. |
| `ledger report UID... \| --all` | Mark done tasks as told to the person. |
| `ledger close UID...` | Close work on the person's word. |
| `ledger reopen UID...` | Open closed work again. |
| `ledger prune [--older-than 7d] [--yes]` | Remove closed work. Without `--yes` it only prints it. |
| `ledger rm UID` | Remove one row now. |

- `add` prints the id, the uid and the result path. It makes no file. A task needs `--pane`. A plan has none.
- `--send` and `ledger send` add " Write your result to PATH." to the task text. Do not put the path in `--task`.
- Both run every `send` check and exit with its code. Exit 0 sets `sent`. Exit 8 sets `blocked`. A refusal leaves the task `pending`.
- `ledger send` exits 1 and types nothing for a task that is `sent`, `blocked` or `done`, for a plan, for a gone pane, or for a task on another daemon.
- Give every task and plan a `--title` of 3 to 6 words, such as `--title "Fix found-set count"`. Without one, the Ledger panel uses the first sentence of the task.
- `--type T` records the task type. `--ref TEXT` stores a URL or a ticket key. Skiff stores both and does not act on them.
- `--parent UID` joins a task to its work: a plan or any task in it, or a task with no plan. A task with no plan goes into a new plan with its title, project, ref and type, and `add` prints the plan's uid. A plan cannot be under a plan. A task added or moved under closed work opens it again.
- `ls` groups rows by work in its WORK column: `working`, then `open`. `--closed` shows closed work, `--all` every row. `--json` adds `bucket` and `closed_at`.
- `ls --state` takes `sent`, `pending`, `blocked`, `done`, `interrupted` or `gone`. `ls --unreported` lists the done tasks to report.
- Run `ledger report` in the same turn as you tell the person. A plan's uid reports its done tasks. A task that is not done is refused. A state change away from `done` clears the mark and opens the work again.
- `ledger close` takes a plan, or a task with no plan. It refuses work with a task that is not done, and a task in a plan. It reports the done tasks of the work.
- `prune` removes only closed work, with its tasks. `rm` and `prune` keep the result files.

Result paths, in `results/` in the agent folder:

- A task with no plan: `results/<date>-<uid>-<slug>.md`.
- A plan: the folder `results/<date>-<uid>-<slug>/`, with its notes in `plan.md`.
- A task in a plan: `<uid>-<slug>.md` in the plan's folder.

### The watcher

`skiff watch` runs with the Skiff agent's session. It types one `[watch]` line into the agent when a ledger pane ends a turn, exits, restarts or is gone, or is idle with a pending task. It types only while the agent is idle and its input box is empty. A `waiting` pane gives the person a desktop notification and gives the agent nothing.

1. Read each pane that a `[watch]` line names with `skiff peek`.
2. Then run `skiff seen S`. An unread pane is named again on the next run.
3. A gone pane cannot be marked seen. Reassign each open task with `skiff ledger update UID --pane S --state pending`, or set it to `done`.
4. On `[watch] N done tasks not reported`, run `skiff ledger ls --unreported` and report them.

### The Ledger panel

The app shows the ledger in a panel at the right of the Skiff agent's pane. The panel is read-only. It shows three sections: Working, Open and Finished. Open lists the rows that need the person first, then done tasks, then the rest. Finished shows the last 20 closed work items. A row that needs the person is amber. `ledger close` moves work to Finished. The panel shows `--title`.

## Recipe

Hand a task to a new agent and collect the result:

```sh
id=$(skiff new --agent claude --name reviewer --cwd /path/to/repo)
skiff wait "$id" --for idle                # let it boot
skiff peek "$id" -n 40                     # check the agent and its input box
skiff send "$id" "Review the diff on this branch. List bugs only." --enter
skiff wait "$id" --for idle                # on exit 1, peek and wait again
skiff peek "$id" -n 80
```

## Rules

- Never signal skiffd (`kill`, `pkill`, `killall`). skiffd ignores SIGTERM and SIGINT from its own panes and logs the sender.
- Run `skiff daemon stop` or `skiff daemon reload` only when the user asks. `stop` ends every session.
- Do not `kill` a session, or take it out of its group, unless the user named it or you started it in this task.
- Do not `send` to a session you did not start unless the user asked. A request in this conversation counts. A standing permission in the instructions file of your own folder also counts, such as the Skiff agent's `AGENTS.md`.
- A standing permission covers `send` only. It does not cover `kill`, removal from a group, approvals or theme changes. Every `send` and `wait` refusal still applies.
- Only a person answers an approval. On exit 3 from `send` or `wait`, tell the user the pane's name and id. End the turn. Do not answer the prompt. Do not retry until the user says so.
- Never wait for the session that started you. A send to a busy parent gets exit 4. Then leave the result on screen or in a file and end your turn.
- If a child's report names another pane that needs a person, pass the report to the user and stop.
- Change a project or app theme, or an Open with command, only when the user asks. They change `projects.toml` and apply to every pane.
