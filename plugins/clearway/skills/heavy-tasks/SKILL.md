---
name: heavy-tasks
description: Use before running any heavy command on this machine — a build, a full test suite, a render, a docker build, a local AI model (Ollama, image or speech generation), a large install or anything that needs several GB of RAM, many cores or the GPU. Asks Clearway for a slot, waits for the turn, runs the task, then releases the slot.
---

# Heavy tasks go through Clearway

Several AI sessions share this machine. Clearway keeps them from running heavy tasks at the same time and
freezing it. A task is heavy when it may need more than about 1 GB of RAM, more than two cores for over a
minute, or the GPU.

## Steps

1. **Ask for a slot** with the `request_clearance` tool of the `clearway` MCP server
   (`mcp__plugin_clearway_clearway__request_clearance` in Claude Code). Give:
   - `description`: what the task is, in a few words (shown to the user in the queue);
   - `ram` at peak (`"6G"`), `cpu` cores, `duration` (`"20m"`, the lease) — estimate them honestly; when
     unsure, leave them out and the defaults apply;
   - `side`: `wsl` or `windows`, where the command runs;
   - `dir`: the absolute folder the command runs in, when it is not the session's folder (a git worktree, for
     instance), so that the queue shows the right branch.

   The call waits until it is your turn and may take minutes. Do nothing else heavy meanwhile.
2. **Read the result.**
   - `cleared: true` — run the heavy command now.
   - `state: "waiting"` (the call reached its time limit) — call `await_clearance` with the ticket number. The
     ticket kept its place. Repeat until cleared.
   - A refusal is not an error: it says why (for example more RAM than the machine can ever give). Lower the
     request or split the task, then ask again.
3. **Run the task** in the shell, as usual.
4. **Release the slot** with `release(ticket, code)`, passing the exit code — also when the task failed. If the
   task takes longer than declared, call `extend(ticket, duration, reason)` before the lease ends.
5. If you abandon the task, call `cancel(ticket)`.

`status` shows the queue, the free resources and why each ticket waits. Report its explanation to the user
when they ask why the task has not started.

## Without the MCP server

In Codex, always use the tools: its sandbox cannot start Windows programs, so `clearway run` and
`clearway status` cannot reach Clearway from there.

If the `clearway` tools are not available, wrap the command instead:

```
clearway run --ram 6G --cpu 4 --duration 20m "<what>" -- <command…>
```

`clearway run` waits for the slot, runs the command and releases the slot. Exit code 75 means the wait timed
out: run the same command again, it keeps the place. Exit code 3 is a refusal: lower the request. If neither the tools nor the `clearway` command exist, Clearway is not
installed: run the task normally and tell the user.

## Waiting without blocking the session (Claude Code)

To keep working while the ticket waits, launch the `clearway:clearance-waiter` agent in the background with
the ticket number. It answers `CLEARED #<n>` or the reason of the failure; run the heavy task only after that.
