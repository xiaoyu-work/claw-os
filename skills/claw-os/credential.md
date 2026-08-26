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

OAuth client registration is runtime system configuration supplied by the
user or administrator; it is never embedded in Claw OS packages. Users never
handle access or refresh tokens themselves:

```bash
# One-time trusted system configuration:
cos credential store GOOGLE_CLIENT_ID "..." --tier 0
cos credential store GOOGLE_CLIENT_SECRET "..." --tier 0

# CLI fallback: opens Google in the system browser, receives the loopback
# callback, and stores access + refresh tokens automatically.
cos credential oauth-login google

# From now on, this always returns a valid token:
cos credential load GOOGLE_ACCESS_TOKEN
# → if expired, automatically calls oauth-refresh, stores new token, returns it
```

Interactive login and token refresh support `google` and `microsoft`:

```bash
cos credential oauth-login microsoft
```

When the system Agent receives an App result containing
`auth_required: true` and a matching `setup.agent_action`, it calls
`cos_oauth_login` itself from an attended local Agent session. The browser and
trusted terminal handle user consent, tokens go directly to the default
encrypted namespace, and the Agent retries the App operation without asking
the user to run a terminal command or paste secrets into chat.

For custom OAuth providers, use `--refresh-cmd` directly:

```bash
cos credential store MY_TOKEN "current-value" --ttl 3600 \
  --refresh-cmd "curl -s https://my-auth.com/refresh?token=xxx"
```
