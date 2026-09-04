# cosmic-screenshot

Utility for capturing screenshots via XDG Desktop Portal

## Claw OS MCP service

The App Host is the only MCP activation path: it starts
`/usr/bin/cosmic-screenshot` with `COS_MCP_SERVER=1`. Tool metadata and
capability needs are authoritative in `apps/cosmic-screenshot/app.json`.
