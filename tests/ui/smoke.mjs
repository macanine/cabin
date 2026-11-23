// UI smoke tests: real Chromium, real mouse/keyboard events, mocked Tauri IPC.
// Run: npm run test:ui   (needs a Chromium/Chrome binary; set CHROME=… to choose one)
import { spawn } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";

const PORT = Number(process.env.UI_TEST_PORT ?? 1599);
const CHROME = process.env.CHROME ?? "chromium";
const base = `http://127.0.0.1:${PORT}/tests/ui`;
const vite = spawn("npx", ["vite", "--port", String(PORT), "--strictPort", "--host", "127.0.0.1"], { stdio: "ignore" });
for (let i = 0; i < 60; i++) { try { await fetch(`${base}/harness8.html`); break; } catch { await sleep(300); } }

async function page(url, { reducedMotion = false, mac = false } = {}) {
  const port = 9700 + Math.floor(Math.random() * 200);
  const chrome = spawn(CHROME, ["--headless=new", "--no-sandbox", "--disable-gpu", `--remote-debugging-port=${port}`, `--user-data-dir=/tmp/ui-${port}`, "about:blank"], { stdio: "ignore" });
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
  await send("Emulation.setDeviceMetricsOverride", { width: 1280, height: 860, deviceScaleFactor: 1, mobile: false });
  // Pin the media preferences: CI machines may have Reduce Motion or Reduce
  // Transparency on, which would silently switch the UI to lite effects.
  await send("Emulation.setEmulatedMedia", { features: [
    { name: "prefers-reduced-motion", value: reducedMotion ? "reduce" : "no-preference" },
    { name: "prefers-reduced-transparency", value: "no-preference" },
  ] });
  if (mac) {
    await send("Emulation.setUserAgentOverride", { userAgent: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7)" });
  }
  await send("Page.navigate", { url }); await sleep(3000);
  const evalJs = async (e) => (await send("Runtime.evaluate", { expression: e, returnByValue: true })).result?.result?.value;
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
  async function escape() {
    const event = { key: "Escape", code: "Escape", windowsVirtualKeyCode: 27 };
    await send("Input.dispatchKeyEvent", { ...event, type: "keyDown" });
    await send("Input.dispatchKeyEvent", { ...event, type: "keyUp" });
    await sleep(250);
  }
  const close = () => { ws.close(); chrome.kill(); };
  return { text, click, type, escape, errors, close, eval: evalPage };
}

let failed = 0;
async function test(name, url, run, options) {
  const p = await page(url, options);
  try {
    const expectText = async (...parts) => { const t = await p.text(); for (const part of parts) if (!t.includes(part)) throw new Error(`missing "${part}"`); };
    await run(p, expectText);
    if (p.errors.length) throw new Error(`runtime errors: ${p.errors[0].slice(0, 200)}`);
    console.log(`ok   ${name}`);
  } catch (e) {
    failed++;
    const runtimeError = p.errors[0] ? ` (${p.errors[0].slice(0, 200)})` : "";
    console.log(`FAIL ${name}: ${e.message}${runtimeError}`);
  }
  p.close();
}

await test("start page leads with status, composer and lists", `${base}/harness8.html?v=new`, async (_, has) => {
  await has("Machine running", "What should an agent work on?", "Continue", "Projects", "Add Tailwind to the UI", "Today $");
});
await test("start page machine menu offers stop and settings", `${base}/harness8.html?v=new`, async (p, has) => {
  await p.click("button", "Machine running"); await has("Stop machine", "Machine settings");
  const menu = await p.eval(`(() => {
    const style = getComputedStyle(document.querySelector('.mantine-Menu-dropdown'));
    return { filter: style.filter, backdrop: style.backdropFilter, transform: style.transform };
  })()`);
  if (menu.filter !== "none" || !menu.backdrop.includes("blur") || menu.transform !== "none") throw new Error(JSON.stringify(menu));
});
await test("dark drawer has a translucent surface and a simple overlay", `${base}/harness8.html?v=mcp&theme=dark&perf=full`, async (p, has) => {
  await p.click("button", "Add MCP"); await has("Add MCP server", "Server ID");
  const styles = await p.eval(`(() => {
    const drawer = getComputedStyle(document.querySelector('.mantine-Drawer-content'));
    const overlay = getComputedStyle(document.querySelector('.mantine-Drawer-overlay'));
    return { background: drawer.backgroundColor, drawerBlur: drawer.backdropFilter, overlayBlur: overlay.backdropFilter };
  })()`);
  if (styles.background !== "rgba(36, 34, 32, 0.93)" || !styles.drawerBlur.includes("blur") || styles.overlayBlur !== "none") {
    throw new Error(JSON.stringify(styles));
  }
  await p.escape();
  const returned = await p.eval(`!document.querySelector('.mantine-Drawer-content') && document.activeElement?.textContent.trim() === 'Add MCP'`);
  if (!returned) throw new Error("The drawer did not return focus to Add MCP");
});
await test("Reduce Motion skips dialog transitions", `${base}/harness8.html?v=new&perf=full`, async (p, has) => {
  await p.click("main button", "New project"); await has("New project", "Machine only");
  const state = await p.eval(`(() => {
    const modal = getComputedStyle(document.querySelector('.mantine-Modal-content'));
    return { system: matchMedia('(prefers-reduced-motion: reduce)').matches, duration: parseFloat(modal.transitionDuration) };
  })()`);
  if (!state.system || state.duration > 0.0001) throw new Error(JSON.stringify(state));
}, { reducedMotion: true });
await test("lite mode removes blur and motion", `${base}/harness8.html?v=new&perf=lite`, async (p) => {
  const r = await p.eval(`(()=>{const g=[...document.querySelectorAll(".glass")][0];const c=getComputedStyle(g);return {perf:document.documentElement.dataset.perf,blur:c.backdropFilter,anim:getComputedStyle(document.querySelector("h1")).animationName}})()`);
  if (r.perf !== "lite" || r.blur !== "none" || r.anim !== "none") throw new Error(JSON.stringify(r));
});
await test("full mode keeps the glass blur", `${base}/harness8.html?v=new&perf=full`, async (p) => {
  const r = await p.eval(`getComputedStyle([...document.querySelectorAll(".glass")][0]).backdropFilter`);
  if (!String(r).includes("blur")) throw new Error(String(r));
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
for (const mode of ["stopped", "paused"]) {
  await test(`Mac network interface can be saved while ${mode}`, `${base}/harness8.html?v=settings&tab=network&network=${mode}`, async (p, has) => {
    await p.click('input[role="combobox"]');
    await p.click('[role="option"]', "USB adapter (en8)");
    await p.click(".net-interface button", "Save interface");
    await has("Saved route", "USB adapter (en8)", mode === "stopped" ? "Machine stopped" : "Internet paused");
    if ((await p.text()).includes("Save interface")) throw new Error("Saved interface remained a draft");
  });
}
await test("Mac permissions rechecks folder access on foreground return", `${base}/harness8.html?v=permissions`, async (p, has) => {
  await has("Mac permissions", "Read access needed", "Read and write", "Open System Settings");
  if ((await p.text()).includes("Check again")) throw new Error("Unexpected permissions refresh button");
  await p.eval(`window.dispatchEvent(new Event('focus'))`);
  await p.click("button", "Open System Settings");
  await p.eval(`window.dispatchEvent(new Event('focus'))`);
  for (let i = 0; i < 30 && !(await p.text()).includes("Available"); i++) await sleep(100);
  await has("Available");
  if ((await p.text()).includes("Read access needed")) throw new Error("Folder status did not refresh");
}, { mac: true });
await test("new Mac project shows access recovery with sync options folded", `${base}/harness8.html?v=new`, async (p, has) => {
  await p.click("main button", "New project");
  await p.click("button", "Choose a folder");
  await has("Cabin needs permission to read this folder", "Request access");
  const disabled = () => p.eval(`[...document.querySelectorAll('button')].find(b => b.textContent.trim() === 'Create and sync')?.disabled`);
  if (!await disabled()) throw new Error("Project creation should wait for access");
  await p.click("button", "Request access");
  if (await disabled()) throw new Error("Project creation did not become available after the folder recheck");
});
await test("Node versions load automatically for the selected mirror", `${base}/harness8.html?v=settings&tab=tools`, async (p, has) => {
  await p.click("button", "Install Node.js");
  await has("Node.js versions", "Continue");
  if ((await p.text()).includes("Load versions")) throw new Error("Unexpected versions refresh button");
  await p.click("label", "npmmirror");
  const ready = () => p.eval(`[...document.querySelectorAll('[role="dialog"] button')].some(b => b.textContent.trim() === 'Continue' && !b.disabled)`);
  for (let i = 0; i < 30 && !await ready(); i++) await sleep(100);
  await p.click('[role="dialog"] button', "Continue");
  await has("v24.10.0");
});
await test("usage shows figures and the Antigravity filter", `${base}/harness8.html?v=usage`, async (p, has) => {
  await has("Total cost", "Requests", "Cache hit rate", "Antigravity");
  if ((await p.text()).includes("Sync now")) throw new Error("Unexpected usage sync button");
});
await test("MCP matrix includes Antigravity bindings", `${base}/harness8.html?v=mcp`, async (p, has) => {
  await has("filesystem", "github", "Add MCP");
  if (!await p.eval(`!!document.querySelector('[aria-label="Antigravity · filesystem"]')`)) throw new Error("Antigravity MCP binding missing");
  if (!await p.eval(`!!document.querySelector('[aria-label="Antigravity · events: Unsupported"]')`)) throw new Error("Antigravity SSE should be unsupported");
});
await test("Skills matrix includes Antigravity bindings", `${base}/harness8.html?v=skills`, async (p, has) => {
  await has("frontend-design");
  if (!await p.eval(`!!document.querySelector('[aria-label="Antigravity · pdf"]')`)) throw new Error("Antigravity skill binding missing");
});
await test("Claude account and limits follow the active seat", `${base}/harness8.html?v=agents&claudeSwitch`, async (p, has) => {
  await p.click('[aria-label="Claude Code: Signed in"]');
  await has("Account & limits", "dev@example.com · Example Studio", "Team", "37% used");
  await p.eval(`window.dispatchEvent(new Event('test:claude-seat'))`);
  for (let i = 0; i < 20 && !(await p.text()).includes("81% used"); i++) await sleep(100);
  await has("dev@example.com", "Pro", "81% used");
  const text = await p.text();
  if (text.includes("37% used") || text.includes("Example Studio")) throw new Error("A previous Claude seat is still visible");
});
await test("a temporary Claude probe failure does not ask the user to sign in again", `${base}/harness8.html?v=agents&claudeUnavailable`, async (p, has) => {
  await p.click('[aria-label="Claude Code: Sign-in check unavailable"]');
  await has("Account & limits", "Claude Code's sign-in status is temporarily unavailable.");
  if ((await p.text()).includes("claude auth login")) throw new Error("An unavailable probe was shown as a sign-out");
});
await test("Antigravity can install without Node.js", `${base}/harness8.html?v=agents`, async (p, has) => {
  await has("3 of 5 installed", "no Node.js yet");
  const enabled = (agent) => p.eval(`document.querySelector('[aria-label="${agent}: Not installed"]')?.parentElement.querySelector('button.mantine-Button-root')?.disabled === false`);
  if (!await enabled("Antigravity") || await enabled("Pi")) throw new Error("Incorrect Node.js install requirement");
  await p.click('[aria-label="Antigravity: Not installed"]');
  const runtimeCard = await p.eval(`document.querySelector('[role="tabpanel"]')?.innerText.includes('Chat runtime')`);
  if (runtimeCard) throw new Error("Unexpected separate Antigravity chat runtime");
});
await test("Antigravity sign-in opens an agent terminal", `${base}/harness8.html?v=agents&agy=installed`, async (p, has) => {
  await p.click('[aria-label="Antigravity: Sign-in needed"]');
  await has("agy acp --login");
  await p.click("button", "Open terminal");
  await has("Opened session agy-login");
});
await test("chat: plan popover, slash menu, diff", `${base}/harness4.html`, async (p, has) => {
  await p.click("button.press", "2/4"); await has("Wire Tailwind", "Verify in the app");
  await p.click("textarea"); await p.type("/"); await has("/compact", "/review");
});
await test("chat: changes popover shows a diff with hunks", `${base}/harness4.html`, async (p, has) => {
  await p.click("button.press", "files changed");
  await p.click(".mantine-Popover-dropdown button", "src/components/Boot.tsx");
  await has("@@", "BootHero");
});
await test("chat: approval card shows the command and decisions", `${base}/harness4.html?m=ask`, async (_, has) => { await has("Run a command", "npm run build", "Approve"); });
await test("chat: model and permission pickers", `${base}/harness4.html?m=empty`, async (p, has) => {
  await p.click("button", "claude-sonnet"); await has("Opus 5.5", "REASONING EFFORT");
});

await test("running chat keeps the composer compact and the activity mark still", `${base}/harness4.html?m=run`, async (p, has) => {
  await has("Thinking", "21%");
  const state = await p.eval(`(() => {
    const input = document.querySelector('[aria-label="Message"]');
    const stop = document.querySelector('[aria-label="Stop (Esc)"]');
    const meter = document.querySelector('[role="meter"]');
    const mark = document.querySelector('[data-status="running"] .animate-breathe');
    return {
      working: document.body.innerText.includes('Working'),
      stopOnRight: stop.getBoundingClientRect().right >= input.getBoundingClientRect().right - 4,
      flatMeter: !meter.querySelector('svg'),
      animation: mark && getComputedStyle(mark).animationName,
      rings: document.querySelectorAll('[data-status="running"] .animate-spin').length,
    };
  })()`);
  if (state.working || !state.stopOnRight || !state.flatMeter || state.animation !== "breathe" || state.rings) throw new Error(JSON.stringify(state));
});
await test("markdown code uses one frame and a compact copy control", `${base}/harness4.html`, async (p, has) => {
  await has("export default defineConfig");
  const state = await p.eval(`(() => {
    const block = document.querySelector('[data-streamdown="code-block"]');
    const body = block.querySelector('[data-streamdown="code-block-body"]');
    const copy = block.querySelector('[data-streamdown="code-block-copy-button"]');
    return { outer: getComputedStyle(block).borderTopWidth, inner: getComputedStyle(body).borderTopWidth,
      copy: getComputedStyle(copy).borderTopWidth, tokens: body.querySelectorAll('code span[style]').length };
  })()`);
  if (state.outer !== "1px" || state.inner !== "0px" || state.copy !== "0px" || !state.tokens) throw new Error(JSON.stringify(state));
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
await test("startup respects Reduce Motion with details available", `${base}/harness8.html?v=start&perf=full`, async (p, has) => {
  await has("Starting your machine", "Waiting for SSH");
  await p.click("button", "Details and logs");
  const styles = await p.eval(`(() => {
    const step = getComputedStyle(document.querySelector('main ol li:nth-child(2)'));
    const details = document.querySelector('main [data-open]');
    return { delay: parseFloat(step.animationDelay), duration: parseFloat(step.animationDuration),
      transition: parseFloat(getComputedStyle(details).transitionDuration), open: !details.firstElementChild.inert };
  })()`);
  if (styles.delay !== 0 || styles.duration > 0.0001 || styles.transition > 0.0001 || !styles.open) throw new Error(JSON.stringify(styles));
}, { reducedMotion: true });

vite.kill();
console.log(failed ? `${failed} UI test(s) failed` : "all UI tests passed");
process.exit(failed ? 1 : 0);
