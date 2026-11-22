import { Button, Radio, TextInput } from "@mantine/core";
import { IconRefresh } from "@tabler/icons-react";
import { useCallback, useEffect, useState } from "react";
import type { MirrorResult } from "../services/ipc/generated/core";
import { machineIpc } from "../services/ipc/app";
import { toIpcError } from "../services/ipc/common";

/** The working mirror with the best measured speed (does not rely on order). */
function fastestMirror(results: MirrorResult[]): MirrorResult | undefined {
  return results
    .filter((r) => r.status === "ok")
    .reduce<MirrorResult | undefined>((best, r) => (!best || (r.speedKbps ?? 0) > (best.speedKbps ?? 0) ? r : best), undefined);
}

function statusPill(result: MirrorResult) {
  if (result.status === "ok") return <span className="pill ok">ARM64 ok</span>;
  if (result.status === "noArm64") return <span className="pill warn">No ARM64</span>;
  return <span className="pill bad">Unreachable</span>;
}

/**
 * Tests the system's ARM64 package mirrors and lets the user pick one.
 * `value` is the chosen mirror URI, or null for the image default (the
 * result the backend marks `default`).
 */
export function MirrorPicker({
  os,
  value,
  onChange,
  autoPick = true,
  heading = true,
}: {
  /** The guest system (`null`: the default system). */
  os: string | null;
  value: string | null;
  onChange: (uri: string | null) => void;
  autoPick?: boolean;
  /** Show the picker's own title (off when the page already has one). */
  heading?: boolean;
}) {
  const [results, setResults] = useState<MirrorResult[] | null>(null);
  const [testing, setTesting] = useState(false);
  const [custom, setCustom] = useState("");
  const [error, setError] = useState<string | null>(null);

  const run = useCallback(
    async (extra: string[] = []) => {
      setTesting(true);
      setError(null);
      try {
        const tested = await machineIpc.testMirrors(os, extra);
        setResults(tested);
        const best = fastestMirror(tested);
        if (autoPick && best && value == null) onChange(best.default ? null : best.uri);
      } catch (err) {
        setError(toIpcError(err).message);
      } finally {
        setTesting(false);
      }
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [autoPick, os],
  );

  useEffect(() => {
    void run();
  }, [run]);

  const fastest = results ? fastestMirror(results)?.uri : undefined;
  const defaultUri = results?.find((r) => r.default)?.uri ?? "";
  const selected = value ?? defaultUri;

  return (
    <div className="mirror-picker">
      <div className="row">
        <strong className="grow fs-13">{heading ? "APT mirror for the VM" : ""}</strong>
        <Button size="compact-xs" variant="subtle" leftSection={<IconRefresh size={13} />} loading={testing} onClick={() => run(custom ? [custom] : [])}>
          Test again
        </Button>
      </div>
      <p className="faint" style={{ fontSize: 12, margin: "2px 0 8px" }}>
        The VM is ARM64. Each mirror is checked for this system's ARM64 packages and timed by downloading its
        package index.
      </p>
      {error && <div className="callout bad">{error}</div>}
      {results == null ? (
        <div className="faint fs-13">Testing mirrors…</div>
      ) : (
        <Radio.Group value={selected} onChange={(uri) => onChange(uri === defaultUri ? null : uri)}>
          <div className="list">
            {results.map((result) => (
              <label key={result.uri} className={`list-row mirror-row${result.status !== "ok" ? " disabled" : ""}`}>
                <Radio value={result.uri} disabled={result.status !== "ok"} aria-label={result.host} />
                <div className="grow">
                  <div className="row" style={{ gap: 6 }}>
                    <span>{result.host}</span>
                    {result.default && <span className="faint fs-12">(default)</span>}
                    {result.uri === fastest && <span className="pill info">Fastest</span>}
                  </div>
                  <div className="faint truncate fs-12">
                    {result.detail}
                  </div>
                </div>
                <span className="faint mono" style={{ fontSize: 12, whiteSpace: "nowrap" }}>
                  {result.speedKbps != null ? `${result.speedKbps} KB/s` : "—"}
                  {result.latencyMs != null ? ` · ${result.latencyMs} ms` : ""}
                </span>
                {statusPill(result)}
              </label>
            ))}
          </div>
        </Radio.Group>
      )}
      <div className="row" style={{ marginTop: 8 }}>
        <TextInput
          className="grow"
          size="xs"
          placeholder="Another mirror, e.g. https://mirror.example.com/ubuntu-ports"
          value={custom}
          onChange={(e) => setCustom(e.currentTarget.value)}
        />
        <Button size="xs" variant="default" disabled={!custom.trim() || testing} onClick={() => run([custom])}>
          Test
        </Button>
      </div>
    </div>
  );
}
