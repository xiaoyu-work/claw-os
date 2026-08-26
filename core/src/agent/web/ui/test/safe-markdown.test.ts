import { describe, expect, test } from "bun:test";
import createDOMPurify from "dompurify";
import { JSDOM } from "jsdom";

import { renderSafeMarkdown } from "../src/lib/safe-markdown";

const window = new JSDOM("").window;
const sanitizer = createDOMPurify(window);

describe("renderSafeMarkdown", () => {
  test("does not turn malicious HTML or URLs into executable markup", () => {
    const html = renderSafeMarkdown(`
<script>window.stolen = localStorage.getItem("cos.token")</script>
<img src=x onerror="window.stolen = true">
<a href="javascript:alert(1)" onclick="window.stolen = true">raw link</a>
<a href="java&#x0a;script:alert(1)">encoded link</a>
[script link](javascript:alert)
[data link](data:text/html;base64,PHNjcmlwdD4=)
`, sanitizer);
    const document = new JSDOM(html).window.document;
    expect(document.querySelector("script")).toBeNull();
    expect(document.querySelector("[onclick], [onerror]")).toBeNull();
    expect(document.querySelector("a[href]")).toBeNull();
  });

  test("keeps normal Markdown, code blocks, tables, and safe links", () => {
    const html = renderSafeMarkdown(`
# Heading

**bold** and [documentation](https://example.com/docs)

\`\`\`ts
const safe = 1 < 2;
\`\`\`

| name | value |
| --- | --- |
| safe | yes |
`, sanitizer);

    expect(html).toContain("<h1>Heading</h1>");
    expect(html).toContain("<strong>bold</strong>");
    expect(html).toContain('href="https://example.com/docs"');
    expect(html).toContain('class="language-ts"');
    expect(html).toContain("const safe = 1 &lt; 2;");
    expect(html).toContain("<table>");
  });
});
