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
