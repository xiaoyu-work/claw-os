---
applyTo: "**/*.md"
---

# Documentation

- Keep commands copy-pastable from the directory stated by the document.
- Use repository-relative links and verify every local target.
- Link to an existing source of truth instead of duplicating architecture,
  package, app-contract, or update details.
- Update `AGENTS.md` when task routing, commands, conventions, or change
  contracts change.
- Update `ARCHITECTURE.md` only for component, dependency, data-flow, entry
  point, or cross-cutting constraint changes.
- Update the nearest maintained `MODULE.md` for local responsibility,
  dependency, key-file, or test changes.
- Do not claim that a workflow runs on push/PR unless its `on:` block does.
- Examples must not contain real credentials, tokens, private endpoints, or
  user-specific absolute paths.
