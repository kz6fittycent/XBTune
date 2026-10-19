# AI-assisted tuning (phase 10): design for approval

Status: **DRAFT, waiting for the owner's approval.** No code until then
(CLAUDE.md, phase 10; KICKOFF §9 for a network feature).

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
local servers), and a "Test connection" button. Presets fill in the URL
for Ollama, LM Studio, llama.cpp, OpenRouter, OpenAI and Anthropic.

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
2. **Allowed variables.** v1: the fuel maps only (`Tab_Fuel_Front/Rear`).
   Spark maps, rev limits, fan and every other variable are refused, with
   the reason shown. (Owner decision Q1: allow spark changes later, and
   then only toward less advance?)
3. **Same limits as auto-tune.** At most ±`max_change` per cell (default
   10%) per round, inside the variable's own range. A cell needs logged
   data (`min_hits`) before it may change, unless you ask for edits
   without data ("smooth these cells") and tick "allow cells without log
   data" for that request.
4. **No leaning at load.** At load ≥ 80 a cell may not end up leaner than
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

## 10. Owner decisions needed

1. Spark maps: never, or later with "less advance only"? (Proposed: fuel
   only in v1.)
2. API key in a 0600 file (proposed), or a keyring (extra snap plug)?
3. Presets: which providers to list (proposed: Ollama, LM Studio,
   llama.cpp, OpenRouter, OpenAI, Anthropic)?
4. A default model to suggest for local use, after A3 measures some?
5. OK to send the data in §4 to a hosted provider when the user clicks
   Send (KICKOFF §9: network-facing feature)?
