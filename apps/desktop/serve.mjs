// Preview server for `apps/desktop`. Not part of the shipped application.
//
// The app itself is loaded by Tauri from disk over a custom protocol, which is
// why there is no bundler and no build step. But browsers refuse to load ES
// modules over `file://` (module scripts are subject to CORS), so previewing
// the same files in a browser needs an origin. This is the smallest thing that
// provides one, with no dependencies.
//
//   node serve.mjs [port]
//
// then open http://127.0.0.1:8787
//
// It binds to loopback only. It is a preview convenience, not a server, and it
// exposes exactly the directory it is run from.

import { createServer } from "node:http";
import { readFile, stat } from "node:fs/promises";
import { extname, join, normalize, sep } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = fileURLToPath(new URL(".", import.meta.url));
const PORT = Number(process.argv[2] ?? 8787);

const TYPES = {
  ".html": "text/html; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
};

createServer(async (req, res) => {
  const url = new URL(req.url, "http://127.0.0.1");
  let rel = decodeURIComponent(url.pathname);
  if (rel === "/") rel = "/index.html";

  // Refuse anything that escapes ROOT. `normalize` collapses `..` segments
  // before the prefix test, so this is not bypassable with encoded traversal.
  const target = normalize(join(ROOT, rel));
  if (target !== ROOT && !target.startsWith(ROOT.endsWith(sep) ? ROOT : ROOT + sep)) {
    res.writeHead(403).end("forbidden");
    return;
  }

  try {
    const info = await stat(target);
    if (info.isDirectory()) {
      res.writeHead(404).end("not found");
      return;
    }
    const body = await readFile(target);
    res.writeHead(200, {
      "content-type": TYPES[extname(target)] ?? "application/octet-stream",
      "cache-control": "no-store",
    });
    res.end(body);
  } catch {
    res.writeHead(404, { "content-type": "text/plain; charset=utf-8" }).end("not found");
  }
}).listen(PORT, "127.0.0.1", () => {
  console.log(`ifami desktop preview → http://127.0.0.1:${PORT}`);
});
