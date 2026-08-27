"""Compatibility helpers for manifest-canonical operation argv."""


class _PositionalToken(str):
    """A string that cannot be reclassified as an option by legacy handlers."""

    def __eq__(self, other):
        if isinstance(other, _PositionalToken):
            return super().__eq__(other)
        if isinstance(other, str) and other.startswith("--"):
            return False
        return super().__eq__(other)

    def __ne__(self, other):
        return not self.__eq__(other)

    __hash__ = str.__hash__

    def startswith(self, prefix, *args):
        prefixes = (prefix,) if isinstance(prefix, str) else prefix
        if any(candidate == "--" for candidate in prefixes):
            return False
        return super().startswith(prefix, *args)


def parse_canonical_argv(args, *, value_flags=(), bool_flags=(), repeatable_flags=()):
    """Parse the complete canonical grammar into positionals and options."""
    value_flags = {name.replace("_", "-") for name in value_flags}
    bool_flags = {name.replace("_", "-") for name in bool_flags}
    repeatable_flags = {name.replace("_", "-") for name in repeatable_flags}
    options = {}
    positionals = []
    parse_options = True
    index = 0
    while index < len(args):
        token = str(args[index])
        if parse_options and token == "--":
            parse_options = False
            index += 1
            continue
        if parse_options and token.startswith("--"):
            name, separator, inline = token[2:].partition("=")
            if name in bool_flags:
                if separator:
                    normalized = inline.strip().lower()
                    if normalized not in {
                        "1", "true", "yes", "on", "0", "false", "no", "off"
                    }:
                        raise ValueError(f"--{name} requires a boolean value")
                    value = normalized in {"1", "true", "yes", "on"}
                else:
                    value = True
                index += 1
            elif name in value_flags or name in repeatable_flags:
                if separator:
                    value = inline
                    index += 1
                elif index + 1 < len(args) and not str(args[index + 1]).startswith("--"):
                    value = str(args[index + 1])
                    index += 2
                else:
                    raise ValueError(f"--{name} requires a value")
            else:
                raise ValueError(f"unknown flag: --{name}")
            key = name.replace("-", "_")
            if name in repeatable_flags:
                options.setdefault(key, []).append(value)
            elif key in options:
                raise ValueError(f"--{name} was supplied more than once")
            else:
                options[key] = value
            continue
        positionals.append(token)
        index += 1
    return positionals, options


def normalize_canonical_argv(args, *, bool_flags=()):
    """Return legacy list-handler tokens for the canonical bridge grammar.

    The authority consumes ``--name=value`` and ``--`` directly. Older
    list-based handlers expect split value flags and bare true booleans, so
    normalize only that representation after authority has validated it.
    """
    bool_flags = {name.replace("_", "-") for name in bool_flags}
    normalized = []
    options = True
    for raw in args:
        token = str(raw)
        if options and token == "--":
            options = False
            continue
        if options and token.startswith("--") and "=" in token:
            name, value = token[2:].split("=", 1)
            flag = f"--{name}"
            if name in bool_flags:
                if value.lower() in {"1", "true", "yes", "on"}:
                    normalized.append(flag)
                elif value.lower() not in {"0", "false", "no", "off"}:
                    raise ValueError(f"{flag} requires a boolean value")
            else:
                normalized.extend((flag, value))
            continue
        normalized.append(token if options else _PositionalToken(token))
    return normalized


def normalize_argparse_booleans(args, *, bool_flags):
    """Translate canonical inline booleans without touching other tokens."""
    bool_flags = {name.replace("_", "-") for name in bool_flags}
    normalized = []
    options = True
    for raw in args:
        token = str(raw)
        if options and token == "--":
            options = False
            normalized.append(token)
            continue
        if options and token.startswith("--") and "=" in token:
            name, value = token[2:].split("=", 1)
            if name in bool_flags:
                lowered = value.strip().lower()
                if lowered in {"1", "true", "yes", "on"}:
                    normalized.append(f"--{name}")
                elif lowered not in {"0", "false", "no", "off"}:
                    raise ValueError(f"--{name} requires a boolean value")
                continue
        normalized.append(token)
    return normalized
