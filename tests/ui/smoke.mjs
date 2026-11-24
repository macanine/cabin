// UI smoke tests: real Chromium, real mouse/keyboard events, mocked Tauri IPC.
// Run: npm run test:ui   (needs a Chromium/Chrome binary; set CHROME=… to choose one)
import { spawn } from "node:child_process";
import { rmSync } from "node:fs";
import { setTimeout as sleep } from "node:timers/promises";

const PORT = Number(process.env.UI_TEST_PORT ?? 1599);
const CHROME = process.env.CHROME ?? "chromium";
const base = `http://127.0.0.1:${PORT}/tests/ui`;
const vite = spawn("npx", ["vite", "--port", String(PORT), "--strictPort", "--host", "127.0.0.1"], { stdio: "ignore" });
for (let i = 0; i < 60; i++) { try { await fetch(`${base}/harness8.html`); break; } catch { await sleep(300); } }

// Every profile this run made; swept again at exit in case Chrome was still writing.
const profiles = [];
async function page(url, { reducedMotion = false, mac = false, windows = false } = {}) {
  const port = 9700 + Math.floor(Math.random() * 200);
  // A throwaway profile per page, removed on close so runs do not fill /tmp.
  const profile = `/tmp/ui-${port}-${process.pid}`;
  profiles.push(profile);
  const chrome = spawn(CHROME, ["--headless=new", "--no-sandbox", "--disable-gpu", `--remote-debugging-port=${port}`, `--user-data-dir=${profile}`, "about:blank"], { stdio: "ignore" });
  let list;
  for (let i = 0; i < 50; i++) { try { list = await (await fetch(`http://127.0.0.1:${port}/json`)).json(); if (list.length) break; } catch {} await sleep(200); }
  const ws = new WebSocket(list.find((t) => t.type === "page").webSocketDebuggerUrl);
  await new Promise((r) => (ws.onopen = r));
  let id = 0; const pending = new Map(); const errors = [];
  ws.onmessage = (m) => {
    const d = JSON.parse(m.data);
    if (d.id && pending.has(d.id)) { pending.get(d.id)(d); pending.delete(d.id); }
    if (d.method === "Runtime.exceptionThrown") errors.push(d.params.exceptionDetails.exception?.description ?? "exception");
    if (d.method === "Runtime.consoleAPICalled" && d.params.type === "error") errors.push(d.params.args.map((a) => a.value ?? a.description).join(" "));
  };
  const send = (method, params = {}) => new Promise((r) => { const i = ++id; pending.set(i, r); ws.send(JSON.stringify({ id: i, method, params })); });
  await send("Runtime.enable"); await send("Page.enable");
  const resize = (width, height, deviceScaleFactor = 1) =>
    send("Emulation.setDeviceMetricsOverride", { width, height, deviceScaleFactor, mobile: false });
  await resize(1280, 860);
  // Pin the media preferences: CI machines may have Reduce Motion or Reduce
  // Transparency on, which would silently switch the UI to lite effects.
  await send("Emulation.setEmulatedMedia", { features: [
    { name: "prefers-reduced-motion", value: reducedMotion ? "reduce" : "no-preference" },
    { name: "prefers-reduced-transparency", value: "no-preference" },
  ] });
  if (mac || windows) {
    const platform = mac ? "Macintosh; Intel Mac OS X 10_15_7" : "Windows NT 10.0; Win64; x64";
    await send("Emulation.setUserAgentOverride", { userAgent: `Mozilla/5.0 (${platform})` });
  }
  await send("Page.navigate", { url }); await sleep(3000);
  const evalJs = async (e) => (await send("Runtime.evaluate", { expression: e, returnByValue: true })).result?.result?.value;
  // A freshly started Vite optimizes dependencies on the first load and can leave
  // that page blank: wait for content, and reload once if none arrives.
  for (let attempt = 0; attempt < 2; attempt++) {
    for (let i = 0; i < 30 && !(await evalJs("document.body?.innerText.trim().length")); i++) await sleep(200);
    if (await evalJs("document.body?.innerText.trim().length")) break;
    await send("Page.reload", { ignoreCache: true }); await sleep(3000);
  }
  const text = () => evalJs("document.body.innerText");
  const evalPage = evalJs;
  async function click(sel, contains = "") {
    const p = await evalJs(`(()=>{const e=[...document.querySelectorAll(${JSON.stringify(sel)})].find(e=>e.textContent.includes(${JSON.stringify(contains)}));if(!e)return null;e.scrollIntoView({block:"center"});const r=e.getBoundingClientRect();return{x:r.x+r.width/2,y:r.y+r.height/2}})()`);
    if (!p) throw new Error(`not found: ${sel} ${contains}`);
    await send("Input.dispatchMouseEvent", { type: "mouseMoved", x: p.x, y: p.y });
    await send("Input.dispatchMouseEvent", { type: "mousePressed", x: p.x, y: p.y, button: "left", clickCount: 1 });
    await send("Input.dispatchMouseEvent", { type: "mouseReleased", x: p.x, y: p.y, button: "left", clickCount: 1 });
    await sleep(600);
  }
  const type = async (s) => { for (const ch of s) await send("Input.dispatchKeyEvent", { type: "char", text: ch }); await sleep(500); };
  const replaceText = async (s) => {
    await send("Input.dispatchKeyEvent", { type: "keyDown", commands: ["selectAll"] });
    await type(s);
  };
  async function escape() {
    const event = { key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 };
    await send("Input.dispatchKeyEvent", { ...event, type: "keyDown" });
    await send("Input.dispatchKeyEvent", { ...event, type: "keyUp" });
    await sleep(250);
  }
  const close = async () => {
    ws.close();
    await new Promise((done) => { chrome.once("exit", done); chrome.kill(); setTimeout(done, 2000); });
    rmSync(profile, { recursive: true, force: true });
  };
  return { text, click, type, replaceText, escape, errors, close, resize, eval: evalPage };
}

let failed = 0;
async function test(name, url, run, options) {
  const p = await page(url, options);
  try {
    // Retry for a few seconds: the first page after Vite starts can still be compiling.
    const expectText = async (...parts) => {
      let missing = parts;
      for (let i = 0; i < 25 && missing.length; i++) {
        if (i) await sleep(200);
        const t = await p.text();
        missing = parts.filter((part) => !t.includes(part));
      }
      if (missing.length) throw new Error(`missing "${missing[0]}" in: ${JSON.stringify((await p.text())?.slice(0, 160))}`);
    };
    await run(p, expectText);
    if (p.errors.length) throw new Error(`runtime errors: ${p.errors[0].slice(0, 200)}`);
    console.log(`ok   ${name}`);
  } catch (e) {
    failed++;
    const runtimeError = p.errors[0] ? ` (${p.errors[0].slice(0, 200)})` : "";
    console.log(`FAIL ${name}: ${e.message}${runtimeError}`);
  }
  await p.close();
}

await test("start page leads with status, composer and lists", `${base}/harness8.html?v=new`, async (_, has) => {
  await has("Machine running", "What should an agent work on?", "Continue", "Projects", "Add Tailwind to the UI", "Today $");
});
await test("installer fits a narrow window and moves from setup to live progress", `${base}/harness8.html?v=install&windows`, async (p, has) => {
  await has("Set up your agent machine", "This computer is ready", "Windows · 8 CPUs", "Size", "Account", "Ubuntu 24.04 LTS", "Preview", "Install Ubuntu");
  await p.resize(390, 765);
  await p.click("button", "More options");
  await has("Package mirror");
  await p.click('[role="radio"]', "Custom");
  await has("CPUs", "grows as used");
  const layout = await p.eval(`(() => {
    const screen = document.querySelector('.install-wizard');
    const top = (selector) => document.querySelector(selector).getBoundingClientRect().top;
    return { fits: screen.scrollWidth <= screen.clientWidth,
      readinessFirst: top('.iw-ready') < top('.iw-card'), cardBeforeForm: top('.iw-card') < top('[aria-label="Machine size"]'),
      prompt: document.querySelector('.iw-prompt').textContent };
  })()`);
  if (!layout.fits || !layout.readinessFirst || !layout.cardBeforeForm || !layout.prompt.includes("dev@dev-vm")) {
    throw new Error(`Narrow setup layout: ${JSON.stringify(layout)}`);
  }
  await p.click("button", "Install Ubuntu");
  await has("Installing Ubuntu 24.04 LTS", "Installation progress", "Download Ubuntu", "Cancel install", "downloading 381 MB");
  const progress = await p.eval(`(() => {
    const screen = document.querySelector('.install-wizard');
    return { fits: screen.scrollWidth <= screen.clientWidth,
      value: Number(document.querySelector('.iw-segments').getAttribute('aria-valuenow')),
      running: document.querySelectorAll('.iw-segments > [data-state="running"]').length,
      tone: document.querySelector('.iw-card').dataset.tone };
  })()`);
  if (!progress.fits || !(progress.value > 0) || progress.running !== 1 || progress.tone !== "building") {
    throw new Error(`Progress view: ${JSON.stringify(progress)}`);
  }
  await p.click("button", "Details and logs");
  await has("Hide details", "In progress");
}, { windows: true });
await test("a failed install explains the stage and returns to the saved settings", `${base}/harness8.html?v=install&installResult=failed`, async (p, has) => {
  await p.click("button", "Install Ubuntu");
  await has("The install stopped", "Ubuntu could not finish its initial setup", "under More options", "Install stopped", "Copy diagnostics", "Retry", "Hide details");
  const state = await p.eval(`({ failed: document.querySelectorAll('.iw-segments > [data-state="failed"]').length,
    tone: document.querySelector('.iw-card').dataset.tone, screen: document.querySelector('.iw-screen-line')?.textContent })`);
  if (state.failed !== 1 || state.tone !== "failed" || !state.screen?.includes("stopped: first boot")) throw new Error(`Failure view: ${JSON.stringify(state)}`);
  await p.click("button", "Change settings");
  await has("Set up your agent machine", "Install Ubuntu");
});
await test("machine settings show the status card and recovery tools", `${base}/harness8.html?v=settings&tab=machine`, async (p, has) => {
  await has("dev-vm", "Running", "4 vCPUs", "Recovery mode", "Review installation", "Erase machine");
  if ((await p.text()).includes("Refresh")) throw new Error("Unexpected machine refresh button");
});
await test("Mac network interface can be applied, retried and switched back", `${base}/harness8.html?v=settings&tab=network&network=reject`, async (p) => {
  const route = () => p.eval(`document.querySelector('.net-route-current').innerText`);
  await p.click('input[role="combobox"]');
  const unavailable = await p.eval(`Array.from(document.querySelectorAll('[role="option"]')).find(e => e.textContent.includes('en9'))?.hasAttribute('data-combobox-disabled')`);
  if (!unavailable) throw new Error("Unplugged interface should be visible and disabled");
  await p.click('[role="option"]', "USB adapter (en8)");
  if (!(await route()).includes("System route")) throw new Error("Draft was shown as active");
  await p.click(".net-interface button", "Apply interface");
  if (!(await route()).includes("System route")) throw new Error("Failed verification committed the candidate");
  await p.click(".net-interface button", "Apply interface");
  if (!(await route()).includes("USB adapter (en8)")) throw new Error("Verified interface did not become active");
  await p.click('input[role="combobox"]');
  await p.click('[role="option"]', "System route");
  await p.click(".net-interface button", "Apply interface");
  if (!(await route()).includes("System route")) throw new Error("System route did not become active again");
});
await test("Folder permissions rechecks folder access on foreground return", `${base}/harness8.html?v=permissions`, async (p, has) => {
  await has("Folder permissions", "Read access needed", "Read and write", "Open System Settings");
  if ((await p.text()).includes("Check again")) throw new Error("Unexpected permissions refresh button");
  await p.eval(`window.dispatchEvent(new Event('focus'))`);
  await p.click("button", "Open System Settings");
  await p.eval(`window.dispatchEvent(new Event('focus'))`);
  for (let i = 0; i < 30 && !(await p.text()).includes("Available"); i++) await sleep(100);
  await has("Available");
  if ((await p.text()).includes("Read access needed")) throw new Error("Folder status did not refresh");
}, { mac: true });
await test("Claude account and limits follow the active seat", `${base}/harness8.html?v=agents&claudeSwitch`, async (p, has) => {
  await p.click('[aria-label="Manage Claude Code account"]');
  await has("Account & limits", "dev@example.com · Example Studio", "Team", "37% used");
  await p.eval(`window.dispatchEvent(new Event('test:claude-seat'))`);
  for (let i = 0; i < 20 && !(await p.text()).includes("81% used"); i++) await sleep(100);
  await has("dev@example.com", "Pro", "81% used");
  const text = await p.text();
  if (text.includes("37% used") || text.includes("Example Studio")) throw new Error("A previous Claude seat is still visible");
});
await test("Plan limits draw burn lines with forecasts and rows share aligned columns", `${base}/harness8.html?v=agents&codexSaved&agy=installed&limitHistory`, async (p, has) => {
  await has("Claude Code · 5-hour", "Codex · Weekly", "~46% used by reset", "Runs out in ~40h 30m");
  const view = await p.eval(`(() => {
    const tiles = [...document.querySelectorAll('.ag-tile')];
    const lefts = new Set([...document.querySelectorAll('.ag-provider')].map((e) => Math.round(e.getBoundingClientRect().left)));
    return { overview: !!document.querySelector('section[aria-label="Plan limits"]'), tiles: tiles.length, bad: tiles.filter((t) => t.dataset.tone === 'bad').length,
      charts: document.querySelectorAll('.ag-tile .burn').length,
      points: [...document.querySelectorAll('.ag-tile .burn-line')].map((l) => l.getAttribute('d').split('L').length),
      exhaust: document.querySelectorAll('.ag-tile .burn-exhaust').length, aligned: lefts.size === 1 };
  })()`);
  if (!view.overview || view.tiles !== 4 || view.bad !== 1 || view.charts !== 4 || view.points.some((n) => n < 4) || view.exhaust !== 1 || !view.aligned) throw new Error(`Plan limits: ${JSON.stringify(view)}`);
  await p.click(".ag-tile", "Codex · Weekly");
  await has("Saved accounts", "64% used");
  await p.resize(560, 800);
  if (await p.eval(`document.querySelector('.agents-workspace').scrollWidth > document.querySelector('.agents-workspace').clientWidth`)) {
    throw new Error("Agents overflows a narrow window");
  }
});
await test("Providers shows and switches Codex accounts without leaving the page", `${base}/harness8.html?v=agents&codexSaved`, async (p, has) => {
  await p.click('nav[aria-label="Agent management"] button', "Providers");
  await has("Account & limits", "Anthropic", "OpenAI");
  await p.click('nav[aria-label="Providers"] button', "OpenAI");
  await has("Saved accounts", "primary@example.com", "backup@example.com", "37% used");
  await p.click("button", "Switch");
  if (!(await p.eval(`document.querySelector('[aria-label="Export backup@example.com"]')?.closest('.list-row')?.innerText.includes('Active')`))) {
    throw new Error("The switched account was not marked Active");
  }
  await p.click("button", "Paste credentials");
  await has("Credential JSON", "Saved accounts");
  if (await p.eval(`!!document.querySelector('[role="dialog"]')`)) throw new Error("Paste opened a dialog");
  await p.resize(580, 800);
  const overflow = await p.eval(`(() => { const page = document.querySelector('.agents-workspace'); return page.scrollWidth > page.clientWidth; })()`);
  if (overflow) throw new Error("Providers overflows a narrow window");
});
await test("chat: plan popover, slash menu, diff", `${base}/harness4.html`, async (p, has) => {
  await p.click("button.press", "2/4"); await has("Wire Tailwind", "Verify in the app");
  await p.click("textarea"); await p.type("/"); await has("/compact", "/review");
});

await test("terminal zoom, Retina backing and IME keep a consistent character grid", `${base}/harness4.html?m=terminal&theme=dark`, async (p, has) => {
  const geometry = () => p.eval(`(() => {
    const canvas = document.querySelector('.terminal-host canvas');
    const slot = document.querySelector('.terminal-slot');
    const input = document.querySelector('.terminal-host textarea');
    const { cols, rows } = __terminalTest.sizes.at(-1);
    return {
      cols, rows, backingWidth: canvas.width, backingHeight: canvas.height,
      canvasWidth: canvas.clientWidth, canvasHeight: canvas.clientHeight,
      slotWidth: slot.clientWidth, slotHeight: slot.clientHeight,
      cellWidth: parseFloat(input.style.width), cellHeight: parseFloat(input.style.lineHeight),
      font: input.style.fontSize, loaded: document.fonts.check('14px "JetBrains Mono"'),
    };
  })()`);
  await has("Running", "14 px");
  const initial = await geometry();
  if (!initial?.loaded || initial.font !== "14px") throw new Error("the bundled terminal font was not loaded");
  await p.click('[aria-label="Increase terminal text size"]');
  await has("15 px");
  const zoomed = await geometry();
  if (zoomed.font !== "15px" || zoomed.cellHeight <= initial.cellHeight || zoomed.rows >= initial.rows) {
    throw new Error("zoom did not update the renderer, input and PTY together");
  }
  await p.resize(380, 620, 1.5);
  await sleep(350);
  const narrow = await geometry();
  if (narrow.cols >= zoomed.cols || narrow.canvasWidth > narrow.slotWidth || narrow.canvasHeight > narrow.slotHeight ||
      narrow.canvasWidth - narrow.cols * narrow.cellWidth !== 14 || narrow.canvasHeight !== narrow.rows * narrow.cellHeight ||
      narrow.backingWidth !== Math.round(narrow.canvasWidth * 1.5) || narrow.backingHeight !== Math.round(narrow.canvasHeight * 1.5)) {
    throw new Error(`narrow terminal grid or backing size mismatch: ${JSON.stringify(narrow)}`);
  }
  await p.eval(`(() => {
    const input = document.querySelector('.terminal-host textarea');
    input.focus();
    input.dispatchEvent(new KeyboardEvent('keydown', { key: '0', ctrlKey: true, bubbles: true, cancelable: true }));
    input.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
    input.dispatchEvent(new CompositionEvent('compositionupdate', { data: '你', bubbles: true }));
    input.dispatchEvent(new CompositionEvent('compositionend', { data: '你', bubbles: true }));
  })()`);
  await sleep(250);
  await has("14 px");
  if (await p.eval("__terminalTest.writes.join('')") !== "你") throw new Error("IME text was not sent exactly once");
});

await test("Git checks Commit all against current staging and ignores an older poll", `${base}/harness4.html?m=git`, async (p, has) => {
  await has("Commit all", "notes.txt");
  await p.click(".wb-commit textarea");
  await p.type("Update notes");
  const buttonEnabled = await p.eval("!document.querySelector('.wb-commit-btn').disabled");
  if (!buttonEnabled) throw new Error("commit stayed disabled after entering the message");
  const pollStarted = await p.eval(`(() => {
    __gitTest.defer = true;
    window.dispatchEvent(new Event('focus'));
    return Boolean(__gitTest.release);
  })()`);
  if (!pollStarted) throw new Error("the older status request did not start");
  await p.eval("__gitTest.stage()");
  await p.click(".wb-commit-btn", "Commit all");
  await has("STAGED CHANGES");
  if (await p.eval("__gitTest.commits.length")) throw new Error("Commit all proceeded after staging changed");
  await p.eval("__gitTest.release()");
  await sleep(100);
  await has("STAGED CHANGES");
  await p.click(".wb-commit-btn", "Commit");
  await has("Working tree clean");
  if (await p.eval("JSON.stringify(__gitTest.commits)") !== "[false]") throw new Error("expected one staged-only commit");
});
await test("machine start animates its entrance and shows only begun steps", `${base}/harness8.html?v=start&perf=full`, async (p, has) => {
  await has("Starting your machine", "Check the configuration", "Connect to the machine", "Waiting for SSH");
  const state = await p.eval(`(() => {
    const hero = document.querySelector('h1').parentElement;
    const steps = [...document.querySelectorAll('main ol li')].map(getComputedStyleForStep);
    function getComputedStyleForStep(step) {
      const style = getComputedStyle(step);
      return { name: style.animationName, delay: parseFloat(style.animationDelay) };
    }
    return { hero: getComputedStyle(hero).animationName, steps,
      mark: getComputedStyle(hero.querySelector('.animate-breathe')).animationName };
  })()`);
  if (state.hero !== "rise-in" || state.steps.length !== 2 || state.steps.some((step) => step.name !== "rise-in") ||
      state.steps[0].delay !== 0 || state.steps[1].delay <= 0 || state.steps[1].delay > 0.1 || state.mark !== "breathe") throw new Error(JSON.stringify(state));

  const details = () => p.eval(`(() => {
    const panel = document.querySelector('main [data-open]');
    return { inert: panel.firstElementChild.inert, height: panel.getBoundingClientRect().height,
      opacity: Number(getComputedStyle(panel).opacity) };
  })()`);
  const closed = await details();
  if (!closed.inert || closed.height !== 0 || closed.opacity !== 0) throw new Error(JSON.stringify(closed));
  await p.click("button", "Details and logs");
  const opened = await details();
  if (opened.inert || opened.height <= 0 || opened.opacity !== 1) throw new Error(JSON.stringify(opened));
  await p.click("button", "Hide details");
  const folded = await details();
  if (!folded.inert || folded.height !== 0) throw new Error("Details remained interactive or occupied space");
});

vite.kill();
console.log(failed ? `${failed} UI test(s) failed` : "all UI tests passed");
for (const profile of profiles) rmSync(profile, { recursive: true, force: true });
process.exit(failed ? 1 : 0);
