# Credential Store

Secure AES-256-GCM encrypted storage for API keys, tokens, and secrets:

```bash
cos credential store OPENAI_KEY "sk-..." --tier 0
cos credential store DB_URL "postgresql://..." --tier 1 --ttl 3600
cos credential store TENANT_KEY "abc" --namespace tenant-42
cos credential load OPENAI_KEY
cos credential list
cos credential list --namespace tenant-42
cos credential revoke OPENAI_KEY
```

## Bundles

Group related credentials for bulk loading:

```bash
cos credential bundle openai-config --keys OPENAI_KEY,OPENAI_ORG
cos credential load-bundle openai-config
```

Credentials are auto-injected into services registered with `--credentials`:
```bash
cos service register --name my-agent --command "python agent.py" --credentials OPENAI_KEY,DB_URL
cos service start my-agent   # OPENAI_KEY and DB_URL injected as env vars
```
