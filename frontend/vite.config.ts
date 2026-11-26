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
        for (const { fileName, source } of releaseAssets) {
          this.addWatchFile(source);
          this.emitFile({ type: "asset", fileName, source: readFileSync(source) });
        }
        const revision = process.env.TOKENCRAFT_SOURCE_REVISION || process.env.VERCEL_GIT_COMMIT_SHA;
        if (revision) {
          if (!/^[a-f0-9]{40}$/.test(revision)) this.error("Use a full lowercase Git SHA for the source revision.");
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
