# Job Scheduling (Cron)

Schedule recurring jobs with agent context (tier, scope, credentials) and overlap protection:

```bash
yu cron add health-check \
  --schedule "*/5 * * * *" \
  --command "yu service health my-api" \
  --overlap skip

yu cron add nightly-backup \
  --schedule "0 2 * * *" \
  --command "yu app exec run 'python backup.py'" \
  --tier 1 --scope /home/cos/data \
  --credentials DB_URL \
  --timeout 3600

yu cron list
yu cron status health-check
yu cron logs health-check --limit 10
yu cron enable health-check
yu cron disable health-check
yu cron remove health-check
yu cron run health-check         # manual trigger
```

Overlap policies: `skip` (default — skip if previous still running), `queue`, `kill`, `allow`.

An external scheduler calls `yu cron tick` every minute to process due jobs.
