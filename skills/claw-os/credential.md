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

## OAuth Token Auto-Refresh

Store OAuth credentials with auto-refresh. When a token expires, `cos credential load` automatically refreshes it:

```bash
# One-time setup: store OAuth client credentials + refresh token
cos credential store GOOGLE_CLIENT_ID "..." --tier 0
cos credential store GOOGLE_CLIENT_SECRET "..." --tier 0
cos credential store GOOGLE_REFRESH_TOKEN "1//..." --tier 0

# Initial token refresh (creates access token with auto-refresh configured)
cos credential oauth-refresh google

# From now on, this always returns a valid token:
cos credential load GOOGLE_ACCESS_TOKEN
# → if expired, automatically calls oauth-refresh, stores new token, returns it
```

Supported providers: `google`, `microsoft`.

For custom OAuth providers, use `--refresh-cmd` directly:

```bash
cos credential store MY_TOKEN "current-value" --ttl 3600 \
  --refresh-cmd "curl -s https://my-auth.com/refresh?token=xxx"
```
