import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const desktopDir = fileURLToPath(new URL("..", import.meta.url));
const uiDir = join(desktopDir, "ui");
const outputDir = fileURLToPath(new URL("../../target/ui-smoke", import.meta.url));
mkdirSync(outputDir, { recursive: true });

const mimeTypes = {
  ".css": "text/css",
  ".html": "text/html",
  ".js": "text/javascript",
  ".png": "image/png",
};

const server = createServer((request, response) => {
  const relativePath = request.url === "/" ? "index.html" : request.url.slice(1).split("?")[0];
  const filePath = join(uiDir, relativePath);
  let body;
  try {
    body = readFileSync(filePath);
  } catch {
    response.writeHead(404);
    response.end("Not found");
    return;
  }
  response.writeHead(200, { "Content-Type": mimeTypes[extname(filePath)] || "application/octet-stream" });
  response.end(body);
});

await new Promise((resolve) => server.listen(4173, "127.0.0.1", resolve));

const chromeCandidates = [
  process.env.CHROME_BIN,
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
  "/usr/bin/google-chrome",
  "/usr/bin/chromium",
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
].filter(Boolean);
const chromePath = chromeCandidates.find(existsSync);
if (!chromePath) {
  server.close();
  throw new Error("Chrome was not found; set CHROME_BIN to run the UI smoke test");
}

const chrome = spawn(chromePath, [
  "--headless=new",
  "--disable-gpu",
  "--no-sandbox",
  "--disable-dev-shm-usage",
  "--hide-scrollbars",
  "--remote-debugging-port=9229",
  `--user-data-dir=${join(outputDir, "chrome-profile")}`,
  "about:blank",
], { stdio: "ignore" });

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

async function waitForDebugger() {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    try {
      const response = await fetch("http://127.0.0.1:9229/json/version");
      if (response.ok) return;
    } catch {}
    await sleep(50);
  }
  throw new Error("Chrome DevTools did not start");
}

class CdpClient {
  constructor(url) {
    this.nextId = 1;
    this.pending = new Map();
    this.socket = new WebSocket(url);
    this.socket.addEventListener("message", ({ data }) => {
      const message = JSON.parse(data);
      if (!message.id) return;
      const pending = this.pending.get(message.id);
      this.pending.delete(message.id);
      if (message.error) pending.reject(new Error(message.error.message));
      else pending.resolve(message.result);
    });
  }

  async connect() {
    if (this.socket.readyState === WebSocket.OPEN) return;
    await new Promise((resolve, reject) => {
      this.socket.addEventListener("open", resolve, { once: true });
      this.socket.addEventListener("error", reject, { once: true });
    });
  }

  send(method, params = {}) {
    const id = this.nextId++;
    this.socket.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => this.pending.set(id, { resolve, reject }));
  }

  close() {
    this.socket.close();
  }
}

const mockSource = `
  const mockStreams = [
    { user_login: "shroud", user_name: "shroud", game_name: "VALORANT", title: "Ranked matches", thumbnail_url: "https://static-cdn.jtvnw.net/previews-ttv/live_user_shroud-{width}x{height}.jpg", viewer_count: 24800, started_at: "2026-07-16T04:00:00Z" },
    { user_login: "pokimane", user_name: "pokimane", game_name: "Just Chatting", title: "Morning stream", thumbnail_url: "https://static-cdn.jtvnw.net/previews-ttv/live_user_pokimane-{width}x{height}.jpg", viewer_count: 12100, started_at: "2026-07-16T05:00:00Z" },
    { user_login: "sodapoppin", user_name: "sodapoppin", game_name: "Variety", title: "New releases", thumbnail_url: "https://static-cdn.jtvnw.net/previews-ttv/live_user_sodapoppin-{width}x{height}.jpg", viewer_count: 8700, started_at: "2026-07-16T06:00:00Z" },
    { user_login: "twitchdev", user_name: "TwitchDev", game_name: "Science & Technology", title: "API workshop", thumbnail_url: "https://static-cdn.jtvnw.net/previews-ttv/live_user_twitchdev-{width}x{height}.jpg", viewer_count: 940, started_at: "2026-07-16T07:00:00Z" }
  ];
  window.__TAURI__ = {
    core: {
      invoke: async (command) => {
        if (command === "session_status") return { authenticated: true, login: "viewer" };
        if (command === "followed_streams") return mockStreams;
        if (command === "popular_streams") return mockStreams.slice(0, 3);
        if (command === "category_streams") return mockStreams.slice(1, 3);
        if (command === "search_streams") return mockStreams.slice(3);
        if (command === "top_categories") return [
          { id: "509658", name: "Just Chatting", box_art_url: "" },
          { id: "516575", name: "VALORANT", box_art_url: "" }
        ];
        return null;
      }
    },
    event: { listen: async () => () => {} }
  };
`;

async function waitForApplication(client) {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const result = await client.send("Runtime.evaluate", {
      expression: "!document.querySelector('#app-view')?.hidden && document.querySelectorAll('.stream-row').length === 4",
      returnByValue: true,
    });
    if (result.result.value) return;
    await sleep(50);
  }
  throw new Error("Mock application did not render");
}

async function capture(client, name, width, height) {
  await client.send("Emulation.setDeviceMetricsOverride", {
    width,
    height,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await sleep(100);

  const metrics = await client.send("Runtime.evaluate", {
    expression: `(() => {
      const rect = (selector) => document.querySelector(selector).getBoundingClientRect();
      const streams = rect('.streams-pane');
      const player = rect('.player-pane');
      const chat = rect('.chat-pane');
      return {
        viewport: [innerWidth, innerHeight],
        documentSize: [document.documentElement.scrollWidth, document.documentElement.scrollHeight],
        panesOrdered: streams.right <= player.left && player.right <= chat.left,
        panesVisible: streams.left >= 0 && chat.right <= innerWidth && player.width > 0,
        videoVisible: rect('.video-stage').width > 0 && rect('.video-stage').height > 0,
      };
    })()`,
    returnByValue: true,
  });
  const value = metrics.result.value;
  if (
    value.documentSize[0] > value.viewport[0]
    || value.documentSize[1] > value.viewport[1]
    || !value.panesOrdered
    || !value.panesVisible
    || !value.videoVisible
  ) {
    throw new Error(`${name} layout failed: ${JSON.stringify(value)}`);
  }

  const screenshot = await client.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(outputDir, `${name}.png`), Buffer.from(screenshot.data, "base64"));
  return value;
}

async function captureDiscover(client) {
  await client.send("Emulation.setDeviceMetricsOverride", {
    width: 1440,
    height: 900,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await client.send("Runtime.evaluate", {
    expression: "document.querySelector('#discover-tab').click()",
  });
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const result = await client.send("Runtime.evaluate", {
      expression: `document.querySelector('#discover-tab').getAttribute('aria-selected') === 'true'
        && !document.querySelector('#discover-tools').hidden
        && document.querySelectorAll('#category-select option').length === 3
        && document.querySelectorAll('.stream-row').length === 3`,
      returnByValue: true,
    });
    if (result.result.value) break;
    if (attempt === 99) throw new Error("Discover view did not load");
    await sleep(50);
  }

  const screenshot = await client.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(outputDir, "discover-1440x900.png"), Buffer.from(screenshot.data, "base64"));

  await client.send("Runtime.evaluate", {
    expression: `(() => {
      const select = document.querySelector('#category-select');
      select.value = '509658';
      select.dispatchEvent(new Event('change'));
    })()`,
  });
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const result = await client.send("Runtime.evaluate", {
      expression: "document.querySelectorAll('.stream-row').length === 2",
      returnByValue: true,
    });
    if (result.result.value) break;
    if (attempt === 99) throw new Error("Category stream view did not load");
    await sleep(50);
  }
  await client.send("Runtime.evaluate", {
    expression: `(() => {
      const input = document.querySelector('#stream-filter');
      input.value = 'twitch';
      document.querySelector('#stream-search-form').dispatchEvent(new Event('submit', { cancelable: true }));
    })()`,
  });
  for (let attempt = 0; attempt < 100; attempt += 1) {
    const result = await client.send("Runtime.evaluate", {
      expression: "document.querySelectorAll('.stream-row').length === 1",
      returnByValue: true,
    });
    if (result.result.value) return { popular: 3, category: 2, search: 1 };
    await sleep(50);
  }
  throw new Error("Channel search view did not load");
}

async function captureQualityMenu(client) {
  await client.send("Runtime.evaluate", {
    expression: `(() => {
      document.querySelector('#player-controls').hidden = false;
      document.querySelector('#quality-button').click();
    })()`,
  });
  await sleep(100);
  const metrics = await client.send("Runtime.evaluate", {
    expression: `(() => {
      const menu = document.querySelector('#quality-menu');
      const rect = menu.getBoundingClientRect();
      const stage = document.querySelector('.video-stage').getBoundingClientRect();
      return {
        visible: !menu.hidden && getComputedStyle(menu).display !== 'none',
        selected: document.querySelector('.quality-option.selected')?.dataset.quality,
        contained: rect.left >= stage.left && rect.right <= stage.right && rect.bottom <= stage.bottom,
      };
    })()`,
    returnByValue: true,
  });
  const value = metrics.result.value;
  if (!value.visible || value.selected !== "best" || !value.contained) {
    throw new Error(`quality menu failed: ${JSON.stringify(value)}`);
  }
  const screenshot = await client.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(outputDir, "quality-menu-1440x900.png"), Buffer.from(screenshot.data, "base64"));
  await client.send("Runtime.evaluate", {
    expression: "document.querySelector('[data-quality=source]').click()",
  });
  return value;
}

async function captureFullscreen(client) {
  await client.send("Emulation.setDeviceMetricsOverride", {
    width: 1440,
    height: 900,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await client.send("Runtime.evaluate", {
    expression: `(() => {
      document.body.classList.add('player-fullscreen');
      document.querySelector('#player-controls').hidden = false;
    })()`,
  });
  await sleep(100);
  const closedMetrics = await client.send("Runtime.evaluate", {
    expression: `(() => {
      const player = document.querySelector('.player-pane').getBoundingClientRect();
      const chat = getComputedStyle(document.querySelector('.chat-pane'));
      return {
        player: [player.left, player.top, player.width, player.height],
        viewport: [innerWidth, innerHeight],
        chatHidden: chat.display === 'none',
        chatButtonVisible: getComputedStyle(document.querySelector('#fullscreen-chat-button')).display !== 'none',
      };
    })()`,
    returnByValue: true,
  });
  const closed = closedMetrics.result.value;
  if (
    closed.player[0] !== 0
    || closed.player[1] !== 0
    || closed.player[2] !== closed.viewport[0]
    || closed.player[3] !== closed.viewport[1]
    || !closed.chatHidden
    || !closed.chatButtonVisible
  ) {
    throw new Error(`fullscreen layout failed: ${JSON.stringify(closed)}`);
  }
  const screenshot = await client.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(outputDir, "fullscreen-1440x900.png"), Buffer.from(screenshot.data, "base64"));

  await client.send("Runtime.evaluate", {
    expression: "document.body.classList.add('fullscreen-chat-open')",
  });
  await sleep(100);
  const openMetrics = await client.send("Runtime.evaluate", {
    expression: `(() => {
      const player = document.querySelector('.player-pane').getBoundingClientRect();
      const chat = document.querySelector('.chat-pane').getBoundingClientRect();
      return {
        player: [player.left, player.top, player.width, player.height],
        chat: [chat.left, chat.top, chat.width, chat.height],
        viewport: [innerWidth, innerHeight],
        ordered: player.right <= chat.left,
      };
    })()`,
    returnByValue: true,
  });
  const open = openMetrics.result.value;
  if (
    open.player[0] !== 0
    || open.player[1] !== 0
    || open.player[2] <= 0
    || open.player[3] !== open.viewport[1]
    || open.chat[2] < 300
    || open.chat[3] !== open.viewport[1]
    || open.chat[0] + open.chat[2] !== open.viewport[0]
    || !open.ordered
  ) {
    throw new Error(`fullscreen chat layout failed: ${JSON.stringify(open)}`);
  }
  const chatScreenshot = await client.send("Page.captureScreenshot", { format: "png" });
  writeFileSync(join(outputDir, "fullscreen-chat-1440x900.png"), Buffer.from(chatScreenshot.data, "base64"));
  return { closed, open };
}

try {
  await waitForDebugger();
  const page = await fetch("http://127.0.0.1:9229/json/new?about:blank", { method: "PUT" }).then((response) => response.json());
  const client = new CdpClient(page.webSocketDebuggerUrl);
  await client.connect();
  await client.send("Page.enable");
  await client.send("Runtime.enable");
  await client.send("Page.addScriptToEvaluateOnNewDocument", { source: mockSource });
  await client.send("Page.navigate", { url: "http://127.0.0.1:4173" });
  await waitForApplication(client);

  const desktop = await capture(client, "desktop-1440x900", 1440, 900);
  const minimum = await capture(client, "minimum-960x640", 960, 640);
  const discover = await captureDiscover(client);
  const quality = await captureQualityMenu(client);
  const fullscreen = await captureFullscreen(client);
  console.log(JSON.stringify({ desktop, minimum, discover, quality, fullscreen }, null, 2));
  client.close();
} finally {
  chrome.kill("SIGTERM");
  server.close();
}
