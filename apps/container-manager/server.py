from claw_os_sdk.mcp import App

from main import (
    inspect,
    kill,
    list_containers,
    logs,
    namespaces,
    pause,
    processes,
    remove,
    restart,
    start,
    stats,
    status,
    stop,
    unpause,
)


app = App.from_manifest()


@app.tool("container-manager.status")
def container_status() -> dict:
    return status()


@app.tool("container-manager.list")
def container_list(runtime: str, namespace: str | None = None) -> dict:
    return list_containers(runtime, namespace)


@app.tool("container-manager.inspect")
def container_inspect(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return inspect(runtime, target, namespace)


@app.tool("container-manager.logs")
def container_logs(
    runtime: str,
    target: str,
    lines: int = 100,
    namespace: str | None = None,
) -> dict:
    return logs(runtime, target, lines, namespace)


@app.tool("container-manager.processes")
def container_processes(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return processes(runtime, target, namespace)


@app.tool("container-manager.stats")
def container_stats(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return stats(runtime, target, namespace)


@app.tool("container-manager.namespaces")
def container_namespaces(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return namespaces(runtime, target, namespace)


@app.tool("container-manager.start")
def container_start(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return start(runtime, target, namespace)


@app.tool("container-manager.stop")
def container_stop(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return stop(runtime, target, namespace)


@app.tool("container-manager.restart")
def container_restart(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return restart(runtime, target, namespace)


@app.tool("container-manager.pause")
def container_pause(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return pause(runtime, target, namespace)


@app.tool("container-manager.unpause")
def container_unpause(
    runtime: str,
    target: str,
    namespace: str | None = None,
) -> dict:
    return unpause(runtime, target, namespace)


@app.tool("container-manager.kill")
def container_kill(
    runtime: str,
    target: str,
    signal: str,
    namespace: str | None = None,
) -> dict:
    return kill(runtime, target, signal, namespace)


@app.tool("container-manager.remove")
def container_remove(
    runtime: str,
    target: str,
    namespace: str | None = None,
    *,
    confirm: bool,
) -> dict:
    return remove(runtime, target, namespace, confirm=confirm)


if __name__ == "__main__":
    app.serve()
