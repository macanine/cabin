import { SpiceMainConn } from "./src/main.js";

const screen = document.getElementById("screen");
const parentOrigin = new URLSearchParams(location.search).get("parentOrigin");
const targetOrigin = parentOrigin === "null" ? "*" : parentOrigin;
const type = "cabin-spice";
let connection;
let timer;
let ready = false;
let stopped = false;

function notify(state, message) {
  if (targetOrigin) window.parent.postMessage({ type, state, message }, targetOrigin);
}

function stop() {
  if (stopped) return;
  stopped = true;
  clearInterval(timer);
  connection?.stop();
  connection = undefined;
}

function fail() {
  stop();
  if (ready) notify("disconnected");
  else notify("error", "SPICE did not connect to the guest desktop.");
}

window.addEventListener("message", (event) => {
  if (event.source !== window.parent || event.origin !== parentOrigin) return;
  if (event.data?.type === type && event.data.action === "disconnect") stop();
});
window.addEventListener("pagehide", stop);
window.addEventListener("keydown", (event) => {
  if (!ready || !event.metaKey || event.code !== "KeyV") return;
  event.preventDefault();
  event.stopImmediatePropagation();
  if (!event.repeat) notify("paste");
}, true);

function fit(canvas) {
  const scale = Math.min(window.innerWidth / canvas.width, window.innerHeight / canvas.height);
  screen.style.width = `${canvas.width * scale}px`;
  screen.style.height = `${canvas.height * scale}px`;
}

try {
  const uri = new URL("./ws", location.href);
  uri.protocol = "ws:";
  connection = new SpiceMainConn({ uri: uri.href, screen_id: screen.id, onerror: fail });
  const deadline = Date.now() + 12_000;
  timer = setInterval(() => {
    const display = connection?.display;
    const surface = display?.surfaces[display.primary_surface];
    if (surface?.canvas) fit(surface.canvas);
    if (!ready && surface?.draw_count > 0 && connection.inputs?.state === "ready") {
      ready = true;
      notify("ready");
    } else if (!ready && Date.now() >= deadline) {
      fail();
    }
  }, 100);
} catch {
  fail();
}
