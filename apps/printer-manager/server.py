from claw_os_sdk.mcp import App

from main import cancel, capabilities, jobs, print_document, status


app = App.from_manifest()


@app.tool("printer-manager.status")
def printer_status() -> dict:
    return status()


@app.tool("printer-manager.capabilities")
def printer_capabilities(printer: str) -> dict:
    return capabilities(printer)


@app.tool("printer-manager.jobs")
def printer_jobs(printer: str | None = None) -> dict:
    return jobs(printer)


@app.tool("printer-manager.print")
def printer_print(
    printer: str,
    source: str,
    sides: str | None = None,
    copies: int = 1,
    title: str | None = None,
    media: str | None = None,
) -> dict:
    return print_document(printer, source, sides, copies, title, media)


@app.tool("printer-manager.cancel")
def printer_cancel(job_id: str, confirm: bool) -> dict:
    return cancel(job_id, confirm)


if __name__ == "__main__":
    app.serve()
