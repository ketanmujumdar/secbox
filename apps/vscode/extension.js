'use strict';
// Shows a running sandbox's /work to VS Code as a virtual folder, through
// `secbox editor-bridge`. See README.md for the rule this keeps.

const vscode = require('vscode');
const { spawn, execFile } = require('child_process');
const fs = require('fs');
const path = require('path');
const readline = require('readline');

const SCHEME = 'secbox-sandbox';
const HEAD_SCHEME = 'secbox-head';
const CALL_LIMIT_MS = 120000;
const LIST_FRESH_MS = 3000;
const GIT_LIMIT_MS = 300000;

// The bridge refuses these too. This only saves asking, and keeps VS Code from
// waiting on a folder it reads at startup.
function isHidden(uriPath) {
  return uriPath.split('/').some((part) => {
    const name = part.toLowerCase().replace(/[.\s]+$/, '');
    return name === '.vscode' || name === '.git' || name.endsWith('.code-workspace');
  });
}

function relative(uri) {
  return uri.path.replace(/^\/+/, '');
}

function hexOf(data) {
  return Buffer.from(data).toString('hex');
}

class Bridge {
  constructor(binary, project) {
    this.binary = binary;
    this.project = project;
    this.child = null;
    this.next = 1;
    this.waiting = new Map();
  }

  start() {
    const child = spawn(this.binary, ['editor-bridge', '--project', this.project], { stdio: ['pipe', 'pipe', 'ignore'] });
    this.child = child;
    readline.createInterface({ input: child.stdout }).on('line', (line) => {
      let reply;
      try {
        reply = JSON.parse(line);
      } catch (_) {
        return;
      }
      const entry = this.waiting.get(reply.id);
      if (!entry) return;
      this.waiting.delete(reply.id);
      clearTimeout(entry.timer);
      entry.resolve(reply);
    });
    const gone = () => {
      if (this.child === child) this.child = null;
      for (const [id, entry] of this.waiting) {
        this.waiting.delete(id);
        clearTimeout(entry.timer);
        entry.resolve({ id, ok: false, code: 'failed', message: 'the connection to secbox ended' });
      }
    };
    child.on('exit', gone);
    child.on('error', gone);
    child.stdin.on('error', () => {});
  }

  call(op, fields) {
    if (!this.child) this.start();
    const id = this.next++;
    return new Promise((resolve) => {
      const timer = setTimeout(() => {
        this.waiting.delete(id);
        resolve({ id, ok: false, code: 'failed', message: 'secbox took too long to answer' });
      }, CALL_LIMIT_MS);
      this.waiting.set(id, { resolve, timer });
      this.child.stdin.write(JSON.stringify({ id, op, ...fields }) + '\n');
    });
  }

  stop() {
    if (this.child) this.child.kill();
    this.child = null;
  }
}

function fileType(entry) {
  return entry.kind === 'dir' ? vscode.FileType.Directory : entry.kind === 'file' ? vscode.FileType.File : vscode.FileType.Unknown;
}

function failure(reply, uri) {
  const message = reply.message || 'secbox could not do that';
  switch (reply.code) {
    case 'not_found':
    case 'hidden':
      return vscode.FileSystemError.FileNotFound(uri);
    case 'exists':
      return vscode.FileSystemError.FileExists(uri);
    case 'is_dir':
      return vscode.FileSystemError.FileIsADirectory(uri);
    default:
      return new vscode.FileSystemError(message);
  }
}

class SandboxFiles {
  constructor(bridge) {
    this.bridge = bridge;
    this.changed = new vscode.EventEmitter();
    this.onDidChangeFile = this.changed.event;
    // What a recent listing said, because VS Code stats each entry it lists.
    this.known = new Map();
  }

  watch() {
    return { dispose() {} };
  }

  forget(uri) {
    this.known.delete(uri.path);
    this.known.delete(path.posix.dirname(uri.path));
  }

  refresh(root) {
    this.known.clear();
    this.changed.fire([{ type: vscode.FileChangeType.Changed, uri: root }]);
  }

  async stat(uri) {
    if (isHidden(uri.path)) throw vscode.FileSystemError.FileNotFound(uri);
    const seen = this.known.get(uri.path);
    if (seen && Date.now() - seen.at < LIST_FRESH_MS) return seen.stat;
    const reply = await this.bridge.call('stat', { path: relative(uri) });
    if (!reply.ok) throw failure(reply, uri);
    return { type: fileType(reply), ctime: reply.mtime * 1000, mtime: reply.mtime * 1000, size: reply.size };
  }

  async readDirectory(uri) {
    const reply = await this.bridge.call('list', { path: relative(uri) });
    if (!reply.ok) throw failure(reply, uri);
    const at = Date.now();
    return reply.entries.map((entry) => {
      const stat = { type: fileType(entry), ctime: entry.mtime * 1000, mtime: entry.mtime * 1000, size: entry.size };
      this.known.set(path.posix.join(uri.path, entry.name), { at, stat });
      return [entry.name, stat.type];
    });
  }

  async readFile(uri) {
    if (isHidden(uri.path)) throw vscode.FileSystemError.FileNotFound(uri);
    const reply = await this.bridge.call('read', { path: relative(uri) });
    if (!reply.ok) throw failure(reply, uri);
    return Buffer.from(reply.data, 'hex');
  }

  async writeFile(uri, content, options) {
    if (isHidden(uri.path)) throw vscode.FileSystemError.NoPermissions('secbox keeps this folder out of the editor');
    const reply = await this.bridge.call('write', { path: relative(uri), data: hexOf(content), create: options.create, overwrite: options.overwrite });
    if (!reply.ok) throw failure(reply, uri);
    this.forget(uri);
    this.changed.fire([{ type: vscode.FileChangeType.Changed, uri }]);
  }

  async createDirectory(uri) {
    const reply = await this.bridge.call('mkdir', { path: relative(uri) });
    if (!reply.ok) throw failure(reply, uri);
    this.forget(uri);
  }

  async delete(uri, options) {
    const reply = await this.bridge.call('delete', { path: relative(uri), recursive: options.recursive });
    if (!reply.ok) throw failure(reply, uri);
    this.forget(uri);
    this.changed.fire([{ type: vscode.FileChangeType.Deleted, uri }]);
  }

  async rename(from, to, options) {
    const reply = await this.bridge.call('rename', { path: relative(from), to: relative(to), overwrite: options.overwrite });
    if (!reply.ok) throw failure(reply, from);
    this.forget(from);
    this.forget(to);
    this.changed.fire([{ type: vscode.FileChangeType.Deleted, uri: from }, { type: vscode.FileChangeType.Created, uri: to }]);
  }
}

// Git, always through the daemon. Nothing here runs Git on this machine.
class SandboxGit {
  constructor(binary, project, root, output) {
    this.binary = binary;
    this.project = project;
    this.root = root;
    this.output = output;
    this.control = vscode.scm.createSourceControl('secbox', 'secbox', root);
    this.control.inputBox.placeholder = 'Message (commits through the secbox daemon)';
    this.control.acceptInputCommand = { command: 'secbox.commit', title: 'Commit' };
    this.group = this.control.createResourceGroup('changes', 'Changes');
    this.group.hideWhenEmpty = false;
  }

  run(args) {
    return new Promise((resolve) => {
      execFile(this.binary, ['git', '--project', this.project, ...args], { timeout: GIT_LIMIT_MS, maxBuffer: 8 * 1024 * 1024 }, (error, stdout, stderr) => {
        resolve({ ok: !error, text: (stdout + stderr).trimEnd() });
      });
    });
  }

  async refresh() {
    const done = await this.run(['status']);
    const states = [];
    for (const line of done.text.split('\n')) {
      const match = line.match(/^([ MADRCUT?!]{2}) (.+)$/);
      if (!match) continue;
      // A name Git had to quote is listed, but cannot be opened from here.
      const name = match[2].includes(' -> ') ? match[2].split(' -> ').pop() : match[2];
      const plain = !name.startsWith('"');
      const uri = plain ? this.root.with({ path: '/' + name.replace(/\/$/, '') }) : this.root.with({ path: '/(a name with unusual characters)' });
      const code = match[1].trim();
      const state = { resourceUri: uri, decorations: { tooltip: code, strikeThrough: code.includes('D') } };
      if (plain && !isHidden(uri.path) && !code.includes('D') && !name.endsWith('/')) {
        const head = vscode.Uri.from({ scheme: HEAD_SCHEME, authority: uri.authority, path: uri.path });
        state.command = { command: 'vscode.diff', title: 'Open Changes', arguments: [head, uri, name + ' (working)'] };
      }
      states.push(state);
    }
    this.group.resourceStates = states;
    this.control.count = states.length;
  }

  // Shows what happened, whether it worked or not. The CLI has escaped it.
  async act(title, args) {
    const done = await vscode.window.withProgress({ location: vscode.ProgressLocation.SourceControl, title }, () => this.run(args));
    this.output.appendLine('$ git ' + args.join(' '));
    if (done.text) this.output.appendLine(done.text);
    if (done.ok) vscode.window.setStatusBarMessage(title + ' done', 4000);
    else {
      this.output.show(true);
      vscode.window.showErrorMessage(title + ' did not work. See the secbox output for why.');
    }
    await this.refresh();
    return done.ok;
  }
}

async function goToFile(bridge, root) {
  const reply = await bridge.call('files', {});
  if (!reply.ok) {
    vscode.window.showErrorMessage(reply.message || 'secbox could not list the files');
    return;
  }
  const picked = await vscode.window.showQuickPick(reply.files, { placeHolder: 'Go to a file in the sandbox', matchOnDescription: false });
  if (picked) await vscode.window.showTextDocument(root.with({ path: '/' + picked }));
}

async function searchText(bridge, root) {
  const query = await vscode.window.showInputBox({ prompt: 'Search the sandbox for this exact text' });
  if (!query) return;
  const reply = await vscode.window.withProgress({ location: vscode.ProgressLocation.Window, title: 'Searching the sandbox' }, () => bridge.call('search', { query }));
  if (!reply.ok) {
    vscode.window.showErrorMessage(reply.message || 'secbox could not search');
    return;
  }
  if (!reply.matches.length) {
    vscode.window.showInformationMessage('Nothing in the sandbox has that text.');
    return;
  }
  const items = reply.matches.map((m) => ({ label: m.text.trim() || '(blank)', description: m.path + ':' + m.line, match: m }));
  const picked = await vscode.window.showQuickPick(items, { placeHolder: reply.cut ? 'The first matches only' : 'Matches', matchOnDescription: true });
  if (!picked) return;
  const line = Math.max(0, picked.match.line - 1);
  const range = new vscode.Range(line, 0, line, 0);
  await vscode.window.showTextDocument(root.with({ path: '/' + picked.match.path }), { selection: range });
}

function activate(context) {
  const output = vscode.window.createOutputChannel('secbox');
  context.subscriptions.push(output);
  const settings = JSON.parse(fs.readFileSync(path.join(context.extensionPath, 'bridge.json'), 'utf8'));
  const folder = (vscode.workspace.workspaceFolders || []).find((f) => f.uri.scheme === SCHEME);
  if (!folder) return;
  const project = Buffer.from(folder.uri.authority, 'hex').toString('utf8');
  const bridge = new Bridge(settings.binary, project);
  context.subscriptions.push({ dispose: () => bridge.stop() });

  const files = new SandboxFiles(bridge);
  context.subscriptions.push(vscode.workspace.registerFileSystemProvider(SCHEME, files, { isCaseSensitive: true }));
  context.subscriptions.push(vscode.workspace.registerTextDocumentContentProvider(HEAD_SCHEME, {
    async provideTextDocumentContent(uri) {
      const reply = await bridge.call('head', { path: relative(uri) });
      return reply.ok ? Buffer.from(reply.data, 'hex').toString('utf8') : '';
    },
  }));

  // Tells this window's terminals which project they are for. See editor-shell.
  context.environmentVariableCollection.replace('SECBOX_EDITOR_PROJECT', project);
  const shell = () => ({ name: 'secbox shell', shellPath: settings.binary, shellArgs: ['shell', '--project', project] });

  const git = new SandboxGit(settings.binary, project, folder.uri, output);
  context.subscriptions.push(git.control);
  const command = (name, run) => context.subscriptions.push(vscode.commands.registerCommand(name, run));
  command('secbox.goToFile', () => goToFile(bridge, folder.uri));
  command('secbox.searchText', () => searchText(bridge, folder.uri));
  command('secbox.openTerminal', () => vscode.window.createTerminal(shell()).show());
  command('secbox.refresh', async () => {
    files.refresh(folder.uri);
    await git.refresh();
  });
  command('secbox.commit', async () => {
    const message = git.control.inputBox.value.trim();
    if (!message) {
      vscode.window.showWarningMessage('Write a commit message first.');
      return;
    }
    if (await git.act('Commit', ['commit', '-m', message])) git.control.inputBox.value = '';
  });
  command('secbox.pull', () => git.act('Pull', ['pull']));
  command('secbox.push', () => git.act('Push', ['push']));

  vscode.commands.executeCommand('setContext', 'secbox.active', true);
  const status = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Left, 100);
  status.text = '$(shield) secbox sandbox';
  status.tooltip = 'These files are inside the sandbox. Git goes through the secbox daemon.';
  status.show();
  context.subscriptions.push(status);

  // Something inside the sandbox may have changed files while this was away.
  context.subscriptions.push(vscode.window.onDidChangeWindowState((state) => {
    if (state.focused) {
      files.refresh(folder.uri);
      git.refresh();
    }
  }));
  context.subscriptions.push(vscode.workspace.onDidSaveTextDocument((doc) => {
    if (doc.uri.scheme === SCHEME) git.refresh();
  }));
  git.refresh();
}

function deactivate() {}

module.exports = { activate, deactivate, isHidden };
