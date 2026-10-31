// Copies the three browser libraries the pages load — OvenPlayer, hls.js and
// the LiveKit client — from node_modules into www/dist/vendor, next to the
// compiled modules.
//
// They used to come from cdn.jsdelivr.net with no integrity check, two of them
// on a floating version, into pages that hold the admin's session token. Pinned
// here instead (exact versions in package.json, hashes in package-lock.json) and
// served from our own origin, which is what lets the CSP say `script-src
// 'self'`.
//
// Usage: node vendor.mjs [outDir]   (default ../www/dist, tsconfig's outDir)

import { copyFileSync, existsSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const outDir = join(process.argv[2] ?? join(here, '..', 'www', 'dist'), 'vendor');

// [package file, name served under /dist/vendor/]
const FILES = [
  ['ovenplayer/dist/ovenplayer.js', 'ovenplayer.js'],
  ['hls.js/dist/hls.min.js', 'hls.min.js'],
  ['livekit-client/dist/livekit-client.umd.js', 'livekit-client.umd.js'],
];

mkdirSync(outDir, { recursive: true });
for (const [from, to] of FILES) {
  const src = join(here, 'node_modules', from);
  copyFileSync(src, join(outDir, to));
  // Each ends in a sourceMappingURL comment; ship the map so devtools finds it
  // rather than logging a 404.
  if (existsSync(`${src}.map`)) copyFileSync(`${src}.map`, join(outDir, `${to}.map`));
}
console.log(`vendor: ${FILES.length} libraries -> ${outDir}`);
