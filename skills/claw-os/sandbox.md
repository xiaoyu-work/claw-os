# Sandboxed Execution

Run untrusted code in an isolated environment with resource limits:

```bash
cos sandbox exec --mem 512M --cpu 50 --timeout 300 --no-network -- python3 script.py
cos sandbox exec --timeout 60 -- node app.js
cos sandbox exec --mem 1G --pids 100 -- bash -c "make && ./test"
```

Flags: `--mem` (memory limit), `--cpu` (percent), `--pids` (max processes), `--timeout` (seconds), `--no-network` (disable network).
