import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { initPerf } from "./lib/perfMode";
import { reportFrontendDiagnostic } from "./services/ipc/common";

window.addEventListener("error", () => reportFrontendDiagnostic("scriptError"));
window.addEventListener("unhandledrejection", () => reportFrontendDiagnostic("unhandledRejection"));

// Effects level first (it also decides the macOS vibrancy, tauri.macos.conf.json).
initPerf();

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
