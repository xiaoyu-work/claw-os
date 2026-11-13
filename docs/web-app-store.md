# Agent Web App Store

`cos agent serve --open` provides **App Store** and **Apps** navigation on the
existing authenticated Agent Web UI. It does not use the static marketing
desktop. Ordinary App controls use `clawd` App services without creating an
Agent task or contacting a model.

The Store reads the separately signed, freshness-bounded
`https://xiaoyu-work.github.io/clawos-app/store/index.json` catalog. Artifact
names bind SHA256 and downloads stay on the declared official HTTPS release
hosts. The compiled archive key authenticates catalog metadata; each extracted
App also passes the ordinary Ed25519 provenance, complete-tree, lint and
package-relative entry checks. Catalog availability, architecture, dependency
availability, installation review and runtime capability authority are
separate facts.

Installation is prepared in OS-owned bounded staging and disclosed through
the shared protected App-review contract. The existing owner-bound polkit
dialog confirms each App's exact displayed revision, transported losslessly
as a decimal string for both individual and bundled reviews. Official delivery
uses the signed APT transaction service and requires separate confirmation for
every App in its authenticated dependency plan.
It changes no owner/App data partition and grants no capability or AI consent.
Pending operations remain inspectable after navigation; ambiguous publication
is never automatically retried.

Product interfaces and a signed `web-ui.json` descriptor belong to the App
release, not the OS frontend. The Host authenticates each served resource and
isolates scripts without same-origin privilege. Its view nonce binds the App,
owner, browser session and package. Messages can call only that App's declared
MCP tools; the existing service manager owns lifetime, scope checks, provenance,
revocation and audit. Updating a package invalidates the old view. App code
cannot obtain the Agent Web token, submit a task, call another App or confirm
its own permissions through the browser transport.

Ubuntu and WSL2 users can use the same `claw-os-agent` runtime without
`claw-os-base`, COSMIC or a full OS image. Required OS services, libraries,
hardware, account setup and exact permissions still apply. Native-only
identities without MCP declarations are labelled explicitly; a Web interface
does not invent those missing operations or merge another identity's grants.

This source feature does not publish Apps, adopt a platform pin or clear the
App repository's integration/publication gate. Before the owner approves a
release, a missing official Store index remains an explicit unavailable state;
there is no runtime Git checkout, bundled-source download fallback or unsigned
catalog override. Existing signed installed Apps remain usable offline.
