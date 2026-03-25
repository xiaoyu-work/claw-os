# Network Firewall & Rate Limiting

Control outbound network access and enforce API quotas:

```bash
cos netfilter default deny-all
cos netfilter add --allow "api.openai.com" --port 443
cos netfilter add --allow "*.github.com"
cos netfilter check "api.openai.com"
```

## Rate Limiting

Prevent agents from exceeding API quotas:

```bash
cos netfilter rate-limit api.openai.com --rpm 60 --burst 10
cos netfilter rate-check api.openai.com
cos netfilter rate-limits
cos netfilter rate-limit-remove api.openai.com
```
