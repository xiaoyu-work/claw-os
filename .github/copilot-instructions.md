# Claw OS Copilot Instructions

The authoritative repository workflow is [`../AGENTS.md`](../AGENTS.md).
Use its task-routing table before searching broadly.

Read [`../ARCHITECTURE.md`](../ARCHITECTURE.md) when a task changes component
boundaries, authority, data flow, persistence, image composition, or
distribution. Read the nearest maintained `MODULE.md` before editing files
under a documented module.

Always:

- Preserve unrelated worktree changes and stage explicit paths.
- Trace callers, contracts, and tests before editing an implementation.
- Cover every coupled surface in the relevant AGENTS.md change contract.
- Use the narrowest existing validation first.
- Treat manifests, generated files, vendored boundaries, capability checks,
  model-visible logging, and update behavior according to AGENTS.md.

Path-specific instructions under `.github/instructions/` add local rules. They
supplement rather than replace AGENTS.md and ARCHITECTURE.md.
