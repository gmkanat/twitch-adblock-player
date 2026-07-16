const tauri = window.__TAURI__;
const QUALITY_LABELS = {
  best: "Best",
  source: "Source",
  "720p60": "720p60",
  "480p": "480p",
  audio_only: "Audio only",
};

function loadQualityPreference() {
  try {
    const quality = window.localStorage.getItem("playback-quality");
    return quality && quality.length <= 64 ? quality : "best";
  } catch {
    return "best";
  }
}

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
  followingTab: document.querySelector("#following-tab"),
  discoverTab: document.querySelector("#discover-tab"),
  streamsEyebrow: document.querySelector("#streams-eyebrow"),
  streamsTitle: document.querySelector("#streams-title"),
  streamSearchForm: document.querySelector("#stream-search-form"),
  streamFilter: document.querySelector("#stream-filter"),
  streamSearchButton: document.querySelector("#stream-search-button"),
  streamCount: document.querySelector("#stream-count"),
  discoverTools: document.querySelector("#discover-tools"),
  category: document.querySelector("#category-select"),
  streamList: document.querySelector("#stream-list"),
  streamsEmpty: document.querySelector("#streams-empty"),
  currentChannel: document.querySelector("#current-channel"),
  currentMeta: document.querySelector("#current-meta"),
  liveIndicator: document.querySelector("#live-indicator"),
  videoStage: document.querySelector(".video-stage"),
  video: document.querySelector("#video"),
  playerPlaceholder: document.querySelector("#player-placeholder"),
  playerLoading: document.querySelector("#player-loading"),
  playerControls: document.querySelector("#player-controls"),
  playButton: document.querySelector("#play-button"),
  muteButton: document.querySelector("#mute-button"),
  volume: document.querySelector("#volume-control"),
  qualityButton: document.querySelector("#quality-button"),
  qualityMenu: document.querySelector("#quality-menu"),
  qualityOptions: [...document.querySelectorAll(".quality-option")],
  fullscreenButton: document.querySelector("#fullscreen-button"),
  fullscreenChatButton: document.querySelector("#fullscreen-chat-button"),
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
  followingStreams: [],
  current: null,
  streamView: "following",
  categoriesLoaded: false,
  streamRequestId: 0,
  searchTimer: null,
  playRequestId: 0,
  hls: null,
  quality: loadQualityPreference(),
  availableQualities: ["best"],
  qualityMenuOpen: false,
  fullscreen: false,
  fullscreenChatOpen: false,
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
  await loadFollowingStreams();
}

function setStreamLoading(message) {
  elements.streamList.replaceChildren();
  elements.streamCount.textContent = "";
  elements.streamsEmpty.textContent = message;
  elements.streamsEmpty.hidden = false;
}

async function loadFollowingStreams() {
  elements.refreshButton.disabled = true;
  if (state.streamView === "following") {
    setStreamLoading("Loading followed channels...");
  }
  try {
    state.followingStreams = await invoke("followed_streams");
    state.followingStreams.sort((a, b) => b.viewer_count - a.viewer_count);
    if (state.streamView === "following") {
      state.streams = state.followingStreams;
      renderStreams();
    }
  } catch (error) {
    if (state.streamView === "following") {
      setStreamLoading(String(error));
    }
  } finally {
    elements.refreshButton.disabled = false;
  }
}

async function loadCategories() {
  if (state.categoriesLoaded) {
    return;
  }
  const categories = await invoke("top_categories");
  const options = categories.map((category) => {
    const option = document.createElement("option");
    option.value = category.id;
    option.textContent = category.name;
    return option;
  });
  elements.category.append(...options);
  state.categoriesLoaded = true;
}

async function loadDiscoverStreams(command = "popular_streams", args = {}) {
  const requestId = ++state.streamRequestId;
  elements.refreshButton.disabled = true;
  setStreamLoading("Loading live channels...");
  try {
    const streams = await invoke(command, args);
    if (requestId !== state.streamRequestId || state.streamView !== "discover") {
      return;
    }
    streams.sort((a, b) => b.viewer_count - a.viewer_count);
    state.streams = streams;
    renderStreams();
  } catch (error) {
    if (requestId === state.streamRequestId && state.streamView === "discover") {
      setStreamLoading(String(error));
    }
  } finally {
    if (requestId === state.streamRequestId) {
      elements.refreshButton.disabled = false;
    }
  }
}

async function setStreamView(view) {
  if (view === state.streamView) {
    return;
  }
  state.streamView = view;
  clearTimeout(state.searchTimer);
  elements.streamFilter.value = "";
  const following = view === "following";
  elements.followingTab.classList.toggle("selected", following);
  elements.followingTab.setAttribute("aria-selected", String(following));
  elements.discoverTab.classList.toggle("selected", !following);
  elements.discoverTab.setAttribute("aria-selected", String(!following));
  elements.discoverTools.hidden = following;
  elements.streamSearchButton.hidden = following;
  elements.streamFilter.placeholder = following ? "Filter followed channels" : "Search live channels";
  elements.streamsEyebrow.textContent = following ? "Following" : "Browse";
  elements.streamsTitle.textContent = following ? "Live channels" : "Recommended live";

  if (following) {
    state.streamRequestId += 1;
    state.streams = state.followingStreams;
    renderStreams();
    return;
  }

  await Promise.all([
    loadCategories().catch(() => {}),
    loadDiscoverStreams(),
  ]);
}

async function refreshChannels() {
  if (state.streamView === "following") {
    return loadFollowingStreams();
  }
  await loadCategories().catch(() => {});
  const query = elements.streamFilter.value.trim();
  if (query.length >= 2) {
    elements.streamsTitle.textContent = "Search results";
    return loadDiscoverStreams("search_streams", { query });
  }
  if (elements.category.value) {
    elements.streamsTitle.textContent = elements.category.selectedOptions[0]?.textContent || "Category";
    return loadDiscoverStreams("category_streams", { gameId: elements.category.value });
  }
  elements.streamsTitle.textContent = "Recommended live";
  return loadDiscoverStreams();
}

function searchDiscoverChannels() {
  const query = elements.streamFilter.value.trim();
  if (query.length < 2) {
    return refreshChannels();
  }
  elements.category.value = "";
  elements.streamsTitle.textContent = "Search results";
  return loadDiscoverStreams("search_streams", { query });
}

function handleStreamFilterInput() {
  if (state.streamView === "following") {
    renderStreams();
    return;
  }
  clearTimeout(state.searchTimer);
  const query = elements.streamFilter.value.trim();
  if (query.length === 1) {
    return;
  }
  state.searchTimer = setTimeout(() => searchDiscoverChannels(), 350);
}

function renderStreams() {
  const query = state.streamView === "following"
    ? elements.streamFilter.value.trim().toLocaleLowerCase()
    : "";
  const streams = state.streams.filter((stream) => {
    return !query
      || stream.user_name.toLocaleLowerCase().includes(query)
      || stream.game_name.toLocaleLowerCase().includes(query);
  });

  elements.streamList.replaceChildren(...streams.map(createStreamRow));
  elements.streamCount.textContent = String(streams.length);
  if (state.streamView === "following") {
    elements.streamsEmpty.textContent = query ? "No channels match this filter." : "No followed channels are live.";
  } else if (elements.streamFilter.value.trim()) {
    elements.streamsEmpty.textContent = "No live channels found.";
  } else if (elements.category.value) {
    elements.streamsEmpty.textContent = "No live channels in this category.";
  } else {
    elements.streamsEmpty.textContent = "No popular live channels available.";
  }
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

async function playStream(stream, { switchChat = true } = {}) {
  const requestId = ++state.playRequestId;
  state.current = stream;
  renderStreams();
  if (switchChat) {
    clearChat();
  }
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
      quality: state.quality,
      switchChat,
    });
    if (requestId !== state.playRequestId) {
      return;
    }
    updateAvailableQualities(playback.qualities || []);
    attachPlaylist(playback.playlistUrl);
  } catch (error) {
    if (requestId !== state.playRequestId) {
      return;
    }
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
  setButtonIcon(elements.qualityButton, "settings", `Quality: ${qualityLabel(state.quality)}`);
  elements.qualityButton.setAttribute("aria-expanded", String(state.qualityMenuOpen));
  const chatLabel = state.fullscreenChatOpen ? "Hide chat" : "Show chat";
  setButtonIcon(
    elements.fullscreenChatButton,
    state.fullscreenChatOpen ? "panel-right-close" : "message-square",
    chatLabel,
  );
  elements.fullscreenChatButton.setAttribute("aria-pressed", String(state.fullscreenChatOpen));
  updateLiveState();
}

function qualityLabel(quality) {
  if (QUALITY_LABELS[quality]) {
    return QUALITY_LABELS[quality];
  }
  return quality.replaceAll("_", " ");
}

function qualityDetail(quality) {
  const normalized = quality.toLocaleLowerCase();
  if (normalized.includes("source") || normalized === "chunked") {
    return "Original quality";
  }
  if (normalized.includes("audio")) {
    return "Audio only";
  }
  return "Video quality";
}

function bindQualityOptions() {
  elements.qualityOptions.forEach((option) => {
    option.addEventListener("click", () => selectQuality(option.dataset.quality));
  });
}

function updateAvailableQualities(qualities) {
  const unique = [];
  const seen = new Set();
  for (const quality of qualities) {
    if (typeof quality !== "string" || !quality.trim()) {
      continue;
    }
    const value = quality.trim();
    const key = value.toLocaleLowerCase();
    if (!seen.has(key)) {
      seen.add(key);
      unique.push(value);
    }
  }
  state.availableQualities = ["best", ...unique];
  if (state.quality !== "best") {
    const match = unique.find(
      (quality) => quality.toLocaleLowerCase() === state.quality.toLocaleLowerCase(),
    );
    state.quality = match || "best";
    try {
      window.localStorage.setItem("playback-quality", state.quality);
    } catch {}
  }

  elements.qualityMenu.querySelectorAll(".quality-option").forEach((option) => option.remove());
  const options = state.availableQualities.map((quality) => {
    const option = document.createElement("button");
    option.className = "quality-option";
    option.type = "button";
    option.role = "menuitemradio";
    option.dataset.quality = quality;
    const label = document.createElement("span");
    label.textContent = qualityLabel(quality);
    const detail = document.createElement("small");
    detail.textContent = quality === "best" ? "Highest available" : qualityDetail(quality);
    option.append(label, detail);
    elements.qualityMenu.append(option);
    return option;
  });
  elements.qualityOptions = options;
  bindQualityOptions();
  setQualityMenu(false);
  updatePlaybackButtons();
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
  if (!elements.video.paused && !state.qualityMenuOpen) {
    state.controlsTimer = setTimeout(() => {
      elements.videoStage.classList.remove("controls-visible");
    }, 2400);
  }
}

function setQualityMenu(open) {
  state.qualityMenuOpen = open;
  elements.qualityMenu.hidden = !open;
  elements.qualityButton.setAttribute("aria-expanded", String(open));
  elements.qualityOptions.forEach((option) => {
    const selected = option.dataset.quality === state.quality;
    option.classList.toggle("selected", selected);
    option.setAttribute("aria-checked", String(selected));
  });
  showPlayerControls();
}

function selectQuality(quality) {
  if (!state.availableQualities.includes(quality)) {
    return;
  }
  const changed = quality !== state.quality;
  state.quality = quality;
  try {
    window.localStorage.setItem("playback-quality", quality);
  } catch {}
  setQualityMenu(false);
  updatePlaybackButtons();
  if (changed && state.current) {
    playStream(state.current, { switchChat: false });
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
    if (!fullscreen) {
      state.fullscreenChatOpen = false;
    }
    document.body.classList.toggle("player-fullscreen", fullscreen);
    document.body.classList.toggle("fullscreen-chat-open", state.fullscreenChatOpen);
    updatePlaybackButtons();
    showPlayerControls();
  } catch (error) {
    elements.playerError.textContent = `Fullscreen failed: ${error}`;
  }
}

function toggleFullscreenChat() {
  if (!state.fullscreen) {
    return;
  }
  state.fullscreenChatOpen = !state.fullscreenChatOpen;
  document.body.classList.toggle("fullscreen-chat-open", state.fullscreenChatOpen);
  updatePlaybackButtons();
  showPlayerControls();
  if (state.fullscreenChatOpen) {
    elements.chatLog.scrollTop = elements.chatLog.scrollHeight;
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
    case "reset":
      elements.chatLog.replaceChildren();
      break;
    case "cleared":
      elements.chatLog.replaceChildren();
      appendSystemMessage("Chat was cleared by a moderator.");
      break;
    case "messageDeleted":
      removeChatLines("messageId", event.payload);
      break;
    case "userCleared":
      removeChatLines("userId", event.payload);
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
  if (message.id) {
    line.dataset.messageId = message.id;
  }
  if (message.user_id) {
    line.dataset.userId = message.user_id;
  }
  const user = document.createElement("span");
  user.className = "chat-user";
  user.textContent = `${message.user}:`;
  if (/^#[0-9a-f]{6}$/i.test(message.color || "")) {
    user.style.color = message.color;
  }
  line.append(user, document.createTextNode(message.text));
  appendChatLine(line);
}

function removeChatLines(key, value) {
  for (const line of [...elements.chatLog.children]) {
    if (line.dataset[key] === value) {
      line.remove();
    }
  }
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

elements.followingTab.addEventListener("click", () => setStreamView("following"));
elements.discoverTab.addEventListener("click", () => setStreamView("discover"));
elements.refreshButton.addEventListener("click", refreshChannels);
elements.streamSearchForm.addEventListener("submit", (event) => {
  event.preventDefault();
  if (state.streamView === "discover") {
    searchDiscoverChannels();
  }
});
elements.streamFilter.addEventListener("input", handleStreamFilterInput);
elements.category.addEventListener("change", () => {
  elements.streamFilter.value = "";
  refreshChannels();
});

elements.playButton.addEventListener("click", togglePlayback);
elements.muteButton.addEventListener("click", toggleMute);
elements.liveIndicator.addEventListener("click", jumpToLive);
elements.liveEdgeButton.addEventListener("click", jumpToLive);
elements.qualityButton.addEventListener("click", () => setQualityMenu(!state.qualityMenuOpen));
bindQualityOptions();
elements.fullscreenChatButton.addEventListener("click", toggleFullscreenChat);
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
  if (!elements.video.paused && !state.qualityMenuOpen) {
    elements.videoStage.classList.remove("controls-visible");
  }
});

document.addEventListener("keydown", (event) => {
  if (event.key === "Escape" && state.qualityMenuOpen) {
    event.preventDefault();
    setQualityMenu(false);
  } else if (event.key === "Escape" && state.fullscreen) {
    event.preventDefault();
    setFullscreen(false);
  } else if (event.code === "Space" && document.activeElement === elements.video) {
    event.preventDefault();
    togglePlayback();
  }
});
document.addEventListener("pointerdown", (event) => {
  if (
    state.qualityMenuOpen
    && !elements.qualityMenu.contains(event.target)
    && !elements.qualityButton.contains(event.target)
  ) {
    setQualityMenu(false);
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
