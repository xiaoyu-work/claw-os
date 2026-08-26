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

Bundle scope does not replace member scope: creating a bundle requires
`secret.grant` for every listed credential, and loading it requires
`secret.read` for every member before any value is decrypted.

Credentials are auto-injected into services registered with `--credentials`:
```bash
cos service register --name my-agent --command "python agent.py" --credentials OPENAI_KEY,DB_URL
cos service start my-agent   # OPENAI_KEY and DB_URL injected as env vars
```

## OAuth Token Auto-Refresh

Use the normal installed-app browser flow for initial Google login. The OAuth
client id/secret identify the Claw OS application; users never handle a refresh
token themselves:

```bash
# One-time application configuration (omit when the package provides these):
cos credential store GOOGLE_CLIENT_ID "..." --tier 0
cos credential store GOOGLE_CLIENT_SECRET "..." --tier 0

# Normal user login: opens Google in the system browser, receives the
# loopback callback, and stores access + refresh tokens automatically.
cos credential oauth-login google

# From now on, this always returns a valid token:
cos credential load GOOGLE_ACCESS_TOKEN
# → if expired, automatically calls oauth-refresh, stores new token, returns it
```

Interactive login and token refresh support `google` and `microsoft`:

```bash
cos credential oauth-login microsoft
```

For custom OAuth providers, use `--refresh-cmd` directly:

```bash
cos credential store MY_TOKEN "current-value" --ttl 3600 \
  --refresh-cmd "curl -s https://my-auth.com/refresh?token=xxx"
```
