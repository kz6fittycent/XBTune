# AI-assisted tuning (phase 10): design for approval

Status: **APPROVED 2026-10-19** (owner's §10 answers below). **A1 built
2026-10-19**: `crates/xbtune-ai` (config + presets, both adapters, prompt
and data block, journal), CLI `ai presets|setup|models|test|ask`, app
Settings "AI advisor" card and Tune "Ask the AI advisor…" (first-use
guide, "What gets sent", answers in words). Tried against a local Ollama
(qwen3-coder): works; see "A1 findings". A2 next.

### A1 findings (2026-10-19, local Ollama, qwen3-coder, owner's logs)

- Ollama's default context (2048 tokens) silently cut the ~4k-token
  request: the model then ignored the question. Now the question comes
  last in the message, XBTune warns when the reported input is at a
  context-window size or far below what was sent, and the Ollama/LM Studio
  presets and the guide say to set 16k context.
- Given cells as row/col indices, the model mapped them to the wrong load
  and RPM. Every cell now carries its load and rpm values.
- It read EGO 95% as "lean" despite the prompt. Each cell now has a
  plain-words reading from XBTune ("map RICH here: the ECM removed 4.7%
  fuel"). The A2 checker and the A3 eval exist for this kind of error.

## 1. What it is

An **advisor**: you ask a question about your tune and logs ("why is it
lean at 3000 rpm cruise?", "smooth the idle area", "suggest fuel changes
from these logs"). A language model answers, and may suggest changes to the
map cells. Its suggestions go through the same review as a remote tuner's
(REMOTE.md R3): every change is listed, you accept or reject it, and
accepted changes are one undo step in the open tune. The AI never:

- writes to the ECM, runs tests, resets anything or clears codes;
- changes anything without your click;
- sees or sends anything you haven't been shown first.

XBTune stays free. You bring the model: one running on your own computer
(nothing leaves the machine), or a hosted one with your own API key (you
pay the provider, if anything).

## 2. Which models: any, through two APIs

Almost every model can be reached through one of two request formats, so
XBTune implements two adapters and nothing provider-specific:

| Adapter | Covers | Default base URL |
|---|---|---|
| **OpenAI-compatible** `POST {base}/chat/completions` | Ollama, llama.cpp `llama-server`, LM Studio, vLLM, LocalAI (local); OpenAI, OpenRouter, Groq, Mistral, DeepSeek, Google Gemini's OpenAI endpoint, and most others (hosted) | `http://localhost:11434/v1` (Ollama) |
| **Anthropic Messages** `POST {base}/v1/messages` | Claude models | `https://api.anthropic.com` |

Settings → AI advisor: adapter, base URL, model name (with a "List models"
button where the server supports `GET /models`), API key (optional for
local servers), and a "Test connection" button. Presets (owner: list them
all) fill in the adapter and URL:

- Local: Ollama, LM Studio, llama.cpp (`llama-server`), vLLM, LocalAI, Jan.
- Hosted, OpenAI-compatible: OpenAI, Google Gemini (its OpenAI endpoint),
  OpenRouter, Groq, Mistral, DeepSeek, xAI, Together AI, Fireworks AI,
  Perplexity, Cerebras.
- Hosted, Anthropic: Anthropic (Claude).
- Custom: any OpenAI-compatible URL. (Azure OpenAI uses a different URL
  scheme and `api-key` header: a Custom entry with a header field covers
  it.)

Preset URLs are checked when the presets are written, not trusted from
memory, and the list is easy to extend.

- HTTP with `ureq` + `serde_json` (both already approved); no new
  dependencies. Non-streaming requests on a background thread, with a
  Cancel button and a timeout (default 120 s; local models can be slow).
- The API key is stored in `<config>/ai.toml` with mode 0600, outside the
  files folder, never logged, never in bug reports (the report masker
  gets a rule for it). A desktop keyring would need the snap's
  `password-manager-service` plug (manual connect, reviewed by the store),
  so the plain file is proposed instead. `XBTUNE_AI_KEY` overrides it.
- Snap: the GUI and CLI already have `network`; localhost servers work
  with it.

## 3. Safety: model quality decides how useful it is, never how safe

Nobody can promise which models tune safely, so XBTune doesn't rely on
the model being right. Every suggestion goes through a fixed checker in
Rust before you see it as a proposal:

1. **Structured output only.** Changes must come as JSON
   (`{"changes":[{"map","row","col","value","why"}], "notes"}`). Free text
   is shown as text. A malformed or partly malformed reply proposes nothing.
2. **Allowed variables:** the fuel maps (`Tab_Fuel_Front/Rear`) and the
   spark maps (`Tab_Spark_Front/Rear`, owner 2026-10-19). Rev limits, fan
   and every other variable are refused, with the reason shown.
   Spark rules (XBTune's proposal, owner to adjust): at most 2° less or
   1° more advance per cell per round; never more advance at load ≥ 80;
   never more advance in a cell the wideband shows lean; a spark change
   needs logged data in that cell like a fuel change. The XB's ECM has no
   knock sensing that XBTune can see, so advance changes are flagged
   "check carefully" in the review.
3. **Same limits as auto-tune.** At most ±`max_change` per cell (default
   10%) per round, inside the variable's own range. A cell needs logged
   data (`min_hits`) before it may change, unless you ask for edits
   without data ("smooth these cells") and tick "allow cells without log
   data" for that request.
4. **No leaning at load** (fuel). At load ≥ 80 a cell may not end up leaner than
   both its current value and the deterministic auto-tune's proposal for
   it. Where a wideband shows the cell already lean (the auto-tune lean
   warning), only richer is allowed.
5. **Checked against the numbers.** Each proposal shows the deterministic
   auto-tune result for the same cell next to it. Where the AI goes the
   other way, or more than 3 points further, the row is flagged.
6. Refused suggestions are listed with the rule that refused them, so the
   model's quality is visible.

Then the normal path: review, Accept/Reject, Undo, Save, Burn dialog with
its §6 checks. The disclaimer gets one more point (AI suggestions can be
wrong) and its version goes up, so everyone sees it again.

### Finding out which models are good enough

`xbtune.cli ai eval --model ...` runs a fixed set of cases built from the
owner's logs and dumps (and made-up ones for lean/rich, WOT, idle): it
sends each prompt, runs the checker, and reports: valid JSON rate, refused
suggestions by rule, agreement with deterministic auto-tune (direction and
size), and time and tokens per case. The results table goes in this
document, so the guide can say "tested with: ..." from measurements, not
guesses. The cases never need the bike.

## 4. RAG? Not to start with

The useful data is small and structured, so it fits in a prompt:

- ECM ID/version, the selected map (13 x 12 cells) and its axes, the
  current AFV, the target AFR table: about 2-3 k tokens.
- Per-cell log statistics from `autotune::analyze` (hits, EGO or AFR,
  target) and the deterministic proposals: about 2-4 k.
- Log summary from `logstats` (duration, rate, O2 split, anomalies): < 1 k.
- **Tuning notes**: a short, versioned text written for XBTune (what the
  maps, EGO, AFV, closed/open loop and the Buell load axis mean, the safety
  rules above, what the checker will refuse), about 2 k. It's ours, so no
  copyright question.

Total ~10 k tokens, inside every current model's context (small local
models included, set to 16 k context). A retrieval system (embeddings,
vector store) only pays off with a large library of text to search, such
as manuals or forum threads, and those are mostly copyrighted. If the
notes outgrow the prompt, the first step is choosing sections by topic
(fuel, spark, idle), with no vector store. Raw log samples are not sent
(they're large and the statistics hold what matters); a "send N samples
around the playhead" option can come later if useful.

## 5. What leaves the machine

Before every request: "What gets sent" (like bug reports) shows the exact
request body, its size and a token estimate, plus where it goes (host
name; "this computer" for localhost). Nothing is sent without a click on
Send. Sent: the items in §4 and your question. Never sent: VIN, Bluetooth
addresses, file paths, user name, the rest of the EEPROM, app logs. The
reply and the request are kept in `~/XBTune/AI/<time>.jsonl` (your record,
like the remote journal; delete at will).

Cost: shown as the provider's reported token usage after each answer.
XBTune has no prices built in (they change); the guide points at the
provider's pricing page.

## 6. Where it shows up

- **Tune view → "Ask the AI advisor…"** (fuel maps): a panel with the
  question box, the conversation, and proposals in the same table as
  Auto-tune (tick/untick, Apply = one undo step "AI: ...").
- **Logs**: the open logs are the data; no logs means questions about the
  tune only, and no data-based changes.
- **Remote sessions**: the host's AI only, and its proposals land like
  the host's own edits. A tuner's AI suggestions are the tuner's edits.
  Nothing new in the remote protocol.
- **CLI**: `xbtune.cli ai ask --tune T [LOG...] "question"` prints the
  answer and checked proposals; `-o OUT` saves them applied (never
  in place), like `autotune`. `xbtune.cli ai eval` (above).
- First use: a short guide (what it does, local vs hosted, cost and
  privacy, "Don't show again").

## 6a. Bug reports: "Check with the AI first" (owner's idea, 2026-10-19)

In Report a problem, when an AI is set up, a button "Check with the AI
first" sends the same masked report the user would send (the "What gets
sent" preview: description, recent app log, crash summary, app version,
ECM ID) to the user's own model, with a system prompt about XBTune's
known local problems. It answers with:

- **Likely local causes** with what to do, e.g. snap permissions not
  connected, adapter not paired or busy (EBUSY), wrong ECM type (factory
  race), ignition off / kill switch, weak battery (logged voltage), a
  noisy Bluetooth link (timeouts, NAKs), the wrong serial port, a
  wideband that never sent data.
- **A short summary for the developer**, which the user can add to the
  report, marked "AI summary (user's model)". The report is still only
  sent when the user clicks Send; the relay doesn't change.

XBTune's own checks come first and don't need an AI: the plug check and
link error hints that already exist feed the prompt as facts, so a small
model mostly has to explain them. Tested like §8 with the mock server and
recorded log excerpts (masked).

## 7. Prompt outline

System prompt (fixed, versioned, shipped in the app):
1. Role: advisor for a Buell DDFI fuel map; the user and a deterministic
   checker review everything; never claims a change is safe.
2. The tuning notes (§4).
3. Output contract: JSON schema with an example; values in the map's
   units; row/col indices as given; `why` per change; say "not enough
   data" instead of guessing.
4. The checker's rules, so the model doesn't waste suggestions.

User message: the question, then the data block as JSON (ECM, map with
axes, cell statistics, deterministic proposals, log summary, targets).

## 8. Tests (no network in CI)

- A local mock server (std `TcpListener`) for both adapters: request shape
  (headers, key, JSON), replies with valid, broken and hostile JSON
  (out-of-range cells, spark changes, leaning at WOT, huge values,
  prompt-injection text in `why`), timeouts and HTTP errors.
- Checker unit tests: every rule, both directions.
- Prompt snapshot test (changes to the prompt are deliberate).
- "What gets sent" equals the bytes sent (the mock compares).
- Masking: the key never appears in app.log or a bug report.
- UI checks with screenshot hooks (`XBTUNE_SHOT_AI=...`) against the mock.

## 9. Stages

- **A1**: settings, two adapters, Test connection, ask/answer as text,
  "What gets sent", journal. No proposals.
- **A2**: structured proposals, checker, review table, apply/undo, CLI
  `ai ask`.
- **A3**: `ai eval` and the measured model table here; guide text from it.
- **A4**: the bug-report helper (§6a).

## 10. Owner decisions (answered 2026-10-19)

1. Spark maps: **included**, with the rules in §3.2.
2. API key: **0600 file** (`<config>/ai.toml`).
3. Presets: **all providers** (list in §2).
4. Default model: **none yet**; A3 measures candidates (a range of local
   models in several sizes, and current models from the hosted providers)
   and the guide recommends from the results.
5. Sending the §4 data to a hosted provider on the user's click: **OK**.
   Plus the bug-report helper (§6a).

Still open: approval of this document as a whole, and the spark limits in
§3.2.
