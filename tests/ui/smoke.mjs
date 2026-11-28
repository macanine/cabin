// UI smoke test: the real app in headless Chromium over a fake Tauri bridge.
// Run: npm run test:ui (set CHROME to choose a browser). No extra packages.
import { spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { setTimeout as sleep } from "node:timers/promises";

const PORT = 1599;
const base = `http://127.0.0.1:${PORT}/tests/ui/index.html?at=`;
const profile = mkdtempSync(join(tmpdir(), "cabin-ui-"));
const vite = spawn("npx", ["vite", "--port", String(PORT), "--strictPort", "--host", "127.0.0.1"], { stdio: "ignore", detached: true });
const chrome = spawn(process.env.CHROME ?? "chromium", ["--headless=new", "--no-sandbox", "--disable-gpu", "--remote-debugging-port=0", `--user-data-dir=${profile}`, "about:blank"], { stdio: ["ignore", "ignore", "pipe"] });
let failed = 0;

async function until(check, what, ms = 15_000) {
  for (const end = Date.now() + ms; Date.now() < end; await sleep(100)) {
    const value = await check();
    if (value) return value;
  }
  throw new Error(`timed out: ${what}`);
}

async function connect() {
  let log = "";
  chrome.stderr.on("data", (data) => { log += data; });
  const browser = await until(() => /ws:\/\/([^/]+)\//.exec(log)?.[1], "browser");
  await until(() => fetch(base).then((r) => r.ok, () => false), "vite", 60_000);
  const tab = (await (await fetch(`http://${browser}/json`)).json()).find((target) => target.type === "page");
  const socket = new WebSocket(tab.webSocketDebuggerUrl);
  await new Promise((open) => { socket.onopen = open; });
  let id = 0;
  const waiting = new Map();
  const errors = [];
  socket.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    waiting.get(message.id)?.(message.result);
    if (message.method === "Runtime.exceptionThrown") errors.push(message.params.exceptionDetails.exception?.description ?? "exception");
  };
  const send = (method, params = {}) => new Promise((done) => { waiting.set(++id, done); socket.send(JSON.stringify({ id, method, params })); });
  await send("Runtime.enable");
  await send("Page.enable");
  // The smallest window the app allows.
  await send("Emulation.setDeviceMetricsOverride", { width: 900, height: 560, deviceScaleFactor: 1, mobile: false });
  const run = async (expression) => (await send("Runtime.evaluate", { expression, returnByValue: true, awaitPromise: true })).result?.value;
  const open = async (at, text) => {
    await send("Page.navigate", { url: base + at });
    await until(() => run(`document.body?.innerText.includes(${JSON.stringify(text)})`), `${at}: "${text}"`, 30_000);
    errors.length = 0;
    await sleep(500);
  };
  const key = async (char, modifiers = 0, code = `Key${char.toUpperCase()}`) => {
    const event = { key: char, code, modifiers, windowsVirtualKeyCode: char === "Enter" ? 13 : char.toUpperCase().charCodeAt(0) };
    await send("Input.dispatchKeyEvent", { ...event, type: "keyDown", text: modifiers || char.length > 1 ? undefined : char });
    await send("Input.dispatchKeyEvent", { ...event, type: "keyUp" });
  };
  return { run, open, key, errors };
}

async function test(name, body) {
  try { await body(); console.log(`ok   ${name}`); }
  catch (error) { failed++; console.log(`FAIL ${name}: ${error.message}`); }
}
const assert = (ok, message) => { if (!ok) throw new Error(message); };

try {
  const { run, open, key, errors } = await connect();
  const visible = (label) => run(`(() => { const b = [...document.querySelectorAll("button")].find((e) => e.textContent.includes(${JSON.stringify(label)})); if (!b) return false;
    const r = b.getBoundingClientRect(); return r.top >= 0 && r.bottom <= innerHeight && r.right <= innerWidth; })()`);

  for (const [at, text] of [["new", "What should an agent work on?"], ["agents", "Claude Code"], ["usage", "Usage"], ["mcp", "Add server"], ["skills", "Add skill"],
    ["settings:general", "Default agent"], ["settings:network", "Route through proxy nodes"], ["settings:tools", "Node.js"], ["settings:machine", "Diagnostics and recovery"], ["settings:permissions", "Folder permissions"]]) {
    await test(`${at} opens and fits the smallest window`, async () => {
      await open(at, text);
      const wide = await run(`[...document.querySelectorAll(".main, .split-detail, .set-group, .net-card, .usage-page")].filter((e) => e.scrollWidth > e.clientWidth + 1).map((e) => e.className).join()`);
      assert(!wide, `overflows: ${wide}`);
      assert(!errors.length, errors[0]);
    });
  }
  await test("machine settings style and switch their three tabs", async () => {
    await open("settings:machine", "Diagnostics and recovery");
    const tabs = 'nav[aria-label="Machine"] > button';
    const styles = await run(`[...document.querySelectorAll(${JSON.stringify(tabs)})].map((tab) => {
      const { appearance, borderInlineStartWidth, borderBottomWidth, borderBottomColor } = getComputedStyle(tab);
      return { appearance, borderInlineStartWidth, borderBottomWidth, borderBottomColor };
    })`);
    assert(styles.length === 3 && styles.every((style) => style.appearance === "none" && style.borderInlineStartWidth === "0px" && style.borderBottomWidth === "2px"), "machine tabs have native button styling");
    assert(styles[0].borderBottomColor !== styles[1].borderBottomColor && styles[1].borderBottomColor === styles[2].borderBottomColor, "the selected machine tab has no distinct underline");
    for (const [index, label, panel] of [[1, "Details", "Saved machine configuration is unavailable"], [2, "Diagnostics and recovery", "Startup and diagnostics"], [0, "Resources", "Waiting for a live sample"]]) {
      await run(`document.querySelectorAll(${JSON.stringify(tabs)})[${index}].click()`);
      await until(() => run(`document.querySelector(${JSON.stringify(`${tabs}[aria-current="page"]`)})?.textContent.trim() === ${JSON.stringify(label)} && document.body.innerText.includes(${JSON.stringify(panel)})`), label);
    }
    assert(!errors.length, errors[0]);
  });
  await test("CLI sign-in opens a terminal directly on a fresh machine", async () => {
    await open("agents:login", "Sign in via CLI");
    await run(`[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Sign in via CLI').click()`);
    await until(() => run(`document.body.innerText.includes('one-time code shown in the terminal') && !!document.querySelector('.terminal-host')`), "sign-in terminal");
    assert(await run(`__cliRequests.length === 1 && __cliRequests[0].kind === 'codex' && __cliRequests[0].login && !('workspaceId' in __cliRequests[0])`), "the CLI request depends on a chosen workspace");
    assert(await run(`!document.querySelector('[role=dialog]')`), "sign-in opened a dialog");
    await run(`[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Return to Account').click()`);
    await until(() => run(`document.body.innerText.includes('Cabin opens the Codex CLI in the virtual machine')`), "Codex account");
    assert(!errors.length, errors[0]);
  });
  await test("terminal scroll reuse matches a full redraw in both directions", async () => {
    const result = await run(`(async () => {
      const { loadGhostty } = await import('/src/lib/terminal/engine.ts');
      const { TerminalCanvasRenderer } = await import('/src/lib/terminal/renderer.ts');
      const { terminalColors, TERMINAL_FONT, toWasmTerminalConfig } = await import('/src/lib/terminal/theme.ts');
      const canvas = document.createElement('canvas');
      const colors = terminalColors('dark');
      const renderer = new TerminalCanvasRenderer(canvas, { fontSize: 13, fontFamily: TERMINAL_FONT, colors });
      const term = (await loadGhostty()).createTerminal(72, 12, toWasmTerminalConfig(colors, 100));
      const overlay = { viewportY: 0, focused: true, selection: null, matches: [], activeMatchIndex: -1, hoveredUrl: null };
      const pixels = () => canvas.getContext('2d').getImageData(0, 0, canvas.width, canvas.height).data;
      try {
        term.write('\\x1bc' + Array.from({length:60}, (_, i) => i + ' 中文 e\\u0301 👩‍💻 ├─ ▓ \\x1b[4;32munderline\\x1b[0m\\r\\n').join(''));
        for (const offset of [30, 33, 31, 3, 1, 0]) {
          overlay.viewportY = offset;
          renderer.render(term, overlay);
          const actual = pixels();
          renderer.render(term, overlay, true);
          const expected = pixels();
          if (actual.some((value, index) => value !== expected[index])) return 'pixel mismatch at offset ' + offset;
        }
        return 'ok';
      } finally { term.free(); }
    })()`);
    assert(result === "ok", result ?? "terminal comparison failed");
  });
  for (const [at, text, action] of [["install", "Set up your agent machine", "Install Ubuntu"], ["failed", "The machine did not start", "Try again"], ["stopped", "Your machine is stopped", "Start machine"]]) {
    await test(`${at} keeps "${action}" in view`, async () => {
      await open(at, text);
      assert(await visible(action), "not in view");
      assert(!errors.length, errors[0]);
    });
  }
  await test("typing a message never approves a command; a deliberate key does", async () => {
    await open("session", "Run a command");
    for (const char of "yes yes") { await key(char, 0, char === " " ? "Space" : undefined); await sleep(60); }
    assert(await run("__sent.length") === 0, "typing as it appeared approved");
    await run(`document.querySelector("textarea[aria-label=Message]").focus()`);
    await sleep(900);
    await key("y");
    assert(await run("__sent.length") === 0, "typing in the box approved");
    await run("document.activeElement.blur()");
    await key("y");
    await until(() => run(`__sent.length === 1 && __sent[0].decision === "accept"`), "approval");
  });
  await test("the palette finds and opens a task", async () => {
    await open("new", "What should an agent work on?");
    await key("K", 2 | 8);
    await until(() => run(`document.activeElement?.getAttribute("role") === "combobox" && !!document.querySelector("#palette-list")`), "palette");
    for (const char of "build") await key(char);
    await key("Enter", 0, "Enter");
    await until(() => run(`document.querySelector(".session-title")?.textContent.includes("Fix the build")`), "session");
  });
} catch (error) {
  failed++;
  console.log(`FAIL ${error.message}`);
} finally {
  const gone = new Promise((done) => chrome.once("exit", done));
  chrome.kill();
  try { process.kill(-vite.pid); } catch { /* already gone */ }
  await Promise.race([gone, sleep(3000)]);
  rmSync(profile, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
}
console.log(failed ? `${failed} failed` : "all passed");
process.exit(failed ? 1 : 0);
