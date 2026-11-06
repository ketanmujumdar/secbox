// Serves ../ui with stub.js in front of app.js, for a plain browser. Only for
// looking at the page during development; the app itself has no web server.
//
//   node apps/desktop/ui-preview/server.js [port]    then open /?scene=projects
'use strict';

const http = require('http');
const fs = require('fs');
const path = require('path');

const UI = path.join(__dirname, '..', 'ui');
const CONF = path.join(__dirname, '..', 'src-tauri', 'tauri.conf.json');
// The CSP the app runs under, sent as a header too.
const CSP = JSON.parse(fs.readFileSync(CONF, 'utf8')).app.security.csp;
const TYPES = { '.html': 'text/html', '.css': 'text/css', '.js': 'text/javascript' };

function serve(port) {
  const server = http.createServer((req, res) => {
    const url = new URL(req.url, 'http://localhost');
    let file;
    if (url.pathname === '/' || url.pathname === '/index.html') file = path.join(UI, 'index.html');
    else if (url.pathname === '/stub.js') file = path.join(__dirname, 'stub.js');
    else file = path.join(UI, path.normalize(url.pathname).replace(/^(\.\.[/\\])+/, ''));
    if (!file.startsWith(UI) && file !== path.join(__dirname, 'stub.js')) {
      res.writeHead(403).end();
      return;
    }
    fs.readFile(file, 'utf8', (error, body) => {
      if (error) { res.writeHead(404).end(); return; }
      if (file.endsWith('index.html')) {
        body = body.replace('<script src="app.js"></script>', '<script src="stub.js"></script>\n  <script src="app.js"></script>');
      }
      res.writeHead(200, { 'Content-Type': TYPES[path.extname(file)] || 'text/plain', 'Content-Security-Policy': CSP });
      res.end(body);
    });
  });
  return new Promise((resolve) => server.listen(port, '127.0.0.1', () => resolve(server)));
}

module.exports = { serve };

if (require.main === module) {
  const port = Number(process.argv[2] || 8765);
  serve(port).then(() => console.log(`http://127.0.0.1:${port}/?scene=projects`));
}
