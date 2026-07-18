import { spawn } from "node:child_process";
import { createServer } from "node:http";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { basename, extname, join } from "node:path";
import { fileURLToPath } from "node:url";

const desktopDir = fileURLToPath(new URL("..", import.meta.url));
const rootDir = fileURLToPath(new URL("../..", import.meta.url));
const uiDir = join(desktopDir, "ui");
const fixtureDir = join(desktopDir, "tests", "fixtures", "readme");
const outputPath = join(rootDir, "docs", "images", "app-overview.webp");
const profileDir = join(rootDir, "target", "readme-capture", `chrome-profile-${process.pid}`);
mkdirSync(join(rootDir, "docs", "images"), { recursive: true });
mkdirSync(profileDir, { recursive: true });

const mimeTypes = {
  ".css": "text/css",
  ".html": "text/html",
  ".jpg": "image/jpeg",
  ".js": "text/javascript",
  ".png": "image/png",
};

function startServer() {
  const server = createServer((request, response) => {
    const url = new URL(request.url, "http://127.0.0.1");
    const fixture = url.pathname.startsWith("/fixtures/");
    const relativePath = url.pathname === "/" ? "index.html" : url.pathname.slice(1);
    const filePath = fixture
      ? join(fixtureDir, basename(relativePath))
      : join(uiDir, relativePath);
    try {
      const body = readFileSync(filePath);
      response.writeHead(200, {
        "Cache-Control": "no-store",
        "Content-Type": mimeTypes[extname(filePath)] || "application/octet-stream",
      });
      response.end(body);
    } catch {
      response.writeHead(404);
      response.end("Not found");
    }
  });
  return new Promise((resolve) => {
    server.listen(0, "127.0.0.1", () => resolve(server));
  });
}

async function reservePort() {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

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

async function waitForDebugger(port) {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    try {
      const response = await fetch(`http://127.0.0.1:${port}/json/version`);
      if (response.ok) return;
    } catch {}
    await sleep(50);
  }
  throw new Error("Chrome DevTools did not start");
}

async function waitForApplication(client) {
  for (let attempt = 0; attempt < 120; attempt += 1) {
    const result = await client.send("Runtime.evaluate", {
      expression: "!document.querySelector('#app-view')?.hidden && document.querySelectorAll('.stream-row').length === 4",
      returnByValue: true,
    });
    if (result.result.value) return;
    await sleep(50);
  }
  throw new Error("Documentation fixture did not render");
}

const server = await startServer();
const { port: serverPort } = server.address();
const baseUrl = `http://127.0.0.1:${serverPort}`;
const debuggerPort = await reservePort();
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
  throw new Error("Chrome was not found; set CHROME_BIN to create the README screenshot");
}

const streams = [
  {
    user_login: "orbitallab",
    user_name: "OrbitalLab",
    game_name: "Science & Technology",
    title: "Earth observation from low orbit",
    thumbnail_url: `${baseUrl}/fixtures/earth-orbit.jpg`,
    viewer_count: 18400,
    started_at: "2026-07-18T04:00:00Z",
  },
  {
    user_login: "nightshift",
    user_name: "NightShift",
    game_name: "Travel & Outdoors",
    title: "City lights above the Yucatan",
    thumbnail_url: `${baseUrl}/fixtures/city-lights.jpg`,
    viewer_count: 9200,
    started_at: "2026-07-18T05:00:00Z",
  },
  {
    user_login: "atmoswatch",
    user_name: "AtmosWatch",
    game_name: "Science & Technology",
    title: "Noctilucent clouds at sunrise",
    thumbnail_url: `${baseUrl}/fixtures/atmosphere.jpg`,
    viewer_count: 6100,
    started_at: "2026-07-18T05:30:00Z",
  },
  {
    user_login: "missioncontrol",
    user_name: "MissionControl",
    game_name: "Science & Technology",
    title: "Live operations desk",
    thumbnail_url: `${baseUrl}/fixtures/mission-control.jpg`,
    viewer_count: 3400,
    started_at: "2026-07-18T06:00:00Z",
  },
];

const mockSource = `
  const mockStreams = ${JSON.stringify(streams)};
  window.__TAURI__ = {
    core: {
      invoke: async (command) => {
        if (command === "session_status") return { authenticated: true, login: "demo_viewer" };
        if (command === "followed_streams") return mockStreams;
        if (command === "top_categories") return [];
        return null;
      }
    },
    event: { listen: async () => () => {} }
  };
`;

const chrome = spawn(chromePath, [
  "--headless=new",
  "--disable-gpu",
  "--no-sandbox",
  "--disable-dev-shm-usage",
  "--hide-scrollbars",
  `--remote-debugging-port=${debuggerPort}`,
  `--user-data-dir=${profileDir}`,
  "about:blank",
], { stdio: "ignore" });

let client;
try {
  await waitForDebugger(debuggerPort);
  const page = await fetch(`http://127.0.0.1:${debuggerPort}/json/new?about:blank`, {
    method: "PUT",
  }).then((response) => response.json());
  client = new CdpClient(page.webSocketDebuggerUrl);
  await client.connect();
  await client.send("Page.enable");
  await client.send("Runtime.enable");
  await client.send("Page.addScriptToEvaluateOnNewDocument", { source: mockSource });
  await client.send("Emulation.setDeviceMetricsOverride", {
    width: 1600,
    height: 900,
    deviceScaleFactor: 1,
    mobile: false,
  });
  await client.send("Page.navigate", { url: baseUrl });
  await waitForApplication(client);

  const messages = [
    ["starboard", "That horizon is unreal.", "#ff75e6"],
    ["orbitfan", "The cloud detail is so clear today.", "#5cafff"],
    ["signalcheck", "Telemetry looks steady.", "#00c7ac"],
    ["northwindow", "Sunrise should be coming up soon.", "#ffb300"],
    ["lowearth", "Perfect pass over the ocean.", "#8d7aff"],
    ["deepfield", "This is why I keep this stream open.", "#ff6b6b"],
    ["nightcycle", "The solar array just caught the light.", "#38bdf8"],
    ["flightdesk", "Connection is stable at 1080p60.", "#22c55e"],
    ["coastline", "You can see the weather front below.", "#f97316"],
    ["stargazer", "What a view from up there.", "#d946ef"],
    ["groundtrack", "Crossing into daylight now.", "#06b6d4"],
    ["orbitfan", "The reflection on the water is beautiful.", "#5cafff"],
    ["signalcheck", "No dropped frames here.", "#00c7ac"],
    ["northwindow", "There is the blue edge of the atmosphere.", "#ffb300"],
    ["deepfield", "This pass has been incredible.", "#ff6b6b"],
    ["flightdesk", "Next ground station handoff in two minutes.", "#22c55e"],
    ["stargazer", "Clear skies all the way to the horizon.", "#d946ef"],
  ];
  const setup = await client.send("Runtime.evaluate", {
    expression: `(async () => {
      state.current = state.streams[0];
      state.activeQuality = '1080p60 (source)';
      renderStreams();
      elements.currentChannel.textContent = 'OrbitalLab';
      elements.currentMeta.textContent = 'Earth observation from low orbit';
      elements.chatChannel.textContent = 'OrbitalLab';
      elements.liveIndicator.hidden = false;
      elements.playerPlaceholder.hidden = true;
      elements.playerLoading.hidden = true;
      elements.playerControls.hidden = false;
      elements.video.style.display = 'none';
      elements.playbackStatus.textContent = 'Live - 1080p60';

      const frame = document.createElement('img');
      frame.id = 'documentation-stream-frame';
      frame.alt = '';
      frame.src = ${JSON.stringify(`${baseUrl}/fixtures/earth-orbit.jpg`)};
      frame.style.cssText = 'position:absolute;inset:0;z-index:1;width:100%;height:100%;object-fit:contain;background:#000';
      elements.videoStage.prepend(frame);
      await frame.decode();

      updateAvailableQualities(['1080p60 (source)', '720p60', '480p', 'audio_only']);
      setButtonIcon(elements.playButton, 'pause', 'Pause');
      setLiveAppearance(true);
      elements.videoStage.classList.add('controls-visible');
      applyChatEvent({ type: 'connected' });
      const messages = ${JSON.stringify(messages)};
      messages.forEach(([user, text, color], index) => applyChatEvent({
        type: 'message',
        payload: { id: String(index), user_id: String(index), user, text, color },
      }));

      const rect = (selector) => document.querySelector(selector).getBoundingClientRect();
      const streamsPane = rect('.streams-pane');
      const playerPane = rect('.player-pane');
      const chatPane = rect('.chat-pane');
      return {
        account: elements.accountName.textContent,
        chatLines: elements.chatLog.childElementCount,
        chatOnline: elements.chatStatus.classList.contains('online'),
        frameReady: frame.complete && frame.naturalWidth > 0,
        selectedChannels: document.querySelectorAll('.stream-row.selected').length,
        streams: document.querySelectorAll('.stream-row').length,
        panesOrdered: streamsPane.right <= playerPane.left && playerPane.right <= chatPane.left,
        viewport: [innerWidth, innerHeight],
        documentSize: [document.documentElement.scrollWidth, document.documentElement.scrollHeight],
      };
    })()`,
    awaitPromise: true,
    returnByValue: true,
  });
  const metrics = setup.result.value;
  if (
    metrics.account !== "@demo_viewer"
    || metrics.chatLines !== messages.length
    || !metrics.chatOnline
    || !metrics.frameReady
    || metrics.selectedChannels !== 1
    || metrics.streams !== streams.length
    || !metrics.panesOrdered
    || metrics.documentSize[0] !== metrics.viewport[0]
    || metrics.documentSize[1] !== metrics.viewport[1]
  ) {
    throw new Error(`README screenshot validation failed: ${JSON.stringify(metrics)}`);
  }

  await sleep(250);
  const screenshot = await client.send("Page.captureScreenshot", {
    format: "webp",
    quality: 92,
    fromSurface: true,
  });
  writeFileSync(outputPath, Buffer.from(screenshot.data, "base64"));
  console.log(JSON.stringify({ outputPath, metrics }, null, 2));
} finally {
  client?.close();
  chrome.kill("SIGTERM");
  await new Promise((resolve) => server.close(resolve));
  await sleep(100);
  rmSync(profileDir, { force: true, recursive: true });
}
