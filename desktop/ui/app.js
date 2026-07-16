const tauri = window.__TAURI__;

const elements = {
  loginView: document.querySelector("#login-view"),
  appView: document.querySelector("#app-view"),
  loginForm: document.querySelector("#login-form"),
  loginButton: document.querySelector("#login-button"),
  clientId: document.querySelector("#client-id"),
  loginError: document.querySelector("#login-error"),
  oauthPrompt: document.querySelector("#oauth-prompt"),
  oauthCode: document.querySelector("#oauth-code"),
  accountName: document.querySelector("#account-name"),
  logoutButton: document.querySelector("#logout-button"),
  refreshButton: document.querySelector("#refresh-button"),
  streamFilter: document.querySelector("#stream-filter"),
  streamCount: document.querySelector("#stream-count"),
  streamList: document.querySelector("#stream-list"),
  streamsEmpty: document.querySelector("#streams-empty"),
  currentChannel: document.querySelector("#current-channel"),
  currentMeta: document.querySelector("#current-meta"),
  liveIndicator: document.querySelector("#live-indicator"),
  quality: document.querySelector("#quality-select"),
  video: document.querySelector("#video"),
  playerPlaceholder: document.querySelector("#player-placeholder"),
  playerLoading: document.querySelector("#player-loading"),
  playbackStatus: document.querySelector("#playback-status"),
  playerError: document.querySelector("#player-error"),
  chatChannel: document.querySelector("#chat-channel"),
  chatStatus: document.querySelector("#chat-status"),
  chatLog: document.querySelector("#chat-log"),
  chatForm: document.querySelector("#chat-form"),
  chatInput: document.querySelector("#chat-input"),
  sendButton: document.querySelector("#send-button"),
  fatalError: document.querySelector("#fatal-error"),
};

const state = {
  streams: [],
  current: null,
  hls: null,
};

async function invoke(command, args = {}) {
  return tauri.core.invoke(command, args);
}

function showLogin() {
  elements.appView.hidden = true;
  elements.loginView.hidden = false;
  elements.clientId.focus();
}

async function showApplication(session) {
  elements.loginView.hidden = true;
  elements.appView.hidden = false;
  elements.accountName.textContent = session.login ? `@${session.login}` : "";
  await loadStreams();
}

async function loadStreams() {
  elements.refreshButton.disabled = true;
  elements.streamsEmpty.hidden = true;
  try {
    state.streams = await invoke("followed_streams");
    state.streams.sort((a, b) => b.viewer_count - a.viewer_count);
    renderStreams();
  } catch (error) {
    elements.streamList.replaceChildren();
    elements.streamsEmpty.textContent = String(error);
    elements.streamsEmpty.hidden = false;
  } finally {
    elements.refreshButton.disabled = false;
  }
}

function renderStreams() {
  const query = elements.streamFilter.value.trim().toLocaleLowerCase();
  const streams = state.streams.filter((stream) => {
    return !query
      || stream.user_name.toLocaleLowerCase().includes(query)
      || stream.game_name.toLocaleLowerCase().includes(query);
  });

  elements.streamList.replaceChildren(...streams.map(createStreamRow));
  elements.streamCount.textContent = String(streams.length);
  elements.streamsEmpty.textContent = query ? "No channels match this filter." : "No followed channels are live.";
  elements.streamsEmpty.hidden = streams.length !== 0;
}

function createStreamRow(stream) {
  const row = document.createElement("button");
  row.type = "button";
  row.className = "stream-row";
  row.dataset.login = stream.user_login;
  row.setAttribute("aria-label", `Watch ${stream.user_name}`);
  if (state.current?.user_login === stream.user_login) {
    row.classList.add("selected");
  }

  const image = document.createElement("img");
  image.className = "stream-thumb";
  image.alt = "";
  image.loading = "lazy";
  if (stream.thumbnail_url) {
    image.src = stream.thumbnail_url
      .replace("{width}", "320")
      .replace("{height}", "180");
  }

  const copy = document.createElement("span");
  copy.className = "stream-copy";
  copy.append(
    textSpan("stream-name", stream.user_name),
    textSpan("stream-game", stream.game_name || "Uncategorized"),
    textSpan("stream-viewers", `${formatViewers(stream.viewer_count)} viewers`),
  );

  row.append(image, copy);
  row.addEventListener("click", () => playStream(stream));
  return row;
}

function textSpan(className, value) {
  const span = document.createElement("span");
  span.className = className;
  span.textContent = value;
  return span;
}

function formatViewers(viewers) {
  return new Intl.NumberFormat(undefined, { notation: "compact", maximumFractionDigits: 1 }).format(viewers);
}

async function playStream(stream) {
  state.current = stream;
  renderStreams();
  clearChat();
  elements.currentChannel.textContent = stream.user_name;
  elements.currentMeta.textContent = stream.title || stream.game_name;
  elements.chatChannel.textContent = stream.user_name;
  elements.liveIndicator.hidden = false;
  elements.playerPlaceholder.hidden = true;
  elements.playerLoading.hidden = false;
  elements.playerError.textContent = "";
  elements.playbackStatus.textContent = "Resolving stream...";

  try {
    const playback = await invoke("play_channel", {
      channel: stream.user_login,
      quality: elements.quality.value,
    });
    attachPlaylist(playback.playlistUrl);
  } catch (error) {
    elements.playerLoading.hidden = true;
    elements.playerError.textContent = String(error);
    elements.playbackStatus.textContent = "Playback failed";
  }
}

function attachPlaylist(url) {
  if (state.hls) {
    state.hls.destroy();
    state.hls = null;
  }

  if (window.Hls?.isSupported()) {
    const hls = new Hls({
      enableWorker: true,
      lowLatencyMode: true,
      backBufferLength: 30,
    });
    state.hls = hls;
    hls.loadSource(url);
    hls.attachMedia(elements.video);
    hls.on(Hls.Events.MANIFEST_PARSED, beginPlayback);
    hls.on(Hls.Events.ERROR, (_, data) => handleHlsError(hls, data));
    return;
  }

  if (elements.video.canPlayType("application/vnd.apple.mpegurl")) {
    elements.video.src = url;
    elements.video.addEventListener("loadedmetadata", beginPlayback, { once: true });
    elements.video.addEventListener("error", () => {
      elements.playerLoading.hidden = true;
      elements.playerError.textContent = "The system WebView could not play this HLS stream.";
    }, { once: true });
    return;
  }

  elements.playerLoading.hidden = true;
  elements.playerError.textContent = "HLS playback is unavailable in this system WebView.";
}

function beginPlayback() {
  elements.playerLoading.hidden = true;
  elements.playbackStatus.textContent = "Live";
  elements.video.play().catch(() => {
    elements.playbackStatus.textContent = "Ready - press play";
  });
}

function handleHlsError(hls, data) {
  if (!data.fatal) {
    return;
  }
  if (data.type === Hls.ErrorTypes.NETWORK_ERROR) {
    elements.playbackStatus.textContent = "Reconnecting stream...";
    hls.startLoad();
  } else if (data.type === Hls.ErrorTypes.MEDIA_ERROR) {
    elements.playbackStatus.textContent = "Recovering playback...";
    hls.recoverMediaError();
  } else {
    elements.playerLoading.hidden = true;
    elements.playerError.textContent = data.details || "Playback stopped.";
    hls.destroy();
  }
}

function clearChat() {
  elements.chatLog.replaceChildren();
  elements.chatStatus.textContent = "Connecting";
  elements.chatStatus.classList.remove("online");
  elements.chatInput.disabled = true;
  elements.sendButton.disabled = true;
}

function applyChatEvent(event) {
  switch (event.type) {
    case "connected":
      elements.chatStatus.textContent = "Online";
      elements.chatStatus.classList.add("online");
      elements.chatInput.disabled = false;
      elements.sendButton.disabled = false;
      break;
    case "reconnecting":
      elements.chatStatus.textContent = "Reconnecting";
      elements.chatStatus.classList.remove("online");
      break;
    case "cleared":
      elements.chatLog.replaceChildren();
      break;
    case "system":
      appendSystemMessage(event.payload);
      break;
    case "message":
      appendChatMessage(event.payload);
      break;
  }
}

function appendChatMessage(message) {
  const line = document.createElement("p");
  line.className = "chat-line";
  const user = document.createElement("span");
  user.className = "chat-user";
  user.textContent = `${message.user}:`;
  if (/^#[0-9a-f]{6}$/i.test(message.color || "")) {
    user.style.color = message.color;
  }
  line.append(user, document.createTextNode(message.text));
  appendChatLine(line);
}

function appendSystemMessage(message) {
  const line = document.createElement("p");
  line.className = "chat-line chat-system";
  line.textContent = message;
  appendChatLine(line);
}

function appendChatLine(line) {
  elements.chatLog.append(line);
  while (elements.chatLog.childElementCount > 500) {
    elements.chatLog.firstElementChild.remove();
  }
  elements.chatLog.scrollTop = elements.chatLog.scrollHeight;
}

async function initializeEvents() {
  await tauri.event.listen("oauth-prompt", ({ payload }) => {
    elements.oauthCode.textContent = payload.user_code;
    elements.oauthPrompt.hidden = false;
  });
  await tauri.event.listen("playback-status", ({ payload }) => {
    elements.playbackStatus.textContent = payload;
  });
  await tauri.event.listen("chat-event", ({ payload }) => applyChatEvent(payload));
}

elements.loginForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  elements.loginError.textContent = "";
  elements.oauthPrompt.hidden = true;
  elements.loginButton.disabled = true;
  elements.loginButton.textContent = "Waiting for authorization...";
  try {
    const session = await invoke("login", { clientId: elements.clientId.value.trim() });
    await showApplication(session);
  } catch (error) {
    elements.loginError.textContent = String(error);
  } finally {
    elements.loginButton.disabled = false;
    elements.loginButton.textContent = "Connect account";
  }
});

elements.logoutButton.addEventListener("click", async () => {
  await invoke("logout");
  window.location.reload();
});

elements.refreshButton.addEventListener("click", loadStreams);
elements.streamFilter.addEventListener("input", renderStreams);
elements.quality.addEventListener("change", () => {
  if (state.current) {
    playStream(state.current);
  }
});

elements.chatForm.addEventListener("submit", async (event) => {
  event.preventDefault();
  const message = elements.chatInput.value.trim();
  if (!message) {
    return;
  }
  elements.chatInput.value = "";
  try {
    await invoke("send_chat", { message });
  } catch (error) {
    appendSystemMessage(String(error));
  }
});

async function initialize() {
  if (!tauri?.core || !tauri?.event) {
    elements.fatalError.textContent = "This interface must run inside the Twitch Adblock desktop application.";
    elements.fatalError.hidden = false;
    return;
  }

  try {
    await initializeEvents();
    const session = await invoke("session_status");
    if (session.authenticated) {
      await showApplication(session);
    } else {
      showLogin();
    }
  } catch (error) {
    elements.fatalError.textContent = String(error);
    elements.fatalError.hidden = false;
  }
}

initialize();
