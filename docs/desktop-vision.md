# Agent desktop vision

On a signed-in Claw OS desktop, bring the target application window forward
and ask the Agent to inspect or click a specific control. The OS prompts for
the named window before sending any screenshot to the model. Approve that
request in the desktop review or Agent Web **Approvals**, then leave the
target window in front. This is not Windows-host or remote-desktop control.
The matching Agent and Desktop packages are required on the signed-in system.

The Agent prefers App tools and the browser's DOM when they are available.
Desktop vision works for controls that must be located visually. It uses the
conversation's selected vision-capable model, including Copilot models with
image input; no separate vision service or model is required. The image is
sent only on the next model request and cannot fall back to another provider.
The image is not written to a file, transcript, memory, or task stream.
The OS records the observation and click as structured facts, not pixels.

A large window first returns a preview. The Agent can observe a region in
native window pixels to see small controls without downscaling. The click
uses zero-based coordinates in that returned crop. The compositor maps the
pixel center through the actual output scale and checks that the pixels still
match, the window has stayed in focus and the normal pointer hit test still
selects that window. An observation can be used at most once. A dispatched
click is not proof that the intended action succeeded; the Agent observes
again to verify the result.

The approval is valid only for the exact window in the current foreground
task, for up to five minutes. It does not allow other windows, the lock
screen, global input, typing, scrolling or dragging. It is not available
to scheduled or delegated background jobs. The OS refuses obscured targets,
protected capture surfaces, stale images and unsupported renderers rather
than moving or clicking somewhere else.
