# Service Management

Manage long-running services with lifecycle hooks and graceful shutdown:

```bash
cos service list
cos service start my-api
cos service stop my-api          # graceful: checkpoint → pre_stop → drain → SIGTERM → wait → SIGKILL → post_stop
cos service stop-all             # stop all in reverse dependency order
cos service restart my-api
cos service status my-api
cos service health my-api
cos service logs my-api --tail 50
```

## Register with Lifecycle Hooks

```bash
cos service register \
  --name my-api \
  --command "python app.py" \
  --workdir /den/api \
  --health-url http://localhost:8000/health \
  --credentials OPENAI_KEY,DB_URL \
  --pre-start "python migrate.py" \
  --pre-stop "python drain.py" \
  --checkpoint-cmd "python save_state.py" \
  --drain-timeout 10 \
  --stop-timeout 30
```
