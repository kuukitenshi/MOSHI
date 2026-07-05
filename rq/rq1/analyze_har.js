#!/usr/bin/env node
//
// analyze_har.js — split each request's time into TRANSPORT vs SERVER (TTFB)
//
// Reads a HAR file produced by breakdown.js (recordHar) and, per host bucket
// (app / ab / ib / wallet / google / other), sums:
//
//   transport = blocked + dns + connect + send + receive   (network/data movement)
//   server    = wait                                        (TTFB ≈ server work + 1 RTT)
//
// NOTE on `ssl`: per the HAR spec the ssl time is already included inside
// `connect`, so we do NOT add it again (we only print it for information).
//
// NOTE on `wait`: TTFB still contains ONE network round-trip. If you pass the
// measured RTT for a host (e.g. --rtt-wallet 110), the tool also prints an
// estimate of the *pure* server time = max(0, wait - RTT) per request.
//
// Usage:
//   node analyze_har.js out/har_hello.har
//   node analyze_har.js out/har_demo.har --rtt-ab 250 --rtt-ib 250
//   node analyze_har.js out/har_hello.har --rtt-wallet 110 --rtt-google 20
//   node analyze_har.js out/har_hello.har --csv out/har_hello_split.csv
//
'use strict';

const fs   = require('fs');
const path = require('path');

function arg(flag, def) {
  const i = process.argv.indexOf(flag);
  return i !== -1 ? process.argv[i + 1] : def;
}

const harPath = process.argv[2];
if (!harPath || harPath.startsWith('--')) {
  console.error('Usage: node analyze_har.js <file.har> [--rtt-<bucket> ms] [--csv out.csv]');
  process.exit(1);
}
const csvOut = arg('--csv', null);

// Per-bucket RTT (ms) to subtract from `wait` for a pure-server estimate.
const rtt = {
  app:    parseFloat(arg('--rtt-app',    'NaN')),
  ab:     parseFloat(arg('--rtt-ab',     'NaN')),
  ib:     parseFloat(arg('--rtt-ib',     'NaN')),
  wallet: parseFloat(arg('--rtt-wallet', 'NaN')),
  google: parseFloat(arg('--rtt-google', 'NaN')),
  other:  parseFloat(arg('--rtt-other',  'NaN')),
};

// Same host → bucket mapping as breakdown.js, so the split lines up with the
// existing per-hop timeline.
function bucket(host) {
  host = host || '';
  if (host.includes('google.com'))                               return 'google';
  if (host.includes('hello.coop') || host.includes('hello.dev')) return 'wallet';
  if (host.includes(':4010'))                                    return 'ab';
  if (host.includes(':4020'))                                    return 'ib';
  if (host.includes(':3000'))                                    return 'app';
  return 'other';
}
function hostOf(url) { try { return new URL(url).host; } catch { return ''; } }
const pos = v => (typeof v === 'number' && v > 0 ? v : 0);

// ── Load HAR ────────────────────────────────────────────────────────────────
let har;
try {
  har = JSON.parse(fs.readFileSync(harPath, 'utf8'));
} catch (e) {
  console.error(`  Could not read/parse HAR: ${harPath}\n  ${e.message}`);
  process.exit(1);
}
const entries = (har.log && har.log.entries) || [];
if (entries.length === 0) {
  console.error('  HAR has no entries.');
  process.exit(1);
}

// ── Aggregate per bucket ────────────────────────────────────────────────────
const ORDER = ['app', 'ab', 'ib', 'wallet', 'google', 'other'];
const acc = {};
for (const b of ORDER) acc[b] = { n: 0, transport: 0, server: 0, ssl: 0, total: 0, pureServer: 0, pureN: 0 };

for (const e of entries) {
  const b = bucket(hostOf(e.request && e.request.url));
  const t = e.timings || {};
  const transport = pos(t.blocked) + pos(t.dns) + pos(t.connect) + pos(t.send) + pos(t.receive);
  const server    = pos(t.wait);
  const a = acc[b];
  a.n++;
  a.transport += transport;
  a.server    += server;
  a.ssl       += pos(t.ssl);
  a.total     += transport + server;
  if (!Number.isNaN(rtt[b])) {
    a.pureServer += Math.max(0, server - rtt[b]);
    a.pureN++;
  }
}

// ── Report ──────────────────────────────────────────────────────────────────
const f = ms => ms.toFixed(1);
const hasRtt = Object.values(rtt).some(v => !Number.isNaN(v));

console.log(`\n  HAR: ${path.basename(harPath)}   (${entries.length} requests)`);
console.log('  ' + '─'.repeat(hasRtt ? 86 : 70));
const head =
  `  ${'bucket'.padEnd(8)} ${'reqs'.padStart(5)} ` +
  `${'transport'.padStart(11)} ${'server(TTFB)'.padStart(13)} ${'total'.padStart(10)} ${'server%'.padStart(8)}` +
  (hasRtt ? `  ${'pure srv*'.padStart(10)}` : '');
console.log(head);
console.log('  ' + '─'.repeat(hasRtt ? 86 : 70));

const sum = { n: 0, transport: 0, server: 0, total: 0, pureServer: 0, pureN: 0 };
const csvRows = ['bucket,reqs,transport_ms,server_ttfb_ms,total_ms,server_pct,pure_server_ms'];

for (const b of ORDER) {
  const a = acc[b];
  if (a.n === 0) continue;
  const pct = a.total > 0 ? (a.server / a.total) * 100 : 0;
  const pure = a.pureN > 0 ? f(a.pureServer) : '';
  console.log(
    `  ${b.padEnd(8)} ${String(a.n).padStart(5)} ` +
    `${f(a.transport).padStart(11)} ${f(a.server).padStart(13)} ${f(a.total).padStart(10)} ${(pct.toFixed(0)+'%').padStart(8)}` +
    (hasRtt ? `  ${pure.padStart(10)}` : '')
  );
  csvRows.push(`${b},${a.n},${f(a.transport)},${f(a.server)},${f(a.total)},${pct.toFixed(1)},${pure}`);
  sum.n += a.n; sum.transport += a.transport; sum.server += a.server;
  sum.total += a.total; sum.pureServer += a.pureServer; sum.pureN += a.pureN;
}

console.log('  ' + '─'.repeat(hasRtt ? 86 : 70));
const totalPct = sum.total > 0 ? (sum.server / sum.total) * 100 : 0;
console.log(
  `  ${'TOTAL'.padEnd(8)} ${String(sum.n).padStart(5)} ` +
  `${f(sum.transport).padStart(11)} ${f(sum.server).padStart(13)} ${f(sum.total).padStart(10)} ${(totalPct.toFixed(0)+'%').padStart(8)}` +
  (hasRtt ? `  ${(sum.pureN ? f(sum.pureServer) : '').padStart(10)}` : '')
);

console.log('\n  Columns are TOTALS across all runs in the HAR (incl. the one-time login).');
console.log('  transport = blocked+dns+connect+send+receive   server(TTFB) = wait');
if (hasRtt) {
  console.log('  pure srv* = sum of max(0, wait - RTT) per request, for buckets given a --rtt-* value.');
} else {
  console.log('  Tip: pass --rtt-<bucket> <ms> (e.g. --rtt-wallet 110) to estimate pure server time.');
}
console.log('  Reminder: TTFB ≈ server work + 1 network round-trip; subtract the RTT for pure server.\n');

if (csvOut) {
  fs.mkdirSync(path.dirname(csvOut), { recursive: true });
  fs.writeFileSync(csvOut, csvRows.join('\n') + '\n');
  console.log(`  CSV: ${csvOut}\n`);
}
