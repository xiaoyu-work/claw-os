from claw_os_sdk.mcp import App

from main import download, fetch


app = App.from_manifest()


@app.tool("net.fetch")
def fetch_http(
    url: str,
    method: str = "GET",
    data: str | None = None,
    header: list[str] | None = None,
    timeout: int = 30,
) -> dict[str, object]:
    return fetch(url, method, data, header, timeout)


@app.tool("net.download")
def download_file(url: str, output: str) -> dict[str, object]:
    return download(url, output)


if __name__ == "__main__":
    app.serve()
