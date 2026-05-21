//! `dua-gui` — eframe/egui dark-themed desktop GUI for the checker.
//!
//! Two modes selectable from a top tab bar:
//!
//! - **Check**: paste/load a username list and check it concurrently.
//! - **Hunt**:  generate 3- or 4-letter candidates and stream them
//!   through the checker, surfacing every AVAILABLE hit live.
//!
//! Long-running work happens on a tokio runtime owned by the app;
//! the runtime communicates back via a `mpsc::channel` and the UI
//! polls it inside `update`.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{self, Color32, RichText, ScrollArea, TextEdit};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use dua_checker::checker::{Checker, CheckerConfig};
use dua_checker::generator::{
    generate, Charset, GenMode, GenerateConfig, Length, SPACE_3_LETTERS, SPACE_4_LETTERS,
};
use dua_checker::persistence::{append_text_log, parse_usernames_text};
use dua_checker::result::{CheckResult, Status};

const ACCENT: Color32 = Color32::from_rgb(88, 101, 242); // Discord blurple
const OK_GREEN: Color32 = Color32::from_rgb(67, 181, 129);
const ERR_RED: Color32 = Color32::from_rgb(240, 71, 71);
const WARN_AMBER: Color32 = Color32::from_rgb(245, 166, 35);
const MUTED: Color32 = Color32::from_rgb(150, 152, 157);
const RATE_PURPLE: Color32 = Color32::from_rgb(170, 110, 232);

const DEFAULT_OUTPUT: &str = "results.txt";
const HUNT_VISIBLE_ROW_CAP: usize = 4096;

fn main() -> eframe::Result {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to build tokio runtime");
    let handle = runtime.handle().clone();
    // Hold the runtime for the lifetime of the GUI.
    let _runtime_guard = runtime;

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([920.0, 680.0])
            .with_min_inner_size([720.0, 520.0])
            .with_title("DUA — Discord Username Checker"),
        ..Default::default()
    };

    eframe::run_native(
        "DUA Checker",
        options,
        Box::new(move |cc| {
            install_dark_theme(&cc.egui_ctx);
            Ok(Box::<App>::new(App::new(handle.clone())))
        }),
    )
}

fn install_dark_theme(ctx: &egui::Context) {
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::from_rgb(24, 25, 28);
    visuals.window_fill = Color32::from_rgb(24, 25, 28);
    visuals.extreme_bg_color = Color32::from_rgb(16, 17, 19);
    visuals.faint_bg_color = Color32::from_rgb(31, 33, 37);
    visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(24, 25, 28);
    visuals.widgets.inactive.bg_fill = Color32::from_rgb(40, 42, 47);
    visuals.widgets.hovered.bg_fill = Color32::from_rgb(54, 57, 63);
    visuals.widgets.active.bg_fill = Color32::from_rgb(64, 68, 75);
    visuals.selection.bg_fill = ACCENT.linear_multiply(0.6);
    visuals.hyperlink_color = ACCENT;
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 6.0);
    ctx.set_style(style);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Check,
    Hunt,
}

#[derive(Clone)]
struct Row {
    username: String,
    status: Option<Status>,
}

impl Row {
    fn pending(username: String) -> Self {
        Self {
            username,
            status: None,
        }
    }
}

#[derive(Default, Clone, Copy)]
struct Counts {
    available: usize,
    taken: usize,
    invalid: usize,
    rate_limited: usize,
    errors: usize,
}

impl Counts {
    fn total(&self) -> usize {
        self.available + self.taken + self.invalid + self.rate_limited + self.errors
    }
}

enum Msg {
    Result { idx: usize, result: CheckResult },
    Hit(CheckResult),
    Done,
    Error(String),
}

struct RunState {
    cancel: Arc<AtomicBool>,
    total: usize,
    done: Arc<AtomicUsize>,
    rx: UnboundedReceiver<Msg>,
    started_at: std::time::Instant,
}

struct App {
    runtime: tokio::runtime::Handle,
    tab: Tab,

    // Check tab inputs
    check_input: String,

    // Hunt tab inputs
    hunt_length: Length,
    hunt_mode: GenMode,
    hunt_charset: Charset,
    hunt_count_text: String,
    hunt_seed_text: String,
    hunt_stop_after_text: String,

    // Common run config
    workers_text: String,
    delay_ms_text: String,
    output_path: String,
    autosave: bool,

    // State
    rows: Vec<Row>,
    counts: Counts,
    status_line: String,
    last_error: Option<String>,
    running: Option<RunState>,

    // Filter chips
    show_available: bool,
    show_taken: bool,
    show_other: bool,
}

impl App {
    fn new(runtime: tokio::runtime::Handle) -> Self {
        Self {
            runtime,
            tab: Tab::Check,
            check_input: String::new(),
            hunt_length: Length::Four,
            hunt_mode: GenMode::Random,
            hunt_charset: Charset::Letters,
            hunt_count_text: "500".into(),
            hunt_seed_text: String::new(),
            hunt_stop_after_text: String::new(),
            workers_text: "8".into(),
            delay_ms_text: "0".into(),
            output_path: DEFAULT_OUTPUT.into(),
            autosave: true,
            rows: Vec::new(),
            counts: Counts::default(),
            status_line: "Ready.".into(),
            last_error: None,
            running: None,
            show_available: true,
            show_taken: true,
            show_other: true,
        }
    }

    fn is_busy(&self) -> bool {
        self.running.is_some()
    }

    fn start_check(&mut self) {
        let names = parse_usernames_text(&self.check_input);
        if names.is_empty() {
            self.last_error = Some("Add at least one handle before checking.".into());
            return;
        }
        let workers = self.parse_workers();
        let delay = self.parse_delay();

        self.rows = names.iter().cloned().map(Row::pending).collect();
        self.counts = Counts::default();
        self.last_error = None;
        self.status_line = format!("Checking 0/{}…", names.len());

        let (tx, rx) = unbounded_channel::<Msg>();
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicUsize::new(0));
        let total = names.len();

        self.running = Some(RunState {
            cancel: cancel.clone(),
            total,
            done: done.clone(),
            rx,
            started_at: std::time::Instant::now(),
        });

        spawn_check_run(
            self.runtime.clone(),
            CheckerConfig {
                workers,
                delay,
                ..Default::default()
            },
            names,
            tx,
            cancel,
            done,
            false,
        );
    }

    fn start_hunt(&mut self) {
        let count = self
            .hunt_count_text
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0);
        let seed = self.hunt_seed_text.trim().parse::<u64>().ok();
        let stop_after = self
            .hunt_stop_after_text
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0);

        let cfg = GenerateConfig {
            length: self.hunt_length,
            charset: self.hunt_charset,
            mode: self.hunt_mode,
            count,
            seed,
        };

        let names = generate(&cfg);
        if names.is_empty() {
            self.last_error = Some("Generator produced 0 candidates.".into());
            return;
        }

        let workers = self.parse_workers();
        let delay = self.parse_delay();

        self.rows.clear();
        self.counts = Counts::default();
        self.last_error = None;
        self.status_line = format!(
            "Hunting {} {}-letter handle(s) over {}…",
            names.len(),
            self.hunt_length.as_usize(),
            self.hunt_charset.label()
        );

        let (tx, rx) = unbounded_channel::<Msg>();
        let cancel = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicUsize::new(0));
        let total = names.len();

        self.running = Some(RunState {
            cancel: cancel.clone(),
            total,
            done: done.clone(),
            rx,
            started_at: std::time::Instant::now(),
        });

        spawn_hunt_run(
            self.runtime.clone(),
            CheckerConfig {
                workers,
                delay,
                ..Default::default()
            },
            names,
            tx,
            cancel,
            done,
            stop_after,
        );
    }

    fn parse_workers(&self) -> usize {
        self.workers_text
            .trim()
            .parse::<usize>()
            .unwrap_or(8)
            .max(1)
    }

    fn parse_delay(&self) -> Duration {
        Duration::from_millis(self.delay_ms_text.trim().parse::<u64>().unwrap_or(0))
    }

    fn drain_messages(&mut self, ctx: &egui::Context) {
        let Some(state) = self.running.as_mut() else {
            return;
        };
        let mut finished = false;
        let mut last_error = None;

        loop {
            match state.rx.try_recv() {
                Ok(Msg::Result { idx, result }) => {
                    self.counts.bump(&result.status);
                    if let Some(slot) = self.rows.get_mut(idx) {
                        slot.status = Some(result.status);
                    }
                }
                Ok(Msg::Hit(result)) => {
                    self.counts.bump(&result.status);
                    self.rows.push(Row {
                        username: result.username,
                        status: Some(result.status),
                    });
                    if self.rows.len() > HUNT_VISIBLE_ROW_CAP {
                        let drop = self.rows.len() - HUNT_VISIBLE_ROW_CAP;
                        self.rows.drain(0..drop);
                    }
                }
                Ok(Msg::Done) => {
                    finished = true;
                    break;
                }
                Ok(Msg::Error(e)) => {
                    last_error = Some(e);
                    finished = true;
                    break;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }

        // Keep status text fresh and request a repaint while the run is live.
        let done = state.done.load(Ordering::Relaxed);
        let total = state.total;
        if !finished {
            let elapsed = state.started_at.elapsed().as_secs_f32();
            let rate = if elapsed > 0.0 {
                done as f32 / elapsed
            } else {
                0.0
            };
            self.status_line = format!(
                "Checking {done}/{total} · {} hits · {:.1}/s",
                self.counts.available, rate
            );
            ctx.request_repaint_after(Duration::from_millis(75));
        } else {
            let elapsed = state.started_at.elapsed().as_secs_f32();
            let summary = format!(
                "Done in {:.1}s · {} available · {} taken · {} invalid · {} rate-limited · {} errors",
                elapsed,
                self.counts.available,
                self.counts.taken,
                self.counts.invalid,
                self.counts.rate_limited,
                self.counts.errors
            );
            self.status_line = summary;

            if self.autosave {
                if let Err(err) = self.persist_results() {
                    self.last_error = Some(format!("autosave failed: {err}"));
                }
            }

            if let Some(e) = last_error {
                self.last_error = Some(e);
            }
            self.running = None;
        }
    }

    fn persist_results(&self) -> std::io::Result<usize> {
        let materialized: Vec<CheckResult> = self
            .rows
            .iter()
            .filter_map(|r| {
                r.status
                    .clone()
                    .map(|s| CheckResult::new(r.username.clone(), s))
            })
            .collect();
        if materialized.is_empty() {
            return Ok(0);
        }
        append_text_log(&self.output_path, materialized)
    }

    fn cancel_run(&mut self) {
        if let Some(state) = &self.running {
            state.cancel.store(true, Ordering::SeqCst);
            self.status_line = "Cancellation requested…".into();
        }
    }

    fn clear(&mut self) {
        if self.is_busy() {
            return;
        }
        self.rows.clear();
        self.counts = Counts::default();
        self.last_error = None;
        self.check_input.clear();
        self.status_line = "Ready.".into();
    }

    fn load_from_file(&mut self) {
        if let Some(path) = rfd_pick_file() {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    if !self.check_input.is_empty() && !self.check_input.ends_with('\n') {
                        self.check_input.push('\n');
                    }
                    self.check_input.push_str(&text);
                    self.tab = Tab::Check;
                }
                Err(err) => self.last_error = Some(format!("could not read {path}: {err}")),
            }
        }
    }

    fn save_now(&mut self) {
        match self.persist_results() {
            Ok(0) => self.last_error = Some("No completed rows to save yet.".into()),
            Ok(n) => self.status_line = format!("Saved {n} row(s) to {}", self.output_path),
            Err(err) => self.last_error = Some(format!("save failed: {err}")),
        }
    }
}

impl Counts {
    fn bump(&mut self, status: &Status) {
        match status {
            Status::Available => self.available += 1,
            Status::Taken => self.taken += 1,
            Status::Invalid { .. } => self.invalid += 1,
            Status::RateLimited { .. } => self.rate_limited += 1,
            Status::Error { .. } => self.errors += 1,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_messages(ctx);

        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.heading(RichText::new("DUA").color(ACCENT).strong());
                ui.label(RichText::new("Discord Username Checker").color(MUTED));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).color(MUTED));
                });
            });
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.tab == Tab::Check, "  Check  ")
                    .clicked()
                {
                    self.tab = Tab::Check;
                }
                if ui
                    .selectable_label(self.tab == Tab::Hunt, "  Hunt 3/4-letter  ")
                    .clicked()
                {
                    self.tab = Tab::Hunt;
                }
            });
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("status_bar").show(ctx, |ui| {
            ui.add_space(2.0);
            ui.horizontal(|ui| {
                if let Some(state) = &self.running {
                    let pct = if state.total == 0 {
                        0.0
                    } else {
                        state.done.load(Ordering::Relaxed) as f32 / state.total as f32
                    };
                    ui.add(
                        egui::ProgressBar::new(pct)
                            .desired_width(220.0)
                            .show_percentage(),
                    );
                }
                ui.label(RichText::new(&self.status_line).color(MUTED));
                if let Some(err) = &self.last_error {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("⚠ {err}")).color(ERR_RED));
                    });
                }
            });
            ui.add_space(2.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| match self.tab {
            Tab::Check => self.render_check_tab(ui),
            Tab::Hunt => self.render_hunt_tab(ui),
        });
    }
}

impl App {
    fn render_common_run_row(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Workers:");
            ui.add(TextEdit::singleline(&mut self.workers_text).desired_width(46.0));
            ui.separator();
            ui.label("Delay (ms):");
            ui.add(TextEdit::singleline(&mut self.delay_ms_text).desired_width(60.0));
            ui.separator();
            ui.checkbox(&mut self.autosave, "Auto-save");
            ui.add(TextEdit::singleline(&mut self.output_path).desired_width(180.0));
        });
    }

    fn render_check_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Usernames").strong());
        ui.label(
            RichText::new("One per line, comma-separated, or any mix.")
                .color(MUTED)
                .small(),
        );
        ScrollArea::vertical()
            .id_salt("check_input_scroll")
            .max_height(160.0)
            .show(ui, |ui| {
                ui.add(
                    TextEdit::multiline(&mut self.check_input)
                        .desired_width(f32::INFINITY)
                        .desired_rows(6)
                        .hint_text("alice\nbob, charlie\ndan"),
                );
            });

        ui.add_space(6.0);
        self.render_common_run_row(ui);

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let busy = self.is_busy();
            let run_label = if busy { "Running…" } else { "▶  Check" };
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(RichText::new(run_label).strong()).fill(ACCENT),
                )
                .clicked()
            {
                self.start_check();
            }
            if ui.add_enabled(busy, egui::Button::new("■  Stop")).clicked() {
                self.cancel_run();
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Load file…"))
                .clicked()
            {
                self.load_from_file();
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Save now"))
                .clicked()
            {
                self.save_now();
            }
            if ui.add_enabled(!busy, egui::Button::new("Clear")).clicked() {
                self.clear();
            }
        });

        ui.add_space(8.0);
        self.render_counts_strip(ui);
        ui.separator();
        self.render_results_table(ui);
    }

    fn render_hunt_tab(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new("Hunt 3- or 4-letter handles").strong());
        ui.label(
            RichText::new(
                "Generate candidate handles and stream them through the checker. Available hits appear in the table as they happen.",
            )
            .color(MUTED)
            .small(),
        );

        ui.add_space(6.0);
        egui::Grid::new("hunt_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Length:");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.hunt_length, Length::Three, "3 letters");
                    ui.selectable_value(&mut self.hunt_length, Length::Four, "4 letters");
                    let space = match self.hunt_length {
                        Length::Three => SPACE_3_LETTERS,
                        Length::Four => SPACE_4_LETTERS,
                    };
                    let space = match self.hunt_charset {
                        Charset::Letters => space,
                        Charset::Alnum => {
                            let n = self.hunt_length.as_usize();
                            36usize.saturating_pow(n as u32)
                        }
                    };
                    ui.label(
                        RichText::new(format!("(space: {space})"))
                            .color(MUTED)
                            .small(),
                    );
                });
                ui.end_row();

                ui.label("Mode:");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.hunt_mode, GenMode::Random, "random");
                    ui.selectable_value(&mut self.hunt_mode, GenMode::All, "all (exhaustive)");
                });
                ui.end_row();

                ui.label("Charset:");
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.hunt_charset, Charset::Letters, "letters (a-z)");
                    ui.selectable_value(&mut self.hunt_charset, Charset::Alnum, "alnum (a-z, 0-9)");
                });
                ui.end_row();

                ui.label("Count:");
                ui.horizontal(|ui| {
                    ui.add(TextEdit::singleline(&mut self.hunt_count_text).desired_width(90.0));
                    if matches!(self.hunt_mode, GenMode::All) {
                        ui.label(
                            RichText::new("Leave blank for the entire space.")
                                .color(MUTED)
                                .small(),
                        );
                    }
                });
                ui.end_row();

                ui.label("Seed (random):");
                ui.add(TextEdit::singleline(&mut self.hunt_seed_text).desired_width(120.0));
                ui.end_row();

                ui.label("Stop after N hits:");
                ui.add(TextEdit::singleline(&mut self.hunt_stop_after_text).desired_width(90.0));
                ui.end_row();
            });

        ui.add_space(6.0);
        self.render_common_run_row(ui);

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let busy = self.is_busy();
            let label = if busy { "Hunting…" } else { "🎯  Hunt" };
            if ui
                .add_enabled(
                    !busy,
                    egui::Button::new(RichText::new(label).strong()).fill(ACCENT),
                )
                .clicked()
            {
                self.start_hunt();
            }
            if ui.add_enabled(busy, egui::Button::new("■  Stop")).clicked() {
                self.cancel_run();
            }
            if ui
                .add_enabled(!busy, egui::Button::new("Save now"))
                .clicked()
            {
                self.save_now();
            }
            if ui.add_enabled(!busy, egui::Button::new("Clear")).clicked() {
                self.rows.clear();
                self.counts = Counts::default();
                self.status_line = "Ready.".into();
            }
        });

        ui.add_space(8.0);
        self.render_counts_strip(ui);
        ui.separator();
        self.render_results_table(ui);
    }

    fn render_counts_strip(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            chip(
                ui,
                &mut self.show_available,
                "available",
                self.counts.available,
                OK_GREEN,
            );
            chip(
                ui,
                &mut self.show_taken,
                "taken",
                self.counts.taken,
                ERR_RED,
            );
            chip(
                ui,
                &mut self.show_other,
                "invalid",
                self.counts.invalid,
                WARN_AMBER,
            );
            chip(
                ui,
                &mut self.show_other,
                "rate-limited",
                self.counts.rate_limited,
                RATE_PURPLE,
            );
            chip(
                ui,
                &mut self.show_other,
                "errors",
                self.counts.errors,
                MUTED,
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!("{} total", self.counts.total()))
                        .color(MUTED)
                        .small(),
                );
            });
        });
    }

    fn render_results_table(&mut self, ui: &mut egui::Ui) {
        let visible: Vec<&Row> = self
            .rows
            .iter()
            .filter(|r| match r.status.as_ref() {
                None => true,
                Some(Status::Available) => self.show_available,
                Some(Status::Taken) => self.show_taken,
                Some(_) => self.show_other,
            })
            .collect();

        ScrollArea::vertical()
            .id_salt("results_scroll")
            .auto_shrink([false, false])
            .show_rows(ui, 22.0, visible.len(), |ui, range| {
                egui::Grid::new("results_table")
                    .striped(true)
                    .num_columns(3)
                    .min_col_width(80.0)
                    .show(ui, |ui| {
                        for i in range {
                            let row = visible[i];
                            ui.monospace(&row.username);
                            let (label, color, detail) = render_status(row.status.as_ref());
                            ui.label(RichText::new(label).color(color).strong());
                            ui.label(RichText::new(detail).color(MUTED));
                            ui.end_row();
                        }
                    });
            });
    }
}

fn render_status(status: Option<&Status>) -> (&'static str, Color32, String) {
    match status {
        None => ("pending", MUTED, String::new()),
        Some(Status::Available) => ("AVAILABLE", OK_GREEN, String::new()),
        Some(Status::Taken) => ("taken", ERR_RED, String::new()),
        Some(Status::Invalid { reason }) => ("invalid", WARN_AMBER, reason.clone()),
        Some(Status::RateLimited { retry_after_ms }) => {
            let detail = retry_after_ms
                .map(|ms| format!("retry after {:.2}s", ms as f64 / 1000.0))
                .unwrap_or_default();
            ("rate-limited", RATE_PURPLE, detail)
        }
        Some(Status::Error { reason }) => ("error", ERR_RED, reason.clone()),
    }
}

fn chip(ui: &mut egui::Ui, on: &mut bool, label: &str, count: usize, color: Color32) {
    let text = RichText::new(format!("{label}  {count}"))
        .color(color)
        .strong();
    let resp = ui.selectable_label(*on, text);
    if resp.clicked() {
        *on = !*on;
    }
}

fn rfd_pick_file() -> Option<String> {
    // No rfd dependency on purpose — keeps the binary small. The GUI takes
    // its input from a `DUA_INPUT_FILE` env var as a low-friction stand-in
    // for a native picker; users on systems with a picker can pipe via
    // the CLI instead.
    std::env::var("DUA_INPUT_FILE").ok()
}

fn spawn_check_run(
    handle: tokio::runtime::Handle,
    cfg: CheckerConfig,
    names: Vec<String>,
    tx: UnboundedSender<Msg>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    _hunt: bool,
) {
    handle.spawn(async move {
        let checker = match Checker::new(cfg) {
            Ok(c) => c,
            Err(err) => {
                let _ = tx.send(Msg::Error(format!("HTTP client init failed: {err}")));
                return;
            }
        };
        let cancel_for_cb = cancel.clone();
        let tx_for_cb = tx.clone();
        let done_for_cb = done.clone();
        let progress: dua_checker::checker::BatchProgress = Arc::new(move |idx, result| {
            if cancel_for_cb.load(Ordering::SeqCst) {
                return;
            }
            done_for_cb.fetch_add(1, Ordering::SeqCst);
            let _ = tx_for_cb.send(Msg::Result {
                idx,
                result: result.clone(),
            });
        });
        let _ = checker.run_batch(names, Some(progress), Some(cancel)).await;
        let _ = tx.send(Msg::Done);
    });
}

fn spawn_hunt_run(
    handle: tokio::runtime::Handle,
    cfg: CheckerConfig,
    names: Vec<String>,
    tx: UnboundedSender<Msg>,
    cancel: Arc<AtomicBool>,
    done: Arc<AtomicUsize>,
    stop_after: Option<usize>,
) {
    handle.spawn(async move {
        let checker = match Checker::new(cfg) {
            Ok(c) => c,
            Err(err) => {
                let _ = tx.send(Msg::Error(format!("HTTP client init failed: {err}")));
                return;
            }
        };
        let hits = Arc::new(AtomicUsize::new(0));
        let cancel_for_cb = cancel.clone();
        let tx_for_cb = tx.clone();
        let done_for_cb = done.clone();
        let hits_for_cb = hits.clone();
        let progress: dua_checker::checker::BatchProgress =
            Arc::new(move |_idx, result: &CheckResult| {
                if cancel_for_cb.load(Ordering::SeqCst) {
                    return;
                }
                done_for_cb.fetch_add(1, Ordering::SeqCst);
                if result.status.is_available() {
                    let _ = tx_for_cb.send(Msg::Hit(result.clone()));
                    let h = hits_for_cb.fetch_add(1, Ordering::SeqCst) + 1;
                    if let Some(limit) = stop_after {
                        if h >= limit {
                            cancel_for_cb.store(true, Ordering::SeqCst);
                        }
                    }
                } else {
                    // Track counts via a placeholder, so the UI sees the
                    // progression even when no hit lands.
                    let _ = tx_for_cb.send(Msg::Result {
                        // idx is ignored for hunt rows that don't enter the table.
                        idx: usize::MAX,
                        result: result.clone(),
                    });
                }
            });
        let _ = checker.run_batch(names, Some(progress), Some(cancel)).await;
        let _ = tx.send(Msg::Done);
    });
}
