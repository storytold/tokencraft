/// <reference types="vite/client" />

interface ImportMetaEnv {
  /** Explicit gateway URL or "/" for a same-origin API proxy. */
  readonly VITE_API_BASE_URL?: string;
  readonly VITE_TOKENCRAFT_PACKAGED?: "true";
}

interface ImportMeta {
  readonly env: ImportMetaEnv;
}
