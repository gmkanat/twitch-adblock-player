const TWITCH_GQL = "https://gql.twitch.tv/gql";
const TWITCH_WEB_CLIENT_ID = "kimne78kx3ncx6brgo4mv6wki5h1ko";

const CORS_HEADERS = {
  "Access-Control-Allow-Origin": "*",
  "Access-Control-Allow-Methods": "GET, POST, OPTIONS",
  "Access-Control-Allow-Headers": "Authorization, Content-Type",
};

export default {
  async fetch(request, env) {
    if (request.method === "OPTIONS") {
      return new Response(null, { status: 204, headers: CORS_HEADERS });
    }
    if (request.method === "GET") {
      return json({
        name: "Twitch 2K metadata relay",
        status: "running",
        placement: request.headers.get("cf-placement") || "local",
      });
    }
    if (request.method !== "POST") {
      return json({ error: "Method not allowed" }, 405);
    }
    if (!isAuthorized(request, env.RELAY_SECRET)) {
      return json({ error: "Unauthorized" }, 401);
    }

    try {
      const payload = await request.json();
      if (payload.type === "gql") {
        return relayPlaybackToken(payload);
      }
      if (payload.type === "usher") {
        return relayMasterPlaylist(payload);
      }
      return json({ error: "Unknown relay request" }, 400);
    } catch (error) {
      return json({ error: error instanceof Error ? error.message : "Relay failed" }, 500);
    }
  },
};

function isAuthorized(request, expectedSecret) {
  if (!expectedSecret) {
    return true;
  }
  return request.headers.get("Authorization") === `Bearer ${expectedSecret}`;
}

async function relayPlaybackToken(payload) {
  if (typeof payload.body !== "string" || typeof payload.auth !== "string") {
    return json({ error: "Invalid playback-token request" }, 400);
  }
  const body = JSON.parse(payload.body);
  if (
    body.operationName !== "PlaybackAccessToken"
    || typeof body.query !== "string"
    || !body.query.includes("streamPlaybackAccessToken")
  ) {
    return json({ error: "Unsupported GraphQL operation" }, 400);
  }
  if (!/^OAuth [A-Za-z0-9_-]{20,512}$/.test(payload.auth)) {
    return json({ error: "Invalid Twitch authorization" }, 400);
  }

  const headers = {
    "Authorization": payload.auth,
    "Client-ID": TWITCH_WEB_CLIENT_ID,
    "Content-Type": "application/json",
  };
  if (typeof payload.deviceId === "string" && /^[a-f0-9]{32}$/.test(payload.deviceId)) {
    headers["Device-ID"] = payload.deviceId;
  }

  const response = await fetch(TWITCH_GQL, {
    method: "POST",
    headers,
    body: payload.body,
  });
  return forward(response, "application/json");
}

async function relayMasterPlaylist(payload) {
  if (typeof payload.url !== "string") {
    return json({ error: "Invalid Usher request" }, 400);
  }
  const url = new URL(payload.url);
  if (
    url.protocol !== "https:"
    || url.hostname !== "usher.ttvnw.net"
    || !url.pathname.startsWith("/api/v2/channel/hls/")
    || !url.searchParams.has("token")
    || !url.searchParams.has("sig")
  ) {
    return json({ error: "Invalid Usher URL" }, 400);
  }

  const response = await fetch(url, {
    headers: { "User-Agent": "Mozilla/5.0" },
  });
  return forward(response, "application/vnd.apple.mpegurl");
}

async function forward(response, fallbackContentType) {
  return new Response(await response.arrayBuffer(), {
    status: response.status,
    headers: {
      ...CORS_HEADERS,
      "Content-Type": response.headers.get("Content-Type") || fallbackContentType,
      "Cache-Control": "no-store",
    },
  });
}

function json(body, status = 200) {
  return new Response(JSON.stringify(body), {
    status,
    headers: {
      ...CORS_HEADERS,
      "Content-Type": "application/json",
      "Cache-Control": "no-store",
    },
  });
}
