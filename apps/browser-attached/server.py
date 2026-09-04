from claw_os_sdk.mcp import App

from main import (
    dom_click,
    dom_fill,
    dom_fill_secret,
    dom_query,
    navigate,
    page_eval,
    page_screenshot,
    page_snapshot,
    tabs_activate,
    tabs_list,
)


app = App.from_manifest()


@app.tool("browser-attached.tabs.list")
def browser_tabs_list() -> dict[str, object]:
    return tabs_list()


@app.tool("browser-attached.tabs.activate")
def browser_tabs_activate(tab_id: int) -> dict[str, object]:
    return tabs_activate(tab_id)


@app.tool("browser-attached.nav.go")
def browser_navigate(tab_id: int, url: str) -> dict[str, object]:
    return navigate(tab_id, url)


@app.tool("browser-attached.dom.query")
def browser_dom_query(
    tab_id: int,
    selector: str,
    page_url: str,
) -> dict[str, object]:
    return dom_query(tab_id, selector, page_url)


@app.tool("browser-attached.dom.click")
def browser_dom_click(
    tab_id: int,
    reference: str,
    page_url: str,
) -> dict[str, object]:
    return dom_click(tab_id, reference, page_url)


@app.tool("browser-attached.dom.fill")
def browser_dom_fill(
    tab_id: int,
    reference: str,
    value: str,
    page_url: str,
) -> dict[str, object]:
    return dom_fill(tab_id, reference, value, page_url)


@app.tool("browser-attached.dom.fill_secret")
def browser_dom_fill_secret(
    tab_id: int,
    reference: str,
    value: str,
    page_url: str,
) -> dict[str, object]:
    return dom_fill_secret(tab_id, reference, value, page_url)


@app.tool("browser-attached.page.snapshot")
def browser_page_snapshot(
    tab_id: int,
    page_url: str,
    kind: str = "ax",
) -> dict[str, object]:
    return page_snapshot(tab_id, page_url, kind)


@app.tool("browser-attached.page.screenshot")
def browser_page_screenshot(
    tab_id: int,
    output: str,
    page_url: str,
) -> dict[str, object]:
    return page_screenshot(tab_id, output, page_url)


@app.tool("browser-attached.eval")
def browser_page_eval(
    tab_id: int,
    expr: str,
    page_url: str,
) -> dict[str, object]:
    return page_eval(tab_id, expr, page_url)


if __name__ == "__main__":
    app.serve()
