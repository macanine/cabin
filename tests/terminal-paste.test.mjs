import assert from "node:assert/strict";
import test from "node:test";
import { blobBase64, pastedImage, typedPath } from "../src/lib/terminalPaste.ts";

const item = (kind, type, file = new Blob(["x"], { type })) => ({ kind, type, getAsFile: () => file });

test("the first supported image in a paste is used; text pastes stay text", () => {
  const png = new Blob(["png"], { type: "image/png" });
  assert.equal(pastedImage([item("string", "text/plain"), item("file", "image/png", png)]), png);
  assert.equal(pastedImage([item("string", "text/plain")]), null);
  assert.equal(pastedImage([item("file", "image/svg+xml")]), null);
  assert.equal(pastedImage(null), null);
});

test("paths are typed as is, or quoted when they need it", () => {
  assert.equal(typedPath("/home/dev/.local/share/cabin/attachments/s1/paste_1.png"), "/home/dev/.local/share/cabin/attachments/s1/paste_1.png");
  assert.equal(typedPath("/tmp/my shot.png"), "'/tmp/my shot.png'");
  assert.equal(typedPath("/tmp/it's.png"), "'/tmp/it'\\''s.png'");
});

test("blobs become plain base64", async () => {
  assert.equal(await blobBase64(new Blob(["hello"])), "aGVsbG8=");
});
