import assert from "node:assert/strict";
import test from "node:test";
import {
  classifyLink,
  decodeHtmlEntities,
  parseMarkdownBlocks,
  repairStreamingMarkdown,
  sanitizeImageSrc,
} from "../src/lib/markdown.ts";

test("classifyLink allows file:// links with line anchors and resolves workspace-relative paths", () => {
  const link = classifyLink(
    "file:///home/cabin/workspace/demo/src/worker.ts#L10-L24",
    "/home/cabin/workspace/demo",
  );
  assert.equal(link.kind, "file");
  assert.equal(link.path, "/home/cabin/workspace/demo/src/worker.ts");
  assert.equal(link.relativePath, "src/worker.ts");
  assert.equal(link.line, 10);
  assert.equal(link.endLine, 24);
  assert.equal(link.displayPath, "src/worker.ts:10-24");
});

test("classifyLink handles bare relative paths, external URLs, anchors, and unsafe schemes", () => {
  const rel = classifyLink("./src/crypto.ts#L42");
  assert.equal(rel.kind, "file");
  assert.equal(rel.relativePath, "src/crypto.ts");
  assert.equal(rel.line, 42);
  assert.equal(rel.displayPath, "src/crypto.ts:42");

  assert.deepEqual(classifyLink("https://example.com/docs"), {
    kind: "external",
    href: "https://example.com/docs",
  });
  assert.deepEqual(classifyLink("mailto:hi@example.com"), {
    kind: "external",
    href: "mailto:hi@example.com",
  });
  assert.deepEqual(classifyLink("#section-2"), {
    kind: "anchor",
    href: "#section-2",
  });

  const unsafe = classifyLink("javascript:alert(1)");
  assert.equal(unsafe.kind, "unsafe");
});

test("repairStreamingMarkdown closes unclosed code fences, inline code, links, and emphasis", () => {
  assert.equal(
    repairStreamingMarkdown("```ts\nconst x = 1;"),
    "```ts\nconst x = 1;\n```",
  );
  assert.equal(
    repairStreamingMarkdown("Use `POST /api/agent"),
    "Use `POST /api/agent`",
  );
  assert.equal(
    repairStreamingMarkdown("See [src/worker.ts](file:///home/cabin/src/work"),
    "See src/worker.ts",
  );
  assert.equal(
    repairStreamingMarkdown("This is **bold"),
    "This is **bold**",
  );
  assert.equal(
    repairStreamingMarkdown("Already closed **bold**."),
    "Already closed **bold**.",
  );
});

test("parseMarkdownBlocks parses GFM tables with file:// links cleanly without blocking", () => {
  const md = [
    "### 技术架构与代码结构",
    "",
    "| 模块 | 文件 | 说明 |",
    "| --- | --- | --- |",
    "| **Worker 入口** | [src/worker.ts](file:///workspace/src/worker.ts) | API 路由 |",
    "| **加密模块** | [`src/crypto.ts`](file:///workspace/src/crypto.ts#L12) | Web Crypto |",
  ].join("\n");

  const blocks = parseMarkdownBlocks(md, false);
  assert.equal(blocks.length, 2);
  assert.equal(blocks[0].token.type, "heading");
  assert.equal(blocks[1].token.type, "table");

  const table = blocks[1].token;
  assert.equal(table.type, "table");
  assert.equal(table.rows.length, 2);
  const firstFileCell = table.rows[0][1];
  const linkToken = firstFileCell.tokens.find((t) => t.type === "link");
  assert.ok(linkToken && linkToken.type === "link");
  assert.equal(classifyLink(linkToken.href, "/workspace").kind, "file");
  assert.ok(!md.includes("[blocked]"));
});

test("decodeHtmlEntities decodes named and numeric entities safely", () => {
  assert.equal(decodeHtmlEntities("POST /api?a=1&amp;b=2 &lt;T&gt; &quot;ok&quot; &#39;x&#39;"), 'POST /api?a=1&b=2 <T> "ok" \'x\'');
});

test("sanitizeImageSrc allows https and safe raster data URIs only", () => {
  assert.equal(sanitizeImageSrc("https://example.com/a.png"), "https://example.com/a.png");
  assert.equal(sanitizeImageSrc("data:image/png;base64,iVBORw0KGgo="), "data:image/png;base64,iVBORw0KGgo=");
  assert.equal(sanitizeImageSrc("javascript:alert(1)"), null);
  assert.equal(sanitizeImageSrc("data:text/html;base64,PHNjcmlwdD4="), null);
});
