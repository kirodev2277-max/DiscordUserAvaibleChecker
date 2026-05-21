//! `dua` — Discord unique-username availability CLI.
//!
//! Subcommands:
//!
//! - `check`    — verify one or more handles (positional and/or from a file).
//! - `generate` — write candidate 3/4-letter handles to a text file.
//! - `hunt`     — generate + check in a single streaming pipeline.
//! - `watch`    — poll a single handle on an interval until it frees up.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use console::{style, Term};
use indicatif::{ProgressBar, ProgressStyle};

use dua_checker::checker::{Checker, CheckerConfig};
use dua_checker::generator::{generate, Charset, GenMode, GenerateConfig, Length};
use dua_checker::persistence::{append_jsonl_log, append_text_log, load_usernames_file};
use dua_checker::result::{CheckResult, Status};

const DEFAULT_OUTPUT: &str = "results.txt";
const DEFAULT_GEN_OUTPUT: &str = "usernames.txt";

#[derive(Parser, Debug)]
#[command(
    name = "dua",
    version,
    about = "Discord unique-username availability checker",
    long_about = None,
    propagate_version = true,
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Check one or more handles against Discord.
    Check(CheckArgs),
    /// Generate a candidate handle list (3 or 4 letters only) to a file.
    Generate(GenerateArgs),
    /// Generate candidates and stream them through the checker in one go.
    Hunt(HuntArgs),
    /// Poll a single handle on an interval until it becomes available.
    Watch(WatchArgs),
}

#[derive(Args, Debug)]
struct CheckArgs {
    /// Handles to check (whitespace or comma separated).
    usernames: Vec<String>,

    /// Read additional handles from a text file (one per line or comma-separated).
    #[arg(long, short = 'i')]
    input: Option<PathBuf>,

    /// Concurrent in-flight requests.
    #[arg(long, default_value_t = 8)]
    workers: usize,

    /// Stagger requests by this many milliseconds (anti-rate-limit).
    #[arg(long, default_value_t = 0)]
    delay_ms: u64,

    /// Append results to this text-log file (legacy-compatible format).
    #[arg(long, short = 'o', default_value = DEFAULT_OUTPUT)]
    output: PathBuf,

    /// Also append results as JSON Lines to this file.
    #[arg(long)]
    jsonl: Option<PathBuf>,

    /// Don't write the text log.
    #[arg(long, conflicts_with = "output")]
    no_save: bool,

    /// Only print one line per result (suppress the progress bar).
    #[arg(long)]
    quiet: bool,
}

#[derive(Args, Debug)]
struct GenerateArgs {
    /// Handle length (3 or 4).
    #[arg(long, short = 'n', value_parser = parse_length, default_value = "4")]
    length: Length,

    /// Generation strategy.
    #[arg(long, value_enum, default_value_t = ModeArg::Random)]
    mode: ModeArg,

    /// Character pool.
    #[arg(long, value_enum, default_value_t = CharsetArg::Letters)]
    charset: CharsetArg,

    /// How many to emit. Defaults to 1000 for `random`, full space for `all`.
    #[arg(long, short = 'c')]
    count: Option<usize>,

    /// RNG seed for reproducible random output.
    #[arg(long)]
    seed: Option<u64>,

    /// File to write to.
    #[arg(long, short = 'o', default_value = DEFAULT_GEN_OUTPUT)]
    output: PathBuf,

    /// Append instead of overwriting.
    #[arg(long)]
    append: bool,
}

#[derive(Args, Debug)]
struct HuntArgs {
    /// Handle length (3 or 4).
    #[arg(long, short = 'n', value_parser = parse_length, default_value = "4")]
    length: Length,

    /// Generation strategy.
    #[arg(long, value_enum, default_value_t = ModeArg::Random)]
    mode: ModeArg,

    /// Character pool.
    #[arg(long, value_enum, default_value_t = CharsetArg::Letters)]
    charset: CharsetArg,

    /// Candidate count. Defaults to 1000 random / full space for `all`.
    #[arg(long, short = 'c')]
    count: Option<usize>,

    /// RNG seed (random mode).
    #[arg(long)]
    seed: Option<u64>,

    /// Concurrent in-flight requests.
    #[arg(long, default_value_t = 16)]
    workers: usize,

    /// Stagger requests by this many milliseconds.
    #[arg(long, default_value_t = 25)]
    delay_ms: u64,

    /// Append results to this text-log file.
    #[arg(long, short = 'o', default_value = DEFAULT_OUTPUT)]
    output: PathBuf,

    /// Also stream only AVAILABLE hits to this file as they happen.
    #[arg(long)]
    hits_file: Option<PathBuf>,

    /// Stop after this many AVAILABLE hits (useful with `all` mode).
    #[arg(long)]
    stop_after: Option<usize>,

    /// Suppress non-result chatter.
    #[arg(long)]
    quiet: bool,
}

#[derive(Args, Debug)]
struct WatchArgs {
    /// Handle to watch.
    username: String,

    /// Seconds between polls.
    #[arg(long, short = 'i', default_value_t = 30)]
    interval_secs: u64,

    /// Stop after this many attempts. 0 = unlimited.
    #[arg(long, default_value_t = 0)]
    max_attempts: u32,

    /// Ring the terminal bell when it becomes available.
    #[arg(long, default_value_t = true)]
    bell: bool,
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum ModeArg {
    All,
    Random,
}

impl From<ModeArg> for GenMode {
    fn from(value: ModeArg) -> Self {
        match value {
            ModeArg::All => GenMode::All,
            ModeArg::Random => GenMode::Random,
        }
    }
}

#[derive(Copy, Clone, Debug, ValueEnum)]
enum CharsetArg {
    Letters,
    Alnum,
}

impl From<CharsetArg> for Charset {
    fn from(value: CharsetArg) -> Self {
        match value {
            CharsetArg::Letters => Charset::Letters,
            CharsetArg::Alnum => Charset::Alnum,
        }
    }
}

impl CharsetArg {
    #[allow(dead_code)]
    fn label(self) -> &'static str {
        Charset::from(self).label()
    }
}

fn parse_length(s: &str) -> std::result::Result<Length, String> {
    match s.trim() {
        "3" => Ok(Length::Three),
        "4" => Ok(Length::Four),
        other => Err(format!("length must be 3 or 4 (got {other:?})")),
    }
}

fn main() {
    let exit_code = match real_main() {
        Ok(code) => code,
        Err(err) => {
            eprintln!("{}: {:#}", style("error").red().bold(), err);
            2
        }
    };
    std::process::exit(exit_code);
}

fn real_main() -> Result<i32> {
    let cli = Cli::parse();

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("building tokio runtime")?;

    runtime.block_on(async move {
        match cli.command {
            Command::Check(args) => cmd_check(args).await,
            Command::Generate(args) => cmd_generate(args),
            Command::Hunt(args) => cmd_hunt(args).await,
            Command::Watch(args) => cmd_watch(args).await,
        }
    })
}

async fn cmd_check(args: CheckArgs) -> Result<i32> {
    if args.workers == 0 {
        bail!("--workers must be >= 1");
    }

    let mut names: Vec<String> = args.usernames;
    if let Some(path) = &args.input {
        names.extend(
            load_usernames_file(path)
                .with_context(|| format!("reading --input {}", path.display()))?,
        );
    }
    if names.is_empty() {
        bail!("no usernames given; pass names as args or use --input");
    }

    let cfg = CheckerConfig {
        workers: args.workers,
        delay: Duration::from_millis(args.delay_ms),
        ..Default::default()
    };
    let checker = Checker::new(cfg).context("building HTTP client")?;

    let total = names.len();
    let bar = if args.quiet {
        None
    } else {
        Some(progress_bar(total as u64))
    };
    let term = Term::stdout();
    let counter = Arc::new(AtomicUsize::new(0));

    let progress: Option<dua_checker::checker::BatchProgress> = if args.quiet {
        None
    } else {
        let term = term.clone();
        let counter = counter.clone();
        let bar = bar.clone();
        Some(Arc::new(move |_idx, result: &CheckResult| {
            let _ = term.write_line(&render_line(result));
            let done = counter.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(b) = &bar {
                b.set_position(done as u64);
            }
        }))
    };

    let results = checker.run_batch(names, progress, None).await;

    if let Some(b) = &bar {
        b.finish_and_clear();
    }

    let summary = summarize(&results);
    println!("\n{}", summary.styled());

    if !args.no_save {
        let path = &args.output;
        let written = append_text_log(path, results.iter().cloned())
            .with_context(|| format!("writing {}", path.display()))?;
        if written > 0 {
            println!("Appended {written} line(s) to {}", path.display());
        }
    }
    if let Some(path) = &args.jsonl {
        let written = append_jsonl_log(path, results.iter().cloned())
            .with_context(|| format!("writing JSONL {}", path.display()))?;
        if written > 0 {
            println!("Appended {written} JSON record(s) to {}", path.display());
        }
    }

    Ok(if summary.errors > 0 { 2 } else { 0 })
}

fn cmd_generate(args: GenerateArgs) -> Result<i32> {
    let cfg = GenerateConfig {
        length: args.length,
        charset: args.charset.into(),
        mode: args.mode.into(),
        count: args.count,
        seed: args.seed,
    };
    let space = cfg.space_size();
    let items = generate(&cfg);
    let n = items.len();

    let path = args.output;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .append(args.append)
        .truncate(!args.append)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    use std::io::Write;
    for item in &items {
        writeln!(file, "{item}")?;
    }

    let verb = if args.append { "Appended" } else { "Wrote" };
    eprintln!(
        "{verb} {} handles (out of {} possible) to {}",
        style(n).cyan().bold(),
        space,
        path.display()
    );
    Ok(0)
}

async fn cmd_hunt(args: HuntArgs) -> Result<i32> {
    if args.workers == 0 {
        bail!("--workers must be >= 1");
    }

    let cfg = GenerateConfig {
        length: args.length,
        charset: args.charset.into(),
        mode: args.mode.into(),
        count: args.count,
        seed: args.seed,
    };
    let names = generate(&cfg);
    if names.is_empty() {
        bail!("no candidates generated");
    }

    let total = names.len();
    if !args.quiet {
        let charset: Charset = args.charset.into();
        eprintln!(
            "Hunting {total} {}-letter handle(s) over {} ({} workers, {}ms stagger)",
            args.length.as_usize(),
            charset.label(),
            args.workers,
            args.delay_ms
        );
    }

    let checker_cfg = CheckerConfig {
        workers: args.workers,
        delay: Duration::from_millis(args.delay_ms),
        ..Default::default()
    };
    let checker = Checker::new(checker_cfg)?;

    let bar = if args.quiet {
        None
    } else {
        Some(progress_bar(total as u64))
    };
    let term = Term::stdout();

    let hits_path = args.hits_file.clone();
    let hits_file: Arc<std::sync::Mutex<Option<std::fs::File>>> =
        Arc::new(std::sync::Mutex::new(match &hits_path {
            Some(p) => Some(
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(p)
                    .with_context(|| format!("opening hits file {}", p.display()))?,
            ),
            None => None,
        }));

    let counter = Arc::new(AtomicUsize::new(0));
    let hits_count = Arc::new(AtomicUsize::new(0));
    let stop_after = args.stop_after;

    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let progress: dua_checker::checker::BatchProgress = {
        let term = term.clone();
        let bar = bar.clone();
        let counter = counter.clone();
        let hits_count = hits_count.clone();
        let hits_file = hits_file.clone();
        let cancel = cancel.clone();
        Arc::new(move |_idx, result: &CheckResult| {
            let done = counter.fetch_add(1, Ordering::SeqCst) + 1;
            if let Some(b) = &bar {
                b.set_position(done as u64);
            }
            if result.status.is_available() {
                let _ = term.write_line(&render_line(result));
                let hc = hits_count.fetch_add(1, Ordering::SeqCst) + 1;
                if let Some(file) = hits_file.lock().unwrap().as_mut() {
                    use std::io::Write;
                    let _ = writeln!(file, "{}", result.username);
                }
                if let Some(limit) = stop_after {
                    if hc >= limit {
                        cancel.store(true, Ordering::SeqCst);
                        if let Some(b) = &bar {
                            b.finish_with_message(format!("stop-after hit at {hc}"));
                        }
                    }
                }
            }
        })
    };

    let results = checker.run_batch(names, Some(progress), Some(cancel)).await;
    if let Some(b) = &bar {
        b.finish_and_clear();
    }

    let summary = summarize(&results);
    println!("\n{}", summary.styled());

    let written = append_text_log(&args.output, results.iter().cloned())
        .with_context(|| format!("writing {}", args.output.display()))?;
    if written > 0 {
        println!("Appended {written} line(s) to {}", args.output.display());
    }
    if let Some(p) = hits_path {
        println!(
            "{} available hit(s) streamed to {}",
            hits_count.load(Ordering::SeqCst),
            p.display()
        );
    }

    Ok(if summary.errors > 0 { 2 } else { 0 })
}

async fn cmd_watch(args: WatchArgs) -> Result<i32> {
    let username = args.username.trim().to_string();
    if username.is_empty() {
        bail!("username must not be empty");
    }

    let checker = Checker::new(CheckerConfig {
        workers: 1,
        ..Default::default()
    })?;
    let interval = Duration::from_secs(args.interval_secs.max(1));
    let term = Term::stdout();

    let mut attempt: u32 = 0;
    loop {
        attempt += 1;
        let res = checker.check(&username).await;
        let _ = term.write_line(&format!(
            "[{}] {}",
            style(format!("#{attempt}")).dim(),
            render_line(&res)
        ));
        if res.status.is_available() {
            if args.bell {
                // Terminal bell.
                let _ = term.write_str("\x07");
            }
            return Ok(0);
        }
        if args.max_attempts > 0 && attempt >= args.max_attempts {
            eprintln!("reached --max-attempts ({}); giving up", args.max_attempts);
            return Ok(1);
        }
        tokio::time::sleep(interval).await;
    }
}

fn render_line(result: &CheckResult) -> String {
    match &result.status {
        Status::Available => format!(
            "{} {}: {}",
            style("[+]").green().bold(),
            result.username,
            style("AVAILABLE").green().bold()
        ),
        Status::Taken => format!(
            "{} {}: {}",
            style("[-]").red(),
            result.username,
            style("taken").red()
        ),
        Status::Invalid { reason } => format!(
            "{} {}: {} ({reason})",
            style("[!]").yellow(),
            result.username,
            style("invalid").yellow()
        ),
        Status::RateLimited { retry_after_ms } => {
            let detail = retry_after_ms
                .map(|ms| format!(" (retry after {:.2}s)", ms as f64 / 1000.0))
                .unwrap_or_default();
            format!(
                "{} {}: {}{detail}",
                style("[?]").magenta(),
                result.username,
                style("rate limited").magenta()
            )
        }
        Status::Error { reason } => format!(
            "{} {}: {} ({reason})",
            style("[?]").red().dim(),
            result.username,
            style("error").red().dim()
        ),
    }
}

#[derive(Default)]
struct Summary {
    available: usize,
    taken: usize,
    invalid: usize,
    rate_limited: usize,
    errors: usize,
}

impl Summary {
    fn styled(&self) -> String {
        format!(
            "{}: {} available · {} taken · {} invalid · {} rate-limited · {} error",
            style("summary").bold(),
            style(self.available).green().bold(),
            style(self.taken).red(),
            style(self.invalid).yellow(),
            style(self.rate_limited).magenta(),
            style(self.errors).red().dim(),
        )
    }
}

fn summarize(results: &[CheckResult]) -> Summary {
    let mut s = Summary::default();
    for r in results {
        match &r.status {
            Status::Available => s.available += 1,
            Status::Taken => s.taken += 1,
            Status::Invalid { .. } => s.invalid += 1,
            Status::RateLimited { .. } => s.rate_limited += 1,
            Status::Error { .. } => s.errors += 1,
        }
    }
    s
}

fn progress_bar(total: u64) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.cyan} [{elapsed_precise}] [{bar:32.cyan/blue}] {pos}/{len} ({eta})",
        )
        .unwrap()
        .progress_chars("=> "),
    );
    pb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_length_accepts_3_and_4() {
        assert!(matches!(parse_length("3").unwrap(), Length::Three));
        assert!(matches!(parse_length("4").unwrap(), Length::Four));
        assert!(parse_length("5").is_err());
        assert!(parse_length("a").is_err());
    }

    #[test]
    fn ensure_parse_usernames_text_works() {
        // Sanity-check the lib re-export.
        let names = parse_usernames_text("a, b\nc");
        assert_eq!(names, vec!["a", "b", "c"]);
    }
}
