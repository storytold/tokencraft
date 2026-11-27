import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFile, readFileSync } from "node:fs";
import path from "node:path";

const releaseAssets = [
  { fileName: "docs/openapi.json", source: path.resolve(__dirname, "../contracts/openapi.json") },
  { fileName: "catalog/artcraft-v1.json", source: path.resolve(__dirname, "../catalog/artcraft-v1.json") },
];

export default defineConfig({
  plugins: [
    react(),
    {
      name: "release-assets",
      apply: "build",
      buildStart() {
        const configuredRevision = process.env.TOKENCRAFT_SOURCE_REVISION;
        const deploymentRevision = process.env.VERCEL_GIT_COMMIT_SHA;
        const revision = configuredRevision || deploymentRevision;
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
        if (revision) {
          this.emitFile({
            type: "asset",
            fileName: "release.json",
            source: JSON.stringify({ source_revision: revision }) + "\n",
          });
        }
      },
    },
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
