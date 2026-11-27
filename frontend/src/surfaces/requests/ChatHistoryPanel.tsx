import { useEffect, useId, useRef, useState, type ReactNode } from "react";
import { ArtifactButton } from "@/components/ArtifactButton";
import { Button } from "@/components/ui";
import { ApiError } from "@/lib/api";
import { scheduleDeadline } from "@/lib/deadline";
import { errorMessage, gatewayRequest, withProject, type ProjectScope } from "@/lib/gateway";
import type { ProviderResult } from "@/surfaces/comparison/api";

type Phase = "processing" | "available" | "archiving" | "archived" | "expired" | "removed";
type RestoreProgress = {
  state: "pending" | "ready" | "failed";
  ready_objects: number;
  total_objects: number;
  retry_at: string;
};
type Metadata = {
  phase: Phase;
  archive_at: string | null;
  expires_at: string | null;
  media?: boolean;
  can_restore?: boolean;
  restore?: RestoreProgress;
};
type SavedContent = Metadata & {
  input?: { prompt?: string; messages?: { role: string; content: string }[] };
  result?: ProviderResult["result"];
};

const phaseLabels: Record<Phase, string> = {
  processing: "Processing",
  available: "Available",
  archiving: "Archiving",
  archived: "Archived",
  expired: "Retention ended",
  removed: "Removed",
};
const dateFormat = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });

export function ChatHistoryPanel({ scope, id, status, fallback }: {
  scope: ProjectScope; id: string; status: string; fallback: ReactNode;
}) {
  const heading = useId();
  const revealed = useId();
  const { account_id, project_id } = scope;
  const base = `/v1/requests/${encodeURIComponent(id)}/content`;
  const scoped = withProject(base, project_id, account_id);
  const metadataPath = `${scoped}${scoped.includes("?") ? "&" : "?"}metadata_only=true`;
  const [revision, setRevision] = useState(0);
  const [metadata, setMetadata] = useState<Metadata | null>();
  const [loadError, setLoadError] = useState("");
  const [content, setContent] = useState<SavedContent>();
  const [revealError, setRevealError] = useState("");
  const [busy, setBusy] = useState<"read" | "restore">();
  const pending = useRef<AbortController | null>(null);

  useEffect(() => {
    const controller = new AbortController();
    setMetadata(undefined);
    setLoadError("");
    setContent(undefined);
    setRevealError("");
    setBusy(undefined);
    gatewayRequest<Metadata>(metadataPath, {
      signal: controller.signal, cache: "no-store",
    }).then((result) => {
      if (!controller.signal.aborted) setMetadata(result);
    }).catch((error) => {
      if (controller.signal.aborted) return;
      if (error instanceof ApiError && error.status === 404) setMetadata(null);
      else setLoadError(errorMessage(error));
    });
    return () => {
      controller.abort();
      pending.current?.abort();
      pending.current = null;
    };
  }, [metadataPath, status, revision]);

  const phase = metadata?.phase;
  const restoreState = metadata?.restore?.state;
  useEffect(() => {
    if (phase !== "archived" || restoreState !== "pending") return;
    const controller = new AbortController();
    let timer = window.setTimeout(refresh, 15_000);
    async function refresh() {
      try {
        const result = await gatewayRequest<Metadata>(metadataPath, { signal: controller.signal, cache: "no-store" });
        if (controller.signal.aborted) return;
        setMetadata(result);
        if (result.phase !== "archived") {
          setContent(undefined);
          setRevealError("");
        }
        if (result.restore?.state !== "pending") return;
      } catch (error) {
        if (controller.signal.aborted) return;
        if (error instanceof ApiError && [401, 403, 404].includes(error.status)) {
          setRevision((value) => value + 1);
          return;
        }
      }
      timer = window.setTimeout(refresh, 15_000);
    }
    return () => {
      controller.abort();
      window.clearTimeout(timer);
    };
  }, [metadataPath, phase, restoreState]);
  const deadline = phase === "available" ? metadata?.archive_at : phase === "archived" ? metadata?.expires_at : null;
  useEffect(() => {
    const cutoff = deadline ? Date.parse(deadline) : NaN;
    if (!Number.isFinite(cutoff)) return;
    let refreshTimer: number | undefined;
    const cancel = scheduleDeadline(cutoff, () => {
      pending.current?.abort();
      pending.current = null;
      setBusy(undefined);
      setContent(undefined);
      // Give the gateway time to cross the same boundary if its clock is behind.
      refreshTimer = window.setTimeout(() => setRevision((value) => value + 1), 1_000);
    });
    return () => {
      cancel();
      window.clearTimeout(refreshTimer);
    };
  }, [deadline, revision]);

  async function reveal() {
    if (content) {
      setContent(undefined);
      return;
    }
    if (pending.current || (phase !== "available" && phase !== "archived")) return;
    const controller = new AbortController();
    pending.current = controller;
    const archived = phase === "archived";
    const path = withProject(archived ? `${base}/retrieve` : base, project_id, account_id);
    setBusy("read");
    setRevealError("");
    try {
      const result = await gatewayRequest<SavedContent>(path, {
        method: archived ? "POST" : "GET", signal: controller.signal, cache: "no-store",
      });
      if (controller.signal.aborted) return;
      const cutoff = archived ? result.expires_at : result.archive_at;
      if (result.phase !== phase || (cutoff && Date.parse(cutoff) <= Date.now())) {
        setRevision((value) => value + 1);
        return;
      }
      setContent(result);
    } catch (error) {
      if (controller.signal.aborted) return;
      if (error instanceof ApiError && error.status === 404) setRevision((value) => value + 1);
      else if (error instanceof ApiError && error.code === "archive_retrieval_required" && metadata?.can_restore) {
        setRevealError("This archive is in cold storage. Start a restore to make it available.");
      } else setRevealError(errorMessage(error));
    } finally {
      if (pending.current === controller) {
        pending.current = null;
        setBusy(undefined);
      }
    }
  }

  async function startRestore() {
    if (pending.current || !metadata?.can_restore || phase !== "archived") return;
    const controller = new AbortController();
    pending.current = controller;
    setBusy("restore");
    setRevealError("");
    try {
      const restore = await gatewayRequest<RestoreProgress>(withProject(`${base}/restore`, project_id, account_id), {
        method: "POST", headers: { "Content-Type": "application/json" }, body: "{}", signal: controller.signal,
      });
      if (!controller.signal.aborted) {
        setMetadata((current) => current?.phase === "archived" ? { ...current, restore } : current);
      }
    } catch (error) {
      if (!controller.signal.aborted) setRevealError(errorMessage(error));
    } finally {
      if (pending.current === controller) {
        pending.current = null;
        setBusy(undefined);
      }
    }
  }

  if (metadata === null) return fallback;
  if (!metadata) return loadError ? (
    <div className="flex flex-wrap items-center gap-2">
      <p role="alert" className="text-sm text-rose-600 dark:text-rose-300">{loadError}</p>
      <Button type="button" variant="outline" size="sm" onClick={() => setRevision((value) => value + 1)}>Retry chat history</Button>
    </div>
  ) : null;
  const canRead = (phase === "available" || phase === "archived") &&
    (!deadline || Date.parse(deadline) > Date.now());

  const label = metadata.media ? "media" : "chat";
  return (
    <section aria-labelledby={heading} className="min-w-0 space-y-3 rounded-xl border border-border p-4">
      <div className="flex flex-wrap items-center justify-between gap-2">
        <h3 id={heading} className="text-sm font-semibold">{metadata.media ? "Media history" : "Chat history"}</h3>
        <p role="status" className="text-xs text-muted">{phaseLabels[metadata.phase]}</p>
      </div>
      <dl className="grid gap-2 text-xs sm:grid-cols-2">
        {metadata.archive_at && <HistoryDate label={phase === "available" ? "Archives on" : "Archive date"} value={metadata.archive_at} />}
        {metadata.expires_at && <HistoryDate label="Retention ends" value={metadata.expires_at} />}
      </dl>
      {canRead && <Button type="button" variant="outline" size="sm" aria-expanded={Boolean(content)}
        aria-controls={revealed} disabled={!!busy} onClick={() => void reveal()}>
        {busy === "read" ? `Reading ${label}…` : content ? `Hide ${label}` : phase === "archived" ? `Retrieve archived ${label}` : `Reveal ${label}`}
      </Button>}
      {phase === "archived" && <ArchiveRestoreControls
        progress={metadata.restore} allowed={Boolean(metadata.can_restore)} busy={busy}
        onRestore={() => void startRestore()}
      />}
      {phase === "archiving" && <Button type="button" variant="outline" size="sm" onClick={() => setRevision((value) => value + 1)}>
        Refresh archive status
      </Button>}
      {revealError && <p role="alert" className="text-sm text-rose-600 dark:text-rose-300">{revealError}</p>}
      <div id={revealed} hidden={!canRead || !content} className="space-y-3">
        {canRead && content?.input?.messages?.map((message, index) => (
          <ChatText key={index} label={message.role} text={message.content} />
        ))}
        {canRead && content?.input?.prompt && <ChatText label="Prompt" text={content.input.prompt} />}
        {canRead && content?.result && ("text" in content.result
          ? <ChatText label="Result" text={content.result.text} />
          : <MediaDownloads result={content.result} scope={scope} id={id} archived={phase === "archived"} />)}
      </div>
    </section>
  );
}

function ArchiveRestoreControls({ progress, allowed, busy, onRestore }: {
  progress?: RestoreProgress; allowed: boolean; busy?: "read" | "restore"; onRestore: () => void;
}) {
  const retry = progress && Date.parse(progress.retry_at) > Date.now();
  return <>
    {allowed && progress?.state !== "pending" && !retry && (
      <Button type="button" variant="outline" size="sm" className="ml-2" disabled={!!busy} onClick={onRestore}>
        {busy === "restore" ? "Starting restore…" : "Restore from cold storage"}
      </Button>
    )}
    {progress && <p role="status" className="text-xs text-muted">
      {progress.state === "pending" ? `Waiting for storage. ${progress.ready_objects} of ${progress.total_objects} objects verified.`
        : progress.state === "ready" ? "Restore verified. Retrieve the archive to view its contents."
        : retry ? `Restore could not be verified. Try again after ${dateFormat.format(new Date(progress.retry_at))}.`
        : "Restore could not be verified. Try again or contact your storage administrator."}
    </p>}
  </>;
}

function HistoryDate({ label, value }: { label: string; value: string }) {
  return <div>
    <dt className="text-muted">{label}</dt>
    <dd className="mt-1"><time dateTime={value}>{dateFormat.format(new Date(value))}</time></dd>
  </div>;
}

function ChatText({ label, text }: { label: string; text: string }) {
  return <div>
    <h4 className="mb-1 text-xs font-medium capitalize">{label}</h4>
    <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded-lg border border-border bg-surface2/60 p-3 font-mono text-xs leading-5 [overflow-wrap:anywhere]">{text}</pre>
  </div>;
}

function MediaDownloads({ result, scope, id, archived }: {
  result: Exclude<ProviderResult["result"], { text: string }>;
  scope: ProjectScope; id: string; archived: boolean;
}) {
  const files = "images" in result ? result.images.map((image, index) => (
    { file: image, label: `Image ${index + 1}`, filename: "tokencraft-image" }
  ))
    : "video" in result ? [{ file: result.video, label: "Video", filename: "tokencraft-video" }]
    : "audio" in result ? [{ file: result.audio, label: "Audio", filename: "tokencraft-audio" }]
    : [{ file: result.mesh, label: "3D model", filename: "tokencraft-mesh" }];
  const endpoint = `/v1/requests/${encodeURIComponent(id)}${archived ? "/content" : ""}/artifacts`;
  return (
    <ul className="space-y-1 text-sm" aria-label="Stored outputs">
      {files.map(({ file, label, filename }) => {
        if (!file.artifact) return null;
        const path = withProject(`${endpoint}/${file.artifact.slot}`, scope.project_id, scope.account_id);
        return <li key={path}>
          <ArtifactButton path={path} label={`Download ${label}`} filename={filename}
            reference={file.artifact} contentType={file.content_type} />
        </li>;
      })}
    </ul>
  );
}
