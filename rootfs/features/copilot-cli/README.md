# copilot-cli

Installs the [GitHub Copilot CLI](https://github.com/github/copilot-cli)
(`@github/copilot`) globally into the rootfs so the `copilot` binary is on
every user's `$PATH`.

Used by **cosmic-term**'s `@`-trigger AI integration: when the user types `@`
at the start of a shell prompt, cosmic-term captures the typed query, drops it
into `$COS_AI_TMP/aq-<id>.txt`, then injects a call to a shell function
`__cos_ai <id>` which `exec`s:

```sh
copilot -p "<terminal_context>...</terminal_context>\n\n<user_request>...</user_request>" \
        --allow-all-tools [--model <name>]
```

The first time a user runs Copilot, it walks them through OAuth device-flow
authentication and writes credentials to `~/.config/github-copilot/`. No
secrets are baked into the image.

## Dependencies

Pulls `nodejs` and `npm` from apt (`packages.txt`) — npm is required to install
`@github/copilot` from the npm registry.

## Files installed

* `/usr/lib/node_modules/@github/copilot/` — npm package payload
* `/usr/bin/copilot` (or `/usr/local/bin/copilot`) — entrypoint script

## Used by

* `desktop/term/src/ai/shell_integration.rs` — embeds `copilot` in the
  generated bash/zsh/fish/pwsh `__cos_ai` functions
* `desktop/term/src/ai/mod.rs` — `$COS_AI_COPILOT_BIN` env override resolves
  the binary path; defaults to bare `copilot` so the user's `$PATH` lookup wins
