# File Watching

Event-driven watching with inotify (Linux) and multi-source aggregation:

```bash
cos watch file /den/output.txt --timeout 30
cos watch dir /den/results --timeout 60
cos watch proc build-1 --timeout 300
```

## Multi-Source Watching

Watch files, dirs, processes, and services simultaneously — returns on first event:

```bash
cos watch multi --file /den/main.py --dir /den/output/ --proc worker-1 --service my-api --timeout 60
```

## Event History

View past events from the persistent log:

```bash
cos watch history --limit 20
cos watch history --since "2026-03-25T10:00:00Z" --source file
```

## OS Events

```bash
cos watch on proc.exit --session build-1 --timeout 600
cos watch on service.health-fail --name my-api --timeout 3600
cos watch on ipc.message --session worker-1 --timeout 30
cos watch on credential.expired --name API_TOKEN --timeout 300
```
