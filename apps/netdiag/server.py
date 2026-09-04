from claw_os_sdk.mcp import App

from main import diagnose, dns, interfaces, routes, tcp


app = App.from_manifest()


@app.tool("netdiag.interfaces")
def netdiag_interfaces() -> dict[str, object]:
    return interfaces()


@app.tool("netdiag.routes")
def netdiag_routes() -> dict[str, object]:
    return routes()


@app.tool("netdiag.dns")
def netdiag_dns(target: str) -> dict[str, object]:
    return dns(target)


@app.tool("netdiag.tcp")
def netdiag_tcp(
    target: str,
    attempts: int = 3,
    timeout: float = 5.0,
) -> dict[str, object]:
    return tcp(target, attempts, timeout)


@app.tool("netdiag.diagnose")
def netdiag_diagnose(target: str) -> dict[str, object]:
    return diagnose(target)


if __name__ == "__main__":
    app.serve()
