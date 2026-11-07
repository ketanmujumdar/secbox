// A stand-in for window.__TAURI__, for looking at ui/ in a plain browser.
// Every answer has the shape the Rust side sends (src-tauri/src/ops.rs and
// lib.rs). The values are made up. This file is never bundled: the app's
// frontendDist is ../ui only.
//
// The scene comes from ?scene=: empty, projects, starting (holds the start
// part-way), confirming (holds it at the Touch ID prompt), stopped, nodaemon.
'use strict';

(() => {
  const params = new URLSearchParams(location.search);
  const scene = params.get('scene') || 'projects';
  const hold = scene === 'starting';
  const holdAtConfirm = scene === 'confirming';
  const METHOD = 'Touch ID or your login password';

  const STATE = '/Users/ana/.secbox';
  const CRM = STATE + '/remote/7f3a9c0e21/demo_shop';
  const DOCS = '/Users/ana/code/docs-site';
  const BILLING = '/Users/ana/code/billing-api';

  const projects = scene === 'empty' ? [] : [
    { name: 'demo_shop', path: CRM, state: (scene === 'stopped' || scene === 'confirming') ? 'stopped' : 'running', label: (scene === 'stopped' || scene === 'confirming') ? 'Stopped' : 'Running' },
    { name: 'docs-site', path: DOCS, state: 'none', label: 'No sandbox' },
    { name: 'billing-api', path: BILLING, state: 'stopped', label: 'Stopped' },
  ];
  const daemonRunning = scene !== 'nodaemon';

  const resources = { memory: '6 GB', cpus: '4', disk: '30 GB', build_memory: '8 GB', clamped: false };

  function status(path) {
    const row = projects.find((p) => p.path === path) || { state: 'none', label: 'No sandbox' };
    const running = row.state === 'running';
    return {
      path,
      state: row.state,
      label: row.label,
      services: running ? [
        { name: 'web', state: 'running (healthy)' },
        { name: 'api', state: 'running (healthy)' },
        { name: 'worker', state: 'running' },
        { name: 'postgres', state: 'running (healthy)' },
      ] : [],
      ports: running ? ['127.0.0.1:3000', '127.0.0.1:8080'] : [],
      resources: row.state === 'none' ? null : resources,
      blocked_total: row.state === 'none' ? null : 41,
      blocked_domains: row.state === 'none' ? null : 3,
      exposure: row.state === 'none' ? [] : [
        'STRIPE_KEY is given to api',
        'DATABASE_URL is given to api, worker',
        'Code in those services could send them to any of these 1 hosts, and nowhere else:',
        '  api.stripe.com',
      ],
      notes: running ? ['These came from inside the sandbox, so they can be wrong.'] : [],
      log_services: running ? ['api', 'postgres', 'web', 'worker'] : [],
    };
  }

  const plan = {
    path: CRM,
    digest: '9e1d4c07a3b2f6118d0c55e2a1f4b3c9d8e7f6a5b4c3d2e1f0a9b8c7d6e5f4a3',
    summary: [
      '4 services from commit 4f2c1a9e0b7d',
      'Forward to 127.0.0.1:  3000  8080  5432',
      'Anything on this machine could then reach them.',
      '3 hosts allowed for the build, 1 while it runs.',
      'Build hosts stay reachable after the build.',
    ],
    services: ['web', 'api', 'worker', 'postgres'],
    commit: '4f2c1a9e0b7d5c3a2f1e0d9c8b7a6f5e4d3c2b1a',
    ports: [
      { service: 'web', host_port: 3000, container_port: 3000, note: 'Asked for every interface; forwarded on 127.0.0.1 only.' },
      { service: 'api', host_port: 8080, container_port: 8080, note: null },
      { service: 'postgres', host_port: 5432, container_port: 5432, note: null },
    ],
    build_hosts: ['registry.npmjs.org', 'deb.debian.org', 'pypi.org'],
    run_hosts: ['api.stripe.com'],
    secrets: ['STRIPE_KEY: put in the environment for api', 'DATABASE_URL: put in the environment for api, worker'],
    resources,
    findings: [
      'High (worker): asks for privileged: true. It is dropped.',
      'Medium (web): mounts ./. It stays inside the sandbox and never reaches your disk.',
    ],
    warnings: [],
    generated_compose: [],
    exposure: [
      'STRIPE_KEY is given to api',
      'DATABASE_URL is given to api, worker',
      'Code in those services could send them to any of these 4 hosts, and nowhere else:',
      '  api.stripe.com',
      '  deb.debian.org',
      '  pypi.org',
      '  registry.npmjs.org',
    ],
    blockers: [],
  };

  const blocked = {
    recording: true,
    total: 41,
    rows: [
      { time: '2026-11-06 14:06:09', count: 28, domain: 'telemetry.example-cdn.com' },
      { time: '2026-11-06 14:05:51', count: 9, domain: 'sentry.io' },
      { time: '2026-11-06 14:02:12', count: 4, domain: 'fonts.googleapis.com' },
    ],
    notes: [
      { weight: 'warn', text: "This shows blocked hostnames only. Raw IP, metadata and UDP attempts are blocked but not recorded, so an empty list doesn't mean nothing was tried." },
      { weight: 'plain', text: 'The engine writes this record on the host. As measured, the guest cannot write to its folder, but the guest does choose what it looks up.' },
    ],
  };

  const diff = {
    baseline: '4f2c1a9e0b7d',
    flags: [
      { path: '.git/hooks/pre-commit', message: 'adds a Git hook, which would run on your machine the next time you commit there' },
      { path: 'package.json:14', message: 'adds a postinstall script: node scripts/setup.js' },
    ],
    files: [
      { change: 'modified', path: 'src/billing/refunds.ts' },
      { change: 'added', path: 'src/billing/refunds.test.ts' },
      { change: 'modified', path: 'src/api/routes.ts' },
      { change: 'added', path: 'migrations/0042_refunds.sql' },
      { change: 'deleted', path: 'src/legacy/charge_v1.ts' },
      { change: 'modified', path: 'package.json' },
      { change: 'added', path: '.git/hooks/pre-commit' },
    ],
    notes: [
      { weight: 'alert', text: '2 flags to look at. Flags are a heuristic aid. Read the changes yourself before anything from this sandbox reaches your machine.' },
      { weight: 'warn', text: 'Nothing was copied out of the sandbox. Export is not built yet.' },
    ],
  };

  const LOG = [
    'web       | ready on http://0.0.0.0:3000\n',
    'api       | listening on :8080\n',
    'postgres  | database system is ready to accept connections\n',
    'worker    | connected to queue "emails"\n',
    'api       | POST /charges 201 182ms\n',
    'api       | stripe: using key [STRIPE_KEY]\n',
    'web       | GET /dashboard 200 41ms\n',
    'worker    | error: getaddrinfo ENOTFOUND sentry.io\n',
    'worker    | retrying in 30s\n',
    'api       | GET /customers?page=2 200 23ms\n',
  ];

  const later = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
  let nextLog = 1;

  class Channel {
    constructor() { this.onmessage = null; }
    send(message) { if (this.onmessage) this.onmessage(message); }
  }

  async function invoke(cmd, args) {
    args = args || {};
    window.__calls = window.__calls || [];
    window.__calls.push({ cmd, args: JSON.parse(JSON.stringify(args, (k, v) => (v instanceof Channel ? '[channel]' : v))) });
    await later(30);
    const needDaemon = !['app_info', 'daemon_state', 'list_projects', 'start_daemon', 'logs_stop'].includes(cmd);
    if (needDaemon && !daemonRunning) throw 'The daemon is not running. Start it from the menu bar icon.';
    switch (cmd) {
      case 'app_info':
        return { product: 'secbox', cli_bin: 'secbox', daemon_bin: 'secboxd', version: '0.1.0', state_dir: STATE, bundled_cli: '/Applications/secbox.app/Contents/MacOS/secbox' };
      case 'daemon_state':
        return { running: daemonRunning, message: daemonRunning ? null : 'Nothing answered on the socket.', notice: null };
      case 'start_daemon':
        return args.restart ? 'Restarted secboxd from /Applications/secbox.app/Contents/MacOS/secboxd.' : 'The daemon is already running.';
      case 'list_projects':
        return daemonRunning ? projects : projects.map((p) => Object.assign({}, p, { state: 'no_answer', label: 'No answer' }));
      case 'add_project':
        projects.push({ name: args.path.split('/').filter(Boolean).pop(), path: args.path, state: 'none', label: 'No sandbox' });
        return 'Added.';
      case 'import_remote':
        if (!projects.some((p) => p.path === CRM)) projects.push({ name: 'demo_shop', path: CRM, state: 'none', label: 'No sandbox' });
        return { project_path: CRM, commit: '4f2c1a9e0b7d5c3a2f1e0d9c8b7a6f5e4d3c2b1a', fetched: true, message: 'Fetched ' + args.url + ' at 4f2c1a9e0b7d. Your key stayed on this machine.' };
      case 'status':
        return status(args.project);
      case 'plan':
        return Object.assign({}, plan, { path: args.project });
      case 'up': {
        const ch = args.onProgress;
        const steps = ['planning', 'confirm', 'sandbox', 'snapshot', 'docker', 'secrets', 'build'];
        for (const step of steps) {
          ch.send({ step, detail: step === 'build' ? 'web, api, worker' : step === 'confirm' ? METHOD : null });
          if (hold && step === 'docker') return new Promise(() => {});
          if (holdAtConfirm && step === 'confirm') return new Promise(() => {});
          await later(40);
        }
        const row = projects.find((p) => p.path === args.project);
        if (row) { row.state = 'running'; row.label = 'Running'; }
        return 'Started in 41s.' + (args.ports.length ? ' Forwarded to ' + args.ports.map((p) => '127.0.0.1:' + p).join(', ') + '.' : '');
      }
      case 'down': {
        const row = projects.find((p) => p.path === args.project);
        if (row) { row.state = 'stopped'; row.label = 'Stopped'; }
        return 'Stopped. Its data is kept for the next start.';
      }
      case 'destroy': {
        const row = projects.find((p) => p.path === args.project);
        if (row) { row.state = 'none'; row.label = 'No sandbox'; }
        return 'Destroyed. Rotate every secret it was given.';
      }
      case 'remove_project': {
        const i = projects.findIndex((p) => p.path === args.project);
        if (i >= 0) projects.splice(i, 1);
        return 'Removed, with its imported copy. The remote itself was not touched. Any secrets you stored for it stay in your keychain.';
      }
      case 'logs': {
        const id = nextLog++;
        const ch = args.onEvent;
        (async () => {
          for (const line of LOG) {
            if (!args.service || line.startsWith(args.service)) ch.send({ kind: 'text', text: line });
          }
          if (!args.follow) ch.send({ kind: 'end', note: null });
        })();
        return id;
      }
      case 'logs_stop':
        return null;
      case 'blocked':
        return blocked;
      case 'diff':
        await later(100);
        return diff;
      case 'git':
        if (args.op === 'run') {
          const pullStuck = args.args[0] === 'pull' && !args.args.includes('--autostash');
          return {
            ok: !pullStuck,
            title: 'git ' + args.args[0] + (pullStuck ? ' failed' : ' done'),
            output: pullStuck
              ? ['Updating 9a96291..4e04d53', 'error: Your local changes to the following files would be overwritten by merge:', '\tcompose.yaml', 'Please commit your changes or stash them before you merge.', 'Aborting']
              : args.args[0] === 'log' ? ['4f2c1a9 Fix the refund total', '9a96291 Add refunds'] : [],
            updates: [],
            branch: 'refunds',
            refused: null,
          };
        }
        return {
          ok: true,
          title: { status: 'Status', pull: 'Pull', commit: 'Commit', push: 'Push' }[args.op] + ' done',
          output: args.op === 'status' ? ['On branch refunds', 'Changes not staged for commit:', '  modified:   src/billing/refunds.ts'] : [],
          updates: args.op === 'push' ? ['refunds 0000000000 -> 4f2c1a9e0b'] : [],
          branch: 'refunds',
          refused: null,
        };
      default:
        throw 'The stub has no answer for ' + cmd;
    }
  }

  window.__TAURI__ = { core: { invoke, Channel } };
})();
