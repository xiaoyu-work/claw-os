# Permission Tiers

Control what a process can do. Tier 0 is highest privilege, tier 3 is read-only:

| Tier | Name    | Allowed Operations                    |
|------|---------|---------------------------------------|
| 0    | ROOT    | Read, Write, Delete, Exec, Net, System |
| 1    | OPERATE | Read, Write, Delete, Exec             |
| 2    | CREATE  | Read, Write                           |
| 3    | OBSERVE | Read                                  |

```bash
cos proc spawn --tier 3 --session reader-1 -- analyze.py
cos proc spawn --tier 1 --scope /den/project --session builder-1 -- build.py
cos proc spawn --tier 2 --scope /den/output --parent lead --session writer-1 -- report.py
```

Child processes cannot escalate beyond parent's tier or widen parent's scope.
