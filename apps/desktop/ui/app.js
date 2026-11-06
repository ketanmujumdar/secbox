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

  const state = {
    info: null,
    projects: [],
    selected: null,
    status: null,
    tab: 'overview',
    busy: false,
    starting: null,
    daemonRunning: false,
    logId: null,
  };

  // ---------- banner, notice and busy line ----------

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

  // ---------- dialog ----------

  let dialogResolve = null;
  function ask({ title, build, okText, danger, okHidden }) {
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

  function section(body, heading, items, empty, mono) {
    body.appendChild(el('h3', heading));
    if (!items || items.length === 0) {
      body.appendChild(el('p', empty, 'muted'));
      return;
    }
    const list = el('ul');
    for (const item of items) list.appendChild(el('li', item, mono ? 'mono' : ''));
    body.appendChild(list);
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
    const dot = $('daemon-dot');
    dot.className = 'dot ' + (daemon.running ? 'on' : 'off');
    const bin = state.info ? state.info.daemon_bin : 'daemon';
    const text = $('daemon-text');
    clear(text);
    text.appendChild(document.createTextNode(daemon.running ? bin + ' is running' : bin + ' is not running '));
    if (!daemon.running) {
      const start = el('button', 'Start it', 'link');
      start.addEventListener('click', startDaemon);
      text.appendChild(start);
    }
    show($('notice'), !!daemon.notice);
    $('notice').textContent = daemon.notice || '';
    if (!daemon.running && daemon.message) {
      text.title = daemon.message;
    } else {
      text.title = '';
    }
    return daemon;
  }

  async function startDaemon() {
    busy('Starting the daemon.');
    try {
      banner(await invoke('start_daemon'), 'ok');
    } catch (error) {
      banner(errorText(error));
    }
    busy('');
    await refreshDaemon();
    await refreshProjects();
    await loadStatus();
  }

  // ---------- projects ----------

  function stateClass(s) {
    return s === 'running' ? 'running' : s === 'stopped' ? 'stopped' : '';
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
    if (rows.length === 0) {
      list.appendChild(el('li', 'No projects yet. Add a folder below, or run a command in one.', 'empty-line'));
    }
    for (const row of rows) {
      const item = el('li');
      item.title = row.path;
      item.appendChild(el('span', row.name, 'name'));
      item.appendChild(el('span', row.label, 'pill small-pill ' + stateClass(row.state)));
      if (row.path === state.selected) item.classList.add('selected');
      item.addEventListener('click', () => select(row.path));
      list.appendChild(item);
    }
    if (!state.selected && rows.length > 0) {
      await select(rows[0].path);
    } else if (state.selected) {
      renderHeader();
    }
  }

  function selectedRow() {
    return state.projects.find((p) => p.path === state.selected) || null;
  }

  async function select(path) {
    if (state.selected !== path) {
      stopLogs();
      clear($('log-text'));
      $('log-note').textContent = '';
      clearReview();
    }
    state.selected = path;
    for (const item of $('projects').children) {
      item.classList.toggle('selected', item.title === path);
    }
    banner('');
    await loadStatus();
    // Fills the overview card as well as the Blocked traffic tab.
    await readTraffic();
    if (state.tab === 'logs') await readLogs();
  }

  function renderHeader() {
    const row = selectedRow();
    $('project-name').textContent = row ? row.name : 'No project selected';
    $('project-path').textContent = row ? row.path : '';
    const pill = $('project-state');
    const s = state.status;
    if (row && s) {
      pill.textContent = s.label;
      pill.className = 'pill ' + stateClass(s.state);
      show(pill, true);
    } else if (row) {
      pill.textContent = row.label;
      pill.className = 'pill ' + stateClass(row.state);
      show(pill, true);
    } else {
      show(pill, false);
    }
    updateButtons();
  }

  function updateButtons() {
    const s = state.status;
    const ready = !!state.selected && state.daemonRunning;
    const has = ready && !state.busy;
    // Stop and Destroy stay on during a start: they are the only way out of one
    // that is stuck. The status is not refreshed while busy, so for the project
    // being started it is stale, and the daemon is left to decide.
    const canEnd = ready && (!state.busy || !!state.starting);
    const row = selectedRow();
    const startingThis = !!state.starting && !!row && row.path === state.starting;
    // A button is off only when the known state makes it pointless. With no
    // state at all, the daemon is left to decide, as the terminal does.
    $('btn-start').disabled = !has || (!!s && s.state === 'running');
    $('btn-restart').disabled = !has || !s || s.state !== 'running';
    $('btn-stop').disabled = !canEnd || (!startingThis && !!s && s.state !== 'running');
    // Destroy is always on: the daemon says so if there is nothing to destroy.
    $('btn-destroy').disabled = !canEnd;
    $('btn-remove').disabled = !has;
  }

  // ---------- overview ----------

  function row(list, left, right, leftClass) {
    const item = el('li');
    item.appendChild(el('span', left, leftClass || ''));
    item.appendChild(el('span', right, 'right'));
    list.appendChild(item);
  }
  function emptyRow(list, text) {
    list.appendChild(el('li', text, 'empty-line'));
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
    renderHeader();
    renderOverview(failure);
    renderLogServices();
  }

  function renderOverview(failure) {
    const s = state.status;
    show($('overview-grid'), !!s);
    show($('overview-empty'), !s);
    $('overview-empty').textContent = failure || '';
    const notes = $('status-notes');
    clear(notes);
    if (!s) return;

    const res = $('resources');
    clear(res);
    if (s.resources) {
      const r = s.resources;
      for (const [k, v] of [['Memory', r.memory], ['CPU', r.cpus], ['Disk', r.disk], ['Build memory', r.build_memory]]) {
        res.appendChild(el('dt', k));
        res.appendChild(el('dd', v));
      }
      $('resources-note').textContent = r.clamped
        ? 'Lowered to fit your own maximum. Changes apply on the next start.'
        : 'Changes apply on the next start.';
    } else {
      res.appendChild(el('dt', 'No limits on record'));
      res.appendChild(el('dd', 'They are set when it starts'));
      $('resources-note').textContent = '';
    }

    const services = $('services');
    clear(services);
    if (s.services.length === 0) emptyRow(services, s.state === 'running' ? 'None reported.' : 'Nothing is running.');
    for (const svc of s.services) row(services, svc.name, svc.state);
    $('services-note').textContent = s.services.length ? 'These came from inside the sandbox, so they can be wrong.' : '';

    const ports = $('ports');
    clear(ports);
    if (s.ports.length === 0) emptyRow(ports, 'No ports forwarded.');
    for (const p of s.ports) row(ports, p, 'forwarded', 'mono');

    $('blocked-count').textContent = s.blocked_total === null || s.blocked_total === undefined
      ? 'Not reported'
      : `${s.blocked_total} refused lookups, ${s.blocked_domains} hostnames`;
    clear($('blocked-top'));
    $('blocked-note').textContent = 'Blocked hostnames only. Raw IP, metadata and UDP attempts are blocked but not recorded, so zero proves nothing.';

    const exposure = $('exposure');
    clear(exposure);
    show($('exposure-card'), s.exposure.length > 0);
    for (const line of s.exposure) exposure.appendChild(el('li', line));

    for (const text of s.notes) notes.appendChild(el('li', text, 'warn'));
  }

  // ---------- start, stop, destroy ----------

  // A restart asks about the plan before stopping anything, so saying no leaves
  // the sandbox running. Its data is kept, as with Stop and then Start.
  async function startProject(restart) {
    const row = selectedRow();
    if (!row) return;
    const verb = restart ? 'restarted' : 'started';
    busy('Working out what starting it would do.');
    let plan;
    try {
      plan = await invoke('plan', { project: row.path });
    } catch (error) {
      busy('');
      banner(errorText(error));
      return;
    }
    busy('');
    if (plan.blockers.length > 0) {
      await ask({
        title: 'This cannot be ' + verb + ' yet',
        okHidden: true,
        build: (body) => {
          for (const b of plan.blockers) body.appendChild(el('p', b, 'alert'));
        },
      });
      banner('Nothing was ' + verb + '.', 'info');
      return;
    }
    const ports = plan.ports.map((p) => p.host_port);
    const agreed = await ask({
      title: (restart ? 'Restart ' : 'Start ') + row.name,
      okText: restart ? 'Restart with this plan' : 'Start with this plan',
      build: (body) => {
        for (const line of plan.summary) body.appendChild(el('p', line));
        section(body, 'Services', plan.services, 'None.');
        if (plan.generated_compose.length) {
          body.appendChild(el('h3', 'Compose file written for this Dockerfile'));
          body.appendChild(el('pre', plan.generated_compose.join('\n'), 'mono'));
        }
        body.appendChild(el('h3', 'Files'));
        body.appendChild(el('p', plan.commit ? 'From commit ' + plan.commit + '. Nothing uncommitted is copied in.' : 'From the working tree.', 'mono'));
        section(body, 'Ports to forward, on 127.0.0.1 only',
          plan.ports.map((p) => `127.0.0.1:${p.host_port}  →  ${p.service}:${p.container_port}` + (p.note ? '  (' + p.note + ')' : '')),
          'None.', true);
        section(body, 'Hosts allowed during the build', plan.build_hosts, 'None.', true);
        section(body, 'Hosts allowed while it runs', plan.run_hosts, 'None. Everything else is refused.', true);
        section(body, 'Secrets (names only)', plan.secrets, 'None.');
        if (plan.exposure.length) section(body, 'Where your secrets could go', plan.exposure, '', true);
        body.appendChild(el('h3', 'Limits'));
        body.appendChild(el('p', `${plan.resources.memory} memory, ${plan.resources.cpus} CPUs, ${plan.resources.disk} disk, ${plan.resources.build_memory} during the build.`));
        if (plan.findings.length) section(body, 'What the checks found', plan.findings, '');
        if (plan.warnings.length) section(body, 'Files that look like they hold a secret', plan.warnings, '', true);
        body.appendChild(el('p', 'Plan ' + plan.digest.slice(0, 16), 'muted mono'));
      },
    });
    if (!agreed) {
      banner('Nothing was ' + verb + '.', 'info');
      return;
    }
    state.starting = row.path;
    if (restart) {
      stopLogs();
      busy('Stopping, to restart it.');
      try {
        await invoke('down', { project: row.path });
      } catch (error) {
        banner('Could not stop it to restart it: ' + errorText(error));
        state.starting = null;
        busy('');
        await loadStatus();
        return;
      }
    }
    busy('Starting. A first build can take minutes.');
    const progress = new Channel();
    progress.onmessage = (event) => {
      busy('Starting. ' + event.step + (event.detail ? ': ' + event.detail : ''));
    };
    try {
      banner(await invoke('up', { project: row.path, digest: plan.digest, ports, onProgress: progress }), 'ok');
    } catch (error) {
      banner('Could not start it: ' + errorText(error));
    }
    state.starting = null;
    busy('');
    await refreshProjects();
    await loadStatus();
    if (state.tab === 'logs') await readLogs();
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
    busy(destroy ? 'Destroying.' : 'Stopping.');
    try {
      banner(await invoke(destroy ? 'destroy' : 'down', { project: row.path }), 'ok');
    } catch (error) {
      banner((destroy ? 'Could not destroy it: ' : 'Could not stop it: ') + errorText(error));
    }
    busy('');
    await refreshProjects();
    await loadStatus();
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
      state.selected = null;
      state.status = null;
      clear($('log-text'));
      clearReview();
    }
    await refreshProjects();
    if (state.selected) await loadStatus();
    else renderHeader();
    updateButtons();
  }

  $('btn-remove').addEventListener('click', removeProject);
  $('btn-start').addEventListener('click', () => startProject(false));
  $('btn-restart').addEventListener('click', () => startProject(true));
  $('btn-stop').addEventListener('click', () => stopProject(false));
  $('btn-destroy').addEventListener('click', () => stopProject(true));

  // ---------- logs ----------

  function renderLogServices() {
    const select = $('log-service');
    const keep = select.value;
    clear(select);
    select.appendChild(el('option', 'All services'));
    select.options[0].value = '';
    const names = (state.status && state.status.log_services) || [];
    for (const name of names) {
      const option = el('option', name);
      option.value = name;
      select.appendChild(option);
    }
    select.value = names.includes(keep) ? keep : '';
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

  function stopLogs() {
    if (state.logId !== null) {
      invoke('logs_stop', { id: state.logId }).catch(() => {});
      state.logId = null;
    }
    $('log-stop').disabled = true;
  }

  async function readLogs() {
    const row = selectedRow();
    if (!row) return;
    stopLogs();
    clear($('log-text'));
    const s = state.status;
    if (!s) {
      $('log-note').textContent = 'The daemon could not report on this project, so its logs were not asked for.';
      clearReview();
      return;
    }
    if (s.state !== 'running') {
      $('log-note').textContent = s.state === 'stopped'
        ? 'The sandbox is stopped. Start it to read its logs.'
        : 'This project has no running sandbox. Start it to read its logs.';
      return;
    }
    const follow = $('log-follow').checked;
    $('log-note').textContent = follow ? 'Following. New lines appear as they are written.' : 'Reading.';
    const channel = new Channel();
    let mine = null;
    channel.onmessage = (event) => {
      if (event.kind === 'text') {
        appendLog(event.text);
      } else if (event.kind === 'end') {
        if (mine !== null && state.logId === mine) {
          state.logId = null;
          $('log-stop').disabled = true;
        }
        const empty = $('log-text').textContent.length === 0;
        $('log-note').textContent = event.note || (empty ? 'Nothing has been logged yet.' : (follow ? 'Stopped following.' : ''));
      }
    };
    try {
      const service = $('log-service').value || null;
      mine = await invoke('logs', { project: row.path, service, follow, onEvent: channel });
      if (follow) {
        state.logId = mine;
        $('log-stop').disabled = false;
      }
    } catch (error) {
      $('log-note').textContent = errorText(error);
      clearReview();
    }
  }

  $('log-read').addEventListener('click', readLogs);
  $('log-stop').addEventListener('click', () => {
    stopLogs();
    $('log-note').textContent = 'Stopped following.';
  });
  $('log-service').addEventListener('change', readLogs);
  $('log-follow').addEventListener('change', readLogs);

  // ---------- blocked traffic ----------

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
      notes.appendChild(el('li', errorText(error), 'alert'));
      return;
    }
    if (state.selected !== row.path) return;
    for (const note of view.notes) notes.appendChild(el('li', note.text, note.weight === 'plain' ? '' : note.weight));
    $('traffic-total').textContent = view.recording ? `${view.total} refused lookups counted` : '';
    for (const event of view.rows) {
      const tr = el('tr');
      tr.appendChild(el('td', event.time));
      tr.appendChild(el('td', event.count, 'num'));
      tr.appendChild(el('td', event.domain));
      rows.appendChild(tr);
    }
    // The overview card shows the newest few.
    const top = $('blocked-top');
    clear(top);
    for (const event of view.rows.slice(0, 4)) row2(top, event);
  }
  function row2(list, event) {
    const item = el('li');
    item.appendChild(el('span', event.domain, 'mono'));
    item.appendChild(el('span', '×' + event.count, 'right'));
    list.appendChild(item);
  }

  $('traffic-read').addEventListener('click', readTraffic);
  $('btn-traffic').addEventListener('click', () => openTab('traffic'));

  // ---------- git review ----------

  // Clears the page, so a comparison is never shown under another project.
  function clearReview() {
    clear($('review-notes'));
    clear($('review-flags'));
    clear($('review-files'));
    $('review-base').textContent = '';
  }

  // Runs only when asked: the comparison runs inside the sandbox.
  async function readReview() {
    const row = selectedRow();
    clearReview();
    const notes = $('review-notes');
    if (!row) {
      notes.appendChild(el('li', 'No project selected.', 'warn'));
      return;
    }
    busy('Comparing inside the sandbox. A large project can take a while.');
    let view;
    try {
      view = await invoke('diff', { project: row.path });
    } catch (error) {
      notes.appendChild(el('li', errorText(error), 'alert'));
      return;
    } finally {
      busy('');
    }
    if (state.selected !== row.path) return;
    $('review-base').textContent = `Snapshot commit ${view.baseline}`;
    for (const note of view.notes) notes.appendChild(el('li', note.text, note.weight === 'plain' ? '' : note.weight));
    const flags = $('review-flags');
    for (const flag of view.flags) {
      const tr = el('tr', null, 'alert');
      tr.appendChild(el('td', 'FLAG', 'flag'));
      tr.appendChild(el('td', flag.path, 'mono'));
      tr.appendChild(el('td', flag.message));
      flags.appendChild(tr);
    }
    const files = $('review-files');
    for (const file of view.files) {
      const tr = el('tr');
      tr.appendChild(el('td', file.change));
      tr.appendChild(el('td', file.path, 'mono'));
      files.appendChild(tr);
    }
  }

  $('review-read').addEventListener('click', readReview);

  // ---------- tabs ----------

  async function openTab(tab) {
    state.tab = tab;
    for (const button of document.querySelectorAll('.tab')) {
      button.classList.toggle('active', button.dataset.tab === tab);
    }
    for (const panel of document.querySelectorAll('.panel')) {
      show(panel, panel.dataset.panel === tab);
    }
    if (tab !== 'logs') stopLogs();
    if (tab === 'traffic') await readTraffic();
    if (tab === 'logs' && state.selected) {
      await loadStatus();
      await readLogs();
    }
  }
  for (const button of document.querySelectorAll('.tab')) {
    button.addEventListener('click', () => openTab(button.dataset.tab));
  }

  // ---------- adding a project ----------

  $('add-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const path = $('add-path').value.trim();
    if (!path) return;
    try {
      const said = await invoke('add_project', { path });
      $('add-path').value = '';
      banner(said === 'Added.' ? '' : said, 'info');
      await refreshProjects();
    } catch (error) {
      banner(errorText(error));
    }
  });

  // ---------- importing from Git ----------

  $('import-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const url = $('import-url').value.trim();
    if (!url) return;
    busy('Fetching on this machine. Your SSH agent may ask you to approve.');
    let view;
    try {
      view = await invoke('import_remote', { url });
    } catch (error) {
      busy('');
      banner(errorText(error));
      return;
    }
    busy('');
    $('import-url').value = '';
    banner(view.message, 'info');
    await refreshProjects();
    await select(view.project_path);
    if (view.fetched) await startProject();
  });

  // ---------- Git ----------

  async function runGit(op, message) {
    const row = selectedRow();
    if (!row) { banner('Choose a project first.'); return; }
    const waiting = op === 'pull' || op === 'push';
    busy(waiting ? (op === 'pull' ? 'Pulling' : 'Pushing') + '. Your SSH agent may ask you to approve.' : 'Asking Git');
    let view;
    try {
      view = await invoke('git', { project: row.path, op, commitMessage: message || null });
    } catch (error) {
      busy('');
      banner(errorText(error));
      return;
    }
    busy('');
    const title = $('git-title');
    title.textContent = view.title;
    title.className = view.ok ? '' : 'bad';
    show(title, true);
    const updates = $('git-updates');
    clear(updates);
    for (const line of view.updates) updates.appendChild(el('li', 'pushed ' + line));
    if (view.refused) updates.appendChild(el('li', view.refused, 'bad'));
    $('git-output').textContent = view.output.join('\n');
    $('git-branch').textContent = view.branch ? 'On branch ' + view.branch : '';
    return view;
  }

  $('git-status').addEventListener('click', () => runGit('status'));
  $('git-pull').addEventListener('click', () => runGit('pull'));
  $('git-push').addEventListener('click', () => runGit('push'));
  $('git-commit-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const message = $('git-message').value.trim();
    if (!message) { banner('Write a commit message first.'); return; }
    const view = await runGit('commit', message);
    if (view && view.ok) $('git-message').value = '';
  });

  $('refresh').addEventListener('click', async () => {
    await refreshDaemon();
    await refreshProjects();
    await loadStatus();
  });

  // ---------- about and the terminal ----------

  function shellQuote(text) {
    return "'" + String(text).split("'").join("'\\''") + "'";
  }

  function renderInfo(info) {
    $('brand').textContent = info.product;
    document.title = info.product;
    $('secrets-cmd').textContent = `cd /path/to/project\n${info.cli_bin} secrets list\n${info.cli_bin} secrets set NAME`;
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
    const about = $('about');
    clear(about);
    const pairs = [
      ['Version', info.version],
      ['State folder', info.state_dir || 'not found'],
      ['Daemon', info.daemon_bin],
      ['Command line tool', info.bundled_cli || 'not bundled'],
    ];
    for (const [k, v] of pairs) {
      about.appendChild(el('dt', k));
      about.appendChild(el('dd', v));
    }
  }

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
    if (state.projects.length === 0) {
      show($('overview-grid'), false);
      show($('overview-empty'), true);
      $('overview-empty').textContent = 'No project selected. Import an SSH remote or add a project folder in the sidebar.';
    }
    // The menu bar icon may have started or restarted the daemon meanwhile.
    setInterval(async () => {
      if (state.busy || $('dialog').open) return;
      const before = state.daemonRunning;
      await refreshDaemon();
      if (before !== state.daemonRunning) await refreshProjects();
      if (state.selected && state.tab === 'overview') await loadStatus();
    }, 10000);
  }

  init();
})();
