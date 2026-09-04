from claw_os_sdk.mcp import App

from main import image, web


app = App.from_manifest()


@app.tool("search.web")
def search_web(
    provider: str,
    query: str,
    max_results: int = 5,
) -> dict[str, object]:
    return web(provider, query, max_results)


@app.tool("search.image")
def search_image(
    provider: str,
    query: str,
    max_results: int = 5,
) -> dict[str, object]:
    return image(provider, query, max_results)


if __name__ == "__main__":
    app.serve()
