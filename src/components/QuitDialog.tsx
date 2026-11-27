import { Button, Checkbox, Modal } from "@mantine/core";
import { IconMoon, IconPower, IconServer } from "@tabler/icons-react";
import { useEffect, useState } from "react";
import { lifecycleIpc } from "../services/ipc/lifecycle";
import { toIpcError } from "../services/ipc/common";
import { useApp } from "../state/store";
import { uiActions } from "../state/ui";
import { readQuitPref, writeQuitPref } from "../lib/quitPref";
import { useI18n } from "../i18n";

/**
 * Quit never ends the machine silently. When the app is asked to leave
 * (⌘Q, the tray's Quit) and the VM runs: quit fully (stop the machine), keep
 * everything running in the background, or cancel. The answer can be
 * remembered (Settings → General → When quitting).
 */
export function QuitDialog() {
  const { t } = useI18n();
  const machine = useApp((state) => state.machine);
  const working = useApp((state) => state.sessions.filter((s) => s.status.state === "running").length);
  const running = machine?.state === "running";
  const [opened, setOpened] = useState(false);
  const [busy, setBusy] = useState<"stop" | "background" | null>(null);
  const [remember, setRemember] = useState(false);

  async function choose(choice: "stop" | "background", persist: boolean) {
    if (persist) writeQuitPref(choice);
    setBusy(choice);
    try {
      if (choice === "stop") await lifecycleIpc.quit(true);
      else { await lifecycleIpc.background(); setOpened(false); }
    } catch (error) {
      uiActions.toast(toIpcError(error).message, "error");
    } finally {
      setBusy(null);
    }
  }

  useEffect(() => lifecycleIpc.onQuitRequested(() => {
    // Nothing running: nothing to protect, leave at once.
    if (!running) { void lifecycleIpc.quit(false); return; }
    const pref = readQuitPref();
    if (pref === "ask") { setRemember(false); setOpened(true); } else void choose(pref, false);
  }), [running]);

  const option = "press grid w-full cursor-pointer grid-cols-[32px_minmax(0,1fr)] items-center gap-3 rounded-xl border border-line bg-surface/70 p-3 text-left hover:border-accent/50 disabled:cursor-default disabled:opacity-60";
  return (
    <Modal opened={opened} onClose={() => setOpened(false)} title={t("quit.title")} size="sm" closeOnClickOutside={busy == null}>
      <p className="m-0 mb-3 flex items-start gap-2 text-[13px] leading-relaxed text-muted">
        <IconServer size={16} className="mt-0.5 shrink-0 text-ok" />
        <span>
          {t("quit.runningBody", { tasks: working > 0 ? t("quit.runningTasks", { count: working }) : "" })}
        </span>
      </p>
      <div className="grid gap-2">
        <button type="button" className={option} disabled={busy != null} onClick={() => void choose("stop", remember)}>
          <span className="grid size-8 place-items-center rounded-lg bg-bad/13 text-bad"><IconPower size={16} /></span>
          <span className="grid"><strong className="text-[13px]">{t("quit.stopTitle")}</strong><span className="text-xs text-muted">{t("quit.stopHint")}</span></span>
        </button>
        <button type="button" className={option} disabled={busy != null} onClick={() => void choose("background", remember)}>
          <span className="grid size-8 place-items-center rounded-lg bg-accent-soft text-accent"><IconMoon size={16} /></span>
          <span className="grid"><strong className="text-[13px]">{t("quit.bgTitle")}</strong><span className="text-xs text-muted">{t("quit.bgHint")}</span></span>
        </button>
      </div>
      <div className="mt-4 flex items-center justify-between gap-3">
        <Checkbox size="xs" label={t("quit.remember")} checked={remember} onChange={(e) => setRemember(e.currentTarget.checked)} />
        <Button variant="default" size="xs" disabled={busy != null} onClick={() => setOpened(false)}>{t("common.cancel")}</Button>
      </div>
    </Modal>
  );
}
