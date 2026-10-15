// SPDX-License-Identifier: GPL-3.0-or-later
//
// XBTune remote-tuning relay (docs/REMOTE.md 4.1). Pairs two WebSockets,
// the bike owner's ("host") and the tuner's ("guest"), by a room number
// and forwards binary frames between them. The frames are end-to-end
// encrypted by the apps (SPAKE2 + ChaCha20-Poly1305); this relay never
// sees the 6 secret digits, the keys or any content. Nothing is stored
// but the room's tickets, and only while the room exists.
//
// Routes (WebSocket upgrades):
//   /v1/open                          host asks for a room
//   /v1/join?room=N                   guest joins (once per room)
//   /v1/rejoin?room=N&role=host|guest after a dropped connection, with the
//                                     ticket in the x-xbtune-ticket header
// Refusals are HTTP statuses: 404 no room, 409 room full / taken,
// 403 bad ticket, 429 rate limit, 503 no free room.
// Text frames from the relay: opened, joined, peer {here|gone}.
// Text frame from an app: {"t":"close"} ends the room for both.
//
// Must behave like crates/xbtune-remote/src/local_relay.rs (the tests use
// that one; tools/test_remote_relay.sh runs them against this Worker).

const ROOM_RE = /^[1-9][0-9]{0,2}$/;
const MAX_FRAME = (1 << 20) + 1024;
const EMPTY_ROOM_MS = 30 * 60 * 1000; // both sides gone this long: room closes
const MAX_ROOM_MS = 12 * 60 * 60 * 1000; // a room lives at most this long
const CHECK_MS = 5 * 60 * 1000;

function text(status, body) {
  return new Response(body + "\n", { status, headers: { "content-type": "text/plain" } });
}

function randomTicket() {
  const b = new Uint8Array(16);
  crypto.getRandomValues(b);
  return [...b].map((x) => x.toString(16).padStart(2, "0")).join("");
}

function randomRoom() {
  const b = new Uint32Array(1);
  crypto.getRandomValues(b);
  return String(1 + (b[0] % 999));
}

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname === "/" || url.pathname === "/v1") {
      return text(200, "XBTune remote relay");
    }
    const action = { "/v1/open": "open", "/v1/join": "join", "/v1/rejoin": "rejoin" }[url.pathname];
    if (!action) return text(404, "not found");
    if ((request.headers.get("Upgrade") || "").toLowerCase() !== "websocket") {
      return text(400, "expected a WebSocket upgrade");
    }
    if (action !== "rejoin" && env.LIMITER) {
      const ip = request.headers.get("CF-Connecting-IP") || "unknown";
      const { success } = await env.LIMITER.limit({ key: ip });
      if (!success) return text(429, "rate");
    }
    if (action === "open") {
      // Random free room: a taken one answers 409, try another.
      for (let i = 0; i < 25; i++) {
        const room = randomRoom();
        const stub = env.ROOMS.get(env.ROOMS.idFromName(room));
        const resp = await stub.fetch(new Request(`https://room/open?room=${room}`, request));
        if (resp.status !== 409) return resp;
      }
      return text(503, "no free rooms");
    }
    const room = url.searchParams.get("room") || "";
    if (!ROOM_RE.test(room)) return text(404, "no-room");
    const stub = env.ROOMS.get(env.ROOMS.idFromName(room));
    const q = new URLSearchParams({ room, role: url.searchParams.get("role") || "" });
    return stub.fetch(new Request(`https://room/${action}?${q}`, request));
  },
};

export class Room {
  constructor(ctx, env) {
    this.ctx = ctx;
  }

  async state() {
    return (await this.ctx.storage.get("state")) || null;
  }

  sockets(role, except) {
    return this.ctx.getWebSockets(role).filter((w) => w !== except);
  }

  accept(role) {
    const [client, server] = Object.values(new WebSocketPair());
    this.ctx.acceptWebSocket(server, [role]);
    return { client, server };
  }

  // Tell both sides the other is here, if it is.
  announce(role, server) {
    const other = role === "host" ? "guest" : "host";
    const others = this.sockets(other);
    if (others.length > 0) {
      for (const o of others) o.send(JSON.stringify({ t: "peer", state: "here" }));
      server.send(JSON.stringify({ t: "peer", state: "here" }));
    }
  }

  async fetch(request) {
    const url = new URL(request.url);
    const action = url.pathname.slice(1);
    const room = url.searchParams.get("room");
    let st = await this.state();

    if (action === "open") {
      if (st) return text(409, "taken");
      st = { room, hostTicket: randomTicket(), guestTicket: null, created: Date.now(), emptySince: null };
      await this.ctx.storage.put("state", st);
      await this.ctx.storage.setAlarm(Date.now() + CHECK_MS);
      const { client, server } = this.accept("host");
      server.send(JSON.stringify({ t: "opened", room, ticket: st.hostTicket }));
      return new Response(null, { status: 101, webSocket: client });
    }
    if (!st) return text(404, "no-room");

    if (action === "join") {
      if (st.guestTicket) return text(409, "room-full");
      st.guestTicket = randomTicket();
      st.emptySince = null;
      await this.ctx.storage.put("state", st);
      const { client, server } = this.accept("guest");
      server.send(JSON.stringify({ t: "joined", ticket: st.guestTicket }));
      this.announce("guest", server);
      return new Response(null, { status: 101, webSocket: client });
    }

    if (action === "rejoin") {
      const role = url.searchParams.get("role");
      if (role !== "host" && role !== "guest") return text(400, "role");
      const want = role === "host" ? st.hostTicket : st.guestTicket;
      const given = request.headers.get("x-xbtune-ticket") || "";
      if (!want || given !== want) return text(403, "ticket");
      // A rejoin replaces a stale connection of the same role.
      for (const old of this.sockets(role)) {
        try {
          old.close(1000, "replaced");
        } catch (e) {}
      }
      st.emptySince = null;
      await this.ctx.storage.put("state", st);
      const { client, server } = this.accept(role);
      this.announce(role, server);
      return new Response(null, { status: 101, webSocket: client });
    }
    return text(404, "not found");
  }

  async webSocketMessage(ws, msg) {
    const role = this.ctx.getTags(ws)[0];
    if (typeof msg === "string") {
      if (msg.includes('"t":"close"')) await this.closeRoom("closed");
      return;
    }
    if (msg.byteLength > MAX_FRAME) {
      ws.close(1009, "frame too large");
      return;
    }
    const other = role === "host" ? "guest" : "host";
    for (const o of this.sockets(other)) {
      try {
        o.send(msg);
      } catch (e) {}
    }
  }

  async gone(ws) {
    const role = this.ctx.getTags(ws)[0];
    try {
      ws.close(1000, "bye");
    } catch (e) {}
    const st = await this.state();
    if (!st) return;
    if (this.sockets(role, ws).length > 0) return; // replaced by a rejoin
    const other = role === "host" ? "guest" : "host";
    const others = this.sockets(other);
    for (const o of others) {
      try {
        o.send(JSON.stringify({ t: "peer", state: "gone" }));
      } catch (e) {}
    }
    if (others.length === 0) {
      st.emptySince = Date.now();
      await this.ctx.storage.put("state", st);
    }
  }

  async webSocketClose(ws) {
    await this.gone(ws);
  }

  async webSocketError(ws) {
    await this.gone(ws);
  }

  async alarm() {
    const st = await this.state();
    if (!st) return;
    const now = Date.now();
    const empty = this.ctx.getWebSockets().length === 0;
    if (now - st.created > MAX_ROOM_MS || (empty && st.emptySince && now - st.emptySince > EMPTY_ROOM_MS)) {
      await this.closeRoom("expired");
      return;
    }
    await this.ctx.storage.setAlarm(now + CHECK_MS);
  }

  async closeRoom(why) {
    for (const w of this.ctx.getWebSockets()) {
      try {
        w.close(1000, why);
      } catch (e) {}
    }
    await this.ctx.storage.deleteAll();
  }
}
