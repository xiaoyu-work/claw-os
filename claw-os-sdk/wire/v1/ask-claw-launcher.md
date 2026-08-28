# Ask Claw launcher protocol v1

All SDK GUI bindings invoke the fixed trusted helper:

```text
/usr/local/bin/cos-ask-claw-launcher --protocol 1
```

The helper validates Yama isolation, becomes non-dumpable, writes exactly
`READY 1\n` to stdout, then reads one bounded JSON request from stdin:

```json
{"protocol":1,"app":"example.app","hint":"optional context"}
```

Bindings must not write before READY and must never place `app` or `hint` in
argv, environment variables, or files. The helper exclusively owns the
readiness-gated exact-FD launch of `/usr/local/bin/cos-agent-ui`.
