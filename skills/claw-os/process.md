# Process Sessions

Spawn background processes tracked by session ID. Output is buffered and queryable:

```bash
cos proc spawn --session build-1 -- npm run build
cos proc status build-1
cos proc output build-1 --tail 50
cos proc output build-1 --stream stderr
cos proc kill build-1
cos proc list
```

## Process Groups and Hierarchy

Organize related processes into groups. Child processes inherit parent context:

```bash
cos proc spawn --group research --session search-1 -- search.py "topic A"
cos proc spawn --group research --session search-2 -- search.py "topic B"
cos proc spawn --parent lead --session sub-1 -- worker.py
cos proc list --group research
cos proc kill --group research
```

## Wait, Signal, and Result

Wait for processes to finish, send signals, and get one-call result summaries:

```bash
cos proc wait build-1 --timeout 300
cos proc wait --group research
cos proc signal build-1 TERM
cos proc result build-1
```

`cos proc result` returns a comprehensive summary: status, duration, output tails, output sizes, and a `likely_success` heuristic — everything an agent needs in one call.

## Output Streaming

Read output incrementally without re-reading old content:

```bash
cos proc output build-1 --follow
cos proc output build-1 --since-offset 4096
```

## Isolated Workspaces

Give each process its own private workspace directory:

```bash
cos proc spawn --workspace isolated --session task-1 -- agent.py
```
