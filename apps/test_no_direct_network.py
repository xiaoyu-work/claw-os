"""Contract test: no shipped sandboxed operation dials directly.

Every App operation, adapter and gateway runs inside the worker sandbox
(`core/src/worker/MODULE.md`). There it has no route and, under the
worker seccomp filter, may create only ``AF_UNIX`` sockets, so a direct
`socket` / `urlopen` / `smtplib` dial is not a slower path — it is a
failure that surfaces as a confusing ``EPERM`` at the worst moment.

This walks the shipped Python and refuses any raw network call that is
not on the explicit exception list below. Adding a hit here is a
deliberate act with a written reason, which is the point: the
alternative is a migration silently regressing the next time somebody
copies an old snippet.

It is a maintenance control, not a security boundary: the kernel is the
boundary, and the sandbox refuses an ``AF_INET`` socket whatever this
file says. What it buys is that the refusal is found in review rather
than by a user whose calendar stopped syncing. It therefore resolves
import aliases and local rebindings before matching, so
``from urllib.request import urlopen`` and ``import socket as s`` are
caught as readily as the dotted spelling.

Run it like the rest of the bundled suite::

    PYTHONPATH=claw-os-sdk/python/src:cos-runtime/python/src \\
      python3 -m pytest -q apps/test_no_direct_network.py
"""

from __future__ import annotations

import ast
import pathlib
import unittest

REPO = pathlib.Path(__file__).resolve().parents[1]

# Trees whose *production* modules are checked. Tests are excluded: they
# stand up local fixtures on purpose and never run in a sandbox.
SHIPPED_TREES = ("apps", "adapters", "cos-runtime/python/src")

# Calls that reach the network directly, written as the fully-qualified
# name they resolve to after aliases. Anything here must be justified
# below.
DIRECT_CALLS = {
    "socket.socket",
    "socket.create_connection",
    "socket.create_server",
    "socket.getaddrinfo",
    "socket.gethostbyname",
    "urllib.request.urlopen",
    "urllib.request.urlretrieve",
    "urllib.request.build_opener",
    "urllib.request.OpenerDirector",
    "http.client.HTTPConnection",
    "http.client.HTTPSConnection",
    "smtplib.SMTP",
    "smtplib.SMTP_SSL",
    "smtplib.LMTP",
    "imaplib.IMAP4",
    "imaplib.IMAP4_SSL",
    "poplib.POP3",
    "poplib.POP3_SSL",
    "ftplib.FTP",
    "ftplib.FTP_TLS",
    "telnetlib.Telnet",
    "ssl.wrap_socket",
    "requests.get",
    "requests.post",
    "requests.put",
    "requests.patch",
    "requests.delete",
    "requests.head",
    "requests.request",
    "requests.Session",
    "httpx.get",
    "httpx.post",
    "httpx.put",
    "httpx.patch",
    "httpx.delete",
    "httpx.head",
    "httpx.request",
    "httpx.stream",
    "httpx.Client",
    "httpx.AsyncClient",
    "aiohttp.request",
    "aiohttp.ClientSession",
    "aiohttp.TCPConnector",
    "websockets.connect",
    "websocket.create_connection",
    "websocket.WebSocket",
    "asyncio.open_connection",
    "asyncio.start_server",
}

# External binaries that dial on the caller's behalf.
DIRECT_BINARIES = {
    "curl",
    "wget",
    "nc",
    "ncat",
    "netcat",
    "nc.traditional",
    "nc.openbsd",
    "socat",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "telnet",
    "ftp",
    "openssl",
}

# Calls whose argv is worth reading for one of those binaries.
SPAWNING_CALLS = ("subprocess.", "os.exec", "os.spawn", "os.system", "os.popen")

# Every accepted direct hit, with the reason it is safe. A path maps to
# the set of resolved call names that may appear in it. Exceptions are
# per symbol, never per directory: a whole-path exemption would hide
# the next unrelated regression in the same file.
#
# The three classes are:
#   * *transport* — the module that implements the brokered tunnel, or
#     the one place a non-sandboxed dial still happens behind a
#     brokered-first branch;
#   * *AF_UNIX only* — a local socket, which the seccomp filter allows;
#   * *explicit denial* — code that detects the sandbox and refuses.
EXCEPTIONS: dict[str, set[str]] = {
    # transport: chooses the broker inside a sandbox and the pinned
    # direct dial outside one, and is what every App HTTP path goes
    # through.
    "apps/_shared/safe_http.py": {
        "socket.create_connection",
        "socket.getaddrinfo",
        "http.client.HTTPConnection",
        "http.client.HTTPSConnection",
        "urllib.request.build_opener",
    },
    # transport: the gateway's equivalent, same shape.
    "apps/gateway/_shared/safe_egress.py": {
        "socket.socket",
        "socket.getaddrinfo",
        "http.client.HTTPConnection",
        "http.client.HTTPSConnection",
        "urllib.request.build_opener",
    },
    # transport: the brokered tunnel itself. Its only socket is the
    # AF_UNIX one it opens to the broker.
    "cos-runtime/python/src/cos_runtime/egress.py": {"socket.socket"},
    # transport: substitutes `smtplib`'s dial. The class names appear
    # because it subclasses them; `_get_socket` is overridden so no
    # instance can reach `socket.create_connection`.
    "cos-runtime/python/src/cos_runtime/smtp.py": {
        "smtplib.SMTP",
        "smtplib.SMTP_SSL",
    },
    # trusted-native exemption: the native host runs outside the
    # sandbox under the kernel-side allowlist, and its socket is
    # AF_UNIX.
    "apps/browser-attached/native_host.py": {"socket.socket"},
}


def shipped_modules():
    for tree in SHIPPED_TREES:
        root = REPO / tree
        if not root.is_dir():
            continue
        for path in sorted(root.rglob("*.py")):
            name = path.name
            if name.startswith("test_") or name.endswith("_test.py"):
                continue
            if "__pycache__" in path.parts:
                continue
            yield path


def dotted(node: ast.AST) -> str:
    """Render `a.b.c` / `name` for an expression, or ``""``."""
    parts = []
    current = node
    while isinstance(current, ast.Attribute):
        parts.append(current.attr)
        current = current.value
    if isinstance(current, ast.Name):
        parts.append(current.id)
    else:
        return ""
    return ".".join(reversed(parts))


def local_bindings(tree: ast.AST) -> dict[str, str]:
    """Map every local name to the module path it stands for.

    Covers ``import x.y as z``, ``from x.y import z``,
    ``from x.y import z as w`` and a plain ``w = z`` rebinding, so the
    match below sees one canonical spelling instead of however the
    module happened to write it.
    """
    bindings: dict[str, str] = {}
    for node in ast.walk(tree):
        if isinstance(node, ast.Import):
            for alias in node.names:
                if alias.asname:
                    bindings[alias.asname] = alias.name
        elif isinstance(node, ast.ImportFrom):
            # A relative import names a sibling module, not a stdlib
            # dialler, and resolving it here would invent a path.
            if node.level or not node.module:
                continue
            for alias in node.names:
                bindings[alias.asname or alias.name] = f"{node.module}.{alias.name}"
    # Rebindings are resolved after the imports so `f = urlopen` picks
    # up whatever `urlopen` was bound to.
    for node in ast.walk(tree):
        if not isinstance(node, ast.Assign) or len(node.targets) != 1:
            continue
        target = node.targets[0]
        if not isinstance(target, ast.Name):
            continue
        source = resolve(dotted(node.value), bindings)
        if source and source in DIRECT_CALLS:
            bindings[target.id] = source
    return bindings


def resolve(name: str, bindings: dict[str, str]) -> str:
    """Canonicalize `name` through the module's local bindings."""
    if not name:
        return ""
    head, _, rest = name.partition(".")
    base = bindings.get(head, head)
    return f"{base}.{rest}" if rest else base


def spawned_binaries(node: ast.Call):
    """Binary names a subprocess-style call would run, when literal."""
    for argument in node.args:
        if isinstance(argument, ast.List):
            for element in argument.elts:
                if isinstance(element, ast.Constant) and isinstance(element.value, str):
                    yield pathlib.PurePath(element.value).name
                    break
        elif isinstance(argument, ast.Constant) and isinstance(argument.value, str):
            # `shell=True` takes a command line; every token in it can
            # be the dialler, including one after a pipe.
            for token in argument.value.replace("|", " ").split():
                yield pathlib.PurePath(token).name


class NoDirectNetworkTests(unittest.TestCase):
    def test_shipped_operations_have_no_unreviewed_direct_dial(self):
        violations = []
        for path in shipped_modules():
            relative = path.relative_to(REPO).as_posix()
            allowed = EXCEPTIONS.get(relative, set())
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            bindings = local_bindings(tree)
            for node in ast.walk(tree):
                if not isinstance(node, ast.Call):
                    continue
                name = resolve(dotted(node.func), bindings)
                if name in DIRECT_CALLS and name not in allowed:
                    violations.append(f"{relative}:{node.lineno}: {name}")
        self.assertEqual(
            violations,
            [],
            "shipped sandboxed code gained a direct network call; migrate it to "
            "cos_runtime.egress / _shared.safe_http / cos_runtime.smtp, or add an "
            "explicit reviewed exception to EXCEPTIONS",
        )

    def test_shipped_operations_do_not_shell_out_to_a_dialler(self):
        violations = []
        for path in shipped_modules():
            relative = path.relative_to(REPO).as_posix()
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            bindings = local_bindings(tree)
            for node in ast.walk(tree):
                if not isinstance(node, ast.Call):
                    continue
                name = resolve(dotted(node.func), bindings)
                if not name.startswith(SPAWNING_CALLS):
                    continue
                for binary in spawned_binaries(node):
                    if binary in DIRECT_BINARIES:
                        violations.append(f"{relative}:{node.lineno}: {binary}")
        self.assertEqual(
            violations,
            [],
            "shipped sandboxed code shells out to a network client; a sandboxed "
            "worker has no route, so this cannot work — use the brokered tunnel",
        )

    def test_every_exception_is_still_needed(self):
        """A stale exception is a hole nobody is looking at any more."""
        stale = []
        for relative, names in EXCEPTIONS.items():
            path = REPO / relative
            if not path.is_file():
                stale.append(f"{relative}: file is gone")
                continue
            tree = ast.parse(path.read_text(encoding="utf-8"), filename=str(path))
            bindings = local_bindings(tree)
            # Any mention counts, not only a call: `smtplib.SMTP` is
            # subclassed rather than called by the transport module, and
            # an exception for it is still doing real work.
            present = {
                resolve(dotted(node), bindings)
                for node in ast.walk(tree)
                if isinstance(node, (ast.Attribute, ast.Name))
            }
            unused = sorted(names - present)
            if unused:
                stale.append(f"{relative}: {', '.join(unused)}")
        self.assertEqual(stale, [], "remove exceptions that no longer apply")

    def test_the_guard_resolves_aliases_and_rebindings(self):
        """The control itself, on the shapes it exists to catch."""
        module = ast.parse(
            "from urllib.request import urlopen as fetch\n"
            "import socket as sock\n"
            "from socket import create_connection\n"
            "grab = fetch\n"
        )
        bindings = local_bindings(module)
        self.assertEqual(resolve("fetch", bindings), "urllib.request.urlopen")
        self.assertEqual(resolve("grab", bindings), "urllib.request.urlopen")
        self.assertEqual(resolve("sock.socket", bindings), "socket.socket")
        self.assertEqual(
            resolve("create_connection", bindings), "socket.create_connection"
        )
        # A name nobody rebound is left exactly as written.
        self.assertEqual(resolve("json.dumps", bindings), "json.dumps")

    def test_the_guard_reads_shell_command_lines(self):
        call = ast.parse("subprocess.run('curl https://x.example', shell=True)").body[0]
        self.assertIn("curl", set(spawned_binaries(call.value)))
        argv = ast.parse("subprocess.run(['/usr/bin/wget', '-q', 'u'])").body[0]
        self.assertIn("wget", set(spawned_binaries(argv.value)))


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
