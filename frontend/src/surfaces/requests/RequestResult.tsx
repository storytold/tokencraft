import { useEffect, useState } from "react";
import { ArtifactButton } from "@/components/ArtifactButton";
import { Button } from "@/components/ui";
import { API_BASE_URL } from "@/lib/api";
import { scheduleDeadline } from "@/lib/deadline";
import { withProject, type ProjectScope } from "@/lib/gateway";
import { comparisonApi, errorMessage, type ProviderAudio, type ProviderResult, type ProviderVideo } from "@/surfaces/comparison/api";

export function RequestResult({ scope, id, expiresAt, autoLoad = false }: {
  scope: ProjectScope; id: string; expiresAt: string; autoLoad?: boolean;
}) {
  const [data, setData] = useState<ProviderResult>();
  const [error, setError] = useState("");
  const [enabled, setEnabled] = useState(autoLoad);
  const [revision, setRevision] = useState(0);
  const [expired, setExpired] = useState(() => Date.parse(expiresAt) <= Date.now());
  const [copyNotice, setCopyNotice] = useState("");

  useEffect(() => {
    const deadline = Date.parse(expiresAt);
    if (!Number.isFinite(deadline)) {
      setExpired(true);
      return;
    }
    setExpired(deadline <= Date.now());
    return scheduleDeadline(deadline, () => {
      setData(undefined);
      setExpired(true);
    });
  }, [expiresAt]);

  useEffect(() => {
    if (!enabled || expired) return;
    const controller = new AbortController();
    setError("");
    comparisonApi.result(scope, id, controller.signal).then((value) => {
      if (!controller.signal.aborted) setData(value);
    }).catch((cause) => {
      if (!controller.signal.aborted) setError(errorMessage(cause));
    });
    return () => controller.abort();
  }, [scope, id, enabled, expired, revision]);

  if (expired) return <p className="text-sm text-muted">The stored result has expired.</p>;
  const result = enabled ? data?.result : undefined;
  const loading = enabled && !result && !error;

  async function copyText(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      setCopyNotice("Copied.");
    } catch {
      setCopyNotice("Select the output text to copy it.");
    }
  }

  return (
    <section className="space-y-3" aria-label="Provider result">
      <div className="flex flex-wrap items-center gap-2">
        {!autoLoad && <Button size="sm" variant="outline" disabled={loading} onClick={() => {
          if (result) { setData(undefined); setEnabled(false); }
          else if (error) setRevision((value) => value + 1);
          else setEnabled(true);
        }}>{loading ? "Reading…" : result ? "Hide result" : "Read result"}</Button>}
        <p className="text-xs text-muted">Available until <time dateTime={expiresAt}>{new Date(expiresAt).toLocaleString()}</time></p>
      </div>
      {autoLoad && loading && <p role="status" className="text-sm text-muted">Loading output…</p>}
      {result && ("images" in result ? (
        <ul className={autoLoad ? "grid gap-3 sm:grid-cols-2" : "space-y-1 border border-border p-3 text-xs"}>
          {result.images.map((image, index) => {
            const path = image.artifact && artifactPath(id, image.artifact.slot, scope);
            const url = path ? API_BASE_URL + path : image.url;
            return <li key={path ?? index} className={autoLoad ? "min-w-0 overflow-hidden rounded-lg border border-border bg-surface2/50" : ""}>
              {autoLoad && <img className="max-h-80 w-full bg-surface2 object-contain" src={url} alt={`Generated image ${index + 1}`} loading="lazy" />}
              <div className={autoLoad ? "flex flex-wrap items-center justify-between gap-2 p-3 text-xs" : "flex flex-wrap gap-2"}>
                {image.artifact && path ? <ArtifactButton path={path} label={`Download image ${index + 1}`}
                  filename="tokencraft-image" reference={image.artifact} contentType={image.content_type} />
                  : <a href={url} target="_blank" rel="noopener noreferrer" className="font-medium text-brand hover:underline">
                    Open image {index + 1}
                  </a>}
                <span className="text-muted">{image.width} × {image.height} · {image.content_type}</span>
              </div>
            </li>;
          })}
        </ul>
      ) : "video" in result ? (
        <TimedMediaResult key={artifactPath(id, result.video.artifact.slot, scope)}
          media={result.video} path={artifactPath(id, result.video.artifact.slot, scope)} />
      ) : "audio" in result ? (
        <TimedMediaResult key={artifactPath(id, result.audio.artifact.slot, scope)}
          media={result.audio} path={artifactPath(id, result.audio.artifact.slot, scope)} />
      ) : "mesh" in result ? (
        <div className="flex flex-wrap items-center gap-3 rounded-lg border border-border p-3 text-sm">
          <ArtifactButton key={artifactPath(id, result.mesh.artifact.slot, scope)}
            path={artifactPath(id, result.mesh.artifact.slot, scope)} label="Download GLB"
            filename="tokencraft-mesh" reference={result.mesh.artifact} contentType="model/gltf-binary" />
          <span className="text-muted">
            {result.mesh.mesh_count} {result.mesh.mesh_count === 1 ? "mesh" : "meshes"}
            {" · "}{result.mesh.primitive_count} {result.mesh.primitive_count === 1 ? "primitive" : "primitives"}
          </span>
        </div>
      ) : (
        <div className="space-y-2">
          <pre className="max-h-96 overflow-auto whitespace-pre-wrap break-words rounded-lg border border-border bg-surface2/60 p-4 font-mono text-sm leading-6">{result.text}</pre>
          {autoLoad && <div className="flex items-center gap-2">
            <Button type="button" variant="outline" size="sm" onClick={() => void copyText(result.text)}>Copy text</Button>
            <p className="text-xs text-muted" role="status">{copyNotice}</p>
          </div>}
        </div>
      ))}
      {error && <div className="flex flex-wrap items-center gap-2">
        <p className="text-sm text-rose-600 dark:text-rose-300" role="alert">{error}</p>
        {autoLoad && <Button type="button" size="sm" variant="outline" onClick={() => setRevision((value) => value + 1)}>Retry output</Button>}
      </div>}
    </section>
  );
}

function TimedMediaResult({ media, path }: { media: ProviderVideo | ProviderAudio; path: string }) {
  const kind = "width" in media ? "video" : "audio";
  const mime = kind === "video" ? "video/mp4" : "audio/wav";
  const details = "width" in media ? `${media.width} × ${media.height}` : `${media.sample_rate_hz / 1000} kHz · ${media.channels === 1 ? "Mono" : "Stereo"}`;
  const [source, setSource] = useState("");
  useEffect(() => () => {
    if (source) URL.revokeObjectURL(source);
  }, [source]);

  return (
    <div className="space-y-2 border border-border p-3">
      <div className="flex flex-wrap items-center gap-3 text-xs">
        <ArtifactButton path={path} label={`Download ${kind}`} filename={`tokencraft-${kind}`}
          reference={media.artifact} contentType={mime} />
        <span className="text-muted">{details} · {(media.duration_ms / 1000).toFixed(2)}s</span>
        {!source && <ArtifactButton path={path} label={`Preview ${kind}`} reference={media.artifact}
          contentType={mime} onPreview={(file) => setSource(URL.createObjectURL(file))} />}
      </div>
      {source && (kind === "video"
        ? <video aria-label="Generated video preview" className="w-full bg-black" src={source} controls playsInline />
        : <audio aria-label="Generated audio preview" className="w-full" src={source} controls />)}
    </div>
  );
}

function artifactPath(id: string, slot: number, scope: ProjectScope) {
  return withProject(`/v1/model-requests/${encodeURIComponent(id)}/artifacts/${slot}`, scope.project_id, scope.account_id);
}
