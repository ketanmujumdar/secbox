// Answers on the two ports the Dockerfile exposes.
const http = require("http");
for (const port of [3000, 8080]) {
  http
    .createServer((req, res) => res.end(`ok from ${port}\n`))
    .listen(port, "0.0.0.0");
}
