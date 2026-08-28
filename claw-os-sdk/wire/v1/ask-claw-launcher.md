# Ask Claw launcher protocol v1

All SDK GUI bindings invoke the fixed trusted helper:

```text
/usr/local/bin/cos-ask-claw-launcher --protocol 1
```

The helper first captures its direct parent PID and UID, validates Yama
isolation, and becomes non-dumpable. It creates an abstract AF_UNIX listener
and publishes only its non-sensitive endpoint:

```text
SOCKET 1 @claw-ask-v1-<helper-pid>-<sequence>
```

The SDK connects while its process remains the helper's direct parent. The
helper verifies `SO_PEERCRED` has that exact PID and UID. A mismatched
connection is closed and the helper continues accepting until its five-second
deadline. The helper then writes `READY 1\n` over the accepted socket and
reads one big-endian u32-length-prefixed JSON request:

```json
{"protocol":1,"app":"example.app","hint":"optional context"}
```

The frame is limited to 32 KiB. Bindings must not write before READY and must
remain alive until the helper validates the frame and replies
`ACCEPTED 1\n`. They must never place `app` or `hint` in argv, environment
variables, pipes, or files. Abstract sockets leave no filesystem entry to
clean up.

The helper exclusively owns the readiness-gated exact-FD launch of
`/usr/local/bin/cos-agent-ui`. That second handoff uses an inherited
`UnixStream::pair`: the UI hardens itself before sending READY and the helper
then sends one bounded length-prefixed activation over the same socket. Socket
descriptors cannot be reopened through `/proc`, and plaintext activation is
never forwarded over the well-known D-Bus activation channel.
