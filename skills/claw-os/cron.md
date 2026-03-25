# Job Scheduling (Cron)

Schedule recurring jobs with agent context (tier, scope, credentials) and overlap protection:

```bash
cos cron add health-check \
  --schedule "*/5 * * * *" \
  --command "cos service health my-api" \
  --overlap skip

cos cron add nightly-backup \
  --schedule "0 2 * * *" \
  --command "cos app exec run 'python backup.py'" \
  --tier 1 --scope /den/data \
  --credentials DB_URL \
  --timeout 3600

cos cron list
cos cron status health-check
cos cron logs health-check --limit 10
cos cron enable health-check
cos cron disable health-check
cos cron remove health-check
cos cron run health-check         # manual trigger
```

Overlap policies: `skip` (default — skip if previous still running), `queue`, `kill`, `allow`.

An external scheduler calls `cos cron tick` every minute to process due jobs.
