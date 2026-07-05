#!/usr/bin/env node
//
// breakdown.js — Fine-grained phase breakdown for demo_app AND hello_playground
//
// demo_app phases (from log timestamps across 5 services):
//
//   A  demo_app → AB         [auth/start → AB /authorize]           browser clock (skew-free)
//   B  AB → IB               [AB → IB /authorize]                   local redirect
//   C  IB → Google → IB      [IB redirect → Google callback]        ★ Google OAuth
//   D  Google token exchange  [IB callback → identity extracted]     server-to-server
//   E  IB blinds iss/sub      [identity extracted → forward to tTS]  crypto (fast)
//   F  IB → tTS → AB         [forward to tTS → token delivered to AB] FROST + push
//     F1 FROST signing only   [ceremony start → ceremony complete]
//   G  IB redirect → AB       [IB redirects AB → AB /callback resumes] browser redirect
//   H  AB → demo_app          [AB redirects → demo JWT received]     code exchange
//
// hello_playground phases (from Next.js response-time logs):
//
//   A  op=login               SDK setup — no Hellō call yet
//   B  op=login&redirect_uri  ★ Hellō API call — get wallet auth URL
//   C  code= callback         ★ Hellō token exchange (server-to-server)
//   D  final redirect         cookie set + load /
//
// Usage:
//   node breakdown.js --system demo  [--runs 5]
//   node breakdown.js --system hello [--runs 5]

'use strict';

const { chromium } = require('playwright');
const path = require('path');
const fs   = require('fs');

function arg(flag, def) {
  const i = process.argv.indexOf(flag);
  return i !== -1 ? process.argv[i + 1] : def;
}
const system  = arg('--system', null);
const runs    = parseInt(arg('--runs', '5'), 10);
// Warm-up iterations run BEFORE the measured runs and are not recorded — the
// first login is always slower (cold TLS/connections/caches), so we discard it.
const warmup  = parseInt(arg('--warmup', '1'), 10);
// --cold-hello clears the Hellō WALLET session (hello.coop cookies) between runs
// while keeping Google warm, so the wallet re-sends the browser through Google
// each login → the Google segment becomes visible (and comparable) for Hellō too.
const coldHello = process.argv.includes('--cold-hello');
// --hello-prompt <login|consent> forwards an OIDC prompt to the Hellō wallet so a
// returning user is RE-AUTHENTICATED at the upstream IdP (Google) each login.
// The wallet session + Google session stay warm, so the browser visits Google
// silently (no password) — this makes the Google segment measurable for Hellō
// without the cold-wallet password prompt.
const helloPrompt = arg('--hello-prompt', '');
// --tag <name> suffixes the browser-timeline output file (e.g. _cold) so warm and
// cold runs can coexist for the Google round-trip differential.
const outTag = arg('--tag', '');
// --headless forces a true headless browser (no display server). Used when running
// the Hellō measurement on a headless cluster with a pre-persisted wallet session
// (no interactive login needed), so neither a monitor nor Xvfb is required.
const headlessFlag = process.argv.includes('--headless');
const baseUrl = arg('--base-url', 'http://localhost:3000');
const csvOut  = arg('--csv', path.join(__dirname, 'out', `breakdown_${system}.csv`));
// HAR capture: one archive per session (all runs), so the analyzer can split each
// request's transport (dns/connect/ssl/send/receive) from the server `wait` (TTFB).
const harOut  = arg('--har', path.join(__dirname, 'out', `har_${system}${outTag ? '_' + outTag : ''}.har`));
const debug   = process.argv.includes('--debug');

if (!['demo', 'hello'].includes(system)) {
  console.error('Usage: node breakdown.js --system [demo|hello] [--runs N]');
  process.exit(1);
}

const ROOT    = path.join(__dirname, '../..');
const LOG_DIR = path.join(ROOT, 'logs');

// ── Helpers ────────────────────────────────────────────────────────────────
function linesCount(f) {
  try { return fs.readFileSync(f, 'utf8').split('\n').length; } catch { return 0; }
}
function readNew(f, before) {
  try { return fs.readFileSync(f, 'utf8').split('\n').slice(before); } catch { return []; }
}
// Find FIRST line that contains ALL patterns (case-sensitive)
function first(lines, ...patterns) {
  return lines.find(l => patterns.every(p => l.includes(p))) || null;
}
// Parse ISO timestamp from log line
function ts(line) {
  if (!line) return null;
  const m = line.match(/(\d{4}-\d{2}-\d{2}T[\d:.]+Z)/);
  return m ? new Date(m[1]).getTime() : null;
}
// Diff: b - a, return null if either is null or result is negative
function diff(a, b, label) {
  if (a == null || b == null) return null;
  const d = b - a;
  if (d < 0) {
    // Negative means wrong marker matched — skip rather than corrupt stats
    console.warn(`    [warn] Phase ${label}: negative (${d}ms) — marker mismatch, skipping`);
    return null;
  }
  return d;
}

// ── Unified browser-side timeline bucketing ─────────────────────────────────
// Attributes wall-clock time to a host category, using the SAME method for both
// systems so the resulting breakdown is directly comparable:
//   app    — the relying-party app           (:3000)
//   broker — partitioned broker (AB :4010 / IB :4020)  OR  Hellō wallet (hello.coop)
//   google — the upstream IdP (accounts.google.com / *.google.com)
//   other  — about:blank, blank initial nav, etc.
//
// We time the chain via main-frame NAVIGATION REQUESTS (page.on('request')),
// not page.url() polling. Polling only sees hosts that render an HTML document,
// so pure 302 redirect hops — the partitioned broker (AB/IB) and a silent Google
// re-auth — are invisible to it. Every redirect emits its own navigation
// request, so request timing captures those hops in both systems.
// Fine-grained host → component (used for the detailed, same-side breakdown).
// The coarse "broker" bucket is just the sum of its components (ab+ib+wallet).
function hostSubBucket(host) {
  host = host || '';
  if (host.includes('google.com'))                       return 'google';
  if (host.includes('hello.coop') || host.includes('hello.dev')) return 'wallet';
  if (host.includes(':4010'))                            return 'ab';
  if (host.includes(':4020'))                            return 'ib';
  if (host.includes(':3000'))                            return 'app';
  return 'other';
}
function hostOfUrl(url) {
  try { return new URL(url).host; } catch { return ''; }
}
// Attribute the time from each navigation hop to the next (last hop → tEnd) to
// the component of the host being contacted during that interval. Returns both
// the fine components and the coarse 3-way split — same clean partition of the
// total, just at two granularities.
function bucketsFromHops(hops, tEnd) {
  const f = { app: 0, ab: 0, ib: 0, wallet: 0, google: 0, other: 0 };
  for (let i = 0; i < hops.length; i++) {
    const next = (i + 1 < hops.length) ? hops[i + 1].t : tEnd;
    f[hostSubBucket(hops[i].host)] += Math.max(0, next - hops[i].t);
  }
  return {
    app: f.app, ab: f.ab, ib: f.ib, wallet: f.wallet,
    google: f.google, other: f.other,
    broker: f.ab + f.ib + f.wallet,   // coarse broker = sum of its components
  };
}
// Clears only the Hellō wallet cookies (hello.coop / hello.dev), leaving the
// Google session and localhost intact — used by --cold-hello so the wallet must
// re-authenticate the user through Google each run (no credentials needed since
// Google stays warm).
async function clearWalletCookies(ctx) {
  const cookies = await ctx.cookies();
  for (const c of cookies) {
    if (/hello\.(coop|dev)/.test(c.domain || '')) {
      await ctx.clearCookies({ domain: c.domain, name: c.name }).catch(() => {});
    }
  }
}

// Records the timestamp + host of every main-frame navigation (incl. redirects).
function attachHopTracker(page) {
  const hops = [];
  const handler = req => {
    try {
      if (req.frame() === page.mainFrame() && req.isNavigationRequest()) {
        hops.push({ t: Date.now(), host: hostOfUrl(req.url()) });
      }
    } catch { /* frame detached */ }
  };
  page.on('request', handler);
  return { hops, detach: () => page.off('request', handler) };
}

// Phase A spans the demo_app(local)↔broker(cosmos) boundary, so a log-based
// diff would carry the inter-machine clock offset (which drifts between sessions
// and can inflate A by hundreds of ms). Measure it instead from the browser's
// own navigation hops (a single clock → skew-free): from the /auth/start
// navigation (:3000) to the first AB navigation (:4010).
function phaseAFromHops(hops) {
  const a = hops.find(h => (h.host || '').includes(':3000'));
  const b = hops.find(h => (h.host || '').includes(':4010'));
  if (!a || !b) return null;
  const d = b.t - a.t;
  return d >= 0 ? d : null;
}

function mean(arr)   { return arr.length ? arr.reduce((s,v) => s+v, 0) / arr.length : null; }
function fmt(v)      { return v != null ? v.toFixed(1) : 'N/A'; }
function pct(a, tot) { return a != null && tot ? `${((a/tot)*100).toFixed(0)}%` : ' — '; }

function statsOf(arr) {
  if (!arr.length) return null;
  const s = [...arr].sort((a, b) => a - b);
  const n = s.length;
  const m = arr.reduce((a, v) => a + v, 0) / n;
  const stddev = Math.sqrt(arr.reduce((a, v) => a + (v - m) ** 2, 0) / n);
  const median = n % 2 === 0 ? (s[n/2-1] + s[n/2]) / 2 : s[Math.floor(n/2)];
  const p95    = s[Math.max(0, Math.ceil(0.95 * n) - 1)];
  return { n, mean: m, median, stddev, p95, min: s[0], max: s[n-1] };
}

function writeRunsAndSummary(system, results, csvOut) {
  const sfx = outTag ? `_${outTag}` : '';
  const runsPath    = csvOut.replace(`breakdown_${system}.csv`, `rq1_${system}${sfx}_runs.csv`);
  const summaryPath = csvOut.replace(`breakdown_${system}.csv`, `rq1_${system}${sfx}_summary.json`);

  const runsCsv = ['run,elapsed_ms,status',
    ...results.map(r => `${r.run},${r.elapsed ?? ''},${r.status}`)
  ].join('\n') + '\n';
  fs.writeFileSync(runsPath, runsCsv);

  const times = results.filter(r => r.status === 'ok').map(r => r.elapsed);
  const failures = results.filter(r => r.status !== 'ok').length;
  const s = statsOf(times);
  fs.writeFileSync(summaryPath, JSON.stringify({ system, ...s, failures }, null, 2));

  console.log(`  Runs CSV:  ${runsPath}`);
  console.log(`  Summary:   ${summaryPath}`);
}

// Writes the unified browser-side timeline (same columns for both systems).
function writeTimeline(system, tl, csvOut) {
  const sfx = outTag ? `_${outTag}` : '';
  const tlPath = csvOut.replace(`breakdown_${system}.csv`, `browser_timeline_${system}${sfx}.csv`);
  const ok = tl.filter(r => r.status === 'ok');
  // Coarse (app/broker/google) + fine components (ab/ib/wallet) — same partition.
  const header = 'run,total_ms,app_ms,broker_ms,google_ms,other_ms,ab_ms,ib_ms,wallet_ms,status';
  const R = v => Math.round(v || 0);
  const lines = tl.map(r => r.status === 'ok'
    ? `${r.run},${r.total},${R(r.app)},${R(r.broker)},${R(r.google)},${R(r.other)},${R(r.ab)},${R(r.ib)},${R(r.wallet)},ok`
    : `${r.run},,,,,,,,,FAILED`);
  fs.writeFileSync(tlPath, [header, ...lines].join('\n') + '\n');

  if (ok.length) {
    const avg = k => ok.reduce((s, r) => s + r[k], 0) / ok.length;
    console.log('\n  Browser-side timeline (mean ms, comparable across systems):');
    console.log(`    app=${avg('app').toFixed(0)}  broker=${avg('broker').toFixed(0)}  ` +
                `google=${avg('google').toFixed(0)}  other=${avg('other').toFixed(0)}`);
  }
  console.log(`  Timeline:  ${tlPath}`);
}

// ── Next.js log parser (for hello) ────────────────────────────────────────
// Parses:  " GET /api/hellocoop?op=login 200 in 979ms (next.js: 858ms, application-code: 29ms)"
function parseNextjs(line) {
  if (!line) return null;
  const toMs = (v, u) => u === 's' ? parseFloat(v) * 1000 : parseFloat(v);
  const total   = line.match(/in ([\d.]+)(ms|s)/);
  const appCode = line.match(/application-code:\s*([\d.]+)(ms|s)/);
  const status  = line.match(/\s(\d{3})\s/);
  if (!total) return null;
  return {
    status:     status   ? parseInt(status[1]) : null,
    total_ms:   toMs(total[1],   total[2]),
    appcode_ms: appCode  ? toMs(appCode[1], appCode[2]) : null,
  };
}

// ══════════════════════════════════════════════════════════════════════════
// DEMO breakdown
// ══════════════════════════════════════════════════════════════════════════
async function runDemo() {
  const PROFILE = path.join(__dirname, '.profiles', 'demo');
  if (!fs.existsSync(path.join(PROFILE, '.setup_done'))) {
    console.error('  No demo session — run: node setup_sessions.js --system demo');
    process.exit(1);
  }

  const logs = {
    demo: path.join(LOG_DIR, 'demo_app.log'),
    ab:   path.join(LOG_DIR, 'ab.log'),
    ib:   path.join(LOG_DIR, 'ib.log'),
    tts:  path.join(LOG_DIR, 'tts.log'),
  };
  for (const [k, f] of Object.entries(logs)) {
    if (!fs.existsSync(f)) { console.error(`  Missing log: ${f} — start services first`); process.exit(1); }
  }

  fs.mkdirSync(path.dirname(harOut), { recursive: true });
  const ctx = await chromium.launchPersistentContext(PROFILE, {
    headless: true,
    args: ['--no-sandbox','--disable-setuid-sandbox',
           '--disable-blink-features=AutomationControlled','--disable-automation'],
    recordHar: { path: harOut, content: 'omit' },  // timings only, no response bodies
  });
  await ctx.addInitScript(() => {
    Object.defineProperty(navigator, 'webdriver', { get: () => undefined });
  });
  const page = await ctx.newPage();

  // CSV header
  const csvCols = ['run','total_ms','A_ms','B_ms','C_ms','D_ms','E_ms','F_ms','F1_ms','G_ms','H_ms'];
  const rows = [csvCols.join(',')];
  const data  = { total:[], A:[], B:[], C:[], D:[], E:[], F:[], F1:[], G:[], H:[] };
  const results = [];
  const tl = [];   // unified browser-side timeline rows (comparable across systems)

  const W = { A:7, B:7, C:10, D:10, E:7, F:8, F1:8, G:7, H:8 };
  console.log(`\n  ${'Run'.padEnd(4)} ${'Total'.padStart(8)} ${'A'.padStart(W.A)} ${'B'.padStart(W.B)} ${'C:Google'.padStart(W.C)} ${'D:GtokEx'.padStart(W.D)} ${'E:blind'.padStart(W.E)} ${'F:tTS'.padStart(W.F)} ${'F1:FROST'.padStart(W.F1)} ${'G:relay'.padStart(W.G)} ${'H:code'.padStart(W.H)}`);
  console.log('  ' + '─'.repeat(90));

  for (let run = 1 - warmup; run <= runs; run++) {
    const isWarmup = run < 1;
    const tag = isWarmup ? 'W' : String(run);
    const before = {};
    for (const [k, f] of Object.entries(logs)) before[k] = linesCount(f);

    const hopTracker = attachHopTracker(page);
    let lastClick = 0;

    const t_wall = Date.now();
    let navOk = true;
    try {
      await page.goto(`${baseUrl}/auth/start?provider=google`, { waitUntil:'commit', timeout:15_000 });
    } catch (e) {
      // Transient navigation hiccup (interrupted / ERR_ABORTED / etc.) — retry once,
      // then mark the run FAILED rather than crash the whole session.
      await page.waitForTimeout(400);
      navOk = await page.goto(`${baseUrl}/auth/start?provider=google`, { waitUntil:'commit', timeout:15_000 })
        .then(() => true).catch(() => false);
    }

    const deadline = t_wall + 90_000;
    while (Date.now() < deadline) {
      const url = page.url();
      const now = Date.now();
      if (url.startsWith(`${baseUrl}/callback`)) break;
      // Best-effort account-picker click (a no-op under silent re-auth), throttled.
      if (url.includes('accounts.google.com') && now - lastClick > 200) {
        lastClick = now;
        await page.evaluate(() => {
          for (const s of ['[data-identifier]','li[tabindex]','button[jsname="LgbsSe"]','form button','button']) {
            const el = document.querySelector(s);
            if (el) { el.click(); return; }
          }
        }).catch(() => {});
      }
      await page.waitForTimeout(40);
    }
    hopTracker.detach();
    const bkt = bucketsFromHops(hopTracker.hops, Date.now());

    if (!navOk || !page.url().startsWith(`${baseUrl}/callback`)) {
      console.log(`  ${tag.padEnd(4)} FAILED — ${navOk ? page.url().substring(0,60) : 'navigation aborted'}`);
      if (!isWarmup) {
        rows.push(`${run},FAILED`);
        results.push({ run, elapsed: null, status: 'FAILED' });
        tl.push({ run, status: 'FAILED' });
      }
      continue;
    }
    await page.waitForLoadState('domcontentloaded', { timeout:10_000 }).catch(() => {});
    const total = Date.now() - t_wall;
    await page.waitForTimeout(400); // flush logs

    if (isWarmup) {
      console.log(`  ${tag.padEnd(4)} ${fmt(total).padStart(8)}   (warm-up — discarded)`);
      if (run < runs) await page.waitForTimeout(500);
      continue;
    }

    const L = {};
    for (const [k, f] of Object.entries(logs)) L[k] = readNew(f, before[k]);

    // ── Debug: show what new lines were captured ───────────────────────
    if (debug) {
      console.log(`\n  [debug run ${run}] new lines per file:`);
      for (const [k, lines] of Object.entries(L)) {
        console.log(`    ${k}: ${lines.length} lines`);
        lines.filter(l => l.trim()).slice(0, 4).forEach(l =>
          console.log(`      ${l.substring(0,100)}`)
        );
      }
    }

    // ── Exact markers from actual log output ───────────────────────────
    // demo_app.log  (Phase A no longer uses a demo_app marker — see phaseAFromHops)
    const demo_callback  = ts(first(L.demo, '[Demo App] Callback received'));
    const demo_jwt       = ts(first(L.demo, '[Demo App] JWT received successfully'));

    // ab.log
    const ab_authorize   = ts(first(L.ab,  '[AB] Browser /authorize (HTTP :4010)'));
    const ab_callback    = ts(first(L.ab,  '[AB] Browser /callback — received from IB'));
    const ab_delivered   = ts(first(L.ab,  '[AB] Token delivered by tTS'));
    const ab_redirect    = ts(first(L.ab,  '[AB] Redirecting browser to demo app'));

    // ib.log
    const ib_authorize   = ts(first(L.ib,  '[IB] Browser /authorize — received from AB'));
    const ib_redir_goog  = ts(first(L.ib,  '[IB] Redirecting browser to Google'));
    const ib_gcb         = ts(first(L.ib,  '[IB] Google callback received'));
    const ib_identity    = ts(first(L.ib,  '[IB] Extracted identity from Google ID Token'));
    const ib_forward_tts = ts(first(L.ib,  '[IB] Forwarding blinded tuple to tTS'));
    const ib_redir_ab    = ts(first(L.ib,  '[IB] Redirecting browser back to AB'));

    // tts.log
    const tts_frost_start= ts(first(L.tts, 'FROST Threshold Signing Ceremony'));
    const tts_frost_done = ts(first(L.tts, 'FROST Ceremony Complete'));
    const tts_returns    = ts(first(L.tts, 'Step 8 — JWT signed with FROST'));

    // ── Phase durations ────────────────────────────────────────────────
    const ph = {
      A:  phaseAFromHops(hopTracker.hops),            // demo_app → AB (browser clock; skew-free)
      B:  diff(ab_authorize,   ib_authorize,   'B'),  // AB → IB
      C:  diff(ib_redir_goog,  ib_gcb,         'C'),  // IB → Google → IB (round-trip)
      D:  diff(ib_gcb,         ib_identity,    'D'),  // Google token exchange (server-to-server)
      E:  diff(ib_identity,    ib_forward_tts, 'E'),  // IB blinds iss/sub (crypto)
      F:  diff(ib_forward_tts, ab_delivered,  'F'),  // IB→tTS + FROST + tTS→AB push
      F1: diff(tts_frost_start,tts_frost_done, 'F1'), // FROST signing only
      G:  diff(ib_redir_ab,    ab_callback,    'G'),  // browser redirect IB→AB (no relay fetch)
      // H is measured from demo_app's OWN log markers (both local clock) to avoid
      // any cosmos↔local clock skew: callback received → JWT exchanged + rendered.
      H:  diff(demo_callback,  demo_jwt,       'H'),  // demo_app code exchange + render
    };

    for (const [k, v] of Object.entries(ph)) { if (v != null) data[k].push(v); }
    data.total.push(total);
    results.push({ run, elapsed: total, status: 'ok' });
    tl.push({ run, total, ...bkt, status: 'ok' });

    const r = v => fmt(v).padStart;
    const p = (v, w) => fmt(v).padStart(w);
    console.log(`  ${tag.padEnd(4)} ${fmt(total).padStart(8)} ${p(ph.A,W.A)} ${p(ph.B,W.B)} ${p(ph.C,W.C)} ${p(ph.D,W.D)} ${p(ph.E,W.E)} ${p(ph.F,W.F)} ${p(ph.F1,W.F1)} ${p(ph.G,W.G)} ${p(ph.H,W.H)}`);
    rows.push(`${run},${total},${ph.A??''},${ph.B??''},${ph.C??''},${ph.D??''},${ph.E??''},${ph.F??''},${ph.F1??''},${ph.G??''},${ph.H??''}`);

    if (run < runs) await page.waitForTimeout(500);
  }

  await ctx.close();
  const demoCsv = outTag ? csvOut.replace('breakdown_demo.csv', `breakdown_demo_${outTag}.csv`) : csvOut;
  fs.writeFileSync(demoCsv, rows.join('\n') + '\n');
  writeRunsAndSummary('demo', results, csvOut);
  writeTimeline('demo', tl, csvOut);

  // Summary — printed to terminal AND saved to a text file.
  const S = [];
  const log = s => { console.log(s); S.push(s.replace(/^\n/, '')); };
  const tot = mean(data.total) || 1;
  log('\n  ' + '═'.repeat(90));
  log('  Phase means across runs (demo — partitioned broker):');
  log('  ' + '─'.repeat(90));
  const phases = [
    ['A  demo_app → AB',        data.A,   'auth/start processing + local redirect to AB'],
    ['B  AB → IB',              data.B,   'AB blinds app_id, stores session, redirects to IB'],
    ['C  IB → Google → IB',    data.C,   '★ Google OAuth browser round-trip (user already logged in)'],
    ['D  Google token exchange', data.D,   'IB exchanges code with Google API (server-to-server)'],
    ['E  IB blinds iss/sub',     data.E,   'HMAC-SHA256 blinding (crypto, sub-ms)'],
    ['F  IB → tTS → AB',        data.F,   'HTTPS to tTS + FROST signing + tTS pushes token to AB'],
    ['F1   └ FROST only',        data.F1,  'threshold signature ceremony (within F)'],
    ['G  IB redirect → AB',      data.G,   'browser redirect IB→AB; AB picks up the delivered token (no relay fetch)'],
    ['H  code exchange + render', data.H,   'demo_app POST /token + PKCE + render JWT page'],
    ['TOTAL (wall clock)',        data.total,'measured by Playwright browser'],
  ];
  log(`  ${'Phase'.padEnd(28)} ${'Mean (ms)'.padStart(11)} ${'% of total'.padStart(12)}  Description`);
  for (const [label, arr, desc] of phases) {
    const m = mean(arr);
    log(`  ${label.padEnd(28)} ${fmt(m).padStart(11)} ${pct(m,tot).padStart(12)}  ${desc}`);
  }
  const sumPath = csvOut.replace('breakdown_demo.csv', 'breakdown_demo_summary.txt');
  fs.writeFileSync(sumPath, S.join('\n') + '\n');
  console.log(`\n  CSV:     ${csvOut}`);
  console.log(`  Summary: ${sumPath}`);
}

// ══════════════════════════════════════════════════════════════════════════
// HELLO breakdown
// ══════════════════════════════════════════════════════════════════════════
async function runHello() {
  const HELLO_LOG = path.join(LOG_DIR, 'app_hello_playground.log');
  if (!fs.existsSync(HELLO_LOG)) {
    console.error(`  Missing log: ${HELLO_LOG}`);
    console.error('  Start hello_playground with timestamps:');
    console.error(`    cd app_hello_playground`);
    console.error(`    npm run dev 2>&1 | while IFS= read -r l; do printf '%s %s\\n' "$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)" "$l"; done | tee ../logs/app_hello_playground.log`);
    process.exit(1);
  }

  const PROFILE = path.join(__dirname, '.profiles', 'hello_e2e');
  fs.mkdirSync(PROFILE, { recursive: true });

  fs.mkdirSync(path.dirname(harOut), { recursive: true });
  const ctx = await chromium.launchPersistentContext(PROFILE, {
    headless: headlessFlag,
    args: ['--no-sandbox','--disable-setuid-sandbox',
           ...(headlessFlag ? [] : ['--ozone-platform=x11']),
           '--disable-blink-features=AutomationControlled','--disable-automation'],
    recordHar: { path: harOut, content: 'omit' },  // timings only, no response bodies
  });
  await ctx.addInitScript(() => {
    Object.defineProperty(navigator, 'webdriver', { get: () => undefined });
  });
  const page = await ctx.newPage();

  const isBack = url => url.startsWith(`${baseUrl}/`) && !url.includes('/api/hellocoop');

  // Login URL. With --hello-prompt, forward an OIDC prompt to the wallet so each
  // measured run re-authenticates at Google (browser visits it, warm → silent).
  const LOGIN_URL = `${baseUrl}/api/hellocoop?op=login` +
    (helloPrompt ? `&prompt=${encodeURIComponent(helloPrompt)}` : '');

  // Initial login.
  await page.goto(`${baseUrl}/api/hellocoop?op=login`, { waitUntil:'commit', timeout:15_000 });
  if (!headlessFlag) {
    // Headed (the proven path): a human completes the Hellō login once.
    console.log('\n  Browser opened — log in to Hellō once, then breakdown starts automatically.');
    await page.waitForURL(url => isBack(url.href), { timeout:300_000 });
  } else {
    // Headless: no human, so auto-dismiss the wallet "Continue" confirmation; the
    // session must come from the profile/storageState. Abort cleanly if it never
    // returns (e.g. the IdP forces re-auth) rather than hanging.
    console.log('\n  Headless login (session from profile, auto-continue)...');
    const loginDeadline = Date.now() + 90_000;
    let lastClick = 0;
    while (Date.now() < loginDeadline && !isBack(page.url())) {
      if (page.url().includes('hello.coop') && Date.now() - lastClick > 250) {
        lastClick = Date.now();
        await page.evaluate(() => {
          const vis = e => e.offsetParent !== null;
          const txt = e => (e.innerText || e.textContent || '').trim();
          const els = [...document.querySelectorAll('button, a, [role="button"], [type="submit"]')].filter(vis);
          let el = els.find(e => /continue|continuar|prosseguir/i.test(txt(e))) || els.find(e => /google/i.test(txt(e)));
          if (el) el.click();
        }).catch(() => {});
      }
      await page.waitForTimeout(100);
    }
    if (!isBack(page.url())) {
      console.error(`\n  Headless Hellō login did not complete (at ${page.url()}). Session invalid here. Aborting.\n`);
      await ctx.close();
      process.exit(1);
    }
  }
  await page.waitForLoadState('domcontentloaded').catch(() => {});
  console.log('  Login detected! Starting runs...\n');
  await page.waitForTimeout(1000);

  // Hellō has no parseable server logs (the wallet is a black box and the local
  // Next.js RP, especially in production, emits no per-request timing line), so the
  // old Next.js-log A/B/C/D parse always read N/A. We instead attribute time from
  // the browser navigation hops (the same hop tracker used for the timeline), which
  // is always available and consistent with the HAR-based breakdown.
  const csvCols = ['run','total_ms','app_ms','wallet_ms','google_ms','other_ms'];
  const rows = [csvCols.join(',')];
  const data = { total:[], app:[], wallet:[], google:[], other:[] };
  const results = [];
  const tl = [];   // unified browser-side timeline rows (comparable across systems)

  console.log(`  ${'Run'.padEnd(4)} ${'Total'.padStart(8)} ${'app'.padStart(10)} ${'wallet'.padStart(10)} ${'google'.padStart(10)}`);
  console.log('  ' + '─'.repeat(70));

  for (let run = 1 - warmup; run <= runs; run++) {
    const isWarmup = run < 1;
    const tag = isWarmup ? 'W' : String(run);
    const before = linesCount(HELLO_LOG);
    await ctx.clearCookies({ domain: 'localhost' });
    if (coldHello) await clearWalletCookies(ctx);   // force a fresh wallet→Google round-trip

    let hopTracker = null;
    try {
    // Reset the previous run's page first — a still-live page is the usual cause
    // of ERR_ABORTED on the next op=login navigation.
    await page.goto('about:blank', { timeout: 5_000 }).catch(() => {});
    hopTracker = attachHopTracker(page);
    let lastClick = 0;

    const t0 = Date.now();
    // The previous run can still be settling at "/" when we start the next one,
    // so an in-flight navigation may interrupt this goto. Tolerate it and retry.
    let navOk = true;
    try {
      await page.goto(LOGIN_URL, { waitUntil:'commit', timeout:15_000 });
    } catch (e) {
      // Transient navigation hiccup (interrupted / ERR_ABORTED / etc.) — retry once,
      // then mark the run FAILED rather than crash the whole session.
      await page.waitForTimeout(400);
      navOk = await page.goto(LOGIN_URL, { waitUntil:'commit', timeout:15_000 })
        .then(() => true).catch(() => false);
    }

    const deadline = t0 + 90_000;
    while (Date.now() < deadline) {
      const url = page.url();
      const now = Date.now();
      if (isBack(url)) break;
      // Auto-dismiss the Hellō wallet "Continue (with Google)" confirmation so a
      // returning-user login needs no manual click. Throttled; best-effort.
      if (url.includes('hello.coop') && now - lastClick > 250) {
        lastClick = now;
        await page.evaluate(() => {
          const vis = e => e.offsetParent !== null;
          const txt = e => (e.innerText || e.textContent || '').trim();
          const els = [...document.querySelectorAll('button, a, [role="button"], [type="submit"]')].filter(vis);
          let el = els.find(e => /continue|continuar|prosseguir/i.test(txt(e)));
          if (!el) el = els.find(e => /google/i.test(txt(e)));
          if (el) el.click();
        }).catch(() => {});
      }
      await page.waitForTimeout(40);
    }
    const bkt = bucketsFromHops(hopTracker.hops, Date.now());
    hopTracker.detach(); hopTracker = null;

    if (!navOk || !isBack(page.url())) {
      console.log(`  ${tag.padEnd(4)} FAILED — ${navOk ? page.url().substring(0,60) : 'navigation aborted'}`);
      if (!isWarmup) {
        rows.push(`${run},FAILED`);
        results.push({ run, elapsed: null, status: 'FAILED' });
        tl.push({ run, status: 'FAILED' });
      }
      continue;
    }
    await page.waitForLoadState('domcontentloaded', { timeout:10_000 }).catch(() => {});
    const total = Date.now() - t0;
    await page.waitForTimeout(500);

    if (isWarmup) {
      console.log(`  ${tag.padEnd(4)} ${fmt(total).padStart(8)}   (warm-up — discarded)`);
      if (run < runs) await page.waitForTimeout(500);
      continue;
    }

    // Per-host time from the browser navigation hops (always available, unlike the
    // Next.js logs). `bkt` is the same partition used for the timeline below.
    data.total.push(total);
    data.app.push(bkt.app);
    data.wallet.push(bkt.wallet);
    data.google.push(bkt.google);
    data.other.push(bkt.other);
    results.push({ run, elapsed: total, status: 'ok' });
    tl.push({ run, total, ...bkt, status: 'ok' });

    console.log(`  ${tag.padEnd(4)} ${fmt(total).padStart(8)} ${fmt(bkt.app).padStart(10)} ${fmt(bkt.wallet).padStart(10)} ${fmt(bkt.google).padStart(10)}`);
    rows.push(`${run},${total},${bkt.app},${bkt.wallet},${bkt.google},${bkt.other}`);

    if (run < runs) await page.waitForTimeout(500);
    } catch (err) {
      // Any unexpected per-run failure (e.g. ERR_ABORTED in the wallet redirect
      // chain) is contained here so the rest of the runs survive.
      if (hopTracker) try { hopTracker.detach(); } catch {}
      console.log(`  ${tag.padEnd(4)} FAILED — ${String((err && err.message) || err).slice(0, 70)}`);
      if (!isWarmup) {
        rows.push(`${run},FAILED`);
        results.push({ run, elapsed: null, status: 'FAILED' });
        tl.push({ run, status: 'FAILED' });
      }
      await page.waitForTimeout(500).catch(() => {});
    }
  }

  await ctx.close();
  const helloCsv = outTag ? csvOut.replace('breakdown_hello.csv', `breakdown_hello_${outTag}.csv`) : csvOut;
  fs.writeFileSync(helloCsv, rows.join('\n') + '\n');
  writeRunsAndSummary('hello', results, csvOut);
  writeTimeline('hello', tl, csvOut);

  const S = [];
  const log = s => { console.log(s); S.push(s.replace(/^\n/, '')); };
  const tot = mean(data.total) || 1;
  log('\n  ' + '═'.repeat(70));
  log('  Per-host means (hellō — from browser navigation hops):');
  log('  ' + '─'.repeat(70));
  const phases = [
    ['app    (RP / SDK glue)',      data.app,    'local relying-party app'],
    ['wallet (Hellō broker)',       data.wallet, '★ wallet redirects + consent (Google is server-side inside)'],
    ['google (IdP, browser-side)',  data.google, 'direct browser hits to Google (≈0 on warm runs)'],
    ['other',                       data.other,  'blank navs, etc.'],
    ['TOTAL (wall clock)',          data.total,  'measured by Playwright'],
  ];
  log(`  ${'Host'.padEnd(30)} ${'Mean(ms)'.padStart(11)}  ${'%'.padStart(5)}  Description`);
  for (const [label, arr, desc] of phases) {
    const m = mean(arr);
    log(`  ${label.padEnd(30)} ${fmt(m).padStart(11)}  ${pct(m,tot).padStart(5)}  ${desc}`);
  }
  const sumPath = csvOut.replace('breakdown_hello.csv', 'breakdown_hello_summary.txt');
  fs.writeFileSync(sumPath, S.join('\n') + '\n');
  console.log(`\n  CSV:     ${csvOut}`);
  console.log(`  Summary: ${sumPath}`);
}

// ── Entry ──────────────────────────────────────────────────────────────────
(async () => {
  fs.mkdirSync(path.dirname(csvOut), { recursive: true });
  console.log(`\n${'━'.repeat(60)}`);
  console.log(`  Breakdown: ${system === 'demo' ? 'Demo App (Partitioned Broker)' : 'Hellō Playground'}`);
  console.log(`  Runs: ${runs}   (warm-up: ${warmup}` +
    `${system === 'hello' && coldHello ? ', cold-wallet' : ''}` +
    `${system === 'hello' && helloPrompt ? `, prompt=${helloPrompt}` : ''})`);
  console.log(`${'━'.repeat(60)}`);
  if (system === 'demo')  await runDemo();
  if (system === 'hello') await runHello();
})().catch(err => {
  console.error(`\n  [breakdown] fatal error: ${err && err.message ? err.message : err}`);
  process.exit(1);
});
