import DOMPurify from "dompurify";
import { marked } from "marked";

const MARKDOWN_TAGS = [
  "a",
  "blockquote",
  "br",
  "code",
  "del",
  "em",
  "h1",
  "h2",
  "h3",
  "h4",
  "h5",
  "h6",
  "hr",
  "img",
  "input",
  "li",
  "ol",
  "p",
  "pre",
  "strong",
  "table",
  "tbody",
  "td",
  "th",
  "thead",
  "tr",
  "ul",
];

const MARKDOWN_ATTRIBUTES = [
  "align",
  "alt",
  "checked",
  "class",
  "disabled",
  "href",
  "src",
  "start",
  "title",
  "type",
];

const SAFE_URI = /^(?:(?:https?|mailto):|[^a-z]|[a-z+.-]+(?:[^a-z+.\-:]|$))/i;

const SANITIZE_OPTIONS = {
  ALLOWED_ATTR: MARKDOWN_ATTRIBUTES,
  ALLOWED_TAGS: MARKDOWN_TAGS,
  ALLOWED_URI_REGEXP: SAFE_URI,
  ALLOW_ARIA_ATTR: false,
  ALLOW_DATA_ATTR: false,
};

type Sanitizer = Pick<typeof DOMPurify, "sanitize">;

export function renderSafeMarkdown(
  markdown: string,
  sanitizer: Sanitizer = DOMPurify,
): string {
  const html = marked.parse(markdown, { async: false }) as string;
  return sanitizer.sanitize(html, SANITIZE_OPTIONS);
}
