// Walk the first-run journey in headless Chrome and save screenshots.
// Run: node tests/ui/journey.mjs <scenario> [outDir]
//   scenarios: mac | win-nowhpx | win-noqemu | fail-image | fail-provision
import { spawn } from "node:child_process";
import fs from "node:fs";
import { setTimeout as sleep } from "node:timers/promises";

const scenario = process.argv[2] ?? "mac";
const out = process.argv[3] ?? `/tmp/cabin-journey/${scenario}`;
fs.mkdirSync(out, { recursive: true });
const PORT = Number(process.env.UI_TEST_PORT ?? 1611);
const CHROME = process.env.CHROME ?? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const query = {
  mac: "host=mac", "win-nowhpx": "host=win-nowhpx", "win-noqemu": "host=win-noqemu",
  "fail-image": "host=win&fail=image", "fail-provision": "host=mac&fail=provision",
}[scenario];
const vite = spawn("npx", ["vite", "--port", String(PORT), "--strictPort", "--host", "127.0.0.1"], { stdio: "ignore" });
const base = `http://127.0.0.1:${PORT}/tests/ui/install.html?${query}`;
for (let i = 0; i < 60; i++) { try { await fetch(base); break; } catch { await sleep(300); } }

const port = 9300 + Math.floor(Math.random() * 300);
const chrome = spawn(CHROME, ["--headless=new", "--disable-gpu", `--remote-debugging-port=${port}`, `--user-data-dir=/tmp/journey-${port}`, "about:blank"], { stdio: "ignore" });
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
await send("Emulation.setDeviceMetricsOverride", { width: 1240, height: 800, deviceScaleFactor: 1, mobile: false });
await send("Page.navigate", { url: base }); await sleep(3500);
const evalJs = async (e) => (await send("Runtime.evaluate", { expression: e, returnByValue: true, awaitPromise: true })).result?.result?.value;
let shot = 0;
async function snap(name) {
  const r = await send("Page.captureScreenshot", { format: "png", captureBeyondViewport: false });
  const file = `${out}/${String(++shot).padStart(2, "0")}-${name}.png`;
  fs.writeFileSync(file, Buffer.from(r.result.data, "base64"));
  console.log("shot", file);
}
async function click(text, sel = "button") {
  let p;
  const deadline = Date.now() + 8000;
  do {
    p = await evalJs(`(()=>{const e=[...document.querySelectorAll(${JSON.stringify(sel)})].find(e=>e.textContent.trim().includes(${JSON.stringify(text)}) && !e.disabled && e.offsetParent);if(!e)return null;e.scrollIntoView({block:"center"});const r=e.getBoundingClientRect();return{x:r.x+r.width/2,y:r.y+r.height/2}})()`);
    if (!p) await sleep(100);
  } while (!p && Date.now() < deadline);
  if (!p) { console.log("MISSING button:", text); return false; }
  for (const type of ["mouseMoved", "mousePressed", "mouseReleased"]) await send("Input.dispatchMouseEvent", { type, x: p.x, y: p.y, button: "left", clickCount: 1 });
  await sleep(700);
  return true;
}
const buttons = () => evalJs(`[...document.querySelectorAll("button")].filter(b=>b.offsetParent).map(b=>(b.disabled?"[disabled] ":"")+b.textContent.trim()).filter(Boolean)`);
const text = () => evalJs("document.body.innerText");

const failures = [];
const expect = (ok, what) => { if (!ok) failures.push(what); console.log(ok ? "ok  " : "FAIL", what); };
const has = async (needle) => (await text())?.includes(needle) ?? false;

async function verifyDiagnostics(from, checkReport) {
  expect(await click("Copy diagnostics"), `${from} offers Copy diagnostics`);
  const report = await evalJs("window.__journey.diagnostics()");
  expect(report?.versions?.cabin === "0.2.0" && report.readiness?.length > 0 && report.logTails?.length > 0,
    `${from} copies versions, readiness, and log tails`);
  expect(checkReport(report), `${from} copies the recorded report`);
}

await snap("setup");
const setupButtons = await buttons();
expect(setupButtons.some((b) => /Install$/.test(b)), "setup offers Install");
expect(!(await has("Machine settings")), "first run has no Machine settings detour");
if (await click("Advanced")) { await snap("advanced"); await click("Advanced"); }
if (scenario === "win-noqemu") {
  expect((await buttons()).includes("[disabled] Install"), "a missing bundled QEMU blocks Install");
  expect((await buttons()).includes("Download Cabin"), "a missing bundled QEMU offers Download Cabin");
}
if (scenario === "win-nowhpx") {
  expect(await has("Windows Hypervisor Platform is turned off"), "WHPX off is explained");
  if (await click("Turn on")) {
    await sleep(1000); await snap("after-fix");
    expect(!(await buttons()).includes("Turn on"), "Turn on is replaced after it succeeds");
    expect((await buttons()).includes("Restart Windows"), "restart is offered after turning WHPX on");
  }
}
const probe = () => evalJs(`import("/src/state/store.ts").then(m=>{let v;m.patchState(s=>{v=s;return s});return JSON.stringify({busy:v.machine?.busy,steps:v.install.map(s=>s.id+":"+s.state)})})`);
if (scenario !== "win-noqemu") {
  expect(await click("Install"), "ready setup starts the installation");
  if (process.env.PROBE) for (let i = 0; i < 12; i++) { console.log("PROBE", await probe()); await sleep(500); }
  await sleep(1200); await snap("installing-early");
  expect(await has("Installing Ubuntu"), "the install shows progress");
  await sleep(2500); await snap("installing-mid");
  const subtitle = await evalJs(`document.querySelector("h1")?.nextElementSibling?.textContent ?? ""`);
  expect(!/Check the virtual machine engine/.test(subtitle) || scenario.startsWith("fail"), "progress moves past the first stage");
  await sleep(5000); await snap("end");
  if (scenario.startsWith("fail-image")) expect(await has("could not be downloaded"), "a download failure gets download advice");
  else if (scenario.startsWith("fail-provision")) expect(await has("took too long"), "a provisioning timeout is explained");
  else expect(await has("Your machine is ready"), "the install ends ready");
  if (scenario.startsWith("fail")) {
    expect((await buttons()).includes("Retry"), "a failure offers Retry");
    await verifyDiagnostics("the install failure", (report) =>
      Boolean(report?.lastInstall.error && report.lastInstall.steps.some((step) => step.state === "failed")));
  } else if (scenario === "mac") {
    await evalJs('import("/src/state/ui.ts").then(({ uiActions }) => uiActions.navigate({ kind: "settings", tab: "machine" }))');
    await sleep(500);
    await verifyDiagnostics("Machine settings", (report) =>
      report?.lastStart?.state === "succeeded" && report.lastInstall.steps.some((step) => step.state === "done"));
  }
}
expect(errors.length === 0, `no page errors (${errors.length})`);
if (errors.length) console.log(errors.join("\n---\n").slice(0, 4000));
ws.close(); chrome.kill(); vite.kill();
process.exit(failures.length ? 1 : 0);
