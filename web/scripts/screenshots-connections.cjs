// Screenshots of the Connections screen in its four states.
// Usage: npm run build && node scripts/screenshots-connections.cjs <outDir>
// Needs Playwright with Chromium already installed (require('playwright')).
const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const dist = path.join(__dirname, "..", "dist");
const outDir = process.argv[2];
if (!outDir) {
  console.error("Usage: node scripts/screenshots-connections.cjs <outDir>");
  process.exit(2);
}
const TYPES = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".png": "image/png" };

// Apply the same headers Cloudflare will (public/_headers), so a CSP problem shows up here.
function readHeaders() {
  const lines = fs.readFileSync(path.join(dist, "_headers"), "utf8").split("\n");
  const headers = {};
  for (const line of lines.filter((l) => /^\s+\S/.test(l))) {
    const i = line.indexOf(":");
    headers[line.slice(0, i).trim()] = line.slice(i + 1).trim();
  }
  return headers;
}

// Like the real app: a path that is not a file (here /settings) gets index.html.
function serve() {
  const headers = readHeaders();
  const server = http.createServer((req, res) => {
    let name = req.url.split("?")[0];
    if (!path.extname(name)) name = "/index.html";
    const file = path.join(dist, path.normalize(name));
    if (!(file === dist || file.startsWith(dist + path.sep)) || !fs.existsSync(file)) {
      res.writeHead(404).end();
      return;
    }
    res.writeHead(200, { ...headers, "Content-Type": TYPES[path.extname(file)] || "application/octet-stream" });
    fs.createReadStream(file).pipe(res);
  });
  return new Promise((resolve) => server.listen(0, "127.0.0.1", () => resolve(server)));
}

const STATES = [
  { name: "not-connected-denied", status: { connected: false, needs_reconnect: false }, query: "?connect_error=access_denied", wait: "Not connected" },
  { name: "connected", status: { connected: true, needs_reconnect: false }, query: "?connected=1", wait: "Connected" },
  { name: "needs-reconnect", status: { connected: true, needs_reconnect: true }, query: "", wait: "Needs reconnecting" },
  { name: "not-connected", status: { connected: false, needs_reconnect: false }, query: "", wait: "Not connected" },
];

function fail(message) {
  console.error(message);
  process.exitCode = 1;
}

async function shoot(browser, base, label, viewport) {
  for (const state of STATES) {
    const ctx = await browser.newContext({ viewport, deviceScaleFactor: 2, locale: "en-AU" });
    const page = await ctx.newPage();
    page.on("console", (m) => m.type() === "error" && fail(`console error: ${m.text()}`));
    page.on("pageerror", (e) => fail(`page error: ${e.message}`));
    await page.route("**/api/connect/status", (r) =>
      r.fulfill({ status: 200, contentType: "application/json", body: JSON.stringify({ youtube: state.status }) }),
    );
    await page.goto(`${base}settings${state.query}`);
    await page.getByText(state.wait, { exact: true }).waitFor();
    await page.mouse.move(0, 0);
    await page.screenshot({ path: path.join(outDir, `connections-${state.name}-${label}.png`), fullPage: true });
    await ctx.close();
  }
}

(async () => {
  fs.mkdirSync(outDir, { recursive: true });
  const server = await serve();
  const base = `http://127.0.0.1:${server.address().port}/`;
  const browser = await chromium.launch();
  try {
    await shoot(browser, base, "phone", { width: 390, height: 844 });
    await shoot(browser, base, "desktop", { width: 1280, height: 800 });
  } finally {
    await browser.close();
    server.close();
  }
})();
