import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";

const root = new URL("../", import.meta.url);
const design = readFileSync(new URL("frontend/public/design/index.html", root), "utf8").replace(/\r\n?/g, "\n");
const hashes = (tag) => [...design.matchAll(new RegExp(`<${tag}\\b[^>]*>([\\s\\S]*?)</${tag}>`, "gi"))]
  .filter(([, block]) => block.trim())
  .map(([, block]) => `'sha256-${createHash("sha256").update(block).digest("base64")}'`);
const identity = "https://accounts.google.com/gsi/";
const contentPolicy = [
  "default-src 'none'",
  "base-uri 'none'",
  "object-src 'none'",
  "frame-ancestors 'none'",
  "form-action 'self'",
  ["script-src", "'self'", identity + "client", ...hashes("script")].join(" "),
  ["style-src", "'self'", "https://fonts.googleapis.com", identity + "style", ...hashes("style")].join(" "),
  "font-src 'self' https://fonts.gstatic.com",
  "img-src 'self' https: data: blob:",
  "media-src 'self' https: blob:",
  `connect-src 'self' ${identity}`,
  `frame-src ${identity}`,
].join("; ");
const source = "/((?!v1(?:/|$)|ready(?:/|$)).*)";
const headers = [
  { key: "Content-Security-Policy", value: contentPolicy },
  { key: "X-Content-Type-Options", value: "nosniff" },
  { key: "X-Frame-Options", value: "DENY" },
  { key: "Referrer-Policy", value: "strict-origin-when-cross-origin" },
];

const proxyCache = [{ key: "Vercel-CDN-Cache-Control", value: "no-store" }];
const rules = [
  { source: "/v1/:path*", headers: proxyCache },
  { source: "/ready", headers: proxyCache },
  { source, headers },
];

const write = process.argv[2] === "--write";
for (const filename of ["vercel.json", "frontend/vercel.json"]) {
  const path = new URL(filename, root);
  const config = JSON.parse(readFileSync(path, "utf8"));
  const existing = config.headers ?? [];
  const matches = rules.every((rule) => {
    const entries = existing.filter(({ source: pattern }) => pattern === rule.source);
    return entries.length === 1 && JSON.stringify(entries[0]) === JSON.stringify(rule);
  });
  if (matches) continue;
  if (!write) throw new Error(`${filename} has stale console or proxy security headers. Run node scripts/console_security_headers.mjs --write.`);
  config.headers = [
    ...existing.filter(({ source: pattern }) => !rules.some((rule) => rule.source === pattern)),
    ...rules,
  ];
  writeFileSync(path, JSON.stringify(config, null, 2) + "\n");
}
