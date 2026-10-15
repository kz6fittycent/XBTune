# Bug report relay

A Cloudflare Worker (free plan, no card) that turns XBTune's **Report a problem**
(and the crash report the app offers) into a GitHub issue, so users don't need a
GitHub account and the app holds no token. Copied from the Desktop Scriptures
relay. How it keeps the tracker clean (duplicate crashes become comments, rate
limit, auto-close) is described at the top of `worker.js`. Not part of the snap.

The app side is `crates/xbtune-app/src/report.rs` (it posts to
`https://xbtune-bug-report.jimmy-tigert.workers.dev/report`; `XBTUNE_REPORT_URL`
overrides it). Until the Worker is deployed, sends fail and the app keeps the
reports in `<data>/app-log/pending/`, retrying on later launches (up to 5, 30 days).

## One-time setup

1. **Cloudflare account:** the same one as Desktop Scriptures (subdomain
   `jimmy-tigert.workers.dev`). If the subdomain differs, change `RELAY_URL` in
   `report.rs`.
2. **GitHub token:** <https://github.com/settings/personal-access-tokens/new>
   - Repository access: **Only select repositories** → `kz6fittycent/XBTune`
   - Permissions → Repository → **Issues: Read and write** (nothing else)
   - Expiration: the longest offered. When it expires, reports fail and the app saves
     them to retry; make a new token and repeat the `secret put` step.
     The argument is literally `GITHUB_TOKEN` (the name); paste the token only at the prompt.
3. **Label** the Worker files issues under:

   ```bash
   gh label create user-report --repo kz6fittycent/XBTune --color d93f0b --description "Filed from the app's Report a problem"
   gh label create keep --repo kz6fittycent/XBTune --color 0e8a16 --description "Don't auto-close this user report"
   ```

4. **Deploy** (the first `wrangler` run opens a browser to log in):

   ```bash
   cd relay/bug-report
   npx wrangler@4 login
   npx wrangler@4 deploy
   npx wrangler@4 secret put GITHUB_TOKEN
   ```

## Checking it

```bash
curl -s -X POST https://xbtune-bug-report.jimmy-tigert.workers.dev/report \
  -H 'Content-Type: application/json' \
  -d '{"description":"Test report from setup - please close.","version":"test","platform":"curl"}'
```

That should answer with `{"issue":<number>,...}` and open a `user-report` issue. Logs:
`npx wrangler@4 tail`.

From the app: `XBTUNE_CRASH_TEST=panic xbtune` crashes on purpose; the next start
offers to send the crash report (`thread` instead keeps the app running and offers
it at once; `segv` and `abort` test the fatal-signal path).

## The repo is private

Issues in a private repo are visible only to its collaborators, so reports stay
private too. If the repo is ever made public, so are the reports (they never
include tunes or data logs; home paths and Bluetooth addresses are masked), and
the app's dialog text should say so.
