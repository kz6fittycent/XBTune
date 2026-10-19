// SPDX-License-Identifier: GPL-3.0-or-later
//! `ai presets|setup|models|test|ask`: the AI tuning advisor from the
//! terminal (docs/AI.md). Stage A1: answers in words; nothing changes a
//! tune or the ECM. `ask` shows where the request goes and asks before
//! sending (`--yes` skips that; `--show-request` prints the whole body).

use std::io::{BufRead, Write as _};
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow, bail};
use clap::{Args, Subcommand};
use xbtune_ai::client::{self, Role, Turn};
use xbtune_ai::config::{self, Adapter, AiConfig};
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

fn ask(a: AskArgs) -> Result<()> {
    let c = load()?;
    let catalog = Catalog::builtin()?;
    let tune = Tune::open(&a.tune, &catalog).with_context(|| format!("opening {}", a.tune.display()))?;
    tune.var(&a.map).map_err(|e| anyhow!("{e}"))?;
    let logs: Vec<Log> =
        a.logs.iter().map(|p| Log::open(p, &catalog).map_err(anyhow::Error::from)).collect::<Result<_>>()?;
    let data = context::data_block(&tune, &a.map, &logs);
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
        "It holds your question, the {} map and its axes, AFV, and log statistics (no VIN, names or paths).",
        a.map
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
    println!("{}", r.text.trim());
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
    println!("-- {}", xbtune_core::disclaimer::SHORT);
    Ok(())
}
