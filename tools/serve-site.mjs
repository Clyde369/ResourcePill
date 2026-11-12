// Preview the website in docs/ locally: `node tools/serve-site.mjs` then open http://localhost:4173
// Built-in modules only; serves files from docs/ and nothing outside it.
import http from 'node:http';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', 'docs');
const port = Number(process.env.PORT) || 4173;
const types = { '.html': 'text/html; charset=utf-8', '.css': 'text/css', '.png': 'image/png', '.svg': 'image/svg+xml', '.js': 'text/javascript' };

http.createServer((req, res) => {
  const url = decodeURIComponent(new URL(req.url, 'http://x').pathname);
  const file = path.join(root, url.endsWith('/') ? url + 'index.html' : url);
  if (!file.startsWith(root + path.sep)) return res.writeHead(403).end();
  fs.readFile(file, (err, data) => {
    if (err) return res.writeHead(404, { 'Content-Type': 'text/plain' }).end('Not found');
    res.writeHead(200, { 'Content-Type': types[path.extname(file)] || 'application/octet-stream' }).end(data);
  });
}).listen(port, '127.0.0.1', () => console.log(`Peter the Pill site: http://localhost:${port}`));
