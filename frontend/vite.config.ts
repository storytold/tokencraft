import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import { readFile, readFileSync } from "node:fs";
import { createHash } from "node:crypto";
import path from "node:path";

const releaseAssets = [
  { fileName: "docs/openapi.json", source: path.resolve(__dirname, "../contracts/openapi.json") },
  { fileName: "catalog/artcraft-v1.json", source: path.resolve(__dirname, "../catalog/artcraft-v1.json") },
];

function releaseAssetPlugin(): Plugin {
  let launcherApi = false;
  let revision: string | undefined;
  return {
    name: "release-assets",
    apply: "build",
    configResolved({ env }) {
      launcherApi = env.VITE_TOKENCRAFT_PACKAGED === "true";
      if (launcherApi && env.VITE_API_BASE_URL) {
        throw new Error("A packaged console uses the launcher API port. Clear VITE_API_BASE_URL.");
      }
    },
    buildStart() {
      const configuredRevision = process.env.TOKENCRAFT_SOURCE_REVISION;
      const deploymentRevision = process.env.VERCEL_GIT_COMMIT_SHA;
      revision = configuredRevision || deploymentRevision;
      if (configuredRevision && deploymentRevision && configuredRevision !== deploymentRevision) {
        this.error("TOKENCRAFT_SOURCE_REVISION and VERCEL_GIT_COMMIT_SHA must match.");
      }
      if (!revision && process.env.TOKENCRAFT_REQUIRE_SOURCE_REVISION === "1") {
        this.error("A hosted console build needs a source revision. Expose Vercel system variables or set TOKENCRAFT_SOURCE_REVISION.");
      }
      if (revision && (revision.length !== 40 || !/^[a-f0-9]{40}$/.test(revision))) {
        this.error("Use a full lowercase Git SHA for the source revision.");
      }
      for (const { fileName, source } of releaseAssets) {
        this.addWatchFile(source);
        this.emitFile({ type: "asset", fileName, source: readFileSync(source) });
      }
    },
    generateBundle: {
      order: "post",
      handler(_options, bundle) {
        if (!revision && !launcherApi) return;
        let indexSha256: string | undefined;
        if (!launcherApi) {
          const index = bundle["index.html"];
          if (index?.type !== "asset") this.error("The hosted console build is missing index.html.");
          indexSha256 = createHash("sha256").update(index.source).digest("hex");
        }
        this.emitFile({
          type: "asset",
          fileName: "release.json",
          source: JSON.stringify({
            source_revision: revision || undefined,
            launcher_api: launcherApi || undefined,
            index_sha256: indexSha256,
          }) + "\n",
        });
      },
    },
  };
}

export default defineConfig({
  plugins: [
    react(),
    releaseAssetPlugin(),
    {
      name: "serve-release-assets",
      configureServer(server) {
        server.middlewares.use((request, response, next) => {
          const pathname = request.url?.split("?", 1)[0];
          const asset = releaseAssets.find(({ fileName }) => pathname === `/${fileName}`);
          if (!asset || (request.method !== "GET" && request.method !== "HEAD")) {
            return next();
          }
          readFile(asset.source, (error, bytes) => {
            if (error) return next(error);
            response.setHeader("Content-Type", "application/json");
            response.setHeader("Content-Length", bytes.length);
            response.setHeader("Cache-Control", "no-cache");
            response.end(request.method === "HEAD" ? undefined : bytes);
          });
        });
      },
    },
  ],
  resolve: {
    alias: { "@": path.resolve(__dirname, "src") },
  },
  server: { port: 3000 },
});
