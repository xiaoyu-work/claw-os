from claw_os_sdk.mcp import App

from main import summarize


app = App.from_manifest()


@app.tool("summarize.run")
def summarize_text(text: str) -> dict:
    return summarize(text)


if __name__ == "__main__":
    app.serve()
