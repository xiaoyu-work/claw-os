# Error Codes

Every error response includes a `code` field for programmatic handling:

```json
{
  "error": "credential not found: OPENAI_KEY",
  "code": "auth.credential_not_found",
  "recovery": {"hint": "...", "try": ["..."]}
}
```

## Error Code Reference

### auth — Permission and credential issues
| Code | Meaning |
|---|---|
| `auth.tier_denied` | Session tier too low for this operation |
| `auth.scope_violation` | Path outside session's allowed scope |
| `auth.credential_not_found` | Credential name not in store |
| `auth.credential_expired` | Credential TTL has passed |
| `auth.refresh_failed` | Auto-refresh command failed |

### resource — Resource state issues
| Code | Meaning |
|---|---|
| `resource.not_found` | Session, service, job, or pipe not found |
| `resource.already_exists` | Name/ID already in use |
| `resource.busy` | Port or resource in use by another process |

### input — Bad arguments
| Code | Meaning |
|---|---|
| `input.missing_required` | Required argument not provided |
| `input.invalid_value` | Argument value out of range or wrong type |
| `input.unknown_command` | Command or subcommand not recognized |

### limit — Quota and rate issues
| Code | Meaning |
|---|---|
| `limit.rate_exceeded` | Too many requests (check `retry_after_secs`) |
| `limit.quota_exceeded` | Disk/checkpoint quota full |
| `limit.timeout` | Operation timed out |
| `limit.out_of_memory` | Process killed by OOM |

### io — Filesystem and network
| Code | Meaning |
|---|---|
| `io.file_not_found` | File or directory doesn't exist |
| `io.permission_denied` | OS-level permission denied |
| `io.disk_full` | No space left on device |
| `io.network_error` | General network failure |
| `io.connection_refused` | Target service not running |

### provider — External service issues
| Code | Meaning |
|---|---|
| `provider.not_configured` | No API key/token for this provider |
| `provider.api_error` | External API returned an error |
| `provider.unavailable` | External service unreachable |

### system — Internal errors
| Code | Meaning |
|---|---|
| `system.internal` | Unexpected internal error |
| `system.not_supported` | Feature not available on this platform |

## Handling Errors

```python
result = cos("credential", "load", ["OPENAI_KEY"])
if "code" in result:
    if result["code"] == "auth.credential_expired":
        cos("credential", "oauth-refresh", ["google"])
    elif result["code"] == "auth.credential_not_found":
        # prompt user to configure
    elif result["code"].startswith("limit."):
        # back off and retry
```
