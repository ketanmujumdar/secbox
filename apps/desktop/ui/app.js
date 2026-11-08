// The window. Plain script, no build step, no framework.
//
// Every string that came from the daemon or a sandbox goes into the page with
// textContent, through el() below. Nothing here uses innerHTML, so a service
// name or log line can be a lie but never markup.
'use strict';

(() => {
  const core = window.__TAURI__ && window.__TAURI__.core;
  if (!core) {
    document.body.textContent = 'This page only works inside the desktop app.';
    return;
  }
  const invoke = core.invoke;
  const Channel = core.Channel;

  const $ = (id) => document.getElementById(id);

  // Makes an element whose content is text only.
  function el(tag, text, className) {
    const node = document.createElement(tag);
    if (text !== undefined && text !== null) node.textContent = String(text);
    if (className) node.className = className;
    return node;
  }
  function clear(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }
  function show(node, visible) {
    node.classList.toggle('hidden', !visible);
  }
  function errorText(error) {
    return typeof error === 'string' ? error : 'Something went wrong: ' + String(error);
  }
  function plural(n, one, many) {
    return n + ' ' + (n === 1 ? one : many);
  }
  function lastSegment(path) {
    const parts = String(path).split('/').filter(Boolean);
    return parts.length ? parts[parts.length - 1] : String(path);
  }

  const state = {
    info: null,
    projects: [],
    selected: null,
    status: null,
    statusFailure: null,
    page: null,
    tab: 'overview',
    busy: false,
    // The path of the project being started, while `up` runs.
    starting: null,
    daemonRunning: false,
    logId: null,
    logService: '',
    // The plan on the Review page, and how it was reached.
    review: null,
    // Comparisons made in this window, by project path. Never guessed.
    diffs: new Map(),
    // Which file each project's comparison has on show.
    openFile: new Map(),
    diffMode: 'split',
    comparing: false,
    compareAgain: false,
    // The newest blocked hostnames, for the overview card.
    blocked: null,
    // The Secrets tab's names, for the selected project. Never a value.
    secrets: null,
    // The secret being edited: its name, or '' for a new one.
    editing: null,
  };

  // ---------- messages ----------

  // kind is 'ok', 'info' or anything else for a failure.
  function banner(text, kind) {
    const node = $('banner');
    node.textContent = text || '';
    node.classList.toggle('ok', kind === 'ok');
    node.classList.toggle('info', kind === 'info');
    show(node, !!text);
  }
  function busy(text) {
    state.busy = !!text;
    $('busy').textContent = text || '';
    show($('busy'), !!text);
    updateButtons();
  }

  // The daemon, not this window, asks the person to confirm a start, a new
  // hostname or a push, in the system's own prompt. This window cannot answer
  // that prompt; it only says one is up and what to check.
  function confirmWords(method) {
    return 'Confirm with ' + (method || 'the system prompt') +
      '. Check the prompt names what you asked for; if you did not ask, cancel it.';
  }
  // Said after an action this computer could not ask the person about, so a
  // start, approval or push on Linux never reads as confirmed.
  const NOT_CHECKED = 'Not confirmed by fingerprint: this computer has no fingerprint or password check, so it went ahead on this request alone.';
  function personChannel() {
    const channel = new Channel();
    channel.unchecked = false;
    channel.onmessage = (event) => {
      if (event.step === 'confirm') busy(confirmWords(event.detail));
      if (event.step === 'unconfirmed') channel.unchecked = true;
    };
    return channel;
  }

  // ---------- dialog ----------

  let dialogResolve = null;
  function ask({ title, build, okText, danger, okHidden }) {
    closeMenu();
    $('dialog-title').textContent = title;
    const body = $('dialog-body');
    clear(body);
    build(body);
    const ok = $('dialog-ok');
    ok.textContent = okText || 'OK';
    ok.className = danger ? 'danger' : 'primary';
    show(ok, !okHidden);
    $('dialog-cancel').textContent = okHidden ? 'Close' : 'Cancel';
    $('dialog').showModal();
    // The safe answer has the focus, so Enter never agrees by accident.
    $('dialog-cancel').focus();
    return new Promise((resolve) => { dialogResolve = resolve; });
  }
  function closeDialog(answer) {
    if ($('dialog').open) $('dialog').close();
    if (dialogResolve) {
      const resolve = dialogResolve;
      dialogResolve = null;
      resolve(answer);
    }
  }
  $('dialog-ok').addEventListener('click', () => closeDialog(true));
  $('dialog-cancel').addEventListener('click', () => closeDialog(false));
  $('dialog').addEventListener('cancel', () => closeDialog(false));

  // ---------- pages ----------

  function showPage(page) {
    if (state.page === 'project' && page !== 'project') stopLogs();
    state.page = page;
    for (const node of document.querySelectorAll('.page')) {
      show(node, node.dataset.page === page);
    }
    const hasProjects = state.projects.length > 0;
    show($('rail'), hasProjects);
    show($('choose-cancel'), page === 'choose' && hasProjects && !!state.selected);
    for (const item of $('projects').children) {
      item.classList.toggle('selected', page === 'project' && item.dataset.path === state.selected);
    }
    $('btn-problems').classList.toggle('selected', page === 'problems');
    $('content').scrollTop = 0;
  }

  function goBack() {
    if (state.page === 'git' || state.page === 'folder') {
      showPage('choose');
    } else if (state.selected && state.projects.length) {
      openProject(state.selected);
    } else {
      showPage('choose');
    }
  }
  for (const button of document.querySelectorAll('[data-back]')) {
    button.addEventListener('click', goBack);
  }

  // ---------- daemon ----------

  async function refreshDaemon() {
    let daemon;
    try {
      daemon = await invoke('daemon_state');
    } catch (error) {
      daemon = { running: false, message: errorText(error), notice: null };
    }
    state.daemonRunning = daemon.running;
    const bin = state.info ? state.info.daemon_bin : 'The daemon';
    const starting = !daemon.running && !!daemon.starting;
    const words = bin + (daemon.running ? ' running' : starting ? ' starting' : ' not running');
    for (const id of ['daemon-dot', 'choose-dot']) $(id).className = 'dot ' + (daemon.running ? 'on' : 'off');
    $('daemon-text').textContent = words;
    $('choose-daemon').textContent = daemon.running || starting ? words : words + '. Start it from the menu bar icon, or below.';
    const action = $('daemon-action');
    action.textContent = daemon.running ? 'Restart' : 'Start it';
    show(action, !starting);
    const title = !daemon.running && !starting && daemon.message ? daemon.message : '';
    $('daemon-text').title = title;
    $('choose-daemon').title = title;
    show($('notice'), !!daemon.notice);
    $('notice').textContent = daemon.notice || '';
    updateButtons();
    if (starting) watchDaemonStart();
    return daemon;
  }

  // The app starts the daemon by itself when it opens. Show the result as soon
  // as there is one, not at the next regular refresh.
  function watchDaemonStart() {
    if (state.daemonWatch) return;
    state.daemonWatch = setTimeout(async () => {
      state.daemonWatch = null;
      await refreshDaemon();
      if (!state.daemonRunning || state.busy || state.starting) return;
      await refreshProjects();
      if (state.page === 'project') await loadStatus();
    }, 500);
  }

  async function startDaemon(restart) {
    if (restart) {
      const agreed = await ask({
        title: 'Restart the daemon',
        okText: 'Restart it',
        build: (body) => {
          body.appendChild(el('p', 'The daemon stops and starts again. Sandboxes are separate processes and keep running.'));
          body.appendChild(el('p', 'A start, log or Git command in progress is cut off.', 'muted'));
        },
      });
      if (!agreed) return;
    }
    stopLogs();
    busy(restart ? 'Restarting the daemon.' : 'Starting the daemon.');
    try {
      banner(await invoke('start_daemon', { restart: !!restart }), 'ok');
    } catch (error) {
      banner(errorText(error));
    }
    busy('');
    await refreshDaemon();
    await refreshProjects();
    if (state.page === 'project') await loadStatus();
  }
  $('daemon-action').addEventListener('click', () => startDaemon(state.daemonRunning));

  // ---------- projects ----------

  function stateClass(s) {
    return s === 'running' ? 'running' : s === 'stopped' ? 'stopped' : s === 'no_answer' ? 'no-answer' : '';
  }

  function isRemote(path) {
    const dir = state.info && state.info.state_dir;
    if (!dir) return null;
    return String(path).startsWith(dir.replace(/\/+$/, '') + '/remote/');
  }

  async function refreshProjects() {
    let rows = [];
    try {
      rows = await invoke('list_projects');
    } catch (error) {
      banner(errorText(error));
    }
    state.projects = rows;
    const list = $('projects');
    clear(list);
    for (const row of rows) {
      const item = el('li');
      item.dataset.path = row.path;
      item.title = row.path;
      const startingThis = state.starting === row.path;
      item.appendChild(el('span', null, 'dot ' + (startingThis ? 'starting' : stateClass(row.state))));
      item.appendChild(el('span', row.name, 'name'));
      item.appendChild(el('span', startingThis ? 'starting' : row.label.toLowerCase(), 'word'));
      if (state.page === 'project' && row.path === state.selected) item.classList.add('selected');
      item.addEventListener('click', () => openProject(row.path));
      list.appendChild(item);
    }
    if (state.selected && !rows.some((r) => r.path === state.selected)) {
      state.selected = null;
      state.status = null;
    }
    show($('rail'), rows.length > 0);
    if (state.page === 'project') renderHeader();
  }

  function selectedRow() {
    return state.projects.find((p) => p.path === state.selected) || null;
  }

  async function openProject(path) {
    if (state.selected !== path) {
      stopLogs();
      clear($('log-text'));
      $('log-note').textContent = '';
      state.logService = '';
      state.status = null;
      state.blocked = null;
      clearReview();
      clearGit();
    }
    state.selected = path;
    showPage('project');
    renderHeader();
    renderOverview(null);
    await openTab(state.tab);
    await loadStatus();
    await readTraffic();
    if (state.tab === 'logs') await readLogs();
  }

  function renderHeader() {
    const row = selectedRow();
    const s = state.status;
    $('project-name').textContent = row ? row.name : lastSegment(state.selected || '');
    const remote = row ? isRemote(row.path) : null;
    $('project-source').textContent = row
      ? (remote ? 'Cloned from Git · copy kept at ' + row.path : row.path)
      : '';
    const pill = $('project-state');
    const startingThis = !!state.starting && state.starting === state.selected;
    if (startingThis) {
      pill.textContent = 'Starting';
      pill.className = 'pill starting';
    } else if (s || row) {
      pill.textContent = s ? s.label : row.label;
      pill.className = 'pill ' + stateClass(s ? s.state : row.state);
    }
    show(pill, startingThis || !!s || !!row);
    renderBadges();
    updateButtons();
  }

  function renderBadges() {
    const s = state.status;
    const net = $('badge-network');
    const hosts = s && typeof s.blocked_domains === 'number' ? s.blocked_domains : 0;
    net.textContent = String(hosts);
    show(net, hosts > 0);
    const changes = $('badge-changes');
    const diff = state.diffs.get(state.selected);
    changes.textContent = diff ? String(diff.view.files.length) : '';
    show(changes, !!diff && diff.view.files.length > 0);
  }

  function updateButtons() {
    const s = state.status;
    const ready = !!state.selected && state.daemonRunning;
    const free = ready && !state.busy;
    const startingThis = !!state.starting && state.starting === state.selected;
    // Stop and Destroy stay on during a start: they are the only way out of one
    // that is stuck. The daemon is left to decide.
    const canEnd = ready && (!state.busy || startingThis);
    const running = !!s && s.state === 'running';
    // A button is hidden only when the known state makes it pointless. With no
    // state at all, both are offered and the daemon decides, as the terminal does.
    show($('btn-start'), !startingThis && (!s || !running));
    show($('btn-stop'), startingThis || !s || running);
    $('btn-start').disabled = !free || !!state.starting;
    $('btn-stop').disabled = !canEnd;
    $('menu-restart').disabled = !free || !running || !!state.starting;
    $('menu-destroy').disabled = !canEnd;
    $('menu-remove').disabled = !free;
    $('settings-destroy').disabled = !canEnd;
    // The agent's tools never start a sandbox, so there is nothing to connect to.
    $('agent-connect').disabled = !free || !running;
    $('btn-vscode').disabled = !free || !running;
    $('settings-remove').disabled = !free;
    $('btn-more').disabled = !ready;
    for (const id of ['git-go', 'folder-go', 'review-start']) $(id).disabled = state.busy;
    for (const id of ['git-status', 'git-pull', 'git-push', 'git-autostash', 'git-run-go', 'review-read', 'log-read', 'traffic-read']) $(id).disabled = !free;
    if (state.comparing) $('review-read').disabled = true;
    if ($('daemon-action')) $('daemon-action').disabled = state.busy;
  }

  // ---------- the ••• menu ----------

  function closeMenu() {
    show($('more-menu'), false);
    $('btn-more').setAttribute('aria-expanded', 'false');
  }
  $('btn-more').addEventListener('click', (event) => {
    event.stopPropagation();
    const open = $('more-menu').classList.contains('hidden');
    show($('more-menu'), open);
    $('btn-more').setAttribute('aria-expanded', open ? 'true' : 'false');
  });
  document.addEventListener('click', (event) => {
    if (!$('more-menu').contains(event.target)) closeMenu();
  });
  document.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') closeMenu();
  });

  // ---------- overview ----------

  function row(list, left, right, leftClass, rightClass) {
    const item = el('li');
    item.appendChild(el('span', left, leftClass || ''));
    item.appendChild(el('span', right, 'right' + (rightClass ? ' ' + rightClass : '')));
    list.appendChild(item);
    return item;
  }
  function emptyRow(list, text) {
    list.appendChild(el('li', text, 'empty-line'));
  }
  function kv(dl, pairs) {
    clear(dl);
    for (const [k, v, cls] of pairs) {
      dl.appendChild(el('dt', k));
      dl.appendChild(el('dd', v, cls || ''));
    }
  }

  async function loadStatus() {
    const path = state.selected;
    if (!path) return;
    let status = null;
    let failure = null;
    try {
      status = await invoke('status', { project: path });
    } catch (error) {
      failure = 'The daemon could not report on this project: ' + errorText(error);
    }
    if (state.selected !== path) return;
    state.status = status;
    state.statusFailure = failure;
    renderHeader();
    renderOverview(failure);
    renderLogServices();
    renderSecrets();
    renderSettings();
    if (state.tab === 'secrets') loadSecrets();
    if (state.tab === 'settings') loadLimits();
  }

  function resourcePairs(r) {
    return r
      ? [['Memory', r.memory], ['CPUs', r.cpus], ['Disk', r.disk], ['Memory while building', r.build_memory]]
      : [['No limits on record', 'They are set when it starts']];
  }

  function usageRow(label, text, percent) {
    const item = el('div', null, 'usage-row');
    const line = el('div', null, 'usage-line');
    line.appendChild(el('span', label, 'label-text'));
    line.appendChild(el('span', text));
    item.appendChild(line);
    if (percent !== null && percent !== undefined) {
      const meter = el('meter');
      meter.min = 0;
      meter.max = 100;
      meter.low = 70;
      meter.high = 90;
      meter.optimum = 0;
      meter.value = percent;
      item.appendChild(meter);
    }
    return item;
  }

  function renderUsage(s) {
    const box = $('usage');
    clear(box);
    const usage = s.usage;
    show(box, !!usage);
    show($('resources'), !usage);
    if (!usage) return;
    box.appendChild(usageRow('CPU', usage.cpu, usage.cpu_percent));
    box.appendChild(usageRow('Memory', usage.memory, usage.memory_percent));
    if (usage.disk) box.appendChild(usageRow('Disk', usage.disk, usage.disk_percent));
  }

  function renderOverview(failure) {
    const s = state.status;
    show($('overview-grid'), !!s);
    show($('overview-empty'), !s);
    $('overview-empty').textContent = failure || (s ? '' : 'Asking the daemon.');
    const notes = $('status-notes');
    clear(notes);
    if (!s) return;

    kv($('resources'), resourcePairs(s.resources));
    renderUsage(s);
    $('resources-note').textContent = s.usage
      ? 'This Mac is holding ' + s.usage.host_memory + ' of memory for it. Disk is what the sandbox has written.'
      : !s.resources ? ''
      : s.resources.clamped ? 'Lowered to fit your own maximum.'
      : 'Limits, not current use.';

    const services = $('services');
    clear(services);
    if (s.services.length === 0) emptyRow(services, s.state === 'running' ? 'None reported.' : 'Nothing is running.');
    for (const svc of s.services) {
      const item = el('li');
      const up = /^running/i.test(svc.state);
      item.appendChild(el('span', null, 'dot ' + (up ? 'running' : 'stopped')));
      item.appendChild(el('span', svc.name, 'strong'));
      item.appendChild(el('span', svc.state, 'right muted'));
      services.appendChild(item);
    }

    const ports = $('ports');
    clear(ports);
    if (s.ports.length === 0) emptyRow(ports, 'No ports forwarded.');
    for (const p of s.ports) row(ports, p, 'forwarded', 'mono', 'muted');

    renderBlockedCard();
    renderChangesCard();

    const exposure = $('exposure');
    fillExposure(exposure, s.exposure);
    show($('exposure-card'), s.exposure.length > 0);

    for (const text of s.notes) notes.appendChild(el('li', text, 'warn'));
  }

  function renderBlockedCard() {
    const s = state.status;
    const count = $('blocked-count');
    const top = $('blocked-top');
    clear(top);
    if (!s || s.blocked_total === null || s.blocked_total === undefined) {
      count.textContent = 'Not reported';
      count.className = 'count muted';
    } else {
      count.textContent = plural(s.blocked_domains, 'host', 'hosts') + ' · ' + plural(s.blocked_total, 'try', 'tries');
      count.className = 'count' + (s.blocked_total > 0 ? ' bad-text' : ' muted');
    }
    $('blocked-card').classList.toggle('alert-card', !!s && s.blocked_total > 0);
    const recorded = !!s && typeof s.blocked_total === 'number';
    const rows = recorded && state.blocked ? state.blocked.rows.slice(0, 3) : [];
    for (const event of rows) row(top, event.domain, String(event.count), 'mono', 'muted');
    if (rows.length === 0) emptyRow(top, 'Blocked hostnames only. Zero proves nothing.');
    if (s && s.blocked_cut_off) emptyRow(top, 'Some older attempts were lost before they could be read.');
  }

  function renderChangesCard() {
    const diff = state.diffs.get(state.selected);
    const count = $('changes-count');
    const flagged = $('changes-flagged');
    if (!diff) {
      count.textContent = 'Not compared yet';
      count.className = 'big-number quiet';
      flagged.textContent = 'Runs inside the sandbox when you ask. Nothing is copied out.';
      flagged.className = 'small-text muted';
      $('changes-when').textContent = '';
      $('btn-changes').textContent = 'Compare in Changes →';
      return;
    }
    const v = diff.view;
    count.textContent = plural(v.files.length, 'file', 'files');
    count.className = 'big-number';
    if (v.flags.length) {
      flagged.textContent = plural(v.flags.length, 'flag', 'flags') + ': ' + v.flags[0].path + (v.flags.length > 1 ? ' and more' : '');
      flagged.className = 'small-text bad-text';
    } else {
      flagged.textContent = 'Nothing flagged. Flags are a heuristic aid.';
      flagged.className = 'small-text muted';
    }
    $('changes-when').textContent = 'compared at ' + diff.when;
    $('btn-changes').textContent = 'Review changes →';
  }

  function fillExposure(list, lines) {
    clear(list);
    for (const line of lines) {
      // Host lines under a secret are indented by the daemon.
      list.appendChild(el('li', line.trim(), /^\s/.test(line) ? 'host' : 'secret'));
    }
  }

  $('btn-traffic').addEventListener('click', () => openTab('network'));
  $('btn-changes').addEventListener('click', () => openTab('changes'));

  // ---------- secrets and settings ----------

  function renderSecrets() {
    const s = state.status;
    const list = $('secrets-exposure');
    if (s && s.exposure.length) {
      fillExposure(list, s.exposure);
    } else {
      clear(list);
      list.appendChild(el('li', s ? 'No secret is assigned to this project.' : 'Not known: the daemon did not report on it.', 'empty-line'));
    }
  }

  async function loadSecrets() {
    const path = state.selected;
    if (!path) return;
    let view = null;
    try {
      view = await invoke('secrets', { project: path });
    } catch (error) {
      if (state.selected === path) banner('Could not list the secrets: ' + errorText(error));
    }
    if (state.selected !== path) return;
    state.secrets = view;
    renderSecretRows();
  }

  function renderSecretRows() {
    const view = state.secrets;
    const list = $('secret-rows');
    closeSecretForm();
    clear(list);
    if (!view) {
      emptyRow(list, 'Not known: the daemon did not list them.');
      show($('secret-empty-note'), false);
      return;
    }
    if (!view.rows.length) {
      emptyRow(list, 'No secrets yet. Press Use a file if the project has an env file, or add one by hand.');
    }
    for (const secret of view.rows) {
      const item = el('li', null, secret.stored ? '' : 'empty-secret');
      item.dataset.name = secret.name;
      item.appendChild(el('span', secret.name, 'secret-name'));
      item.appendChild(el('span', secret.stored ? 'Stored' : 'Empty', 'tag ' + (secret.stored ? 'good' : 'warn')));
      item.appendChild(el('span', secret.goes_to, 'secret-goes'));
      const button = el('button', secret.stored ? 'Replace' : 'Set value', secret.stored ? '' : 'primary');
      button.addEventListener('click', () => openSecretForm(secret, item));
      item.appendChild(button);
      list.appendChild(item);
    }
    const empty = view.rows.filter((r) => !r.stored).length;
    $('secret-empty-note').textContent = empty
      ? plural(empty, 'name has', 'names have') + ' no value yet. Nothing is delivered for ' + (empty === 1 ? 'it' : 'them') + ' until you set one.'
      : '';
    show($('secret-empty-note'), empty > 0);
  }

  function cliBin() {
    return state.info ? state.info.cli_bin : 'secbox';
  }

  // One form, moved under the row being edited, or to the end for a new one.
  function openSecretForm(secret, item) {
    closeSecretForm();
    const view = state.secrets;
    if (!view) return;
    const form = $('secret-form');
    state.editing = secret ? secret.name : '';
    show($('secret-name-field'), !secret);
    $('secret-name').value = '';
    $('secret-value').value = '';
    $('secret-value-label').textContent = secret
      ? (secret.stored ? 'New value for ' : 'Value for ') + secret.name
      : 'Value';
    const boxes = $('secret-services');
    clear(boxes);
    const ticked = new Set(secret ? secret.services : []);
    for (const name of view.services) {
      const li = el('li');
      const label = el('label');
      const box = document.createElement('input');
      box.type = 'checkbox';
      box.value = name;
      box.checked = ticked.has(name);
      label.appendChild(box);
      label.appendChild(el('span', name, 'mono'));
      li.appendChild(label);
      boxes.appendChild(li);
    }
    $('secret-services-note').textContent = view.services.length
      ? 'Only the services ticked get it. Leave all unticked to store it without giving it to any.'
      : (view.note || 'This project has no services to give it to.') + ' It can still be stored.';
    const holder = el('li', null, 'secret-edit');
    holder.appendChild(form);
    if (item) item.after(holder);
    else $('secret-rows').appendChild(holder);
    show(form, true);
    (secret ? $('secret-value') : $('secret-name')).focus();
  }

  function closeSecretForm() {
    const form = $('secret-form');
    $('secret-value').value = '';
    show(form, false);
    const holder = form.parentElement;
    if (holder && holder.classList.contains('secret-edit')) {
      $('secret-card').appendChild(form);
      holder.remove();
    }
    state.editing = null;
  }

  // Using an env file. Only names reach this window: the daemon reads the file
  // and writes the keychain, each time it is asked.
  const FILE_EFFECTS = {
    store: ['Will be stored', 'good'],
    replace: ['Replaces the stored value', 'warn'],
    empty: ['No value, add to fill in', 'warn'],
    keep: ['No value, stored one stays', ''],
    skip: ['Left out: value has a line break', 'warn'],
  };
  let filePreview = null;

  function closeFilePanel() {
    filePreview = null;
    show($('file-panel'), false);
  }

  async function openFilePanel() {
    const path = state.selected;
    if (!path) return;
    closeSecretForm();
    filePreview = null;
    show($('file-panel'), true);
    show($('file-choose'), true);
    show($('file-review'), false);
    show($('file-apply'), false);
    $('file-path').value = '';
    const rows = $('file-rows');
    clear(rows);
    emptyRow(rows, 'Looking for env files.');
    show($('file-note'), false);
    let found;
    try {
      found = await invoke('secret_files', { project: path });
    } catch (error) {
      if (state.selected !== path) return;
      clear(rows);
      emptyRow(rows, 'Could not look: ' + errorText(error));
      return;
    }
    if (state.selected !== path) return;
    clear(rows);
    const add = (where, source, at) => {
      const item = el('li');
      item.appendChild(el('span', where, 'file-where'));
      item.appendChild(el('span', at, 'file-at'));
      const button = el('button', 'Choose');
      button.addEventListener('click', () => readFile(source));
      item.appendChild(button);
      rows.appendChild(item);
    };
    for (const at of found.sandbox) add('In the sandbox', { where: 'sandbox', path: at }, at);
    for (const at of found.host) add('On this Mac', { where: 'host', path: at }, at);
    if (!found.sandbox.length && !found.host.length) emptyRow(rows, 'No env files found. Type a path below.');
    $('file-note').textContent = found.note || '';
    show($('file-note'), !!found.note);
  }

  async function readFile(source) {
    const path = state.selected;
    let preview;
    try {
      preview = await invoke('read_secret_file', { project: path, source });
    } catch (error) {
      banner('That file can\'t be used: ' + errorText(error));
      return;
    }
    if (state.selected !== path || $('file-panel').classList.contains('hidden')) return;
    if (!preview.names.length) {
      banner('That file has no names in it.');
      return;
    }
    filePreview = preview;
    show($('file-choose'), false);
    show($('file-review'), true);
    show($('file-apply'), true);
    $('file-title').textContent = preview.source.path + ': ' + plural(preview.names.length, 'name', 'names');
    const names = $('file-names');
    clear(names);
    for (const entry of preview.names) {
      const item = el('li');
      item.appendChild(el('span', entry.name, 'secret-name'));
      const [words, tone] = FILE_EFFECTS[entry.effect] || ['', ''];
      item.appendChild(el('span', words, 'tag ' + tone));
      names.appendChild(item);
    }
    const warnings = [];
    if (preview.source.where === 'sandbox') warnings.push('This file came from the sandbox, so anything running there could have written these values.');
    if (preview.names_only) warnings.push('This is a template, so only its names are used.');
    $('file-warning').textContent = warnings.join(' ');
    show($('file-warning'), warnings.length > 0);
    const boxes = $('file-services');
    clear(boxes);
    for (const name of preview.services) {
      const li = el('li');
      const label = el('label');
      const box = document.createElement('input');
      box.type = 'checkbox';
      box.value = name;
      box.checked = true;
      label.appendChild(box);
      label.appendChild(el('span', name, 'mono'));
      li.appendChild(label);
      boxes.appendChild(li);
    }
    $('file-services-note').textContent = preview.services.length
      ? 'A service can read every secret it is given. Each gets them in its environment.'
      : (preview.services_note || 'This project has no services to give them to.') + ' They can still be stored.';
  }

  $('file-use').addEventListener('click', () => openFilePanel());
  $('file-cancel').addEventListener('click', () => closeFilePanel());
  $('file-path-form').addEventListener('submit', (event) => {
    event.preventDefault();
    const typed = $('file-path').value.trim();
    if (!typed) { banner('Type the file\'s path first.'); return; }
    readFile({ where: 'host', path: typed });
  });
  $('file-apply').addEventListener('click', async () => {
    const path = state.selected;
    const preview = filePreview;
    if (!path || !preview) return;
    const services = [...$('file-services').querySelectorAll('input:checked')].map((box) => box.value);
    $('file-apply').disabled = true;
    try {
      banner(await invoke('use_secret_file', {
        project: path,
        source: preview.source,
        names: preview.names.map((n) => n.name),
        services,
      }), 'ok');
      closeFilePanel();
      await loadSecrets();
      loadStatus();
    } catch (error) {
      banner('Not used: ' + errorText(error));
    } finally {
      $('file-apply').disabled = false;
    }
  });

  $('secret-add').addEventListener('click', () => { closeFilePanel(); openSecretForm(null, null); });
  $('secret-cancel').addEventListener('click', () => closeSecretForm());
  $('secret-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const path = state.selected;
    if (!path || state.editing === null) return;
    const name = state.editing || $('secret-name').value.trim();
    if (!name) { banner('Give the secret a name first.'); return; }
    // Taken out of the field at once, so it is not left on the page while the
    // daemon answers. The browser cannot wipe a string, but it can drop it.
    let value = $('secret-value').value;
    $('secret-value').value = '';
    if (!value) { banner('Type a value first. An empty value is not stored.'); return; }
    const services = [...$('secret-services').querySelectorAll('input:checked')].map((box) => box.value);
    $('secret-save').disabled = true;
    try {
      banner(await invoke('set_secret', { project: path, name, value, services }), 'ok');
      value = null;
      closeSecretForm();
      await loadSecrets();
      loadStatus();
    } catch (error) {
      value = null;
      banner('Not stored: ' + errorText(error));
    } finally {
      $('secret-save').disabled = false;
    }
  });

  function renderSettings() {
    const row = selectedRow();
    const s = state.status;
    const remote = row ? isRemote(row.path) : null;
    kv($('settings-kv'), [
      ['Name', row ? row.name : ''],
      ['Path', state.selected || '', 'mono'],
      ['Comes from', remote === null ? 'Not known' : remote ? 'Cloned from Git; this is the copy the daemon keeps' : 'A folder on this computer, copied in from its commit'],
      ['Sandbox', s ? s.label : (row ? row.label : '')],
    ]);
    kv($('settings-resources'), resourcePairs(s && s.resources));
    $('settings-resources-note').textContent = s && s.resources && s.resources.clamped
      ? 'Lowered to fit your own maximum.'
      : '';
  }

  // ---------- limits ----------

  const GB = 1024 * 1024 * 1024;
  const gb = (bytes) => Math.round((bytes / GB) * 10) / 10;

  async function loadLimits() {
    const path = state.selected;
    if (!path) return;
    let report;
    try {
      report = await invoke('resource_choices', { project: path });
    } catch (error) {
      if (state.selected === path) $('limits-note').textContent = 'Could not read the limits: ' + errorText(error);
      return;
    }
    if (state.selected === path) renderLimits(report);
  }

  function renderLimits(report) {
    const { chosen, next, current } = report;
    // A blank field leaves that limit to the project, so its placeholder says
    // what the project would get.
    $('limit-memory').value = chosen.memory_bytes ? gb(chosen.memory_bytes) : '';
    $('limit-cpus').value = chosen.millicpus ? chosen.millicpus / 1000 : '';
    $('limit-disk').value = chosen.disk_bytes ? gb(chosen.disk_bytes) : '';
    $('limit-memory').placeholder = next ? String(gb(next.memory_bytes)) : '';
    $('limit-cpus').placeholder = next ? String(next.millicpus / 1000) : '';
    $('limit-disk').placeholder = next ? String(gb(next.disk_bytes)) : '';
    $('limit-memory-hint').textContent = 'This Mac can spare ' + Math.floor(gb(report.max_memory_bytes)) + ' GB now.';
    $('limit-cpus-hint').textContent = 'This Mac can spare ' + Math.floor(report.max_millicpus / 1000) + '.';
    $('limit-disk-hint').textContent = current
      ? 'At least ' + gb(current.disk_bytes) + ' GB, since a disk can only grow. ' + Math.floor(gb(report.max_disk_bytes)) + ' GB free.'
      : Math.floor(gb(report.max_disk_bytes)) + ' GB free.';
    const words = report.problem
      ? 'This Mac cannot spare these limits now: ' + report.problem
      : report.restart_needed
        ? 'It is running with other limits. Restart it to apply these; its data is kept.'
        : 'Leave a field blank to use what the project asks for. Changes are shown for review before a start.';
    $('limits-note').textContent = words;
    show($('limits-restart'), report.restart_needed);
    $('limits-restart').disabled = !report.restart_needed || $('menu-restart').disabled;
  }

  function limitFromField(id, unit) {
    const text = $(id).value.trim();
    return text === '' ? null : Math.round(Number(text) * unit);
  }

  async function saveLimits(choice) {
    const path = state.selected;
    if (!path) return;
    $('limits-save').disabled = true;
    $('limits-reset').disabled = true;
    try {
      const report = await invoke('set_resources', { project: path, choice });
      if (state.selected === path) renderLimits(report);
      const row = selectedRow();
      banner(report.restart_needed
        ? 'Saved. Restart it to apply them.'
        : row && row.state === 'stopped' ? 'Saved. It uses them on its next start.'
        : 'Saved.', 'ok');
    } catch (error) {
      banner('Not saved: ' + errorText(error));
    } finally {
      $('limits-save').disabled = false;
      $('limits-reset').disabled = false;
    }
  }

  $('limits-form').addEventListener('submit', (event) => {
    event.preventDefault();
    saveLimits({
      memory_bytes: limitFromField('limit-memory', GB),
      millicpus: limitFromField('limit-cpus', 1000),
      disk_bytes: limitFromField('limit-disk', GB),
    });
  });
  $('limits-reset').addEventListener('click', () => saveLimits({ memory_bytes: null, millicpus: null, disk_bytes: null }));
  $('limits-restart').addEventListener('click', () => startProject(true));
  $('btn-limits').addEventListener('click', () => openTab('settings'));

  // ---------- review and start ----------

  // Lines the Ports card says for itself, since a port can be unticked there.
  const PORT_SUMMARY = [/^Forward to 127\.0\.0\.1:/, /^No ports would be forwarded\.$/, /^Anything on this machine could then reach them\.$/];

  async function reviewPlan({ path, name, restart, from, commit }) {
    busy('Working out what starting it would do.');
    let plan;
    try {
      plan = await invoke('plan', { project: path });
    } catch (error) {
      busy('');
      banner(errorText(error));
      return false;
    }
    busy('');
    banner('');
    state.review = { plan, path, name: name || lastSegment(plan.path || path), restart: !!restart, from: from || null, commit: commit || null };
    renderReview();
    showPage('review');
    // The safe answer has the focus, so Enter never agrees by accident.
    $('review-cancel').focus({ preventScroll: true });
    return true;
  }

  function chips(node, hosts, empty) {
    clear(node);
    if (!hosts.length) {
      node.appendChild(el('span', empty, 'muted small-text'));
      return;
    }
    for (const host of hosts) node.appendChild(el('span', host, 'chip mono'));
  }

  function renderReview() {
    const { plan, name, restart, from } = state.review;
    const blocked = plan.blockers.length > 0;
    $('review-step').textContent = from ? 'Step 2 of 3 · Review' : (restart ? 'Restart · Review' : 'Start · Review');
    $('review-title').textContent = blocked ? name + " can't start yet" : name + ' will be able to…';

    const summary = $('review-summary');
    clear(summary);
    for (const line of plan.summary) {
      if (PORT_SUMMARY.some((re) => re.test(line))) continue;
      summary.appendChild(el('p', line));
    }
    summary.appendChild(el('p', plan.commit
      ? 'Only committed files go in. Nothing else is allowed.'
      : 'From the working tree. Nothing else is allowed.', 'muted'));

    const blockers = $('review-blockers');
    clear(blockers);
    show(blockers, blocked);
    if (blocked) {
      blockers.appendChild(el('h3', 'This cannot start yet'));
      for (const b of plan.blockers) blockers.appendChild(el('p', b));
    }

    chips($('review-build-hosts'), plan.build_hosts, 'None.');
    chips($('review-run-hosts'), plan.run_hosts, 'None. Everything else is refused.');

    const ports = $('review-ports');
    clear(ports);
    plan.ports.forEach((p, index) => {
      const item = el('li');
      const label = el('label');
      const box = el('input');
      box.type = 'checkbox';
      box.checked = true;
      box.dataset.index = String(index);
      box.disabled = blocked;
      label.appendChild(box);
      label.appendChild(el('span', '127.0.0.1:' + p.host_port, 'mono'));
      label.appendChild(el('span', p.service + ':' + p.container_port, 'right muted'));
      item.appendChild(label);
      if (p.note) item.appendChild(el('div', p.note, 'hint'));
      ports.appendChild(item);
    });
    if (!plan.ports.length) ports.appendChild(el('li', 'None. Nothing is forwarded.', 'empty-line'));
    $('review-ports-note').textContent = plan.ports.length
      ? 'Untick a port to leave it closed. Anything on this machine could reach a ticked one.'
      : '';

    const secrets = $('review-secrets');
    clear(secrets);
    for (const line of plan.secrets) {
      // "NAME: how for services", as ops.rs writes it.
      const cut = line.indexOf(': ');
      if (cut > 0) row(secrets, line.slice(0, cut), line.slice(cut + 2), 'mono', 'muted');
      else secrets.appendChild(el('li', line));
    }
    if (!plan.secrets.length) secrets.appendChild(el('li', 'None.', 'empty-line'));
    // The rows above already say which service gets each one.
    fillExposure($('review-exposure'), plan.exposure.filter((line) => !/^\S+ is given to /.test(line)));

    const res = $('review-resources');
    clear(res);
    for (const [value, unit] of [[plan.resources.memory, 'memory'], [plan.resources.cpus, plan.resources.cpus === '1' ? 'CPU' : 'CPUs'], [plan.resources.disk, 'disk']]) {
      const fig = el('div', null, 'figure');
      fig.appendChild(el('div', value, 'figure-value'));
      fig.appendChild(el('div', unit, 'figure-unit'));
      res.appendChild(fig);
    }
    $('review-resources-note').textContent = plan.resources.build_memory + ' while building.'
      + (plan.resources.clamped ? ' Lowered to fit your own maximum.' : '');

    const findings = $('review-findings');
    clear(findings);
    const found = plan.findings.length + plan.warnings.length;
    show(findings, found > 0);
    if (plan.findings.length) {
      findings.appendChild(el('h3', plural(plan.findings.length, 'finding', 'findings')));
      for (const f of plan.findings) findings.appendChild(el('p', f));
    }
    if (plan.warnings.length) {
      findings.appendChild(el('h3', 'Files that look like they hold a secret'));
      for (const w of plan.warnings) findings.appendChild(el('p', w, 'mono'));
    }

    const more = $('review-more-body');
    clear(more);
    more.appendChild(el('h4', 'Services'));
    more.appendChild(el('p', plan.services.length ? plan.services.join(', ') : 'None.'));
    more.appendChild(el('h4', 'Files'));
    more.appendChild(el('p', plan.commit ? 'From commit ' + plan.commit + '. Nothing uncommitted is copied in.' : 'From the working tree.', 'mono'));
    if (plan.generated_compose.length) {
      more.appendChild(el('h4', 'Compose file written for this Dockerfile'));
      more.appendChild(el('pre', plan.generated_compose.join('\n'), 'cmd'));
    }
    more.appendChild(el('h4', 'Plan digest'));
    more.appendChild(el('p', plan.digest, 'mono wrap'));
    show($('review-more'), true);
    $('review-more').open = false;

    $('review-digest').textContent = 'plan ' + plan.digest.slice(0, 16);
    show($('review-start'), !blocked);
    $('review-start').textContent = restart ? 'Restart with this plan' : 'Start with this plan';
    $('review-cancel').textContent = blocked ? 'Close' : 'Cancel';
  }

  function cancelReview() {
    const review = state.review;
    state.review = null;
    if (review && !review.from) banner('Nothing was ' + (review.restart ? 'restarted.' : 'started.'), 'info');
    if (review && review.from) banner('Nothing was started. The project is in the list; start it from there when you want.', 'info');
    const target = review && projectPathFor(review);
    if (target) openProject(target);
    else goBack();
  }
  $('review-cancel').addEventListener('click', cancelReview);
  $('review-back').addEventListener('click', cancelReview);

  // The list's own path for a reviewed project, which the daemon canonicalized.
  function projectPathFor(review) {
    const names = [review.path, review.plan && review.plan.path];
    const found = state.projects.find((p) => names.includes(p.path));
    return found ? found.path : null;
  }

  $('review-start').addEventListener('click', () => {
    const review = state.review;
    if (!review || review.plan.blockers.length) return;
    // Only ports from the plan that was shown, and only those left ticked.
    const ports = [];
    for (const box of $('review-ports').querySelectorAll('input[type=checkbox]')) {
      if (box.checked) ports.push(review.plan.ports[Number(box.dataset.index)].host_port);
    }
    state.review = null;
    runStart(review, ports);
  });

  const STEP_ORDER = ['planning', 'confirm', 'sandbox', 'snapshot', 'docker', 'secrets', 'build'];
  const STEP_WORDS = {
    planning: 'Checking the plan again',
    confirm: 'Waiting for you to confirm in the system prompt',
    sandbox: 'Getting the virtual machine ready',
    snapshot: 'Copying the snapshot in',
    docker: 'Waiting for Docker inside it',
    secrets: 'Delivering assigned secrets',
    build: 'Building and starting services',
  };

  function stepRow(key, words, status) {
    const item = el('li', null, 'step ' + status);
    item.dataset.key = key;
    item.appendChild(el('span', null, 'mark'));
    const text = el('span', null, 'step-text');
    text.appendChild(el('span', words, 'step-words'));
    text.appendChild(el('span', '', 'step-detail mono'));
    item.appendChild(text);
    item.appendChild(el('span', '', 'step-time'));
    return item;
  }
  function seconds(ms) {
    const s = Math.max(0, Math.round(ms / 1000));
    return s < 60 ? s + 's' : Math.floor(s / 60) + 'm ' + (s % 60) + 's';
  }

  async function runStart(review, ports) {
    const path = review.path;
    const name = review.name;
    const list = $('starting-steps');
    clear(list);
    show($('starting-error'), false);
    show($('starting-done'), false);
    $('starting-step').textContent = review.from ? 'Step 3 of 3 · Starting' : (review.restart ? 'Restarting' : 'Starting');
    $('starting-title').textContent = (review.restart ? 'Restarting ' : 'Starting ') + name;
    $('starting-note').textContent = 'You can leave this page; it keeps going and shows up on the left.';
    if (review.from === 'git') {
      const fetched = stepRow('fetch', 'Code fetched with your key', 'done');
      fetched.querySelector('.step-detail').textContent = review.commit ? 'at ' + review.commit.slice(0, 12) : '';
      list.appendChild(fetched);
    }
    if (review.restart) list.appendChild(stepRow('down', 'Stopping it, keeping its data', 'pending'));
    for (const key of STEP_ORDER) list.appendChild(stepRow(key, STEP_WORDS[key], 'pending'));
    list.appendChild(stepRow('ports', ports.length ? 'Opening ports ' + ports.join(', ') + ' on 127.0.0.1' : 'No ports to open', 'pending'));
    showPage('starting');

    let current = null;
    let since = Date.now();
    const timer = setInterval(() => {
      if (current) current.querySelector('.step-time').textContent = seconds(Date.now() - since);
    }, 1000);
    function finishCurrent(status) {
      if (!current) return;
      const words = current.querySelector('.step-words');
      if (current.dataset.key === 'confirm' && words.textContent === STEP_WORDS.confirm) {
        words.textContent = status === 'done' ? 'Confirmed by you in the system prompt' : 'Not confirmed, so nothing was started';
      }
      current.className = 'step ' + status;
      current.querySelector('.step-time').textContent = seconds(Date.now() - since);
      current = null;
    }
    function begin(key, words, detail) {
      let item = list.querySelector('li[data-key="' + CSS.escape(key) + '"]');
      if (!item) {
        // A step this window does not know: shown as the daemon named it.
        item = stepRow(key, words || key, 'pending');
        list.insertBefore(item, list.querySelector('li[data-key="ports"]'));
      }
      // Steps the daemon skipped are not shown as done.
      for (let node = item.previousElementSibling; node; node = node.previousElementSibling) {
        if (node.classList.contains('pending')) node.remove();
      }
      if (item !== current) {
        finishCurrent('done');
        current = item;
        since = Date.now();
        item.className = 'step active';
      }
      item.querySelector('.step-detail').textContent = detail || '';
    }

    state.starting = path;
    state.startedReview = review;
    busy('');
    await refreshProjects();
    renderHeader();

    if (review.restart) {
      stopLogs();
      begin('down');
      try {
        await invoke('down', { project: path });
      } catch (error) {
        finishCurrent('failed');
        clearInterval(timer);
        failStart('Could not stop it to restart it: ' + errorText(error));
        return;
      }
    }

    const leaveNote = 'You can leave this page; it keeps going and shows up on the left.';
    const progress = new Channel();
    progress.onmessage = (event) => {
      if (event.step === 'unconfirmed') {
        begin('confirm', null, 'not asked: this computer has no fingerprint check');
        current.querySelector('.step-words').textContent = 'Started on this window\u2019s request alone';
        return;
      }
      $('starting-note').textContent = event.step === 'confirm' ? confirmWords(event.detail) : leaveNote;
      begin(event.step, STEP_WORDS[event.step], event.detail);
    };
    try {
      const said = await invoke('up', { project: path, digest: review.plan.digest, ports, onProgress: progress });
      finishCurrent('done');
      for (const node of [...list.querySelectorAll('li.pending')]) {
        if (node.dataset.key !== 'ports') node.remove();
      }
      const last = list.querySelector('li[data-key="ports"]');
      if (last) last.className = 'step done';
      $('starting-done').textContent = said;
      show($('starting-done'), true);
      $('starting-note').textContent = '';
      banner(said, 'ok');
    } catch (error) {
      finishCurrent('failed');
      failStart('Could not start it: ' + errorText(error));
    }
    clearInterval(timer);
    state.starting = null;
    await refreshProjects();
    if (state.selected === path) {
      await loadStatus();
      await readTraffic();
    }
    if (state.page === 'project' && state.tab === 'logs') await readLogs();
  }

  function failStart(text) {
    const box = $('starting-error');
    box.textContent = text;
    show(box, true);
    $('starting-note').textContent = 'Nothing more will happen. Fix what it says, then press Start on the project.';
    banner(text);
    state.starting = null;
    refreshProjects();
  }

  $('starting-open').addEventListener('click', () => {
    const started = state.startedReview && projectPathFor(state.startedReview);
    const target = started || (state.projects.find((p) => p.path === state.selected) ? state.selected : (state.projects[0] && state.projects[0].path));
    if (target) openProject(target);
    else showPage('choose');
  });

  async function startProject(restart) {
    const row = selectedRow();
    if (!row) return;
    closeMenu();
    await reviewPlan({ path: row.path, name: row.name, restart });
  }

  async function stopProject(destroy) {
    const row = selectedRow();
    if (!row) return;
    const agreed = await ask(destroy ? {
      title: 'Destroy ' + row.name,
      okText: 'Destroy it',
      danger: true,
      build: (body) => {
        body.appendChild(el('p', 'This removes the sandbox and everything in it, including its database and uploads.'));
        body.appendChild(el('p', 'This cannot be undone.', 'alert'));
        body.appendChild(el('p', 'Afterwards, rotate every secret it was given.', 'muted'));
      },
    } : {
      title: 'Stop ' + row.name,
      okText: 'Stop it',
      build: (body) => {
        body.appendChild(el('p', 'Its services stop and their ports close.'));
        body.appendChild(el('p', 'Its data is kept for the next start.'));
      },
    });
    if (!agreed) {
      banner('Nothing was changed.', 'info');
      return;
    }
    if (destroy) state.diffs.delete(row.path);
    stopLogs();
    busy(destroy ? 'Destroying.' : 'Stopping.');
    try {
      banner(await invoke(destroy ? 'destroy' : 'down', { project: row.path }), 'ok');
    } catch (error) {
      banner((destroy ? 'Could not destroy it: ' : 'Could not stop it: ') + errorText(error));
    }
    busy('');
    await refreshProjects();
    await loadStatus();
    await readTraffic();
  }

  async function removeProject() {
    const row = selectedRow();
    if (!row) return;
    const agreed = await ask({
      title: 'Remove ' + row.name,
      okText: 'Remove it',
      danger: true,
      build: (body) => {
        body.appendChild(el('p', 'This destroys its sandbox and everything in it, and removes the project from this list, with its approved ports and hosts.', 'alert'));
        body.appendChild(el('p', 'A project imported from a remote also loses the copy on this machine. The remote is not touched.'));
        body.appendChild(el('p', 'A folder of your own is not touched: ' + row.path, 'mono'));
        body.appendChild(el('p', 'Any secrets you stored for it stay in your keychain.', 'muted'));
      },
    });
    if (!agreed) {
      banner('Nothing was changed.', 'info');
      return;
    }
    stopLogs();
    busy('Removing.');
    let removed = false;
    try {
      banner(await invoke('remove_project', { project: row.path }), 'ok');
      removed = true;
    } catch (error) {
      banner('Could not remove it: ' + errorText(error));
    }
    busy('');
    if (removed) {
      // Asking about it again would make a new record of it.
      state.diffs.delete(row.path);
      state.selected = null;
      state.status = null;
      clear($('log-text'));
      clearReview();
      clearGit();
    }
    await refreshProjects();
    if (state.selected) {
      await loadStatus();
    } else if (state.projects.length) {
      await openProject(state.projects[0].path);
    } else {
      showPage('choose');
    }
  }

  $('btn-start').addEventListener('click', () => startProject(false));
  $('btn-stop').addEventListener('click', () => stopProject(false));
  $('menu-restart').addEventListener('click', () => startProject(true));
  $('menu-destroy').addEventListener('click', () => { closeMenu(); stopProject(true); });
  $('menu-remove').addEventListener('click', () => { closeMenu(); removeProject(); });
  $('settings-destroy').addEventListener('click', () => stopProject(true));

  $('btn-vscode').addEventListener('click', async () => {
    const row = selectedRow();
    if (!row) return;
    busy('Opening VS Code.');
    try {
      await invoke('open_in_vscode', { project: row.path });
      banner('VS Code is opening on the sandbox, in a separate profile. Git there goes through the daemon.', 'ok');
    } catch (error) {
      banner('Could not open VS Code: ' + errorText(error));
    }
    busy('');
  });

  $('agent-connect').addEventListener('click', async () => {
    const row = selectedRow();
    if (!row) return;
    busy('Setting up Claude Code for this sandbox.');
    try {
      const done = await invoke('connect_agent', { project: row.path });
      $('agent-cmd').textContent = done.command;
      show($('agent-cmd'), true);
      banner(done.opened
        ? 'Claude Code is opening in Terminal. Trust the folder and approve the server when it asks.'
        : 'Run the command under AI agent in a terminal to start Claude Code.', 'ok');
    } catch (error) {
      banner('Could not set up Claude Code: ' + errorText(error));
    }
    busy('');
  });
  $('settings-remove').addEventListener('click', removeProject);

  // ---------- logs ----------

  function renderLogServices() {
    const group = $('log-services');
    clear(group);
    const names = (state.status && state.status.log_services) || [];
    if (!names.includes(state.logService)) state.logService = '';
    for (const name of [''].concat(names)) {
      const chip = el('button', name || 'All', 'chip-btn' + (name === state.logService ? ' active' : ''));
      chip.type = 'button';
      chip.setAttribute('aria-pressed', name === state.logService ? 'true' : 'false');
      chip.addEventListener('click', () => {
        if (state.logService === name) return;
        state.logService = name;
        renderLogServices();
        readLogs();
      });
      group.appendChild(chip);
    }
  }

  const MAX_LOG_CHARS = 400000;
  function appendLog(text) {
    const pre = $('log-text');
    const atBottom = pre.scrollTop + pre.clientHeight >= pre.scrollHeight - 20;
    pre.appendChild(document.createTextNode(text));
    // Keep the page bounded however much a sandbox writes.
    while (pre.textContent.length > MAX_LOG_CHARS && pre.firstChild) pre.removeChild(pre.firstChild);
    if (atBottom) pre.scrollTop = pre.scrollHeight;
  }

  function setFollowing(on) {
    $('log-stop').disabled = !on;
    show($('log-following'), on);
  }

  function stopLogs() {
    if (state.logId !== null) {
      invoke('logs_stop', { id: state.logId }).catch(() => {});
      state.logId = null;
    }
    setFollowing(false);
  }

  async function readLogs() {
    const row = selectedRow();
    if (!row) return;
    stopLogs();
    clear($('log-text'));
    const s = state.status;
    if (!s) {
      $('log-note').textContent = 'The daemon could not report on this project, so its logs were not asked for.';
      return;
    }
    if (s.state !== 'running') {
      $('log-note').textContent = s.state === 'stopped'
        ? 'The sandbox is stopped. Start it to read its logs.'
        : 'This project has no running sandbox. Start it to read its logs.';
      return;
    }
    const follow = $('log-follow').checked;
    $('log-note').textContent = follow ? '' : 'Reading.';
    const channel = new Channel();
    let mine = null;
    channel.onmessage = (event) => {
      if (event.kind === 'text') {
        appendLog(event.text);
      } else if (event.kind === 'end') {
        if (mine !== null && state.logId === mine) {
          state.logId = null;
          setFollowing(false);
        }
        const empty = $('log-text').textContent.length === 0;
        $('log-note').textContent = event.note || (empty ? 'Nothing has been logged yet.' : (follow ? 'Stopped following.' : ''));
      }
    };
    try {
      const service = state.logService || null;
      mine = await invoke('logs', { project: row.path, service, follow, onEvent: channel });
      if (follow && state.selected === row.path) {
        state.logId = mine;
        setFollowing(true);
      } else if (follow) {
        invoke('logs_stop', { id: mine }).catch(() => {});
      }
    } catch (error) {
      $('log-note').textContent = errorText(error);
    }
  }

  $('log-read').addEventListener('click', readLogs);
  $('log-stop').addEventListener('click', () => {
    stopLogs();
    $('log-note').textContent = 'Stopped following.';
  });
  $('log-follow').addEventListener('change', readLogs);

  // ---------- network ----------

  async function readTraffic() {
    const row = selectedRow();
    const notes = $('traffic-notes');
    const rows = $('traffic-rows');
    clear(notes);
    clear(rows);
    $('traffic-total').textContent = '';
    if (!row) return;
    let view;
    try {
      view = await invoke('blocked', { project: row.path });
    } catch (error) {
      if (state.selected === row.path) notes.appendChild(el('li', errorText(error), 'alert'));
      return;
    }
    if (state.selected !== row.path) return;
    // Two reads can overlap, as when a project opens on this tab. Only what
    // the last one to finish found is shown.
    clear(notes);
    clear(rows);
    state.blocked = view;
    for (const note of view.notes) notes.appendChild(el('li', note.text, note.weight === 'plain' ? '' : note.weight));
    $('traffic-total').textContent = view.recording ? plural(view.total, 'refused lookup', 'refused lookups') + ' counted' : '';
    for (const event of view.rows) {
      const tr = el('tr');
      tr.appendChild(el('td', event.time, 'mono muted'));
      tr.appendChild(el('td', event.count, 'num'));
      tr.appendChild(el('td', event.domain, 'mono'));
      const cell = el('td', '', 'num');
      const approve = el('button', 'Approve', 'small');
      approve.disabled = state.busy;
      approve.addEventListener('click', () => approveHost(event.domain));
      cell.appendChild(approve);
      tr.appendChild(cell);
      rows.appendChild(tr);
    }
    renderBlockedCard();
  }

  async function approveHost(host) {
    const row = selectedRow();
    if (!row) return;
    const agreed = await ask({
      title: 'Let ' + row.name + ' reach this host?',
      okText: 'Approve',
      build: (body) => {
        body.appendChild(el('p', host, 'mono'));
        body.appendChild(el('p', 'Everything in the sandbox will be able to reach it, including any service holding a secret. What runs inside chooses which names it looks up, so a name can itself be data trying to get out. Approve only a name you recognise.', 'alert'));
        body.appendChild(el('p', 'Its data is kept. Its services restart, which takes a moment.', 'muted'));
      },
    });
    if (!agreed) {
      banner('Nothing was changed.', 'info');
      return;
    }
    stopLogs();
    busy('Approving ' + host + '. Its services are restarting.');
    try {
      const asking = personChannel();
      const said = await invoke('approve_host', { project: row.path, host, onProgress: asking });
      banner(asking.unchecked ? said + ' ' + NOT_CHECKED : said, 'ok');
    } catch (error) {
      banner('Could not approve it: ' + errorText(error));
    }
    busy('');
    await refreshProjects();
    await loadStatus();
    await readTraffic();
  }

  $('traffic-read').addEventListener('click', readTraffic);

  // ---------- problems ----------

  // Rows of things refused or failed, newest first. Every field is text from
  // the audit log, escaped by the app's Rust side, and goes in as text.
  function renderProblems(table, tbody, notes, view, withProject) {
    clear(tbody);
    clear(notes);
    for (const note of view.notes) notes.appendChild(el('li', note.text, note.weight === 'plain' ? '' : note.weight));
    for (const problem of view.rows) {
      const tr = el('tr');
      tr.appendChild(el('td', problem.time, 'mono muted time'));
      if (withProject) {
        const cell = el('td', null, 'project');
        if (problem.project_path) {
          const open = el('button', problem.project || lastSegment(problem.project_path), 'link');
          open.title = problem.project_path;
          open.addEventListener('click', () => openProject(problem.project_path));
          cell.appendChild(open);
        } else {
          cell.appendChild(el('span', problem.project || 'No project', problem.project ? '' : 'muted'));
        }
        tr.appendChild(cell);
      }
      tr.appendChild(el('td', problem.what, 'what'));
      tr.appendChild(el('td', problem.why, 'why'));
      tbody.appendChild(tr);
    }
    show(table, view.rows.length > 0);
  }

  // How many problems are less than a day old, for the rail and first run.
  function recentCount(view) {
    const since = Date.now() / 1000 - 24 * 60 * 60;
    return view.rows.filter((r) => r.at_unix >= since).length;
  }

  async function refreshProblemCount() {
    let view;
    try {
      view = await invoke('problems', { project: null });
    } catch (error) {
      return;
    }
    const recent = recentCount(view);
    const badge = $('badge-problems');
    badge.textContent = String(recent);
    badge.title = plural(recent, 'problem', 'problems') + ' in the last 24 hours';
    show(badge, recent > 0);
    show($('choose-problems'), view.rows.length > 0);
    return view;
  }

  async function readProblems() {
    let view;
    try {
      view = await invoke('problems', { project: null });
    } catch (error) {
      view = { rows: [], notes: [{ weight: 'alert', text: errorText(error) }] };
    }
    renderProblems($('problems-table'), $('problems-rows'), $('problems-notes'), view, true);
    await refreshProblemCount();
  }

  // The flags of every running sandbox, as its Changes tab shows them first.
  // The comparisons run together, and each answer is kept for that project's
  // Changes tab. A stopped sandbox cannot be compared, so it is only named.
  let flaggedRun = 0;

  function flaggedProject(row, diff, failed) {
    const block = el('div', null, 'flagged-project');
    const head = el('div', null, 'flagged-head');
    const open = el('button', row.name, 'link strong');
    open.title = row.path;
    open.addEventListener('click', async () => {
      await openProject(row.path);
      await openTab('changes');
    });
    head.appendChild(open);
    if (failed) {
      head.appendChild(el('span', 'could not be compared: ' + failed, 'small-text warn-text'));
      block.appendChild(head);
      return block;
    }
    const flags = diff.view.flags;
    head.appendChild(el('span', plural(flags.length, 'change', 'changes') + ' to look at', 'small-text bad-text'));
    block.appendChild(head);
    const list = el('ul', null, 'flags');
    for (const flag of flags) {
      const item = el('li');
      item.appendChild(el('div', flag.path, 'mono strong'));
      item.appendChild(el('div', flag.message));
      list.appendChild(item);
    }
    block.appendChild(list);
    return block;
  }

  async function readFlagged(force) {
    const run = ++flaggedRun;
    const notes = $('flagged-notes');
    const box = $('flagged-projects');
    clear(notes);
    clear(box);
    const running = state.projects.filter((p) => p.state === 'running');
    const stopped = state.projects.length - running.length;
    if (!running.length) {
      notes.appendChild(el('li', 'No sandbox is running, so nothing was compared.'));
      return;
    }
    const button = $('flagged-read');
    button.disabled = true;
    const waiting = el('li', 'Comparing ' + plural(running.length, 'running sandbox', 'running sandboxes') + '…');
    notes.appendChild(waiting);
    const results = await Promise.all(running.map(async (row) => {
      const kept = state.diffs.get(row.path);
      if (!force && kept && Date.now() - kept.at < COMPARE_FRESH_MS) return { row, diff: kept };
      try {
        const view = await invoke('diff', { project: row.path });
        const now = new Date();
        const diff = { view, at: now.getTime(), when: String(now.getHours()).padStart(2, '0') + ':' + String(now.getMinutes()).padStart(2, '0') };
        state.diffs.set(row.path, diff);
        return { row, diff };
      } catch (error) {
        return { row, failed: errorText(error) };
      }
    }));
    button.disabled = false;
    if (run !== flaggedRun) return;
    waiting.remove();
    let clean = 0;
    for (const result of results) {
      if (!result.failed && result.diff.view.flags.length === 0) {
        clean += 1;
        continue;
      }
      box.appendChild(flaggedProject(result.row, result.diff, result.failed));
    }
    if (clean) notes.appendChild(el('li', plural(clean, 'running sandbox has', 'running sandboxes have') + ' nothing flagged.'));
    if (stopped) notes.appendChild(el('li', plural(stopped, 'project is', 'projects are') + ' not running, so not compared. Start one to check it.'));
  }

  async function openProblems() {
    banner('');
    showPage('problems');
    // The refusals are read from a file and show at once; the comparisons
    // run inside the sandboxes and fill in after.
    const flagged = readFlagged(false);
    await readProblems();
    await flagged;
  }

  async function readRefused() {
    const row = selectedRow();
    clear($('refused-rows'));
    clear($('refused-notes'));
    show($('refused-table'), false);
    if (!row) return;
    let view;
    try {
      view = await invoke('problems', { project: row.path });
    } catch (error) {
      view = { rows: [], notes: [{ weight: 'alert', text: errorText(error) }] };
    }
    if (state.selected !== row.path) return;
    renderProblems($('refused-table'), $('refused-rows'), $('refused-notes'), view, false);
  }

  $('btn-problems').addEventListener('click', openProblems);
  $('choose-problems').addEventListener('click', openProblems);
  $('problems-read').addEventListener('click', readProblems);
  $('flagged-read').addEventListener('click', () => readFlagged(true));
  $('refused-read').addEventListener('click', readRefused);

  // ---------- changes ----------

  // Clears the page, so a comparison is never shown under another project.
  function clearReview() {
    clear($('review-notes'));
    clear($('review-flags'));
    clear($('review-files'));
    clear($('review-diff'));
    show($('review-split'), false);
    show($('review-flags-head'), false);
    show($('review-files-head'), false);
    $('review-heading').textContent = 'Changed since it started';
    $('review-when').textContent = '';
    $('review-read').textContent = 'Compare with the snapshot';
    show($('review-intro'), true);
  }

  const CHANGE_LETTER = { added: 'A', modified: 'M', deleted: 'D', 'type changed': 'T' };

  const LINE_SIGN = { context: ' ', added: '+', removed: '-' };
  // A comparison this recent is not run again just because the tab was reopened.
  const COMPARE_FRESH_MS = 15000;

  // Everything from the sandbox goes in as text, never as markup: el() only
  // sets textContent. Lines wrap, so nothing hides past the edge of the pane.
  function inlineLine(line, oldNumber, newNumber) {
    const row = el('div', null, 'patch-line ' + line.kind);
    row.appendChild(el('span', line.kind === 'added' ? '' : oldNumber, 'patch-number'));
    row.appendChild(el('span', line.kind === 'removed' ? '' : newNumber, 'patch-number'));
    row.appendChild(el('span', LINE_SIGN[line.kind] || '?', 'patch-sign'));
    row.appendChild(el('span', line.text, 'patch-text'));
    return row;
  }

  function splitCell(row, line, number) {
    const kind = line ? line.kind : 'blank';
    row.appendChild(el('span', line ? number : '', 'patch-number ' + kind));
    row.appendChild(el('span', line ? LINE_SIGN[line.kind] : '', 'patch-sign ' + kind));
    row.appendChild(el('span', line ? line.text : '', 'patch-text ' + kind));
  }

  // One hunk as two columns. A run of removed lines faces the added ones that
  // replace it, as in an editor's side-by-side view.
  function splitHunk(code, hunk) {
    let oldNumber = hunk.old_start;
    let newNumber = hunk.new_start;
    let removed = [];
    let added = [];
    const flush = () => {
      for (let at = 0; at < Math.max(removed.length, added.length); at += 1) {
        const row = el('div', null, 'patch-row');
        splitCell(row, removed[at] || null, oldNumber + at);
        splitCell(row, added[at] || null, newNumber + at);
        code.appendChild(row);
      }
      oldNumber += removed.length;
      newNumber += added.length;
      removed = [];
      added = [];
    };
    for (const line of hunk.lines) {
      if (line.kind === 'removed') { removed.push(line); continue; }
      if (line.kind === 'added') { added.push(line); continue; }
      flush();
      const row = el('div', null, 'patch-row');
      splitCell(row, line, oldNumber);
      splitCell(row, line, newNumber);
      code.appendChild(row);
      oldNumber += 1;
      newNumber += 1;
    }
    flush();
  }

  function inlineHunk(code, hunk) {
    let oldNumber = hunk.old_start;
    let newNumber = hunk.new_start;
    for (const line of hunk.lines) {
      code.appendChild(inlineLine(line, oldNumber, newNumber));
      if (line.kind !== 'added') oldNumber += 1;
      if (line.kind !== 'removed') newNumber += 1;
    }
  }

  function patchBody(patch) {
    const body = el('div', null, 'patch');
    if (patch.unread) {
      body.appendChild(el('p', patch.unread, 'patch-note warn'));
      return body;
    }
    if (!patch.hunks.length) {
      body.appendChild(el('p', 'No lines changed. The file is empty, or only its type or permissions changed.', 'patch-note'));
      return body;
    }
    const code = el('div', null, 'patch-code');
    for (const hunk of patch.hunks) {
      code.appendChild(el('div', 'From line ' + (hunk.new_start || hunk.old_start), 'patch-hunk'));
      if (state.diffMode === 'inline') inlineHunk(code, hunk);
      else splitHunk(code, hunk);
    }
    body.appendChild(code);
    if (patch.truncated) body.appendChild(el('p', 'Cut short at a limit. Treat the rest of this file as unread.', 'patch-note warn'));
    return body;
  }

  function modeButton(file, mode, label) {
    const button = el('button', label, 'small' + (state.diffMode === mode ? ' active' : ''));
    button.type = 'button';
    button.setAttribute('aria-pressed', String(state.diffMode === mode));
    button.addEventListener('click', () => {
      state.diffMode = mode;
      renderFileDiff(file);
    });
    return button;
  }

  // Lines are built only for the file on show, because a comparison can hold
  // thousands of them.
  function renderFileDiff(file) {
    const pane = $('review-diff');
    clear(pane);
    const head = el('div', null, 'diff-head');
    head.appendChild(el('span', file.path, 'mono diff-path'));
    if (!file.patch.unread) {
      head.appendChild(el('span', '+' + file.patch.added, 'file-stat good'));
      head.appendChild(el('span', '-' + file.patch.removed, 'file-stat bad'));
      const modes = el('span', null, 'diff-modes');
      modes.appendChild(modeButton(file, 'split', 'Side by side'));
      modes.appendChild(modeButton(file, 'inline', 'Inline'));
      head.appendChild(modes);
    }
    pane.appendChild(head);
    pane.appendChild(patchBody(file.patch));
  }

  function selectFile(file, row) {
    state.openFile.set(state.selected, file.path);
    for (const other of $('review-files').querySelectorAll('.file-row')) other.removeAttribute('aria-current');
    row.setAttribute('aria-current', 'true');
    renderFileDiff(file);
  }

  function fileRow(file) {
    const item = el('li');
    const row = el('button', null, 'file-row');
    row.type = 'button';
    row.title = file.change;
    row.appendChild(el('span', CHANGE_LETTER[file.change] || '?', 'change ' + file.change.replace(' ', '-')));
    row.appendChild(el('span', file.path, 'mono file-path'));
    if (file.patch.unread) {
      row.appendChild(el('span', 'Not shown', 'file-stat warn'));
    } else {
      row.appendChild(el('span', '+' + file.patch.added, 'file-stat good'));
      row.appendChild(el('span', '-' + file.patch.removed, 'file-stat bad'));
    }
    row.addEventListener('click', () => selectFile(file, row));
    item.appendChild(row);
    return { item, row };
  }

  function renderDiff(diff) {
    clearReview();
    const view = diff.view;
    show($('review-intro'), false);
    $('review-read').textContent = 'Compare again';
    $('review-heading').textContent = 'Changed since it started from ' + view.baseline;
    $('review-when').textContent = 'Compared at ' + diff.when;
    const notes = $('review-notes');
    for (const note of view.notes) notes.appendChild(el('li', note.text, note.weight === 'plain' ? '' : note.weight));
    const flags = $('review-flags');
    show($('review-flags-head'), view.flags.length > 0);
    for (const flag of view.flags) {
      const item = el('li');
      item.appendChild(el('div', flag.path, 'mono strong'));
      item.appendChild(el('div', flag.message));
      flags.appendChild(item);
    }
    if (!view.files.length) return;
    const head = $('review-files-head');
    head.textContent = (view.flags.length ? 'Every changed file · ' : 'Changed files · ') + view.files.length;
    show(head, true);
    show($('review-split'), true);
    const files = $('review-files');
    const rows = view.files.map((file) => {
      const made = fileRow(file);
      files.appendChild(made.item);
      return { file, row: made.row };
    });
    // The file already on show stays on show. Otherwise a flagged one comes first.
    const flagged = new Set(view.flags.map((flag) => flag.path.replace(/:\d+$/, '')));
    const open = state.openFile.get(state.selected);
    const first = rows.find((r) => r.file.path === open) || rows.find((r) => flagged.has(r.file.path)) || rows[0];
    selectFile(first.file, first.row);
  }

  // The comparison runs Git inside the sandbox, so it happens only for a running
  // one, one at a time, and the page stays usable meanwhile. What is on screen
  // stays until the new answer replaces it.
  async function readReview() {
    const row = selectedRow();
    if (!row) return;
    if (state.comparing) {
      state.compareAgain = true;
      return;
    }
    state.comparing = true;
    updateButtons();
    $('review-when').textContent = 'Comparing inside the sandbox…';
    try {
      const view = await invoke('diff', { project: row.path });
      const now = new Date();
      const diff = { view, at: now.getTime(), when: String(now.getHours()).padStart(2, '0') + ':' + String(now.getMinutes()).padStart(2, '0') };
      state.diffs.set(row.path, diff);
      if (state.selected === row.path) {
        renderDiff(diff);
        renderBadges();
        renderChangesCard();
      }
    } catch (error) {
      if (state.selected === row.path) {
        clearReview();
        $('review-notes').appendChild(el('li', errorText(error), 'alert'));
      }
    } finally {
      state.comparing = false;
      updateButtons();
    }
    if (state.compareAgain) {
      state.compareAgain = false;
      if (state.tab === 'changes') await readReview();
    }
  }

  // Compares by itself when the Changes tab is on show. `force` is for when the
  // files are known to have moved, as after a Git command.
  async function compareIfWanted(force) {
    if (state.tab !== 'changes' || !state.selected) return;
    if (!state.status) await loadStatus();
    if (!state.status || state.status.state !== 'running') return;
    if (state.comparing) {
      if (force) state.compareAgain = true;
      else if (!state.diffs.has(state.selected)) state.compareAgain = true;
      return;
    }
    const known = state.diffs.get(state.selected);
    if (!force && known && Date.now() - known.at < COMPARE_FRESH_MS) return;
    await readReview();
  }

  $('review-read').addEventListener('click', readReview);

  // ---------- Git ----------

  function clearGit() {
    show($('git-title'), false);
    clear($('git-updates'));
    $('git-output').textContent = '';
    show($('git-output'), false);
    show($('git-stuck'), false);
    $('git-branch').textContent = 'Press Status to see the branch.';
  }

  // Splits a typed command into Git's arguments, as a shell would for plain
  // words, 'single' and "double" quotes and backslashes, and nothing more: no
  // variables, globs or pipes. Returns null for an unclosed quote.
  function splitArgs(line) {
    const args = [];
    let word = null;
    let quote = null;
    for (let i = 0; i < line.length; i++) {
      const c = line[i];
      if (quote) {
        if (c === quote) quote = null;
        else if (c === '\\' && quote === '"' && (line[i + 1] === '"' || line[i + 1] === '\\')) word += line[++i];
        else word += c;
      } else if (c === "'" || c === '"') {
        quote = c;
        if (word === null) word = '';
      } else if (c === '\\' && i + 1 < line.length) {
        word = (word || '') + line[++i];
      } else if (/\s/.test(c)) {
        if (word !== null) { args.push(word); word = null; }
      } else {
        word = (word || '') + c;
      }
    }
    if (quote) return null;
    if (word !== null) args.push(word);
    if (args[0] === 'git') args.shift();
    return args;
  }

  // A pull or merge that stopped because uncommitted changes are in the way.
  function blockedByLocalChanges(view) {
    return !view.ok && view.output.some((line) => /would be overwritten by (merge|checkout)|Please commit your changes or stash them/.test(line));
  }

  function renderGitAvailability() {
    const remote = state.selected ? isRemote(state.selected) : null;
    // When it cannot be told, the controls are offered and the daemon decides.
    const offer = remote !== false;
    show($('git-card'), offer);
    show($('git-status').closest('.card'), offer);
    show($('git-not-remote'), !offer);
  }

  async function runGit(op, message, args) {
    const row = selectedRow();
    if (!row) return null;
    const command = op === 'run' ? (args[0] || '') : op;
    const waiting = ['pull', 'push', 'fetch', 'ls-remote'].includes(command);
    busy(waiting ? (command === 'push' ? 'Pushing' : 'Fetching') + '. Your SSH agent may ask you to approve.' : 'Asking Git.');
    let view;
    const asking = personChannel();
    try {
      view = await invoke('git', { project: row.path, op, commitMessage: message || null, args: args || null, onProgress: asking });
    } catch (error) {
      busy('');
      banner(errorText(error));
      return null;
    }
    busy('');
    if (asking.unchecked && view.ok) banner(NOT_CHECKED, 'info');
    if (state.selected !== row.path) return view;
    const title = $('git-title');
    title.textContent = view.title;
    title.className = view.ok ? 'good-text' : 'bad-text';
    show(title, true);
    const updates = $('git-updates');
    clear(updates);
    for (const line of view.updates) updates.appendChild(el('li', 'pushed ' + line, 'mono'));
    if (view.refused) updates.appendChild(el('li', view.refused, 'alert'));
    $('git-output').textContent = view.output.join('\n');
    show($('git-output'), view.output.length > 0);
    if (view.branch) $('git-branch').textContent = 'On branch ' + view.branch;
    show($('git-stuck'), (command === 'pull' || command === 'merge') && blockedByLocalChanges(view));
    if (op !== 'status') compareIfWanted(true);
    return view;
  }

  $('git-status').addEventListener('click', () => runGit('status'));
  $('git-pull').addEventListener('click', () => runGit('pull'));
  $('git-push').addEventListener('click', () => runGit('push'));
  $('git-autostash').addEventListener('click', () => runGit('run', null, ['pull', '--no-rebase', '--no-edit', '--autostash']));
  $('git-run-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const args = splitArgs($('git-run').value);
    if (args === null) { banner('A quote is not closed.'); return; }
    if (!args.length) { banner('Type a Git command first, such as status or log --oneline.'); return; }
    const view = await runGit('run', null, args);
    if (view && view.ok) $('git-run').value = '';
  });
  $('git-commit-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const message = $('git-message').value.trim();
    if (!message) { banner('Write a commit message first.'); return; }
    const view = await runGit('commit', message);
    if (view && view.ok) $('git-message').value = '';
  });

  // ---------- tabs ----------

  async function openTab(tab) {
    closeMenu();
    state.tab = tab;
    for (const button of document.querySelectorAll('.tab')) {
      button.classList.toggle('active', button.dataset.tab === tab);
    }
    for (const panel of document.querySelectorAll('.panel')) {
      show(panel, panel.dataset.panel === tab);
    }
    // Each tab starts at its top, not where the last one was scrolled to.
    for (const scroller of document.querySelectorAll('.content')) scroller.scrollTop = 0;
    if (tab !== 'logs') stopLogs();
    if (tab !== 'secrets') closeSecretForm();
    if (tab === 'secrets') await loadSecrets();
    if (tab === 'settings') await loadLimits();
    if (tab === 'network') {
      await readTraffic();
      await readRefused();
    }
    if (tab === 'changes') {
      renderGitAvailability();
      const diff = state.diffs.get(state.selected);
      if (diff) renderDiff(diff);
      await compareIfWanted(false);
    }
    if (tab === 'logs' && state.selected) {
      await loadStatus();
      await readLogs();
    }
  }
  for (const button of document.querySelectorAll('.tab')) {
    button.addEventListener('click', () => openTab(button.dataset.tab));
  }

  // ---------- new project ----------

  $('btn-new').addEventListener('click', () => { banner(''); showPage('choose'); });
  $('choose-cancel').addEventListener('click', goBack);
  $('choose-git').addEventListener('click', () => {
    banner('');
    showPage('git');
    $('git-url').focus();
  });
  $('choose-folder').addEventListener('click', () => {
    banner('');
    showPage('folder');
    $('folder-path').focus();
  });

  $('git-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (state.busy) return;
    const url = $('git-url').value.trim();
    if (!url) { banner('Paste an SSH URL first, such as git@github.com:org/repo.git.'); return; }
    const gitRef = $('git-ref').value.trim() || null;
    banner('');
    busy('Fetching on this machine. Your SSH agent may ask you to approve.');
    let view;
    try {
      view = await invoke('import_remote', { url, gitRef });
    } catch (error) {
      busy('');
      banner(errorText(error));
      // A failed fetch is recorded, and is listed under Problems.
      await refreshProblemCount();
      return;
    }
    busy('');
    await refreshProjects();
    if (!view.fetched) {
      banner(view.message, 'info');
      await openProject(view.project_path);
      return;
    }
    $('git-url').value = '';
    $('git-ref').value = '';
    const shown = await reviewPlan({ path: view.project_path, name: lastSegment(url).replace(/\.git$/, ''), from: 'git', commit: view.commit });
    if (!shown) await openProject(view.project_path);
  });

  $('folder-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (state.busy) return;
    const path = $('folder-path').value.trim();
    if (!path) { banner("Give the folder's full path first."); return; }
    banner('');
    busy('Adding the folder.');
    let said;
    try {
      said = await invoke('add_project', { path });
    } catch (error) {
      busy('');
      banner(errorText(error));
      return;
    }
    busy('');
    await refreshProjects();
    $('folder-path').value = '';
    // "Added, but..." means the daemon recorded it and could not read its
    // sandbox's state. The plan does not need that state, so go on to it.
    const shown = await reviewPlan({ path, name: lastSegment(path), from: 'folder' });
    if (shown && said !== 'Added.') banner(said, 'info');
    if (!shown) {
      const row = state.projects.find((p) => p.path === path);
      if (row) await openProject(row.path);
    }
  });

  $('refresh').addEventListener('click', async () => {
    await refreshDaemon();
    await refreshProjects();
    if (state.page === 'project') await loadStatus();
  });

  // ---------- about and the terminal ----------

  function shellQuote(text) {
    return "'" + String(text).split("'").join("'\\''") + "'";
  }

  function renderInfo(info) {
    $('brand').textContent = info.product;
    document.title = info.product;
    $('choose-cli').textContent = `${info.cli_bin} up git@…`;
    $('secrets-cmd').textContent = `cd /path/to/project\n${info.cli_bin} secrets use .env --yes        # store a file's values, for every service\n${info.cli_bin} secrets import .env.example   # add the names, empty\n${info.cli_bin} secrets fill                  # type a value for each\n${info.cli_bin} secrets set NAME --service SERVICE`;
    const box = $('cli-install');
    clear(box);
    if (info.bundled_cli) {
      box.appendChild(el('p', 'This app carries the command line tool. To use it from a terminal, link it onto your PATH once:'));
      box.appendChild(el('pre', `sudo ln -sf ${shellQuote(info.bundled_cli)} /usr/local/bin/${info.cli_bin}`, 'cmd'));
      box.appendChild(el('p', 'Or, without administrator rights, if ~/.local/bin is on your PATH:', 'muted'));
      box.appendChild(el('pre', `mkdir -p ~/.local/bin && ln -sf ${shellQuote(info.bundled_cli)} ~/.local/bin/${info.cli_bin}`, 'cmd'));
      box.appendChild(el('p', 'A link follows the app when it updates in place. Moving the app breaks the link; run the command again.', 'muted'));
    } else {
      box.appendChild(el('p', `This build has no command line tool beside it. Install ${info.cli_bin} from source or Homebrew; it talks to the same daemon.`, 'muted'));
    }
    box.appendChild(el('p', `Running ${info.cli_bin} with no arguments opens the full-screen terminal interface.`, 'muted'));
    kv($('about'), [
      ['Version', info.version],
      ['State folder', info.state_dir || 'not found', 'mono'],
      ['Daemon', info.daemon_path || info.daemon_bin, 'mono'],
      ['Command line tool', info.bundled_cli || 'not bundled', 'mono'],
    ]);
  }

  $('btn-about').addEventListener('click', () => showPage('about'));

  // ---------- start ----------

  async function init() {
    try {
      state.info = await invoke('app_info');
      renderInfo(state.info);
    } catch (error) {
      banner(errorText(error));
    }
    await refreshDaemon();
    await refreshProjects();
    if (state.projects.length) await openProject(state.projects[0].path);
    else showPage('choose');
    await refreshProblemCount();
    // The menu bar icon may have started or restarted the daemon meanwhile.
    setInterval(async () => {
      if (state.busy || state.starting || $('dialog').open) return;
      const before = state.daemonRunning;
      await refreshDaemon();
      if (before !== state.daemonRunning || state.page === 'project') await refreshProjects();
      if (state.page === 'project' && state.selected && state.tab === 'overview') await loadStatus();
      await refreshProblemCount();
    }, 10000);
  }

  init();
})();
