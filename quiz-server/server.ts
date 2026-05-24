import { readFileSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = dirname(fileURLToPath(import.meta.url));

type Question = {
  text: string;
  options: [string, string, string, string];
  correct: number;
};

const QUESTIONS: Question[] = [
  {
    text: "Which crate provides the async runtime on bare-metal Rust?",
    options: ["tokio", "Embassy", "async-std", "smol"],
    correct: 1,
  },
  {
    text: "What does #![no_std] do?",
    options: [
      "Disables the Rust std library",
      "Disables generics",
      "Disables the heap allocator",
      "Disables closures",
    ],
    correct: 0,
  },
  {
    text: "Which BLE host crate powers this firmware?",
    options: ["nrf-softdevice", "btleplug", "trouble-host", "bluer"],
    correct: 2,
  },
  {
    text: "What does the MAX7219 drive?",
    options: ["LED matrices", "An OLED display", "E-paper", "A TFT screen"],
    correct: 0,
  },
];

type Player = {
  name: string;
  correctQids: Set<number>;
  joinedAt: number;
};

type Mode = "question" | "leaderboard";

const state = {
  questionIndex: 0,
  questionId: 1,
  votes: new Map<string, number>(),
  players: new Map<string, Player>(),
  locked: false,
  revealed: false,
  mode: "question" as Mode,
};

function currentQuestion(): Question {
  return QUESTIONS[state.questionIndex % QUESTIONS.length];
}

function tally(): [number, number, number, number] {
  const counts = [0, 0, 0, 0];
  for (const opt of state.votes.values()) {
    if (opt >= 0 && opt < 4) counts[opt]++;
  }
  const total = counts.reduce((a, b) => a + b, 0);
  if (total === 0) return [0, 0, 0, 0];
  const pct = counts.map((c) => Math.round((c / total) * 100));
  return pct as [number, number, number, number];
}

function commitScores() {
  const q = currentQuestion();
  for (const [sid, opt] of state.votes) {
    if (opt === q.correct) {
      const p = state.players.get(sid);
      if (p) p.correctQids.add(state.questionId);
    }
  }
}

function leaderboard(limit = 10) {
  return Array.from(state.players.values())
    .map((p) => ({ name: p.name, score: p.correctQids.size, joinedAt: p.joinedAt }))
    .sort((a, b) => b.score - a.score || a.joinedAt - b.joinedAt)
    .slice(0, limit)
    .map(({ name, score }) => ({ name, score }));
}

function myRank(sid: string | null): { rank: number | null; score: number; name: string | null } {
  if (!sid) return { rank: null, score: 0, name: null };
  const p = state.players.get(sid);
  if (!p) return { rank: null, score: 0, name: null };
  const sorted = Array.from(state.players.values())
    .map((pp) => ({ sid: pp, score: pp.correctQids.size }))
    .sort((a, b) => b.score - a.score || a.sid.joinedAt - b.sid.joinedAt);
  const idx = sorted.findIndex((e) => e.sid === p);
  return { rank: idx >= 0 ? idx + 1 : null, score: p.correctQids.size, name: p.name };
}

function getCookie(req: Request, name: string): string | null {
  const raw = req.headers.get("cookie") ?? "";
  for (const part of raw.split(";")) {
    const [k, v] = part.trim().split("=");
    if (k === name) return v ?? null;
  }
  return null;
}

function newSid(): string {
  return crypto.randomUUID();
}

function ensureSid(req: Request, extra: Record<string, string> = {}): { sid: string; headers: HeadersInit } {
  let sid = getCookie(req, "sid");
  const headers: Record<string, string> = { ...extra };
  if (!sid) {
    sid = newSid();
    headers["Set-Cookie"] = `sid=${sid}; Path=/; Max-Age=86400; SameSite=Lax`;
  }
  return { sid, headers };
}

const AUDIENCE_HTML = readFileSync(join(__dirname, "public", "audience.html"), "utf8");
const ADMIN_HTML = readFileSync(join(__dirname, "public", "admin.html"), "utf8");

const PORT = Number(process.env.PORT ?? 3000);

const ADMIN_PASSWORD = process.env.ADMIN_PASSWORD ?? "rustquiz";
if (!process.env.ADMIN_PASSWORD) {
  console.warn(
    "⚠ ADMIN_PASSWORD not set — defaulting to 'rustquiz'. " +
      "Set ADMIN_PASSWORD env var to override.",
  );
}
const adminTokens = new Set<string>();

function isAdmin(req: Request): boolean {
  const t = getCookie(req, "admin");
  return !!t && adminTokens.has(t);
}

const server = Bun.serve({
  port: PORT,
  hostname: "0.0.0.0",
  async fetch(req) {
    const url = new URL(req.url);

    if (req.method === "GET" && url.pathname === "/") {
      const { headers } = ensureSid(req, { "content-type": "text/html; charset=utf-8" });
      return new Response(AUDIENCE_HTML, { headers });
    }

    if (req.method === "GET" && url.pathname === "/admin") {
      return new Response(ADMIN_HTML, {
        headers: { "content-type": "text/html; charset=utf-8" },
      });
    }

    if (req.method === "POST" && url.pathname === "/join") {
      const { sid, headers } = ensureSid(req);
      const body = await req.json().catch(() => ({}));
      const name = String(body.name ?? "").trim().slice(0, 24);
      if (!name) return new Response("name required", { status: 400, headers });
      const existing = state.players.get(sid);
      if (existing) {
        existing.name = name;
      } else {
        state.players.set(sid, { name, correctQids: new Set(), joinedAt: Date.now() });
      }
      return Response.json({ ok: true, name }, { headers });
    }

    if (req.method === "POST" && url.pathname === "/vote") {
      const sid = getCookie(req, "sid");
      if (!sid) return new Response("missing sid", { status: 400 });
      if (state.locked) return new Response("locked", { status: 423 });
      if (state.mode !== "question") return new Response("not in question mode", { status: 409 });
      const body = await req.json().catch(() => ({}));
      const option = Number(body.option);
      if (!Number.isInteger(option) || option < 0 || option > 3) {
        return new Response("bad option", { status: 400 });
      }
      state.votes.set(sid, option);
      return Response.json({ ok: true });
    }

    if (req.method === "GET" && url.pathname === "/me") {
      const sid = getCookie(req, "sid");
      const player = sid ? state.players.get(sid) : null;
      const vote = sid ? state.votes.get(sid) ?? null : null;
      const rank = myRank(sid);
      return Response.json({
        sid: !!sid,
        name: player?.name ?? null,
        vote,
        questionId: state.questionId,
        locked: state.locked,
        revealed: state.revealed,
        mode: state.mode,
        score: rank.score,
        rank: rank.rank,
        totalPlayers: state.players.size,
      });
    }

    if (req.method === "GET" && url.pathname === "/results") {
      const q = currentQuestion();
      const votes = state.mode === "leaderboard" ? [0, 0, 0, 0] : tally();
      return Response.json({
        questionId: state.questionId,
        questionIndex: state.questionIndex,
        text: q.text,
        options: q.options,
        correct: q.correct,
        votes,
        total: state.votes.size,
        locked: state.locked,
        revealed: state.revealed,
        mode: state.mode,
        leaderboard: leaderboard(10),
        totalPlayers: state.players.size,
      });
    }

    if (req.method === "GET" && url.pathname === "/admin/auth") {
      return Response.json({ authed: isAdmin(req) });
    }

    if (req.method === "POST" && url.pathname === "/admin/login") {
      const body = await req.json().catch(() => ({}));
      const pw = String(body.password ?? "");
      if (pw !== ADMIN_PASSWORD) {
        return new Response("bad password", { status: 401 });
      }
      const token = crypto.randomUUID();
      adminTokens.add(token);
      return Response.json(
        { ok: true },
        {
          headers: {
            "Set-Cookie": `admin=${token}; Path=/; Max-Age=86400; HttpOnly; SameSite=Lax`,
          },
        },
      );
    }

    if (req.method === "POST" && url.pathname === "/admin/logout") {
      const t = getCookie(req, "admin");
      if (t) adminTokens.delete(t);
      return Response.json(
        { ok: true },
        {
          headers: {
            "Set-Cookie": "admin=; Path=/; Max-Age=0; HttpOnly; SameSite=Lax",
          },
        },
      );
    }

    if (req.method === "POST" && url.pathname.startsWith("/admin/")) {
      if (!isAdmin(req)) return new Response("unauthorized", { status: 401 });
      const action = url.pathname.slice("/admin/".length);
      switch (action) {
        case "next":
          state.questionIndex = (state.questionIndex + 1) % QUESTIONS.length;
          state.questionId++;
          state.votes.clear();
          state.locked = false;
          state.revealed = false;
          state.mode = "question";
          return Response.json({ ok: true });
        case "prev":
          state.questionIndex = (state.questionIndex - 1 + QUESTIONS.length) % QUESTIONS.length;
          state.questionId++;
          state.votes.clear();
          state.locked = false;
          state.revealed = false;
          state.mode = "question";
          return Response.json({ ok: true });
        case "lock":
          state.locked = !state.locked;
          return Response.json({ ok: true, locked: state.locked });
        case "reveal":
          if (!state.revealed) commitScores();
          state.revealed = !state.revealed;
          return Response.json({ ok: true, revealed: state.revealed });
        case "reset":
          state.votes.clear();
          state.locked = false;
          state.revealed = false;
          return Response.json({ ok: true });
        case "leaderboard":
          state.mode = state.mode === "leaderboard" ? "question" : "leaderboard";
          return Response.json({ ok: true, mode: state.mode });
        case "reset-all":
          state.questionIndex = 0;
          state.questionId++;
          state.votes.clear();
          state.players.clear();
          state.locked = false;
          state.revealed = false;
          state.mode = "question";
          return Response.json({ ok: true });
        default:
          return new Response("unknown action", { status: 404 });
      }
    }

    return new Response("not found", { status: 404 });
  },
});

console.log(`Quiz server listening on http://localhost:${server.port}`);
console.log(`  Audience: http://<your-lan-ip>:${server.port}/`);
console.log(`  Admin:    http://localhost:${server.port}/admin`);
