import { Button, NumberInput } from "@mantine/core";
import { useState } from "react";
import { Section } from "../../components/common";
import { diskGb } from "../../lib/installForm";
import { machineIpc } from "../../services/ipc/app";
import { toIpcError } from "../../services/ipc/common";
import type { MachineDetails } from "../../services/ipc/generated/common";
import type { MachineStatus } from "../../services/ipc/generated/core";
import { useI18n } from "../../i18n";

const integerIn = (value: string | number, min: number, max: number): value is number =>
  typeof value === "number" && Number.isInteger(value) && value >= min && value <= max;

export function MachineResourceEditor({ machine, details, onSaved }: {
  machine: MachineStatus;
  details: MachineDetails;
  onSaved: () => void;
}) {
  if (machine.cpus == null || machine.memoryMb == null) return null;
  const pending = details.pendingResources;
  const cpus = pending?.cpus ?? machine.cpus;
  const memoryMb = pending?.memoryMb ?? machine.memoryMb;
  const nextDisk = pending?.diskGib ?? null;
  return <ResourceForm key={`${cpus}:${memoryMb}:${nextDisk}`} cpus={cpus} memoryMb={memoryMb}
    currentDisk={diskGb(machine.diskSize)} nextDisk={nextDisk}
    pending={pending != null} notice={details.diskGrowthNotice} running={machine.state === "running"}
    busy={machine.busy != null} onSaved={onSaved} />;
}

function ResourceForm({ cpus, memoryMb, currentDisk, nextDisk, pending, notice, running, busy, onSaved }: {
  cpus: number;
  memoryMb: number;
  currentDisk: number | null;
  nextDisk: number | null;
  pending: boolean;
  notice: string | null;
  running: boolean;
  busy: boolean;
  onSaved: () => void;
}) {
  const { t } = useI18n();
  const [cpu, setCpu] = useState<number | string>(cpus);
  const [memory, setMemory] = useState<number | string>(memoryMb);
  const [disk, setDisk] = useState<number | string>(nextDisk ?? "");
  const [working, setWorking] = useState<"save" | "cancel" | null>(null);
  const [error, setError] = useState<string | null>(null);
  const disabled = busy || working != null;
  const diskMinimum = nextDisk ?? (currentDisk == null ? 1 : currentDisk + 1);
  const verifiesDisk = nextDisk != null && currentDisk === nextDisk;
  const valid = integerIn(cpu, 1, 64) && integerIn(memory, 512, 65536)
    && (disk === "" || integerIn(disk, diskMinimum, 2048));
  const changed = cpu !== cpus || memory !== memoryMb || (disk !== "" && disk !== nextDisk);

  async function submit(cancelDisk: boolean) {
    if (disabled || (!cancelDisk && (!valid || !changed))) return;
    setWorking(cancelDisk ? "cancel" : "save");
    setError(null);
    try {
      await machineIpc.saveResources(cancelDisk ? { cpus, memoryMb, diskGib: null } : {
        cpus: Number(cpu), memoryMb: Number(memory), diskGib: disk === "" ? null : Number(disk),
      }, cancelDisk);
      onSaved();
    } catch (cause) {
      setError(toIpcError(cause).message);
    } finally {
      setWorking(null);
    }
  }

  return (
    <Section title={t("machine.alloc.title")} hint={t("machine.alloc.hint")}>
      <form className="grid gap-3" onSubmit={(event) => { event.preventDefault(); void submit(false); }}>
        <div className="grid gap-3 sm:grid-cols-3">
          <NumberInput label={t("machine.alloc.vcpus")} min={1} max={64} allowDecimal={false} clampBehavior="none"
            value={cpu} onChange={setCpu} disabled={disabled} />
          <NumberInput label={t("machine.alloc.memory")} min={512} max={65536} step={512} allowDecimal={false} clampBehavior="none"
            value={memory} onChange={setMemory} disabled={disabled} />
          <NumberInput label={t("machine.alloc.disk")} placeholder={currentDisk == null ? t("machine.alloc.noChange") : t("machine.alloc.currentDisk", { gb: currentDisk })}
            min={diskMinimum} max={2048} allowDecimal={false} clampBehavior="none" value={disk} onChange={setDisk}
            disabled={disabled || verifiesDisk} />
        </div>
        <p className="m-0 text-sm text-muted">{t("machine.alloc.diskNote")}</p>
        {!valid && <p className="m-0 text-sm text-bad" role="status">{t("machine.alloc.invalid", { min: diskMinimum })}</p>}
        {pending && <p className="m-0 text-sm text-muted" role="status">
          {verifiesDisk ? "The virtual disk has grown. Guest filesystem expansion is still pending; stop and start the machine to retry."
            : `${t("machine.alloc.save")}: ${cpus} vCPUs, ${memoryMb} MiB${nextDisk ? `, ${nextDisk} GiB` : ""}.`}
        </p>}
        {notice && <p className="m-0 text-sm text-warn [overflow-wrap:anywhere]" role="status">{notice}</p>}
        {error && <p className="m-0 text-sm text-bad [overflow-wrap:anywhere]" role="alert">{error}</p>}
        <div className="flex flex-wrap gap-2">
          <Button type="submit" size="xs" loading={working === "save"} disabled={disabled || !valid || !changed}>{t("machine.alloc.save")}</Button>
          {nextDisk != null && !verifiesDisk && !running && <Button type="button" size="xs" variant="default" loading={working === "cancel"}
            disabled={disabled} onClick={() => void submit(true)}>{t("machine.alloc.cancelDisk")}</Button>}
        </div>
      </form>
    </Section>
  );
}
