// Drives ui/ in Chromium against stub.js, saves screenshots in light and dark,
// and fails on a console error, a page error or a CSP violation.
//
//   node apps/desktop/ui-preview/shoot.js OUT_DIR
//
// Needs Playwright (require('playwright')) with Chromium installed.
'use strict';

const path = require('path');
const fs = require('fs');
const { chromium } = require('playwright');
const { serve } = require('./server');

const OUT = path.resolve(process.argv[2] || 'ui-shots');
const PORT = 8765;
const BASE = `http://127.0.0.1:${PORT}/`;

const problems = [];

async function page(browser, scheme, scene) {
  const context = await browser.newContext({ viewport: { width: 1280, height: 800 }, colorScheme: scheme });
  const p = await context.newPage();
  await p.addInitScript(() => {
    window.__csp = [];
    document.addEventListener('securitypolicyviolation', (e) => {
      window.__csp.push(e.violatedDirective + ' ' + e.blockedURI + ' ' + (e.sample || ''));
    });
  });
  p.on('console', (m) => {
    if (m.type() === 'error' || m.type() === 'warning') problems.push(`[${scheme} ${scene}] console ${m.type()}: ${m.text()}`);
  });
  p.on('pageerror', (e) => problems.push(`[${scheme} ${scene}] page error: ${e.message}`));
  await p.goto(BASE + '?scene=' + scene);
  await p.waitForTimeout(300);
  return p;
}

async function check(p, scheme, name) {
  const csp = await p.evaluate(() => window.__csp);
  for (const v of csp) problems.push(`[${scheme} ${name}] CSP violation: ${v}`);
  // No element may carry an inline style attribute: style-src 'self' forbids them.
  const inline = await p.evaluate(() => [...document.querySelectorAll('[style]')].filter((n) => n.getAttribute('style').trim() !== '').map((n) => n.tagName + "#" + n.id + " " + n.getAttribute("style")));
  for (const n of inline) problems.push(`[${scheme} ${name}] inline style attribute on ${n}`);
}

async function shot(p, scheme, name) {
  await check(p, scheme, name);
  await p.screenshot({ path: path.join(OUT, `${name}-${scheme}.png`) });
}

async function run(browser, scheme) {
  // First run, then the import flow up to the plan.
  let p = await page(browser, scheme, 'empty');
  await shot(p, scheme, '01-first-run');
  await p.click('#choose-git');
  await p.fill('#git-url', 'git@github.com:acme/demo_shop.git');
  await p.fill('#git-ref', 'main');
  await shot(p, scheme, '02-import-git');
  await p.click('#git-go');
  await p.waitForSelector('[data-page=review]:not(.hidden)');
  if (await p.evaluate(() => document.activeElement.id) !== 'review-cancel') problems.push(`[${scheme}] Cancel is not focused on the review page`);
  await shot(p, scheme, '03-review');
  await p.context().close();

  // The folder page.
  p = await page(browser, scheme, 'empty');
  await p.click('#choose-folder');
  await p.fill('#folder-path', '/Users/ana/code/billing-api');
  await shot(p, scheme, '02b-add-folder');
  await p.context().close();

  // Starting, held part-way, after unticking one port.
  p = await page(browser, scheme, 'starting');
  await p.click('#btn-new');
  await p.click('#choose-git');
  await p.fill('#git-url', 'git@github.com:acme/demo_shop.git');
  await p.click('#git-go');
  await p.waitForSelector('[data-page=review]:not(.hidden)');
  await p.uncheck('#review-ports li:nth-child(3) input');
  await p.click('#review-start');
  await p.waitForSelector('#starting-steps li.active');
  await p.waitForTimeout(200);
  const up = await p.evaluate(() => window.__calls.find((c) => c.cmd === 'up'));
  if (!up || up.args.digest !== '9e1d4c07a3b2f6118d0c55e2a1f4b3c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3' || JSON.stringify(up.args.ports) !== '[3000,8080]') {
    problems.push(`[${scheme}] up was not sent with the shown plan's digest and the ticked ports: ${JSON.stringify(up)}`);
  }
  await shot(p, scheme, '04-starting');
  await p.context().close();

  // Starting, waiting for the person at the system's Touch ID prompt.
  p = await page(browser, scheme, 'confirming');
  await p.click('#btn-start');
  await p.waitForSelector('[data-page=review]:not(.hidden)');
  await p.click('#review-start');
  await p.waitForSelector('#starting-steps li.active[data-key=confirm]');
  await p.waitForTimeout(200);
  const note = await p.textContent('#starting-note');
  if (!note.includes('Touch ID or your login password') || !note.includes('cancel it')) {
    problems.push(`[${scheme}] the start page does not say a prompt is up and what to check: ${note}`);
  }
  await shot(p, scheme, '04a-confirming');
  await p.context().close();

  // A finished start.
  p = await page(browser, scheme, 'stopped');
  await p.click('#btn-start');
  await p.waitForSelector('[data-page=review]:not(.hidden)');
  await p.click('#review-start');
  await p.waitForSelector('#starting-done:not(.hidden)');
  await shot(p, scheme, '04b-started');
  await p.context().close();

  // A project and its tabs.
  p = await page(browser, scheme, 'projects');
  await shot(p, scheme, '05-overview');
  await p.click('.tab[data-tab=logs]');
  await p.waitForTimeout(300);
  await shot(p, scheme, '06-logs');
  await p.click('.tab[data-tab=network]');
  await p.waitForTimeout(200);
  await shot(p, scheme, '07-network');
  await p.click('.tab[data-tab=problems]');
  await p.waitForSelector('#flagged-notes li, #flagged-list li');
  await p.waitForTimeout(200);
  if (await p.evaluate(() => !document.querySelector('.tab[data-tab=problems] .badge') || document.querySelector('.tab[data-tab=problems] .badge').classList.contains('hidden'))) problems.push(`[${scheme}] the Problems tab has no count for a project with refusals`);
  await shot(p, scheme, '07b-problems');
  await p.click('.tab[data-tab=changes]');
  await p.click('#review-read');
  await p.waitForSelector('#review-files li');
  await p.click('#git-status');
  await p.waitForTimeout(200);
  await shot(p, scheme, '08-changes');
  // A pull stopped by uncommitted changes offers to stash them.
  await p.fill('#git-run', 'pull');
  await p.click('#git-run-go');
  await p.waitForSelector('#git-stuck:not(.hidden)');
  await shot(p, scheme, '08b-changes-pull-stuck');
  await p.click('#git-autostash');
  await p.waitForSelector('#git-stuck.hidden', { state: 'attached' });
  await p.fill('#git-run', 'log --oneline');
  await p.click('#git-run-go');
  await p.waitForFunction(() => document.getElementById('git-output').textContent.includes('Fix the refund total'));
  await shot(p, scheme, '08c-changes-git-command');
  await p.click('.tab[data-tab=secrets]');
  await p.waitForSelector('#secret-rows li[data-name]');
  await shot(p, scheme, '09a-secrets');
  // Fill in an empty name: the form opens under its row, masked.
  await p.click('#secret-rows li[data-name=SENDGRID_API_KEY] button');
  if (await p.evaluate(() => document.getElementById('secret-value').type) !== 'password') problems.push(`[${scheme}] the secret field is not masked`);
  if (await p.evaluate(() => document.activeElement.id) !== 'secret-value') problems.push(`[${scheme}] the secret field does not have the focus`);
  const typed = 'SG.preview-sentinel-value-0042';
  await p.fill('#secret-value', typed);
  await p.check('#secret-services input[value=api]');
  await p.check('#secret-services input[value=worker]');
  await shot(p, scheme, '09b-secret-form');
  await p.click('#secret-save');
  await p.waitForSelector('#secret-form.hidden', { state: 'attached' });
  await p.waitForFunction(() => document.querySelector('#secret-rows li[data-name=SENDGRID_API_KEY] .tag.good'));
  const sent = await p.evaluate(() => window.__calls.filter((c) => c.cmd === 'set_secret').pop());
  if (!sent || sent.args.value !== typed || sent.args.services.join() !== 'api,worker') problems.push(`[${scheme}] set_secret was not sent as typed: ${JSON.stringify(sent && sent.args.services)}`);
  const left = await p.evaluate((t) => ({ field: document.getElementById('secret-value').value, html: document.documentElement.outerHTML.includes(t) }), typed);
  if (left.field || left.html) problems.push(`[${scheme}] the typed value was left on the page`);
  await shot(p, scheme, '09c-secret-saved');
  await p.click('.tab[data-tab=settings]');
  await shot(p, scheme, '09-settings');
  await p.click('.tab[data-tab=overview]');
  await p.waitForTimeout(100);
  await shot(p, scheme, '05b-overview-compared');
  await p.click('#btn-more');
  await shot(p, scheme, '10-menu');
  await p.click('#menu-destroy');
  if (await p.evaluate(() => document.activeElement.id) !== 'dialog-cancel') problems.push(`[${scheme}] Cancel is not focused in the Destroy dialog`);
  await shot(p, scheme, '11-destroy-confirm');
  await p.click('#dialog-ok');
  await p.waitForTimeout(200);
  await shot(p, scheme, '12-destroyed');
  await p.click('#btn-about');
  await shot(p, scheme, '13-about');
  await p.context().close();

  p = await page(browser, scheme, 'nodaemon');
  await shot(p, scheme, '14-no-daemon');
  await p.context().close();
}

(async () => {
  fs.mkdirSync(OUT, { recursive: true });
  const server = await serve(PORT);
  const browser = await chromium.launch();
  try {
    for (const scheme of ['light', 'dark']) await run(browser, scheme);
  } finally {
    await browser.close();
    server.close();
  }
  if (problems.length) {
    console.log(problems.join('\n'));
    process.exitCode = 1;
  } else {
    console.log('No console errors, page errors, CSP violations or inline styles. Screenshots in ' + OUT);
  }
})();
