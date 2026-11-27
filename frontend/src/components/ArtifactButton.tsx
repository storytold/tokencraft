import { useEffect, useRef, useState } from "react";
import { artifactFilename, readArtifact, type ArtifactReference } from "@/lib/artifacts";
import { downloadBlob } from "@/lib/download";

export function ArtifactButton({
  path, label, filename, reference, contentType, onPreview, className,
}: {
  path: string;
  label: string;
  filename?: string;
  reference?: ArtifactReference;
  contentType?: string;
  onPreview?: (file: Blob) => void;
  className?: string;
}) {
  const [progress, setProgress] = useState<"loading" | "waiting">();
  const [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);

  useEffect(() => () => {
    const pending = request.current;
    request.current = null;
    pending?.abort();
  }, [path]);

  async function open() {
    if (request.current) return;
    const transfer = new AbortController();
    request.current = transfer;
    setError("");
    setProgress("loading");
    try {
      const file = await readArtifact(path, transfer.signal, (waiting) => {
        setProgress(waiting ? "waiting" : "loading");
      }, reference, contentType);
      if (transfer.signal.aborted) return;
      if (onPreview) onPreview(file);
      else downloadBlob(file, artifactFilename(filename ?? "tokencraft-output", file.type));
    } catch (cause) {
      if (!transfer.signal.aborted) {
        setError(cause instanceof Error && !(cause instanceof TypeError)
          ? cause.message : "Could not load this output. Try again.");
      }
    } finally {
      if (request.current === transfer) {
        request.current = null;
        setProgress(undefined);
      }
    }
  }

  function cancel() {
    const pending = request.current;
    request.current = null;
    pending?.abort();
    setProgress(undefined);
  }

  return <div className="min-w-0 space-y-1">
    <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
      <button type="button" className={className ?? "text-left font-medium text-brand hover:underline disabled:opacity-60"}
        disabled={!!progress} onClick={() => void open()}>{label}</button>
      {progress && <button type="button" className="text-xs text-muted hover:underline"
        aria-label={`Cancel ${label.toLowerCase()}`} onClick={cancel}>Cancel</button>}
    </div>
    {progress && <p className="text-xs text-muted" role="status">
      {progress === "waiting" ? "Downloads are busy. Retrying…" : "Loading output…"}
    </p>}
    {error && <p className="text-xs text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
  </div>;
}
