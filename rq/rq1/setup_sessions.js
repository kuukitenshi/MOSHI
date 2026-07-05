#!/usr/bin/env node
//
// setup_sessions.js — One-time browser session setup for E2E latency measurement
//
// Usage:
//   node setup_sessions.js --system demo   # log into demo_app via Google
//   node setup_sessions.js --system hello  # log into hello_playground via Hellō
//
// After setup, the browser profile is saved to .profiles/<system>/
// Subsequent automated runs (breakdown.js) reuse the saved session so
// Google/Hellō auto-approve without any UI.

'use strict';

const { chromium } = require('playwright');
const path = require('path');
const fs = require('fs');

const PROFILES_DIR = path.join(__dirname, '.profiles');

const args = process.argv.slice(2);
const systemIdx = args.indexOf('--system');
const system = systemIdx !== -1 ? args[systemIdx + 1] : null;

if (!['demo', 'hello'].includes(system)) {
  console.error('Usage: node setup_sessions.js --system [demo|hello]');
  process.exit(1);
}

const PROFILE_DIR = path.join(PROFILES_DIR, system);
fs.mkdirSync(PROFILE_DIR, { recursive: true });

const DEMO_START_URL  = 'http://localhost:3000/auth/start?provider=google';
const HELLO_LOGIN_URL = 'http://localhost:3000/api/hellocoop?op=login';

// Chromium args:
//  - ozone-platform=x11: makes headed browser visible on Wayland via XWayland
//  - disable-blink-features=AutomationControlled: hides the automation flag from Google
//  - disable-automation: removes the "Chrome is being controlled" banner
const BROWSER_ARGS = [
  '--no-sandbox',
  '--disable-setuid-sandbox',
  '--ozone-platform=x11',
  '--disable-blink-features=AutomationControlled',
  '--disable-automation',
];

// waitForURL gives a WHATWG URL object (not a string) to the predicate.
// Use url.href for string operations.

async function setupDemo() {
  console.log('[setup/demo] Opening browser — please complete Google login when prompted.');
  console.log('[setup/demo] After logging in, the script will detect success automatically.\n');

  const ctx = await chromium.launchPersistentContext(PROFILE_DIR, {
    headless: false,
    args: BROWSER_ARGS,
  });

  // Remove navigator.webdriver before any page loads — Google checks this flag
  await ctx.addInitScript(() => {
    Object.defineProperty(navigator, 'webdriver', { get: () => undefined });
  });

  const page = await ctx.newPage();
  await page.goto(DEMO_START_URL);

  console.log('[setup/demo] Waiting for demo_app /callback page (up to 3 minutes)...');
  console.log('[setup/demo] Log into Google in the browser that just opened.\n');

  // waitForURL predicate receives a WHATWG URL object — use .href for string ops
  await page.waitForURL(
    url => url.href.startsWith('http://localhost:3000/callback'),
    { timeout: 180_000 }
  );
  await page.waitForLoadState('domcontentloaded');

  await ctx.close();

  // Write marker ONLY after successful login — checked on next runs
  fs.writeFileSync(path.join(PROFILE_DIR, '.setup_done'), new Date().toISOString() + '\n');

  console.log('[setup/demo] SUCCESS — demo_app login complete.');
  console.log(`[setup/demo] Session saved to: ${PROFILE_DIR}`);
  console.log('[setup/demo] Google session is now cached. Automated runs will not need login.\n');
}

async function setupHello() {
  console.log('[setup/hello] Opening browser — please complete Hellō login when prompted.');
  console.log('[setup/hello] Select Google as provider inside the Hellō wallet.\n');

  const ctx = await chromium.launchPersistentContext(PROFILE_DIR, {
    headless: false,
    args: BROWSER_ARGS,
  });

  await ctx.addInitScript(() => {
    Object.defineProperty(navigator, 'webdriver', { get: () => undefined });
  });

  const page = await ctx.newPage();
  await page.goto(HELLO_LOGIN_URL);

  console.log('[setup/hello] Waiting for Hellō callback to redirect back to app (up to 3 minutes)...');
  console.log('[setup/hello] Log in with Hellō (select Google) in the browser that just opened.\n');

  // The flow: op=login → JS detects redirect_uri → 307 to wallet → auto-redirect
  // (if session valid) → callback → redirect to localhost:3000/
  // After Hello SDK processes the callback it redirects to / (root, may be 404)
  await page.waitForURL(
    url => url.href.startsWith('http://localhost:3000/') &&
           !url.href.includes('/api/hellocoop'),
    { timeout: 180_000 }
  );
  await page.waitForLoadState('domcontentloaded');

  await ctx.close();

  // Write marker ONLY after successful login
  fs.writeFileSync(path.join(PROFILE_DIR, '.setup_done'), new Date().toISOString() + '\n');

  console.log('[setup/hello] SUCCESS — Hellō login complete.');
  console.log(`[setup/hello] Session saved to: ${PROFILE_DIR}`);
  console.log('[setup/hello] Hellō wallet session is cached. Automated runs will use it.\n');
}

(async () => {
  try {
    if (system === 'demo')  await setupDemo();
    if (system === 'hello') await setupHello();
  } catch (err) {
    console.error(`[setup/${system}] ERROR:`, err.message);
    process.exit(1);
  }
})();
