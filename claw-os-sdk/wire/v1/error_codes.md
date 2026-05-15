| Code                  | When the kernel returns it                                                                                          |
|-----------------------|---------------------------------------------------------------------------------------------------------------------|
| `PERMISSION_DENIED`   | The caps gate refused the call. `detail.verb`, `detail.scope`, `detail.granted`, `detail.hint` describe what / why. |
| `BUDGET_EXCEEDED`     | The app's monthly AI unit budget is exhausted. `detail.period`, `detail.units_used`, `detail.units_cap`.            |
| `SAFETY_VIOLATION`    | Safety pipeline blocked the prompt or response. `detail.stage` = `"prompt"` \| `"response"`, `detail.category`.     |
| `UNKNOWN_APP`         | `--app <id>` references an app not present under `COS_APPS_DIR`.                                                    |
| `UNKNOWN_VERB`        | App doesn't declare this verb in its manifest, OR catalog tool name not in `cos ai tools list`.                     |
| `INVALID_ARGS`        | Args fail the verb / tool's input schema; `detail.path` and `detail.message` point at the offending field.          |
| `KERNEL_UNAVAILABLE`  | `cos` couldn't reach a subsystem it needed (provider down, daemon not running, …).                                  |
| `INTERNAL_ERROR`      | Anything not classified above. `detail.message` carries the raw error.                                              |

SDKs map these to language-idiomatic exception / error types — see e.g.
`rust/src/envelope.rs` (`Error::PermissionDenied`, `Error::BudgetExceeded`, …)
or `python/src/claw_os_sdk/ai.py` (`AiDenied`, `AiBudgetExceeded`, …).
