# Cosmic Settings

## Claw OS MCP service

The App Host starts `/usr/bin/cosmic-settings` with `COS_MCP_SERVER=1`.
The informational page tools and Settings launcher are declared in
`apps/cosmic-settings/app.json`; the Rust service only binds handlers.