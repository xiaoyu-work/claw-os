# Sandboxed Execution

Use the `cos_sandbox` tool — **not the shell** — to run any model-generated, user-supplied, or otherwise untrusted code. It is the only operation in this skill set that requires the dedicated tool; there is no `cos sandbox` CLI command.

## When to use

- Code you wrote during this turn that you have not verified
- Code or scripts the user pasted in
- Anything downloaded over the network
- Commands you would not run on the host directly

For trusted long-running processes, use the `cos_proc` tool (`spawn`) instead — it is not isolated but it tracks state.

## Tool shape

`cos_sandbox` exposes one command: `exec`. Call it like every other cos primitive proxy — a `command` field plus an `args` array of positional flags and the command to run.

```json
{
  "command": "exec",
  "args": [
    "--mem", "512M",
    "--cpu", "50",
    "--pids", "100",
    "--timeout", "300",
    "--no-network",
    "--seccomp-profile", "minimal",
    "--", "python3", "script.py"
  ]
}
```

| Flag | Meaning |
|---|---|
| `--mem <limit>` | Memory cap (e.g. `512M`, `1G`). OOM → exit code 137. |
| `--cpu <percent>` | CPU quota, e.g. `50` for 50%. |
| `--pids <max>` | Cap on processes inside the sandbox. |
| `--timeout <secs>` | Kill after N seconds → exit code 124. |
| `--no-network` | Disable network namespace. |
| `--ro` | Remount root read-only. |
| `--workspace <dir>` | Working directory inside the sandbox. |
| `--seccomp-profile <p>` | `minimal`, `network`, or `full`. |
| `--` | Separator before the actual command. |

The result envelope is `{exit_code, stdout, stderr, isolated, network, ...}`. `isolated: false` means the host doesn't support namespaces and the call ran as a plain subprocess; do not treat such a run as safe.