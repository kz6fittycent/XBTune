// XBTune's "Report a problem" relay - a Cloudflare Worker (free plan) that
// files a report as a GitHub issue, so a user needs no GitHub account.
// (Same design as the Desktop Scriptures relay.)
//
// The app can't hold a GitHub token itself (anyone could pull it out of
// the snap), so the token lives here, as the GITHUB_TOKEN secret: a
// fine-grained token that can only read and write this repo's issues.
//
// Keeping the issue tracker usable without anyone sweeping it by hand:
// - A crash report carries a fingerprint (the app hashes where the panic
//   or fatal signal happened; crates/xbtune-app/src/report.rs). If an open user-report issue already has
//   that fingerprint, the report becomes a comment on it ("another user hit
//   this") instead of a new issue - one issue per bug, with a running count.
// - Per-address rate limit (REPORT_LIMITER, see wrangler.toml) and size
//   caps. (The description is optional - the app tells users they don't
//   have to write anything; the log carries the details.) That limiter is approximate (Cloudflare
//   syncs its counts lazily - a fast burst gets several requests through),
//   so two hard caps, counted on GitHub itself, back it up: at most
//   MAX_NEW_PER_HOUR new issues across all users, and at most
//   MAX_REPEAT_COMMENTS "another user hit this" comments per issue.
// - .github/workflows/close-stale-reports.yml closes user-report issues
//   that go quiet.
//
// Over the free plan's daily request limit, Cloudflare refuses requests
// rather than billing; the app then saves the report to a file instead.

const REPO = "kz6fittycent/XBTune";
const LABEL = "user-report";

const MAX_DESCRIPTION = 5000;
const MAX_CONTACT = 200;
const MAX_FIELD = 200;
// GitHub caps an issue or comment body at 65,536 characters; the log gets
// what's left after the rest of the body, keeping its newest (last) lines.
const MAX_BODY = 60000;

const MAX_NEW_PER_HOUR = 10;
const MAX_REPEAT_COMMENTS = 50;

const FINGERPRINT_RE = /^[0-9a-f]{12}$/;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    if (url.pathname !== "/report") return json(404, { error: "not found" });
    if (request.method !== "POST") return json(405, { error: "POST only" });

    const ip = request.headers.get("CF-Connecting-IP") || "unknown";
    const { success } = await env.REPORT_LIMITER.limit({ key: ip });
    if (!success) return json(429, { error: "Too many reports - please try again in a minute." });

    let report;
    try {
      report = await request.json();
    } catch {
      return json(400, { error: "invalid JSON" });
    }

    const problem = validate(report);
    if (problem) return json(400, { error: problem });

    try {
      const existing = report.fingerprint ? await findOpenIssue(env, report.fingerprint) : null;
      if (existing) {
        // Past the cap the report is still acknowledged - the bug is
        // already known - just not written as yet another comment.
        if (existing.comments >= MAX_REPEAT_COMMENTS) {
          return json(200, { issue: existing.number, url: existing.html_url, duplicate: true });
        }
        await github(env, "POST", `/repos/${REPO}/issues/${existing.number}/comments`, {
          body: buildBody(report, true),
        });
        return json(200, { issue: existing.number, url: existing.html_url, duplicate: true });
      }
      if ((await countNewThisHour(env)) >= MAX_NEW_PER_HOUR) {
        return json(503, { error: "Too many reports right now - please try again later." });
      }
      const issue = await github(env, "POST", `/repos/${REPO}/issues`, {
        title: buildTitle(report),
        body: buildBody(report, false),
        labels: [LABEL],
      });
      return json(201, { issue: issue.number, url: issue.html_url, duplicate: false });
    } catch (err) {
      console.error(err);
      return json(502, { error: "GitHub didn't accept the report." });
    }
  },
};

function validate(r) {
  if (typeof r !== "object" || r === null) return "expected a JSON object";
  if (r.description !== undefined && typeof r.description !== "string") return "invalid description";
  if ((r.description || "").length > MAX_DESCRIPTION) return "description too long";
  if (r.ecm !== undefined && (typeof r.ecm !== "string" || r.ecm.length > MAX_FIELD)) return "invalid ecm";
  for (const field of ["version", "platform"]) {
    if (typeof r[field] !== "string" || !r[field] || r[field].length > MAX_FIELD) return `missing or invalid ${field}`;
  }
  if (r.contact !== undefined && (typeof r.contact !== "string" || r.contact.length > MAX_CONTACT)) return "invalid contact";
  if (r.error !== undefined && (typeof r.error !== "string" || r.error.length > MAX_FIELD)) return "invalid error";
  if (r.fingerprint !== undefined && !FINGERPRINT_RE.test(r.fingerprint)) return "invalid fingerprint";
  if (r.log !== undefined && typeof r.log !== "string") return "invalid log";
  return null;
}

function buildTitle(r) {
  if (r.error) return `[Report] ${r.error}`.slice(0, 120);
  const firstLine = (r.description || "").trim().split("\n")[0];
  return `[Report] ${firstLine || `No description (${r.version}, ${r.platform})`}`.slice(0, 120);
}

function buildBody(r, isRepeat) {
  const head = [
    isRepeat ? "**Another user hit this.**" : "_Filed from XBTune's Report a problem._",
    "",
    "### What happened",
    "",
    (r.description || "").trim() ? quote(r.description.trim()) : "_No description given._",
    "",
    `- **Version:** ${inline(r.version)}`,
    `- **Platform:** ${inline(r.platform)}`,
  ];
  if (r.ecm) head.push(`- **ECM:** ${inline(r.ecm)}`);
  if (r.error) head.push(`- **Error:** ${inline(r.error)}`);
  if (r.fingerprint) head.push(`- **Fingerprint:** \`fp-${r.fingerprint}\``);
  if (r.contact) head.push(`- **Contact:** ${inline(r.contact)}`);

  let text = head.join("\n");
  if (r.log) {
    const open = "\n\n<details><summary>Log</summary>\n\n````text\n";
    const close = "\n````\n\n</details>\n";
    const room = MAX_BODY - text.length - open.length - close.length;
    if (room > 0) {
      let log = r.log.replaceAll("````", "'''' ");
      if (log.length > room) log = "…(older lines trimmed)\n" + log.slice(log.length - room + 30);
      text += open + log + close;
    }
  }
  return text;
}

// User text goes in as a blockquote, and the one-line fields as plain text
// with markdown and mentions defused, so a report can't ping people or
// close other issues ("fixes #12") or inject links.
function quote(s) {
  return defuse(s).split("\n").map((line) => `> ${line}`).join("\n");
}

function inline(s) {
  return defuse(s.replace(/\s+/g, " "));
}

function defuse(s) {
  return s
    .replace(/@/g, "@​")
    .replace(/#(\d)/g, "#​$1")
    .replace(/[<>]/g, (c) => (c === "<" ? "&lt;" : "&gt;"))
    .replace(/\]\(/g, "]​(");
}

async function findOpenIssue(env, fingerprint) {
  const q = encodeURIComponent(`repo:${REPO} is:issue is:open label:${LABEL} "fp-${fingerprint}"`);
  const result = await github(env, "GET", `/search/issues?q=${q}&per_page=1`);
  return result.items && result.items.length ? result.items[0] : null;
}

async function countNewThisHour(env) {
  const since = new Date(Date.now() - 3600 * 1000).toISOString().slice(0, 19) + "Z";
  const q = encodeURIComponent(`repo:${REPO} is:issue label:${LABEL} created:>${since}`);
  const result = await github(env, "GET", `/search/issues?q=${q}&per_page=1`);
  return result.total_count || 0;
}

async function github(env, method, path, body) {
  const response = await fetch(`https://api.github.com${path}`, {
    method,
    headers: {
      Authorization: `Bearer ${env.GITHUB_TOKEN}`,
      Accept: "application/vnd.github+json",
      "X-GitHub-Api-Version": "2022-11-28",
      "User-Agent": "xbtune-bug-report-relay",
      ...(body ? { "Content-Type": "application/json" } : {}),
    },
    body: body ? JSON.stringify(body) : undefined,
  });
  if (!response.ok) throw new Error(`GitHub ${method} ${path}: ${response.status} ${await response.text()}`);
  return response.json();
}

function json(status, data) {
  return new Response(JSON.stringify(data), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}
