// SPDX-License-Identifier: GPL-3.0-or-later
//! `ai presets|setup|models|test|ask`: the AI tuning advisor from the
//! terminal (docs/AI.md). Stage A1: answers in words; nothing changes a
//! tune or the ECM. `ask` shows where the request goes and asks before
//! sending (`--yes` skips that; `--show-request` prints the whole body).

use std::io::{BufRead, Write as _};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use xbtune_ai::bike::{self, Bikes};
use xbtune_ai::client::{self, Role, Turn};
use xbtune_ai::config::{self, Adapter, AiConfig};
use xbtune_ai::{checker, memory};
use xbtune_ai::{context, journal::Journal};
use xbtune_core::logfile::Log;
use xbtune_core::tune::Tune;
use xbtune_defs::Catalog;

#[derive(Subcommand)]
pub enum AiCmd {
    /// List the provider presets.
    Presets,
    /// Show or change the settings (saved in ai.toml, readable only by you).
    Setup(SetupArgs),
    /// List the models the server offers.
    Models,
    /// Send a one-word test request.
    Test,
    /// Ask a question about a tune (and logs). Answers in words; changes nothing.
    Ask(AskArgs),
    /// Show or change your bike's profile (model, mods, notes), sent with every question.
    Bike(BikeArgs),
}

#[derive(Args)]
pub struct BikeArgs {
    /// ECM ID of the bike, e.g. BUEIB (see `xbtune.cli ecms`).
    #[arg(long)]
    ecm: String,
    /// List the models this ECM fits.
    #[arg(long)]
    models: bool,
    /// Model, e.g. XB9SX (any text works).
    #[arg(long)]
    model: Option<String>,
    #[arg(long)]
    year: Option<u16>,
    /// Displacement after a big bore kit, cc (0: stock).
    #[arg(long)]
    big_bore_cc: Option<u32>,
    /// Wideband controller, e.g. "Innovate LC-2" ("" removes it).
    #[arg(long)]
    wideband: Option<String>,
    #[arg(long)]
    exhaust: Option<String>,
    /// Intake / air filter.
    #[arg(long)]
    intake: Option<String>,
    #[arg(long)]
    cams: Option<String>,
    #[arg(long)]
    ported_heads: Option<bool>,
    /// Final drive pulley teeth, front/rear, e.g. 27/64 ("" removes it).
    #[arg(long)]
    final_drive: Option<String>,
    /// Usual fuel octane (0: not set).
    #[arg(long)]
    octane: Option<u32>,
    /// Race ECM, someone else's tune, ...
    #[arg(long)]
    ecm_tune: Option<String>,
    /// Usual riding altitude, metres (0: not set).
    #[arg(long)]
    altitude_m: Option<u32>,
    /// Add another mod (repeatable).
    #[arg(long)]
    add_mod: Vec<String>,
    /// Remove a mod added with --add-mod.
    #[arg(long)]
    remove_mod: Vec<String>,
    /// Add a line to the notes (e.g. a suggestion the advisor made).
    #[arg(long)]
    note: Vec<String>,
    /// Replace the notes ("" removes them).
    #[arg(long)]
    set_notes: Option<String>,
}

#[derive(Args)]
pub struct SetupArgs {
    /// Start from a preset (see `ai presets`), e.g. Ollama, OpenAI, "Anthropic (Claude)".
    #[arg(long)]
    preset: Option<String>,
    /// Request format: openai (OpenAI-compatible) or anthropic.
    #[arg(long)]
    adapter: Option<String>,
    /// Server base URL, e.g. http://localhost:11434/v1.
    #[arg(long)]
    url: Option<String>,
    /// Model name as the provider spells it.
    #[arg(long)]
    model: Option<String>,
    /// Read the API key from standard input (one line). An empty line removes it.
    #[arg(long)]
    key_stdin: bool,
    /// Seconds to wait for an answer.
    #[arg(long)]
    timeout: Option<u64>,
}

#[derive(Args)]
pub struct AskArgs {
    /// The tune the question is about (.xbt or .xpr).
    #[arg(long)]
    tune: PathBuf,
    /// The map in question (default: the front fuel map).
    #[arg(long, default_value = "Tab_Fuel_Front")]
    map: String,
    /// Logs to give as data (XBTune .csv, EcmDroid .bin, .msl).
    logs: Vec<PathBuf>,
    /// The question.
    #[arg(short, long)]
    question: String,
    /// Send without asking first.
    #[arg(long)]
    yes: bool,
    /// Print the whole request body before sending.
    #[arg(long)]
    show_request: bool,
    /// Save the tune with the changes that pass XBTune's checks applied
    /// (never the input file; never the ECM).
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Allow changes to cells without logged data (smoothing requests).
    #[arg(long)]
    allow_no_data: bool,
}

fn config_path() -> Result<PathBuf> {
    AiConfig::path().ok_or_else(|| anyhow!("no config folder (HOME not set?)"))
}

fn load() -> Result<AiConfig> {
    Ok(AiConfig::load(&config_path()?)?)
}

fn describe(c: &AiConfig) {
    println!("Preset:   {}", if c.preset.is_empty() { "-" } else { &c.preset });
    println!("Format:   {}", c.adapter.label());
    println!("URL:      {}", if c.base_url.is_empty() { "-" } else { &c.base_url });
    println!("Model:    {}", if c.model.is_empty() { "-" } else { &c.model });
    let key = if std::env::var_os("XBTUNE_AI_KEY").is_some() {
        "from XBTUNE_AI_KEY"
    } else if c.api_key.is_empty() {
        "none"
    } else {
        "stored"
    };
    println!("API key:  {key}");
    println!("Timeout:  {} s", c.timeout_s);
    println!(
        "Where:    {}",
        if c.base_url.is_empty() {
            "-".to_string()
        } else if c.is_local() {
            "this computer (nothing leaves it)".into()
        } else {
            c.host()
        }
    );
    if let Some(m) = c.missing() {
        println!("Missing:  {m}");
    }
}

pub fn ai(cmd: AiCmd) -> Result<()> {
    match cmd {
        AiCmd::Presets => {
            for p in config::PRESETS {
                let place = if p.local { "local " } else { "hosted" };
                println!("{:<26} {place}  {:<18} {}", p.name, p.adapter.label(), p.base_url);
                if !p.note.is_empty() {
                    println!("{:<26}         {}", "", p.note);
                }
            }
            Ok(())
        }
        AiCmd::Setup(a) => setup(a),
        AiCmd::Models => {
            for m in client::list_models(&load()?)? {
                println!("{m}");
            }
            Ok(())
        }
        AiCmd::Test => {
            let c = load()?;
            eprintln!("Asking {} ({}) for a one-word answer...", c.model, c.host());
            let r = client::test_connection(&c)?;
            println!("Answer: {}", r.text.trim());
            if let Some(m) = &r.model {
                println!("Model:  {m}");
            }
            Ok(())
        }
        AiCmd::Ask(a) => ask(a),
        AiCmd::Bike(a) => bike(a),
    }
}

fn setup(a: SetupArgs) -> Result<()> {
    let path = config_path()?;
    let mut c = AiConfig::load(&path)?;
    let mut changed = false;
    if let Some(name) = &a.preset {
        let p = config::preset(name).ok_or_else(|| anyhow!("no preset '{name}' (see `ai presets`)"))?;
        c.apply_preset(p);
        changed = true;
    }
    if let Some(ad) = &a.adapter {
        c.adapter = match ad.to_ascii_lowercase().as_str() {
            "openai" => Adapter::OpenAi,
            "anthropic" => Adapter::Anthropic,
            _ => bail!("--adapter is openai or anthropic"),
        };
        changed = true;
    }
    if let Some(u) = a.url {
        c.base_url = u.trim().trim_end_matches('/').to_string();
        changed = true;
    }
    if let Some(m) = a.model {
        c.model = m.trim().to_string();
        changed = true;
    }
    if let Some(t) = a.timeout {
        c.timeout_s = t;
        changed = true;
    }
    if a.key_stdin {
        eprint!("API key (input is not hidden; empty removes it): ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        c.api_key = line.trim().to_string();
        changed = true;
    }
    if changed {
        c.save(&path)?;
        println!("Saved {}", path.display());
    }
    describe(&c);
    Ok(())
}

fn files_dir() -> Result<PathBuf> {
    xbtune_core::env::Dirs::detect()
        .map(|d| d.files)
        .ok_or_else(|| anyhow!("no files folder (HOME not set?)"))
}

fn bike(a: BikeArgs) -> Result<()> {
    let catalog = Catalog::builtin()?;
    let id = a.ecm.trim().to_ascii_uppercase();
    let ecm =
        catalog.lookup(&id).ok_or_else(|| anyhow!("unknown ECM ID {id} (see `xbtune.cli ecms`)"))?.clone();
    if a.models {
        println!("{id} fits: {}", ecm.bikes.as_deref().unwrap_or("unknown"));
        for m in bike::models_for(&ecm) {
            println!("  {:<28} {} cc stock", m.name, m.cc);
        }
        return Ok(());
    }
    let files = files_dir()?;
    let mut all = Bikes::load(&files);
    let before = all.get(&ecm);
    let mut p = before.clone();
    let text = |v: &Option<String>, f: &mut String| {
        if let Some(v) = v {
            *f = v.trim().to_string();
        }
    };
    if let Some(m) = &a.model {
        p.model = bike::model(m.trim()).map_or_else(|| m.trim().to_string(), |m| m.name.to_string());
    }
    if let Some(y) = a.year {
        p.year = (y > 0).then_some(y);
    }
    if let Some(cc) = a.big_bore_cc {
        p.mods.big_bore_cc = (cc > 0).then_some(cc);
    }
    text(&a.wideband, &mut p.mods.wideband);
    text(&a.exhaust, &mut p.mods.exhaust);
    text(&a.intake, &mut p.mods.intake);
    text(&a.cams, &mut p.mods.cams);
    text(&a.ecm_tune, &mut p.mods.ecm_tune);
    if let Some(h) = a.ported_heads {
        p.mods.ported_heads = h;
    }
    if let Some(fd) = &a.final_drive {
        let parts: Vec<Option<u32>> = fd.split('/').map(|x| x.trim().parse().ok()).collect();
        (p.mods.front_pulley, p.mods.rear_pulley) = match parts.as_slice() {
            [Some(f), Some(r)] => (Some(*f), Some(*r)),
            _ if fd.trim().is_empty() => (None, None),
            _ => bail!("--final-drive is FRONT/REAR teeth, e.g. 27/64"),
        };
    }
    if let Some(o) = a.octane {
        p.mods.fuel_octane = (o > 0).then_some(o);
    }
    if let Some(alt) = a.altitude_m {
        p.altitude_m = (alt > 0).then_some(alt);
    }
    p.mods.other.extend(a.add_mod.iter().map(|m| m.trim().to_string()).filter(|m| !m.is_empty()));
    p.mods.other.retain(|m| !a.remove_mod.iter().any(|r| r.trim().eq_ignore_ascii_case(m)));
    if let Some(n) = &a.set_notes {
        p.notes = n.trim().to_string();
    }
    for n in &a.note {
        p.add_note(n);
    }
    if p != before {
        all.set(&id, p.clone());
        all.save(&files)?;
        eprintln!("Saved {}", Bikes::path(&files).display());
    }
    let title = p.title();
    println!("Bike:   {}", if title.is_empty() { "not set (--model, --year)".to_string() } else { title });
    println!("ECM:    {id}, fits {}", ecm.bikes.as_deref().unwrap_or("?"));
    let mods = p.mod_lines();
    if mods.is_empty() {
        println!("Mods:   none (stock)");
    } else {
        for (i, m) in mods.iter().enumerate() {
            println!("{}{m}", if i == 0 { "Mods:   " } else { "        " });
        }
    }
    if let Some(alt) = p.altitude_m {
        println!("Usually rides at {alt} m");
    }
    if !p.notes.trim().is_empty() {
        println!("Notes:\n{}", p.notes.trim());
    }
    Ok(())
}

fn ask(a: AskArgs) -> Result<()> {
    let c = load()?;
    let catalog = Catalog::builtin()?;
    let mut tune = Tune::open(&a.tune, &catalog).with_context(|| format!("opening {}", a.tune.display()))?;
    tune.var(&a.map).map_err(|e| anyhow!("{e}"))?;
    if a.output.as_ref() == Some(&a.tune) {
        bail!("-o is the input file; save the changes to a new file");
    }
    let logs: Vec<Log> =
        a.logs.iter().map(|p| Log::open(p, &catalog).map_err(anyhow::Error::from)).collect::<Result<_>>()?;
    let mut data = context::data_block(&tune, &a.map, &logs);
    let files = files_dir()?;
    let profile = Bikes::load(&files).get(&tune.ecm);
    let recent = memory::recent(&files, &tune.ecm.id, None);
    data["memory"] = memory::block(profile.to_json(&tune.ecm), &recent);
    let turns = [Turn { role: Role::User, text: context::user_message(&a.question, &data) }];
    let p = client::prepare(&c, context::SYSTEM_PROMPT, &turns)?;
    if a.show_request {
        println!("POST {}\n{}\n", p.url, p.body);
    }
    let place = if p.local { "this computer".to_string() } else { p.host.clone() };
    eprintln!(
        "Request: {} bytes (about {} tokens) to {place}, model {}.",
        p.body.len(),
        p.approx_tokens,
        p.model
    );
    eprintln!(
        "It holds your question, the {} map and its axes, AFV, log statistics, your bike profile and {} earlier conversation(s) (no VIN, names or paths).",
        a.map,
        recent.len()
    );
    if !a.yes {
        eprint!("Type 'send' to send it, anything else cancels: ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().lock().read_line(&mut line)?;
        if line.trim() != "send" {
            bail!("cancelled; nothing was sent");
        }
    }
    let journal = xbtune_core::env::Dirs::detect().map(|d| Journal::new(&d.files));
    if let Some(j) = &journal {
        j.request(&p).ok();
    }
    eprintln!("Waiting for the answer (up to {} s)...", c.timeout_s);
    let r = client::send(&c, &p);
    if let Some(j) = &journal {
        j.reply(&r).ok();
    }
    let r = r?;
    // The answer without its JSON block (shown as a table below instead).
    let words = r.text.split("```json").next().unwrap_or(&r.text).trim();
    println!("{words}");
    println!();
    let (baseline, base_from) = checker::baseline(&files, &tune, &catalog);
    let checked = checker::parse_changes(&r.text).map(|s| {
        let cx = checker::Context {
            tune: &tune,
            baseline: &baseline,
            logs: &logs,
            allow_no_data: a.allow_no_data,
        };
        checker::check(&s, &cx, &checker::Limits::default())
    });
    let mut accepted = Vec::new();
    match &checked {
        Err(e) => println!("The suggested changes couldn't be read: {e}"),
        Ok((ok, no)) if ok.is_empty() && no.is_empty() => {}
        Ok((ok, no)) => {
            println!("Changes that pass XBTune's checks (drift measured from {base_from}):");
            for c in ok {
                let at = match (c.load, c.rpm) {
                    (Some(l), Some(r)) => format!("load {l:>4} rpm {r:>5}"),
                    _ => format!("row {} col {}", c.row, c.col),
                };
                let flags =
                    if c.flags.is_empty() { String::new() } else { format!("  [{}]", c.flags.join("; ")) };
                println!("  {:<15} {at}: {:>6.1} -> {:>6.1}  {}{flags}", c.map, c.old, c.new, c.why);
            }
            if ok.is_empty() {
                println!("  none");
            }
            if !no.is_empty() {
                println!("Refused:");
                for x in no {
                    println!("  {}: {}", x.what, x.reason);
                }
            }
            accepted = ok.clone();
        }
    }
    println!();
    if r.truncated() {
        println!("(The answer was cut off by the output limit.)");
    }
    if r.input_cut(&p) {
        println!("WARNING: {}", client::INPUT_CUT_HINT);
    }
    let usage = match (r.input_tokens, r.output_tokens) {
        (Some(i), Some(o)) => format!("{i} tokens in, {o} out"),
        _ => "token use not reported".into(),
    };
    println!("-- {} · {usage}", r.model.as_deref().unwrap_or(&p.model));
    if let Some(j) = &journal {
        println!("-- saved in {}", j.path.display());
    }
    for s in context::suggested_notes(&r.text) {
        println!(
            "-- suggested note: xbtune.cli ai bike --ecm {} --note \"{}\"",
            tune.ecm.id,
            s.replace('"', "'")
        );
    }
    match (&a.output, accepted.is_empty()) {
        (Some(out), false) => {
            let model = r.model.clone().unwrap_or_else(|| p.model.clone());
            let n = checker::apply(&mut tune, &accepted, &model).map_err(|e| anyhow!(e))?;
            tune.save(out, &catalog)?;
            println!("-- {n} cell(s) changed; saved {}. Nothing was written to the ECM.", out.display());
        }
        (Some(_), true) => println!("-- no changes passed; nothing saved."),
        (None, false) => println!("-- add -o FILE to save these changes into a new tune."),
        (None, true) => {}
    }
    println!("-- {}", xbtune_core::disclaimer::SHORT);
    Ok(())
}
