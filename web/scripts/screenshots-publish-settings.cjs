// Screenshots of the Publishing card on the Connections screen: off, on and private, the
// confirmation before saving public, and a failed load with Retry.
// Usage: npm run build && node scripts/screenshots-publish-settings.cjs <outDir>
// Needs Playwright with Chromium already installed (require('playwright')).
const http = require("node:http");
const fs = require("node:fs");
const path = require("node:path");
const { chromium } = require("playwright");

const dist = path.join(__dirname, "..", "dist");
const outDir = process.argv[2];
if (!outDir) {
  console.error("Usage: node scripts/screenshots-publish-settings.cjs <outDir>");
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

const OFF = { enabled: false, privacy: "private", updated_at: null, source: "defaults" };
const ON = { enabled: true, privacy: "private", updated_at: 1700000000, source: "settings" };
const STATUS = { youtube: { connected: true, needs_reconnect: false } };

const json = (body, status = 200) => ({ status, contentType: "application/json", body: JSON.stringify(body) });

// Each state: what GET /api/settings/publish answers, and what to do before the picture.
const STATES = [
  { name: "off", settings: () => json(OFF), act: async () => {} },
  { name: "on-private", settings: () => json(ON), act: async () => {} },
  {
    name: "public-confirm",
    settings: () => json(ON),
    act: async (page) => {
      await page.getByLabel("Privacy").selectOption("public");
      await page.getByRole("button", { name: "Save", exact: true }).click();
      await page.getByRole("alertdialog").waitFor();
    },
  },
  {
    name: "load-failure",
    settings: () => ({ status: 500, contentType: "text/plain", body: "internal error" }),
    act: async (page) => {
      await page.getByRole("button", { name: "Retry" }).waitFor();
    },
  },
];

function fail(message) {
  console.error(message);
  process.exitCode = 1;
}

async function shoot(browser, base, label, viewport) {
  for (const state of STATES) {
    const ctx = await browser.newContext({ viewport, deviceScaleFactor: 2, locale: "en-AU" });
    const page = await ctx.newPage();
    // The failed load is a 500 on purpose, so the browser's own log line for it is expected.
    page.on("console", (m) => m.type() === "error" && !/status of 500/.test(m.text()) && fail(`console error: ${m.text()}`));
    page.on("pageerror", (e) => fail(`page error: ${e.message}`));
    await page.route("**/api/connect/status", (r) => r.fulfill(json(STATUS)));
    await page.route("**/api/settings/publish", (r) => r.fulfill(state.settings()));
    await page.goto(`${base}settings`);
    await page.getByRole("heading", { name: "Publishing" }).waitFor();
    if (state.name !== "load-failure") await page.getByRole("switch", { name: "Publishing" }).waitFor();
    await state.act(page);
    await page.mouse.move(0, 0);
    await page.screenshot({ path: path.join(outDir, `publish-settings-${state.name}-${label}.png`), fullPage: true });
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
