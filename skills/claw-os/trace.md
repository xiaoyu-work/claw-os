# Execution Tracing

Track agent tasks as a tree of traces, spans, and operations.

## Start a Trace

```bash
cos trace start "refactor-task"
# → sets COS_TRACE_ID=refactor-task
export COS_TRACE_ID=refactor-task
```

After setting COS_TRACE_ID, all subsequent `cos` commands are automatically attached to this trace.

## Create Spans

```bash
cos trace span "analyze"
export COS_SPAN_ID=analyze

cos app fs read main.py               # attached to analyze span
cos app fs search "def old_func"       # attached to analyze span

cos trace span-end
export COS_SPAN_ID=

cos trace span "verify"
export COS_SPAN_ID=verify

cos app exec run "pytest"              # attached to verify span

cos trace span-end
cos trace end "refactor-task"
```

## View Trace Tree

```bash
cos trace show "refactor-task"
```

Returns complete tree: spans → operations, timing, errors, and summary with `first_error` pointer.

## List Traces

```bash
cos trace list
cos trace list --status active
```
