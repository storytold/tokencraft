import { useEffect, useRef, useState } from "react";
import { API_BASE_URL } from "@/lib/api";
import { artifactFilename, readArtifact, type ArtifactReference, type ArtifactRetryReason } from "@/lib/artifacts";
import { downloadBlob } from "@/lib/download";

type ArtifactButtonProps = {
  path: string;
  label: string;
  filename?: string;
  reference?: ArtifactReference;
  contentType?: string;
  onPreview?: (file: Blob) => void;
  className?: string;
};

export function ArtifactButton(props: ArtifactButtonProps) {
  return <ArtifactTransferButton key={props.path} {...props} />;
}

function ArtifactTransferButton({
  path, label, filename, reference, contentType, onPreview, className,
}: ArtifactButtonProps) {
  const [progress, setProgress] = useState<"loading" | ArtifactRetryReason>();
  const [error, setError] = useState("");
  const request = useRef<AbortController | null>(null);

  useEffect(() => () => {
    const pending = request.current;
    request.current = null;
    pending?.abort();
  }, []);

  async function open() {
    if (request.current) return;
    const transfer = new AbortController();
    request.current = transfer;
    setError("");
    setProgress("loading");
    try {
      const file = await readArtifact(API_BASE_URL + path, transfer.signal, (reason) => {
        if (request.current === transfer) setProgress(reason ?? "loading");
      }, reference, contentType);
      if (transfer.signal.aborted) return;
      if (onPreview) onPreview(file);
      else downloadBlob(file, artifactFilename(filename ?? "tokencraft-output", file.type));
    } catch (cause) {
      if (!transfer.signal.aborted) {
        if (cause instanceof DOMException && cause.name === "TimeoutError") {
          setError("The download timed out. Try downloading this output again.");
        } else {
          setError(cause instanceof Error && !(cause instanceof TypeError)
            ? cause.message : "Could not load this output. Try again.");
        }
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
      {progress === "busy" ? "Downloads are busy. Retrying…"
        : progress === "interrupted" ? "Connection interrupted. Retrying…" : "Loading output…"}
    </p>}
    {error && <p className="text-xs text-rose-600 dark:text-rose-300" role="alert">{error}</p>}
  </div>;
}
