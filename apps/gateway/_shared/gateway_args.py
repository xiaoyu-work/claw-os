"""Canonical list-argument parsing for outbound gateway apps."""

from __future__ import annotations


def parse(
    args,
    *,
    positional=(),
    positional_aliases=(),
    value_flags=(),
    bool_flags=(),
):
    values = {name: "" for name in positional}
    values.update({name: None for name in value_flags})
    values.update({name: False for name in bool_flags})
    positionals = []
    options = True
    index = 0
    while index < len(args):
        token = str(args[index])
        if options and token == "--":
            options = False
            index += 1
            continue
        if options and token.startswith("--"):
            raw = token[2:]
            name, separator, inline = raw.partition("=")
            if name in bool_flags:
                if separator:
                    normalized = inline.strip().lower()
                    if normalized not in {"1", "true", "yes", "on", "0", "false", "no", "off"}:
                        return None, f"--{name} requires a boolean value"
                    values[name] = normalized in {"1", "true", "yes", "on"}
                else:
                    values[name] = True
                index += 1
                continue
            if name in value_flags:
                if separator:
                    values[name] = inline
                    index += 1
                    continue
                if index + 1 >= len(args) or str(args[index + 1]).startswith("--"):
                    return None, f"--{name} requires a value"
                values[name] = str(args[index + 1])
                index += 2
                continue
            return None, f"unknown flag: --{name}"
        positionals.append(token)
        index += 1

    alias_count = min(
        len(positional_aliases),
        max(0, len(positionals) - len(positional)),
    )
    if len(positionals) > len(positional) + alias_count:
        return None, f"too many positional arguments: {positionals!r}"
    for name, value in zip(positional_aliases, positionals[:alias_count]):
        if values.get(name) not in {None, ""}:
            return None, f"{name} was supplied by both positional and flag forms"
        values[name] = value
    positionals = positionals[alias_count:]
    for name, value in zip(positional, positionals):
        values[name] = value
    return values, None
