# cosmic-player
WIP COSMIC media player

## Claw OS MCP service

The App Host starts `/usr/bin/cosmic-player` with `COS_MCP_SERVER=1`.
This mode controls the active MPRIS player over D-Bus and reads its tool
catalog from `apps/cosmic-player/app.json`.
