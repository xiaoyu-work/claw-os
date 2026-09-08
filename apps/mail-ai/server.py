"""Authenticated MCP binding; schemas belong exclusively to app.json."""

from functools import wraps

from claw_os_sdk.mcp import App, ToolResult
import main as mail


def _mcp_result(handler):
    """Encode business failures as MCP tool errors, preserving the signature."""
    @wraps(handler)
    def invoke(**arguments):
        result = handler(**arguments)
        if "error" in result:
            return ToolResult(
                content=[{"type": "text", "text": result["error"]}],
                is_error=True,
                structured_content=result,
            )
        return result
    return invoke


def create_app() -> App:
    app = App.from_manifest()
    app.tool("mail-ai.summarize")(_mcp_result(mail.summarize))
    app.tool("mail-ai.smart_reply")(_mcp_result(mail.smart_reply))
    app.tool("mail-ai.smart_compose")(_mcp_result(mail.smart_compose))
    app.tool("mail-ai.translate")(_mcp_result(mail.translate))
    app.tool("mail-ai.triage")(_mcp_result(mail.triage))
    app.tool("mail-ai.chat")(_mcp_result(mail.chat))
    return app


if __name__ == "__main__":
    create_app().serve()
