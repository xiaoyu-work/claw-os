# Owly lease-service compatibility fork

This is the complete `crates/owlyd` subtree from the MIT-licensed
[`t4t5/owly`](https://github.com/t4t5/owly) revision
`7ff594a61e1e158fe56f22b9efb9d8ea88dd918b`, with its original sources and tests.
`LICENSE` preserves the upstream copyright and permission notice.

Local changes are limited to the standalone manifest and checked retirement:
upstream 0.4.1 logged `Backend::down` failures but still replied successfully to
`Release` and published an idle `LastEnd`. Claw propagates that error, exposes
`recovery-failed` instead of idle, and publishes successful `LastEnd` only after
network retirement completes. Expiry, owner loss, shutdown and link loss use the
same checked path. No protocol/file parser, lease policy or radio backend is
replaced.
Startup also refuses to advertise an idle service when journal recovery fails.
The added private-bus regressions check both failure acknowledgements.

The Claw workspace patches only this crate. AWDL, hardware handling and AirDrop
remain pinned Git dependencies at the recorded revision. This crate keeps an
independent workspace boundary; its upstream CLI is not installed or exposed
by Claw's fixed radio wrapper.
