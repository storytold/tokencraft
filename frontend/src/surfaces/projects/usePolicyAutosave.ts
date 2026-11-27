import { useEffect, useRef, useState } from "react";
import { ApiError } from "@/lib/api";
import { errorMessage, gatewayRequest } from "@/lib/gateway";
import { policyRequestTimeoutMs, type PolicyRevision, type RoutingPolicy } from "@/lib/routing-policy";

type Draft = RoutingPolicy | string;
type SaveStatus = {
  kind: "idle" | "pending" | "saving" | "saved" | "invalid" | "failed" | "conflict";
  message?: string;
};

const activeWrites = new Map<string, Promise<PolicyRevision>>();
const saveDelayMs = 500;

export function usePolicyAutosave(path: string) {
  const [loaded, setLoaded] = useState<PolicyRevision>();
  const [current, setCurrent] = useState<PolicyRevision>();
  const [loadError, setLoadError] = useState("");
  const [status, setStatus] = useState<SaveStatus>({ kind: "idle" });
  const [reloadNumber, setReloadNumber] = useState(0);
  const editor = useRef<{ change: (draft: Draft) => void; flush: () => void; retry: () => void }>(null);

  useEffect(() => {
    const controller = new AbortController();
    let revision: number | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let queued: RoutingPolicy | undefined;
    let lastSaved: string | undefined;
    let invalid = "";
    let blocked: "failed" | "conflict" | undefined;
    let saving = false;

    function notify(next: SaveStatus) {
      if (!controller.signal.aborted) setStatus(next);
    }

    function clearTimer() {
      clearTimeout(timer);
      timer = undefined;
    }

    async function flush() {
      clearTimer();
      if (revision === undefined || !queued || saving || blocked) return;
      const draft = queued;
      queued = undefined;
      const fingerprint = JSON.stringify(draft);
      if (lastSaved === fingerprint) {
        notify({ kind: "saved" });
        return;
      }

      saving = true;
      notify({ kind: "saving" });
      // A queued write keeps its deadline and original scope after the editor unmounts.
      const deadline = AbortSignal.timeout(policyRequestTimeoutMs);
      const write = gatewayRequest<PolicyRevision>(path, {
        method: "PUT",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ expected_revision: revision, policy: draft }),
        signal: deadline,
      });
      activeWrites.set(path, write);
      try {
        const next = await write;
        revision = next.revision;
        lastSaved = fingerprint;
        if (!controller.signal.aborted) setCurrent(next);
        notify(invalid ? { kind: "invalid", message: invalid } : { kind: queued ? "pending" : "saved" });
      } catch (error) {
        queued ??= invalid ? undefined : draft;
        blocked = error instanceof ApiError && error.status === 409 ? "conflict" : "failed";
        let message = `Changes couldn’t be saved. ${errorMessage(error)}`;
        if (blocked === "conflict") {
          message = "The policy revision changed. Reload to see the latest revision before editing again.";
        } else if (deadline.aborted) {
          message = "The gateway did not confirm the save. Retry this draft or reload the policy to check the current revision.";
        }
        notify({ kind: blocked, message });
      } finally {
        if (activeWrites.get(path) === write) activeWrites.delete(path);
        saving = false;
        if (queued && !timer && !blocked) void flush();
      }
    }

    editor.current = {
      change(draft) {
        clearTimer();
        invalid = typeof draft === "string" ? draft : "";
        queued = typeof draft === "string" ? undefined : draft;
        if (blocked === "conflict") return;
        blocked = undefined;
        if (invalid) {
          notify({ kind: "invalid", message: invalid });
          return;
        }
        notify({ kind: saving ? "saving" : "pending" });
        timer = setTimeout(() => { void flush(); }, saveDelayMs);
      },
      flush: () => { void flush(); },
      retry() {
        if (blocked !== "failed") return;
        blocked = undefined;
        if (invalid) notify({ kind: "invalid", message: invalid });
        else void flush();
      },
    };

    async function load() {
      // A change committed just before navigation belongs to the previous editor for this scope.
      let pending = activeWrites.get(path);
      while (pending && !controller.signal.aborted) {
        await pending.catch(() => undefined);
        pending = activeWrites.get(path);
      }
      if (controller.signal.aborted) return;
      const deadline = AbortSignal.timeout(policyRequestTimeoutMs);
      const signal = AbortSignal.any([controller.signal, deadline]);
      try {
        const next = await gatewayRequest<PolicyRevision>(path, { signal });
        if (controller.signal.aborted) return;
        revision = next.revision;
        setLoaded(next);
        setCurrent(next);
      } catch (error) {
        if (!controller.signal.aborted) {
          setLoadError(deadline.aborted ? "Timed out loading the routing policy." : errorMessage(error));
        }
      }
    }
    void load();

    return () => {
      editor.current = null;
      controller.abort();
      // Keep queued saves bound to their original scope when the user switches tabs or projects.
      void flush();
    };
  }, [path, reloadNumber]);

  function reload() {
    setLoaded(undefined);
    setCurrent(undefined);
    setLoadError("");
    setStatus({ kind: "idle" });
    setReloadNumber((number) => number + 1);
  }

  return {
    loaded, current, loadError, status, reload,
    change: (draft: Draft) => editor.current?.change(draft),
    flush: () => editor.current?.flush(),
    retry: () => editor.current?.retry(),
  };
}
