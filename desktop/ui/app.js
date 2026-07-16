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
  videoStage: document.querySelector(".video-stage"),
  video: document.querySelector("#video"),
  playerPlaceholder: document.querySelector("#player-placeholder"),
  playerLoading: document.querySelector("#player-loading"),
  playerControls: document.querySelector("#player-controls"),
  playButton: document.querySelector("#play-button"),
  muteButton: document.querySelector("#mute-button"),
  volume: document.querySelector("#volume-control"),
  fullscreenButton: document.querySelector("#fullscreen-button"),
  liveEdgeButton: document.querySelector("#live-edge-button"),
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
  fullscreen: false,
  controlsTimer: null,
  clickTimer: null,
  volumeBeforeMute: 1,
  buffering: false,
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
  state.buffering = false;
  setLiveAppearance(false);
  elements.playerPlaceholder.hidden = true;
  elements.playerLoading.hidden = false;
  elements.playerControls.hidden = false;
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
  showPlayerControls();
  updateLiveState();
  elements.video.play().catch(() => {
    elements.playbackStatus.textContent = "Ready - press play";
    updatePlaybackButtons();
  });
}

function setButtonIcon(button, icon, label) {
  button.replaceChildren();
  const element = document.createElement("i");
  element.dataset.lucide = icon;
  element.setAttribute("aria-hidden", "true");
  button.append(element);
  button.title = label;
  button.setAttribute("aria-label", label);
  window.lucide?.createIcons();
}

function updatePlaybackButtons() {
  setButtonIcon(
    elements.playButton,
    elements.video.paused ? "play" : "pause",
    elements.video.paused ? "Play" : "Pause",
  );
  const muted = elements.video.muted || elements.video.volume === 0;
  setButtonIcon(elements.muteButton, muted ? "volume-x" : "volume-2", muted ? "Unmute" : "Mute");
  elements.volume.value = muted ? "0" : String(elements.video.volume);
  setButtonIcon(
    elements.fullscreenButton,
    state.fullscreen ? "minimize" : "maximize",
    state.fullscreen ? "Exit fullscreen" : "Enter fullscreen",
  );
  updateLiveState();
}

function setLiveAppearance(isLive) {
  for (const button of [elements.liveIndicator, elements.liveEdgeButton]) {
    button.classList.toggle("is-live", isLive);
    const label = isLive ? "At live edge" : "Jump to live";
    button.title = label;
    button.setAttribute("aria-label", label);
  }
}

function updateLiveState() {
  const hlsLivePosition = state.hls?.liveSyncPosition;
  const ranges = elements.video.seekable;
  const seekableEnd = ranges.length > 0 ? ranges.end(ranges.length - 1) : Number.NaN;
  const hasHlsTarget = Number.isFinite(hlsLivePosition);
  const target = hasHlsTarget ? hlsLivePosition : seekableEnd;
  const tolerance = hasHlsTarget ? 3 : 8;
  const lag = Number.isFinite(target) ? target - elements.video.currentTime : Number.POSITIVE_INFINITY;
  const isLive = Boolean(state.current)
    && !elements.video.paused
    && !elements.video.ended
    && !elements.video.seeking
    && !state.buffering
    && lag <= tolerance;
  setLiveAppearance(isLive);
}

function showPlayerControls() {
  if (elements.playerControls.hidden) {
    return;
  }
  elements.videoStage.classList.add("controls-visible");
  clearTimeout(state.controlsTimer);
  if (!elements.video.paused) {
    state.controlsTimer = setTimeout(() => {
      elements.videoStage.classList.remove("controls-visible");
    }, 2400);
  }
}

function togglePlayback() {
  if (!state.current) {
    return;
  }
  if (elements.video.paused) {
    elements.video.play().catch((error) => {
      elements.playerError.textContent = String(error);
    });
  } else {
    elements.video.pause();
  }
}

function toggleMute() {
  if (elements.video.muted || elements.video.volume === 0) {
    elements.video.muted = false;
    elements.video.volume = state.volumeBeforeMute || 1;
  } else {
    state.volumeBeforeMute = elements.video.volume;
    elements.video.muted = true;
  }
}

function jumpToLive() {
  if (!state.current) {
    return;
  }
  const hlsLivePosition = state.hls?.liveSyncPosition;
  const ranges = elements.video.seekable;
  const seekableEnd = ranges.length > 0 ? ranges.end(ranges.length - 1) : Number.NaN;
  const target = Number.isFinite(hlsLivePosition) ? hlsLivePosition : seekableEnd;
  if (Number.isFinite(target)) {
    elements.video.currentTime = Math.max(0, target - 0.1);
  }
  elements.video.play().catch(() => {});
  elements.playbackStatus.textContent = "Live";
  updateLiveState();
  setTimeout(updateLiveState, 250);
  showPlayerControls();
}

async function setFullscreen(fullscreen) {
  try {
    await invoke("set_fullscreen", { fullscreen });
    state.fullscreen = fullscreen;
    document.body.classList.toggle("player-fullscreen", fullscreen);
    updatePlaybackButtons();
    showPlayerControls();
  } catch (error) {
    elements.playerError.textContent = `Fullscreen failed: ${error}`;
  }
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
      if (elements.chatStatus.textContent === "Connecting") {
        elements.chatStatus.textContent = "Unavailable";
        elements.chatStatus.classList.remove("online");
      }
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

elements.playButton.addEventListener("click", togglePlayback);
elements.muteButton.addEventListener("click", toggleMute);
elements.liveIndicator.addEventListener("click", jumpToLive);
elements.liveEdgeButton.addEventListener("click", jumpToLive);
elements.fullscreenButton.addEventListener("click", () => setFullscreen(!state.fullscreen));
elements.volume.addEventListener("input", () => {
  const volume = Number(elements.volume.value);
  elements.video.muted = false;
  elements.video.volume = volume;
  if (volume > 0) {
    state.volumeBeforeMute = volume;
  }
});
elements.video.addEventListener("click", (event) => {
  if (event.detail !== 1) {
    return;
  }
  clearTimeout(state.clickTimer);
  state.clickTimer = setTimeout(togglePlayback, 220);
});
elements.video.addEventListener("dblclick", () => {
  clearTimeout(state.clickTimer);
  setFullscreen(!state.fullscreen);
});
elements.video.addEventListener("play", () => {
  updatePlaybackButtons();
  showPlayerControls();
});
elements.video.addEventListener("pause", () => {
  updatePlaybackButtons();
  showPlayerControls();
});
elements.video.addEventListener("volumechange", updatePlaybackButtons);
for (const eventName of [
  "timeupdate",
  "progress",
  "seeking",
  "seeked",
  "ended",
  "emptied",
  "loadedmetadata",
]) {
  elements.video.addEventListener(eventName, updateLiveState);
}
elements.video.addEventListener("waiting", () => {
  state.buffering = true;
  updateLiveState();
});
elements.video.addEventListener("playing", () => {
  state.buffering = false;
  updateLiveState();
});
elements.videoStage.addEventListener("pointermove", showPlayerControls);
elements.videoStage.addEventListener("pointerleave", () => {
  if (!elements.video.paused) {
    elements.videoStage.classList.remove("controls-visible");
  }
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && state.fullscreen) {
    event.preventDefault();
    setFullscreen(false);
  } else if (event.code === "Space" && document.activeElement === elements.video) {
    event.preventDefault();
    togglePlayback();
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
    window.lucide?.createIcons();
    updatePlaybackButtons();
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
