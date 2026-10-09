//! Optional desktop window (`"GUI": "Yes"` in config.json): the board
//! with merida pieces, live clocks, the move list, engine stats, and
//! a console that takes the same commands as the terminal `>` prompt.
//!
//! The bot keeps running on a background tokio thread and publishes
//! what the window shows into `GuiShared`. The window owns the main
//! thread (macOS requires that), only reads that state, and sends
//! typed commands down the same channel the terminal prompt uses - so
//! "tell", "engine ...", "quit" etc. behave identically in both.

use std::collections::{BTreeSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use anyhow::Result;
use eframe::egui::{
    self, Align, Color32, FontId, Layout, Pos2, Rect, RichText, Sense, Shape, Stroke, TextStyle,
    text::{LayoutJob, TextFormat},
    Vec2,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::format_clock;
use crate::board::{Relation, Style12};
use crate::engine::EngineInfo;
use crate::pgn::record_move;

/// Oldest console lines are dropped past this. Kept small: the console
/// is laid out in full on every frame, and a long scrollback costs CPU
/// the engine could be using.
const MAX_CONSOLE_LINES: usize = 100;

// Board colors: the average light/dark square colors of Lichess's
// "blue3" board (its textured image isn't bundled, just these colors).
const LIGHT_SQUARE: Color32 = Color32::from_rgb(0xcb, 0xd4, 0xdd);
const DARK_SQUARE: Color32 = Color32::from_rgb(0x56, 0x8a, 0xb6);
const LAST_MOVE: Color32 = Color32::from_rgba_premultiplied(64, 82, 0, 105);
const LOW_TIME: Color32 = Color32::from_rgb(0xe0, 0x40, 0x40);

/// Scores this big (in pawns) are really mates - some engines report a
/// mate as e.g. "cp 799981" - so, like forced mates, they're drawn at
/// the edge of the score chart rather than stretching its scale.
const MATE_LIKE: f32 = 1000.0;
const SCORE_CHART_HEIGHT: f32 = 140.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Text from the ICS.
    Server,
    /// A command we typed (terminal or GUI console).
    Sent,
    /// Our own search stats, as whispered.
    Kibitz,
    /// Bot events: new game, game over, accepted challenge, ...
    System,
    Error,
}

pub struct ConsoleLine {
    pub kind: LineKind,
    /// Local time the line arrived, "HH:MM:SS.mmm".
    pub time: String,
    pub text: String,
}

/// What the engine panel shows for our last move. Moves are in SAN.
pub struct SearchView {
    pub bestmove: String,
    pub from_book: bool,
    pub info: Option<EngineInfo>,
    /// The principal variation, numbered ("39... e3 40. Rd3").
    pub pv: Option<String>,
}

/// Everything the window draws. Written by the bot, read by the window.
#[derive(Default)]
pub struct GuiState {
    pub status: String,
    pub connected: bool,
    pub timeseal: bool,
    pub error: Option<String>,
    /// Latest board of the game we're playing, and when it arrived (the
    /// clocks count down from there).
    pub board: Option<(Style12, Instant)>,
    /// The "{Game N (...) ...} result" line once the game has ended.
    pub game_over: Option<String>,
    pub moves: Vec<String>,
    /// Plies of our moves that came from a book (ours or the engine's
    /// own), highlighted in the move list.
    pub book_plies: BTreeSet<usize>,
    pub search: Option<SearchView>,
    /// The engine's score after each of our moves this game, for the
    /// score chart.
    pub scores: Vec<ScorePoint>,
    pub console: VecDeque<ConsoleLine>,
    /// Set when the bot has shut down normally (e.g. "quit"), so the
    /// window closes itself.
    pub finished: bool,
}

impl GuiState {
    pub fn push_console(&mut self, kind: LineKind, text: impl Into<String>) {
        self.console.push_back(ConsoleLine {
            kind,
            time: chrono::Local::now().format("%H:%M:%S%.3f").to_string(),
            text: text.into(),
        });
        while self.console.len() > MAX_CONSOLE_LINES {
            self.console.pop_front();
        }
    }

    /// A board update for a game we're playing. FICS reuses game
    /// numbers, so a board after a finished game always starts a new
    /// game too, even if the number matches.
    pub fn new_board(&mut self, board: Style12) {
        let same_game = self.game_over.is_none()
            && self
                .board
                .as_ref()
                .is_some_and(|(b, _)| b.game_number == board.game_number);
        if !same_game {
            self.moves.clear();
            self.book_plies.clear();
            self.game_over = None;
            self.search = None;
            self.scores.clear();
        }
        record_move(&mut self.moves, board.ply(), &board.last_move_san);
        // Anything later was taken back.
        self.book_plies.split_off(&(board.ply() + 1));
        self.board = Some((board, Instant::now()));
    }

    /// Our move at `ply`, about to be played, is a book move.
    pub fn mark_book(&mut self, ply: usize) {
        self.book_plies.insert(ply);
    }
}

/// One point on the score chart: the engine's evaluation right after
/// our move, from our point of view (positive = we're better), the same
/// as the engine panel, kibitz and PGN notes show it.
#[derive(Debug, Clone, PartialEq)]
pub struct ScorePoint {
    /// Ply of our move (1 = White's first move).
    pub ply: usize,
    /// Pawns; infinite for mates (and `MATE_LIKE` scores), which the
    /// chart draws at its edge.
    pub value: f32,
    /// As shown on hover: "+1.87", "-0.40", "M3", "-M5".
    pub label: String,
}

impl GuiState {
    /// Record the engine's evaluation `info` of the move we're about to
    /// play from `board`. UCI scores are from the side to move - us -
    /// and are kept that way.
    pub fn push_score(&mut self, board: &Style12, info: &EngineInfo) {
        let (value, label) = match (info.score_mate, info.score_cp) {
            (Some(mate), _) => {
                if mate >= 0 {
                    (f32::INFINITY, format!("M{mate}"))
                } else {
                    (f32::NEG_INFINITY, format!("-M{}", -mate))
                }
            }
            (None, Some(cp)) => {
                let pawns = cp as f32 / 100.0;
                let value = if pawns.abs() >= MATE_LIKE {
                    f32::INFINITY.copysign(pawns)
                } else {
                    pawns
                };
                (value, format!("{pawns:+.2}"))
            }
            (None, None) => return,
        };
        let ply = board.ply() + 1;
        // A takeback can replay a ply; keep only the newest score for it.
        self.scores.retain(|p| p.ply < ply);
        self.scores.push(ScorePoint { ply, value, label });
    }
}

/// The score chart's scale, in pawns either way from zero: the largest
/// finite score rounded up to 1, 2 or 5 times a power of ten (at least
/// 1), so it grows from ±1 through ±2, ±5, ±10, ±20, ...
fn chart_range(scores: &[ScorePoint]) -> f32 {
    let largest = scores
        .iter()
        .map(|p| p.value.abs())
        .filter(|v| v.is_finite())
        .fold(1.0_f32, f32::max);
    let mut step = 1.0_f32;
    loop {
        for nice in [1.0, 2.0, 5.0] {
            if largest <= nice * step {
                return nice * step;
            }
        }
        step *= 10.0;
    }
}

/// "12." for White's move at that ply, "12..." for Black's.
fn move_label(ply: usize) -> String {
    let number = ply.div_ceil(2);
    if ply % 2 == 1 {
        format!("{number}.")
    } else {
        format!("{number}...")
    }
}

/// State shared between the bot thread and the window.
pub struct GuiShared {
    state: Mutex<GuiState>,
    /// Set once the window exists, so updates can wake it up.
    ctx: OnceLock<egui::Context>,
}

impl GuiShared {
    pub fn new() -> Arc<Self> {
        Arc::new(GuiShared {
            state: Mutex::new(GuiState::default()),
            ctx: OnceLock::new(),
        })
    }

    fn lock(&self) -> MutexGuard<'_, GuiState> {
        // A panic while holding the lock leaves the state usable; the
        // window is only a view of it, so carry on rather than crash.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Change the state and repaint the window.
    pub fn update(&self, f: impl FnOnce(&mut GuiState)) {
        f(&mut self.lock());
        if let Some(ctx) = self.ctx.get() {
            ctx.request_repaint();
        }
    }

    pub fn console(&self, kind: LineKind, text: impl Into<String>) {
        self.update(|s| s.push_console(kind, text));
    }
}

/// Open the window and run it until it's closed. Blocks the calling
/// thread, which must be the main thread.
pub fn run(shared: Arc<GuiShared>, cmd_tx: UnboundedSender<String>, title: String) -> Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(&title)
            .with_inner_size([1100.0, 760.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "icsdrone-rs",
        options,
        Box::new(move |cc| {
            egui_extras::install_image_loaders(&cc.egui_ctx);
            let _ = shared.ctx.set(cc.egui_ctx.clone());
            Ok(Box::new(GuiApp {
                shared,
                cmd_tx,
                input: String::new(),
                history: Vec::new(),
                history_pos: None,
                confirm_resign: false,
            }))
        }),
    )
    .map_err(|e| anyhow::anyhow!("GUI failed: {e}"))
}

struct GuiApp {
    shared: Arc<GuiShared>,
    cmd_tx: UnboundedSender<String>,
    /// The console's input line.
    input: String,
    /// Commands sent from the console, for Up/Down recall.
    history: Vec<String>,
    history_pos: Option<usize>,
    /// The Resign button was clicked once and is asking to confirm.
    confirm_resign: bool,
}

impl eframe::App for GuiApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let shared = self.shared.clone();
        let mut state = shared.lock();

        if state.finished {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }

        // F toggles fullscreen - unless it's being typed into the
        // console's input line (or another text field).
        let ctx = ui.ctx().clone();
        let toggle_fullscreen = !ctx.egui_wants_keyboard_input()
            && ctx.input(|i| i.modifiers.is_none() && i.key_pressed(egui::Key::F));
        if toggle_fullscreen {
            let fullscreen = ctx.input(|i| i.viewport().fullscreen.unwrap_or(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
        }
        // T switches between light and dark, likewise.
        let toggle_theme = !ctx.egui_wants_keyboard_input()
            && ctx.input(|i| i.modifiers.is_none() && i.key_pressed(egui::Key::T));
        if toggle_theme {
            ctx.set_theme(match ctx.theme() {
                egui::Theme::Dark => egui::Theme::Light,
                egui::Theme::Light => egui::Theme::Dark,
            });
        }

        egui::Panel::bottom("status")
            .resizable(false)
            .show(ui, |ui| status_bar(ui, &state));

        egui::Panel::bottom("console")
            .resizable(true)
            .default_size(220.0)
            .min_size(120.0)
            .show(ui, |ui| self.console(ui, &mut state));

        egui::Panel::right("info")
            .resizable(true)
            .default_size(300.0)
            .min_size(220.0)
            .show(ui, |ui| {
                self.game_controls(ui, &mut state);
                info_panel(ui, &state);
            });

        egui::CentralPanel::default().show(ui, |ui| board_area(ui, &state));

        // Keep the running clock ticking: redraw just as its shown
        // second changes, rather than many times a second - every frame
        // costs CPU the engine could be using. Input and new boards
        // still redraw straight away.
        if state.game_over.is_none() {
            if let Some((board, _)) = &state.board {
                let (white, black) = live_clocks(&state);
                let running = if board.to_move_white { white } else { black };
                if let Some(ms) = running {
                    ui.ctx().request_repaint_after(until_clock_ticks(ms));
                }
            }
        }
    }
}

impl GuiApp {
    /// A Resign button while a game is on, for when the operator thinks
    /// the bot is lost (or playing badly). It sends FICS's `resign`
    /// like a typed command, after a second click to confirm.
    fn game_controls(&mut self, ui: &mut egui::Ui, state: &mut GuiState) {
        if state.board.is_none() || state.game_over.is_some() {
            self.confirm_resign = false;
            return;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if !self.confirm_resign {
                if ui.button("Resign").on_hover_text("Resign this game").clicked() {
                    self.confirm_resign = true;
                }
                return;
            }
            ui.label("Resign this game?");
            let yes = egui::Button::new(RichText::new("Resign").color(Color32::WHITE))
                .fill(LOW_TIME);
            if ui.add(yes).clicked() {
                self.confirm_resign = false;
                if self.cmd_tx.send("resign".to_string()).is_err() {
                    state.push_console(LineKind::Error, "Bot is not running; could not resign.");
                }
            }
            if ui.button("Cancel").clicked() {
                self.confirm_resign = false;
            }
        });
        ui.add_space(2.0);
        ui.separator();
    }

    fn console(&mut self, ui: &mut egui::Ui, state: &mut GuiState) {
        // Input line at the bottom, scrollback filling the rest.
        let input = egui::Panel::bottom("console_input")
            .resizable(false)
            .show_separator_line(false)
            .show(ui, |ui| {
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.input)
                        .font(TextStyle::Monospace)
                        .hint_text("ICS command, tell, or engine <UCI command> - Enter to send")
                        .desired_width(f32::INFINITY),
                )
            })
            .inner;

        if input.has_focus() {
            self.history_keys(ui);
        }
        if input.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            let cmd = self.input.trim().to_string();
            if !cmd.is_empty() {
                if self.cmd_tx.send(cmd.clone()).is_err() {
                    state.push_console(LineKind::Error, "Bot is not running; command not sent.");
                }
                if self.history.last() != Some(&cmd) {
                    self.history.push(cmd);
                }
            }
            self.input.clear();
            self.history_pos = None;
            input.request_focus();
        }

        // Laid out in full (not `show_rows`) because long server lines
        // wrap to varying heights; the line cap keeps this cheap.
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                let font = TextStyle::Monospace.resolve(ui.style());
                let weak = ui.visuals().weak_text_color();
                for line in &state.console {
                    let color = match line.kind {
                        LineKind::Server => ui.visuals().text_color(),
                        LineKind::Sent => Color32::from_rgb(0x5c, 0x9d, 0xe6),
                        LineKind::Kibitz => Color32::from_rgb(0x4c, 0xae, 0x4f),
                        LineKind::System => ui.visuals().weak_text_color(),
                        LineKind::Error => LOW_TIME,
                    };
                    let mut job = LayoutJob::default();
                    job.append(
                        &line.time,
                        0.0,
                        TextFormat::simple(font.clone(), weak),
                    );
                    job.append(&line.text, 12.0, TextFormat::simple(font.clone(), color));
                    ui.label(job);
                }
            });
    }

    /// Up/Down in the input line steps through earlier commands.
    fn history_keys(&mut self, ui: &egui::Ui) {
        let (up, down) = ui.input(|i| {
            (
                i.key_pressed(egui::Key::ArrowUp),
                i.key_pressed(egui::Key::ArrowDown),
            )
        });
        if self.history.is_empty() || !(up || down) {
            return;
        }
        let last = self.history.len() - 1;
        self.history_pos = match (self.history_pos, up) {
            (None, true) => Some(last),
            (Some(p), true) => Some(p.saturating_sub(1)),
            (Some(p), false) if p < last => Some(p + 1),
            _ => None,
        };
        self.input = self
            .history_pos
            .map(|p| self.history[p].clone())
            .unwrap_or_default();
    }
}

fn status_bar(ui: &mut egui::Ui, state: &GuiState) {
    ui.horizontal(|ui| {
        let (dot, text) = match &state.error {
            Some(err) => (LOW_TIME, err.as_str()),
            None if state.connected => (Color32::from_rgb(0x4c, 0xae, 0x4f), state.status.as_str()),
            None => (Color32::from_rgb(0xe0, 0xb0, 0x30), state.status.as_str()),
        };
        // Drawn rather than a "●" glyph, which the default font lacks.
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 5.0, dot);
        ui.label(text);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some((board, _)) = &state.board {
                if state.game_over.is_none() {
                    ui.label(format!("Game #{}", board.game_number));
                    ui.separator();
                }
            }
            ui.label(if state.timeseal { "Timeseal on" } else { "Timeseal off" });
            ui.separator();
            // ☀/🌙: switch between light and dark (also the T key).
            egui::widgets::global_theme_preference_switch(ui);
        });
    });
}

fn info_panel(ui: &mut egui::Ui, state: &GuiState) {
    ui.add_space(6.0);
    ui.heading("Engine");
    ui.add_space(4.0);
    match &state.search {
        None => {
            ui.weak("No move yet");
        }
        Some(search) if search.from_book => {
            ui.label(format!("Book move: {}", search.bestmove));
        }
        Some(search) => {
            egui::Grid::new("engine_stats")
                .num_columns(2)
                .spacing([16.0, 2.0])
                .show(ui, |ui| {
                    let row = |ui: &mut egui::Ui, name: &str, value: String| {
                        ui.weak(name);
                        ui.monospace(value);
                        ui.end_row();
                    };
                    row(ui, "Best move", search.bestmove.clone());
                    if let Some(info) = &search.info {
                        row(ui, "Score", info.score_string());
                        row(ui, "Depth", info.depth.to_string());
                        let secs = info.time_ms.unwrap_or(0) as f64 / 1000.0;
                        row(ui, "Time", format!("{secs:.2}s"));
                        row(ui, "Nodes", thousands(info.nodes.unwrap_or(0)));
                        row(ui, "NPS", thousands(info.nps.unwrap_or(0)));
                    }
                });
            if let Some(pv) = &search.pv {
                ui.add_space(4.0);
                ui.weak("PV");
                ui.add(egui::Label::new(RichText::new(pv).monospace()).wrap());
            }
        }
    }

    ui.add_space(10.0);
    ui.separator();
    ui.heading("Score");
    ui.add_space(4.0);
    let board = state.board.as_ref().map(|(b, _)| b);
    let we_are_white = board.is_none_or(|b| {
        (b.relation == Relation::PlayingMyMove) == b.to_move_white
    });
    score_chart(ui, &state.scores, we_are_white);

    ui.add_space(10.0);
    ui.separator();
    ui.heading("Moves");
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .stick_to_bottom(true)
        .show(ui, |ui| {
            egui::Grid::new("moves")
                .num_columns(3)
                .spacing([12.0, 2.0])
                .striped(true)
                .show(ui, |ui| {
                    for (i, pair) in state.moves.chunks(2).enumerate() {
                        ui.weak(format!("{}.", i + 1));
                        for (j, san) in pair.iter().enumerate() {
                            let ply = 2 * i + j + 1;
                            if state.book_plies.contains(&ply) {
                                // On the board's light-square blue.
                                let text = RichText::new(san)
                                    .monospace()
                                    .color(Color32::BLACK)
                                    .background_color(LIGHT_SQUARE);
                                ui.label(text).on_hover_text("Book move");
                            } else {
                                ui.monospace(san);
                            }
                        }
                        if pair.len() == 1 {
                            ui.monospace("");
                        }
                        ui.end_row();
                    }
                });
            if let Some(result) = &state.game_over {
                ui.add_space(6.0);
                ui.add(egui::Label::new(RichText::new(result).strong()).wrap());
            }
        });
}

/// Line chart of the engine's score over the game. Hovering shows the
/// move and score. `scores` are from our point of view; the area where we're ahead is
/// shaded in our color, and the other side's where they are.
fn score_chart(ui: &mut egui::Ui, scores: &[ScorePoint], we_are_white: bool) {
    let size = Vec2::new(ui.available_width(), SCORE_CHART_HEIGHT);
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);

    if scores.is_empty() {
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "No engine scores yet",
            FontId::proportional(13.0),
            visuals.weak_text_color(),
        );
        return;
    }

    // Vertical scale: grows with the largest score so far (mates aside,
    // which sit on the edge), in round steps.
    let range = chart_range(scores);
    let first = scores[0].ply as f32;
    let last = (scores[scores.len() - 1].ply as f32).max(first + 1.0);
    let inner = rect.shrink2(Vec2::new(6.0, 8.0));
    let zero_y = inner.center().y;
    let to_screen = |p: &ScorePoint| {
        Pos2::new(
            inner.left() + (p.ply as f32 - first) / (last - first) * inner.width(),
            zero_y - p.value.clamp(-range, range) / range * inner.height() / 2.0,
        )
    };
    let points: Vec<Pos2> = scores.iter().map(to_screen).collect();

    // Shade between the line and zero, one trapezoid per segment, split
    // where the line crosses zero so every piece stays convex.
    let white_fill = Color32::from_white_alpha(70);
    let black_fill = Color32::from_black_alpha(90);
    let (ahead_fill, behind_fill) = if we_are_white {
        (white_fill, black_fill)
    } else {
        (black_fill, white_fill)
    };
    let fill = |a: Pos2, b: Pos2| {
        let color = if a.y + b.y < 2.0 * zero_y { ahead_fill } else { behind_fill };
        Shape::convex_polygon(
            vec![a, b, Pos2::new(b.x, zero_y), Pos2::new(a.x, zero_y)],
            color,
            Stroke::NONE,
        )
    };
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        if (a.y - zero_y) * (b.y - zero_y) < 0.0 {
            let t = (zero_y - a.y) / (b.y - a.y);
            let cross = Pos2::new(a.x + t * (b.x - a.x), zero_y);
            painter.add(fill(a, cross));
            painter.add(fill(cross, b));
        } else {
            painter.add(fill(a, b));
        }
    }

    painter.hline(inner.x_range(), zero_y, Stroke::new(1.0, visuals.weak_text_color()));
    let line_color = Color32::from_rgb(0x5c, 0x9d, 0xe6);
    painter.add(Shape::line(points.clone(), Stroke::new(2.0, line_color)));
    for &p in &points {
        painter.circle_filled(p, 2.5, line_color);
    }

    let axis_font = FontId::proportional(11.0);
    let axis_color = visuals.weak_text_color();
    painter.text(rect.left_top() + Vec2::new(4.0, 2.0), egui::Align2::LEFT_TOP, format!("+{range}"), axis_font.clone(), axis_color);
    painter.text(rect.left_bottom() + Vec2::new(4.0, -2.0), egui::Align2::LEFT_BOTTOM, format!("-{range}"), axis_font.clone(), axis_color);
    painter.text(rect.right_top() + Vec2::new(-4.0, 2.0), egui::Align2::RIGHT_TOP, "+ = us ahead", axis_font, axis_color);

    // Hover: the nearest point by x, marked, with its move and score.
    if let Some(pos) = response.hover_pos() {
        let nearest = points
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| (a.x - pos.x).abs().total_cmp(&(b.x - pos.x).abs()))
            .map(|(i, _)| i);
        if let Some(i) = nearest {
            painter.vline(points[i].x, inner.y_range(), Stroke::new(1.0, axis_color));
            painter.circle_filled(points[i], 4.5, line_color);
            let p = &scores[i];
            response.on_hover_text_at_pointer(format!("{} {}", move_label(p.ply), p.label));
        }
    }
}

/// The board with a player bar (name and clock) above and below it.
/// Our own side is always at the bottom.
fn board_area(ui: &mut egui::Ui, state: &GuiState) {
    let board = state.board.as_ref().map(|(b, _)| b);
    // In a game we're playing, the side to move is us exactly when the
    // relation says it's our move.
    let we_are_white = board.is_none_or(|b| {
        (b.relation == Relation::PlayingMyMove) == b.to_move_white
    });
    let (white_ms, black_ms) = live_clocks(state);

    let bar_height = 44.0;
    let avail = ui.available_size();
    // Room either side of the centered board for the material column.
    let size = (avail.x / (1.0 + 2.0 * MATERIAL_COLUMN))
        .min(avail.y - 2.0 * bar_height - 16.0)
        .max(160.0);

    ui.vertical_centered(|ui| {
        ui.set_width(size);
        let white = (board.map(|b| b.white_name.as_str()), white_ms, true);
        let black = (board.map(|b| b.black_name.as_str()), black_ms, false);
        let (top, bottom) = if we_are_white { (black, white) } else { (white, black) };

        player_bar(ui, state, top.0, top.1, top.2, bar_height);
        let board_rect = draw_board(ui, board, !we_are_white, size);
        player_bar(ui, state, bottom.0, bottom.1, bottom.2, bar_height);

        // Lichess-style material difference to the right of the board,
        // each side's by its own end of the board.
        if let Some(board) = board {
            let icon = board_rect.width() / 8.0 * MATERIAL_ICON;
            let x = board_rect.right() + icon * 0.4;
            let top_y = board_rect.top() + icon * 0.2;
            paint_material(ui, Pos2::new(x, top_y), &board.rows, top.2, icon);
            let bottom_y = board_rect.bottom() - icon * 1.2;
            paint_material(ui, Pos2::new(x, bottom_y), &board.rows, bottom.2, icon);
        }
    });
}

/// How long until a running clock showing `ms` displays a different
/// second (`format_clock` drops the milliseconds), plus a few ms so the
/// redraw lands just after the change rather than just before it.
fn until_clock_ticks(ms: i64) -> Duration {
    let into_second = ms.unsigned_abs() % 1000;
    let wait = if ms > 0 {
        // Counting down: the shown second drops once the remainder runs out.
        if into_second == 0 { 1000 } else { into_second }
    } else {
        // At or past zero the magnitude grows: next whole second up.
        1000 - into_second
    };
    Duration::from_millis(wait + 5)
}

/// Both clocks, with the side to move's clock counting down since its
/// board arrived (until the game ends).
fn live_clocks(state: &GuiState) -> (Option<i64>, Option<i64>) {
    let Some((board, received)) = &state.board else {
        return (None, None);
    };
    let (mut white, mut black) = (board.white_time_ms, board.black_time_ms);
    if state.game_over.is_none() {
        let elapsed = received.elapsed().as_millis() as i64;
        if board.to_move_white {
            white -= elapsed;
        } else {
            black -= elapsed;
        }
    }
    (Some(white), Some(black))
}

fn player_bar(
    ui: &mut egui::Ui,
    state: &GuiState,
    name: Option<&str>,
    clock_ms: Option<i64>,
    is_white: bool,
    height: f32,
) {
    let board = state.board.as_ref().map(|(b, _)| b);
    let to_move =
        state.game_over.is_none() && board.is_some_and(|b| b.to_move_white == is_white);
    let fill = if to_move {
        ui.visuals().selection.bg_fill
    } else {
        ui.visuals().faint_bg_color
    };

    egui::Frame::new()
        .fill(fill)
        .inner_margin(egui::Margin::symmetric(10, 4))
        .corner_radius(4.0)
        .show(ui, |ui| {
            ui.set_height(height - 8.0);
            ui.horizontal_centered(|ui| {
                let side = if is_white { "♔" } else { "♚" };
                ui.label(RichText::new(side).size(20.0));
                ui.label(
                    RichText::new(name.unwrap_or("Waiting for a game…"))
                        .size(18.0)
                        .strong(),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Some(ms) = clock_ms {
                        let mut text = RichText::new(format_clock(ms))
                            .font(FontId::monospace(26.0))
                            .strong();
                        if ms < 10_000 {
                            text = text.color(LOW_TIME);
                        }
                        ui.label(text);
                    }
                });
            });
        });
}

/// Lichess-style material difference for one side of `rows` (style12
/// rows): the pieces it's up on, net, as the opponent's piece letters
/// (most valuable first - what it has taken, in effect), and how many
/// points ahead it is (P=1, N=B=3, R=5, Q=9), or 0 if it isn't.
fn material_edge(rows: &[String; 8], white: bool) -> (Vec<u8>, i32) {
    const PIECES: [(u8, i32); 5] = [(b'q', 9), (b'r', 5), (b'b', 3), (b'n', 3), (b'p', 1)];
    let count = |letter: u8| {
        rows.iter()
            .flat_map(|row| row.bytes())
            .filter(|&b| b == letter)
            .count() as i32
    };
    let mut pieces = Vec::new();
    let mut points = 0;
    for (black_letter, value) in PIECES {
        let white_letter = black_letter.to_ascii_uppercase();
        let (mine, theirs) = if white {
            (white_letter, black_letter)
        } else {
            (black_letter, white_letter)
        };
        let diff = count(mine) - count(theirs);
        points += diff * value;
        for _ in 0..diff.max(0) {
            pieces.push(theirs);
        }
    }
    (pieces, points.max(0))
}

/// Width kept free either side of the board for the material
/// difference, as a fraction of the board's width.
const MATERIAL_COLUMN: f32 = 0.3;
/// Size of its piece icons, as a fraction of a board square.
const MATERIAL_ICON: f32 = 0.5;
/// Behind the material icons: the board's light-square blue (as for
/// book moves), on which both colors of piece show up, whatever the
/// window's theme.
const MATERIAL_STRIP: Color32 = LIGHT_SQUARE;

/// Paint one side's material difference in a row starting at `at`
/// (its top-left): small piece icons, then "+N" when that side is
/// ahead. Pieces of one kind overlap a little, as on Lichess.
/// `icon` is the icons' size in points.
fn paint_material(ui: &egui::Ui, at: Pos2, rows: &[String; 8], white: bool, icon: f32) {
    let (pieces, points) = material_edge(rows, white);
    // Where each icon goes, left to right.
    let mut xs = Vec::with_capacity(pieces.len());
    let mut x = at.x;
    for (i, &piece) in pieces.iter().enumerate() {
        if i > 0 {
            let same = pieces[i - 1] == piece;
            x += if same { icon * 0.55 } else { icon * 1.1 };
        }
        xs.push(x);
    }
    if !pieces.is_empty() {
        // On a light-square blue strip - straight on the window background,
        // white pieces vanish in the light theme and black ones in
        // the dark.
        let pad = icon * 0.15;
        let strip = Rect::from_min_max(
            Pos2::new(at.x - pad, at.y - pad),
            Pos2::new(x + icon + pad, at.y + icon + pad),
        );
        ui.painter().rect_filled(strip, pad, MATERIAL_STRIP);
    }
    for (&piece, &x) in pieces.iter().zip(&xs) {
        if let Some(image) = piece_image(piece) {
            let rect = Rect::from_min_size(Pos2::new(x, at.y), Vec2::splat(icon));
            egui::Image::new(image).paint_at(ui, rect);
        }
    }
    if points > 0 {
        if !pieces.is_empty() {
            x += icon * 1.3;
        }
        ui.painter().text(
            Pos2::new(x, at.y + icon / 2.0),
            egui::Align2::LEFT_CENTER,
            format!("+{points}"),
            FontId::proportional((icon * 0.7).max(12.0)),
            ui.visuals().weak_text_color(),
        );
    }
}

/// The starting position in style12 row form (rank 8 first), shown
/// until the first game's board arrives.
const START_ROWS: [&str; 8] = [
    "rnbqkbnr", "pppppppp", "--------", "--------", "--------", "--------", "PPPPPPPP", "RNBQKBNR",
];

/// Draw the board; returns where it went.
fn draw_board(ui: &mut egui::Ui, board: Option<&Style12>, flipped: bool, size: f32) -> Rect {
    let (rect, _) = ui.allocate_exact_size(Vec2::splat(size), Sense::hover());
    let painter = ui.painter_at(rect);
    let square = size / 8.0;
    let last_move = board.and_then(Style12::last_move_squares);
    let coord_font = FontId::proportional((square * 0.18).max(9.0));

    // (row, col) index `Style12::rows`: row 0 = rank 8, col 0 = file a.
    for row in 0..8 {
        for col in 0..8 {
            let (screen_row, screen_col) = if flipped { (7 - row, 7 - col) } else { (row, col) };
            let min = rect.min + Vec2::new(screen_col as f32, screen_row as f32) * square;
            let sq = Rect::from_min_size(min, Vec2::splat(square));
            let light = (row + col) % 2 == 0;
            painter.rect_filled(sq, 0.0, if light { LIGHT_SQUARE } else { DARK_SQUARE });
            if last_move.is_some_and(|(from, to)| from == (row, col) || to == (row, col)) {
                painter.rect_filled(sq, 0.0, LAST_MOVE);
            }

            // Coordinates along the left and bottom edges, Lichess-style.
            let coord_color = if light { DARK_SQUARE } else { LIGHT_SQUARE };
            if screen_col == 0 {
                painter.text(
                    sq.left_top() + Vec2::splat(square * 0.04),
                    egui::Align2::LEFT_TOP,
                    (8 - row).to_string(),
                    coord_font.clone(),
                    coord_color,
                );
            }
            if screen_row == 7 {
                painter.text(
                    sq.right_bottom() - Vec2::splat(square * 0.04),
                    egui::Align2::RIGHT_BOTTOM,
                    ((b'a' + col as u8) as char).to_string(),
                    coord_font.clone(),
                    coord_color,
                );
            }

            let rows = board.map_or(START_ROWS, |b| b.rows.each_ref().map(String::as_str));
            let piece = rows[row].as_bytes().get(col).copied();
            if let Some(image) = piece.and_then(piece_image) {
                egui::Image::new(image).paint_at(ui, sq);
            }
        }
    }
    rect
}

/// The merida image for a style12 piece letter (uppercase = White).
fn piece_image(piece: u8) -> Option<egui::ImageSource<'static>> {
    Some(match piece {
        b'K' => egui::include_image!("../assets/pieces/merida/wK.svg"),
        b'Q' => egui::include_image!("../assets/pieces/merida/wQ.svg"),
        b'R' => egui::include_image!("../assets/pieces/merida/wR.svg"),
        b'B' => egui::include_image!("../assets/pieces/merida/wB.svg"),
        b'N' => egui::include_image!("../assets/pieces/merida/wN.svg"),
        b'P' => egui::include_image!("../assets/pieces/merida/wP.svg"),
        b'k' => egui::include_image!("../assets/pieces/merida/bK.svg"),
        b'q' => egui::include_image!("../assets/pieces/merida/bQ.svg"),
        b'r' => egui::include_image!("../assets/pieces/merida/bR.svg"),
        b'b' => egui::include_image!("../assets/pieces/merida/bB.svg"),
        b'n' => egui::include_image!("../assets/pieces/merida/bN.svg"),
        b'p' => egui::include_image!("../assets/pieces/merida/bP.svg"),
        _ => return None,
    })
}

/// 17234760 -> "17,234,760".
fn thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(line_tail: &str) -> Style12 {
        Style12::parse(&format!(
            "<12> rnbqkbnr pppppppp -------- -------- -------- -------- PPPPPPPP RNBQKBNR {line_tail}"
        ))
        .unwrap()
    }

    #[test]
    fn book_marks_follow_takebacks_and_new_games() {
        let mut state = GuiState::default();
        state.new_board(board("W -1 1 1 1 1 0 7 a b 1 1 0 39 39 60 60 1 none (0:00) none 0 0 0"));
        state.mark_book(1);
        state.new_board(board("B 4 1 1 1 1 0 7 a b -1 1 0 39 39 60 60 1 P/e2-e4 (0:00) e4 0 0 0"));
        assert!(state.book_plies.contains(&1));
        // Takeback to the start: the mark goes with the move.
        state.new_board(board("W -1 1 1 1 1 0 7 a b 1 1 0 39 39 60 60 1 none (0:00) none 0 0 0"));
        assert!(state.book_plies.is_empty());
        state.mark_book(1);
        state.game_over = Some("{Game 7 (a vs. b) b resigns} 1-0".to_string());
        state.new_board(board("W -1 1 1 1 1 0 7 a b 1 1 0 39 39 60 60 1 none (0:00) none 0 0 0"));
        assert!(state.book_plies.is_empty());
    }

    #[test]
    fn new_board_starts_a_fresh_game_after_game_over() {
        let mut state = GuiState::default();
        state.new_board(board("B 4 1 1 1 1 0 7 a b -1 1 0 39 39 60 60 1 P/e2-e4 (0:00) e4 0 0 0"));
        assert_eq!(state.moves, ["e4"]);
        state.game_over = Some("{Game 7 (a vs. b) b resigns} 1-0".to_string());
        // Same game number reused for the next game.
        state.new_board(board("W -1 1 1 1 1 0 7 a b 1 1 0 39 39 60 60 1 none (0:00) none 0 0 0"));
        assert!(state.moves.is_empty());
        assert!(state.game_over.is_none());
    }

    #[test]
    fn every_piece_svg_renders() {
        let ctx = egui::Context::default();
        egui_extras::install_image_loaders(&ctx);
        for &piece in b"KQRBNPkqrbnp" {
            let Some(egui::ImageSource::Bytes { uri, bytes }) = piece_image(piece) else {
                panic!("no image for {}", piece as char);
            };
            ctx.include_bytes(uri.clone(), bytes);
            let hint = egui::SizeHint::Size {
                width: 64,
                height: 64,
                maintain_aspect_ratio: false,
            };
            match ctx.try_load_image(&uri, hint) {
                Ok(egui::load::ImagePoll::Ready { image }) => {
                    assert_eq!(image.size, [64, 64], "{uri}");
                    // Not just a blank square: some pixels are drawn.
                    assert!(image.pixels.iter().any(|p| p.a() > 0), "{uri} is empty");
                }
                Ok(egui::load::ImagePoll::Pending { .. }) => panic!("{uri} still loading"),
                Err(e) => panic!("{uri} failed to render: {e}"),
            }
        }
    }

    fn info(cp: Option<i64>, mate: Option<i32>) -> EngineInfo {
        EngineInfo {
            score_cp: cp,
            score_mate: mate,
            ..Default::default()
        }
    }

    #[test]
    fn scores_are_from_our_point_of_view() {
        let mut state = GuiState::default();
        // We're White to move at the start.
        let white_to_move = board("W -1 1 1 1 1 0 7 a b 1 1 0 39 39 60 60 1 none (0:00) none 0 0 0");
        state.push_score(&white_to_move, &info(Some(30), None));
        // We're Black after 1. e4: -0.90 for us stays -0.90, as the
        // engine panel shows it.
        let black_to_move = board("B 4 1 1 1 1 0 7 a b 1 1 0 39 39 60 60 1 P/e2-e4 (0:00) e4 0 0 0");
        state.push_score(&black_to_move, &info(Some(-90), None));
        assert_eq!(state.scores[0].ply, 1);
        assert_eq!(state.scores[0].value, 0.3);
        assert_eq!(state.scores[0].label, "+0.30");
        assert_eq!(state.scores[1].ply, 2);
        assert_eq!(state.scores[1].value, -0.9);
        assert_eq!(state.scores[1].label, "-0.90");
    }

    #[test]
    fn big_scores_are_kept_and_mates_sit_at_the_edge() {
        let mut state = GuiState::default();
        let black_to_move = board("B 4 1 1 1 1 0 7 a b 1 1 0 39 39 60 60 1 P/e2-e4 (0:00) e4 0 0 0");
        // We mate in 3.
        state.push_score(&black_to_move, &info(None, Some(3)));
        assert_eq!((state.scores[0].value, state.scores[0].label.as_str()), (f32::INFINITY, "M3"));
        // Replaying the same ply replaces its score; big scores stay.
        state.push_score(&black_to_move, &info(Some(-3615), None));
        assert_eq!(state.scores.len(), 1);
        assert_eq!((state.scores[0].value, state.scores[0].label.as_str()), (-36.15, "-36.15"));
        // An engine's "cp 799981" mate counts as a mate.
        state.push_score(&black_to_move, &info(Some(799_981), None));
        assert_eq!((state.scores[0].value, state.scores[0].label.as_str()), (f32::INFINITY, "+7999.81"));
    }

    #[test]
    fn chart_scale_grows_in_round_steps() {
        let points = |values: &[f32]| -> Vec<ScorePoint> {
            values
                .iter()
                .enumerate()
                .map(|(i, &value)| ScorePoint { ply: i + 1, value, label: String::new() })
                .collect()
        };
        assert_eq!(chart_range(&points(&[0.3])), 1.0);
        assert_eq!(chart_range(&points(&[0.3, -1.5])), 2.0);
        assert_eq!(chart_range(&points(&[4.0, 10.0])), 10.0);
        assert_eq!(chart_range(&points(&[12.95, 34.5])), 50.0);
        assert_eq!(chart_range(&points(&[65.34])), 100.0);
        // Mates don't stretch the scale.
        assert_eq!(chart_range(&points(&[3.0, f32::INFINITY])), 5.0);
    }

    #[test]
    fn redraws_when_the_shown_second_changes() {
        // 4:59.250 shows 4:59 until 250ms from now.
        assert_eq!(until_clock_ticks(299_250), Duration::from_millis(255));
        // Exactly on a second: a full second until the next change.
        assert_eq!(until_clock_ticks(299_000), Duration::from_millis(1005));
        // Overstayed (negative): -0:01.300 becomes -0:02 in 700ms.
        assert_eq!(until_clock_ticks(-1_300), Duration::from_millis(705));
    }

    #[test]
    fn material_edge_counts_pieces_up_and_points() {
        let rows = |r: [&str; 8]| r.map(String::from);
        let start = rows(START_ROWS);
        assert_eq!(material_edge(&start, true), (vec![], 0));
        assert_eq!(material_edge(&start, false), (vec![], 0));
        // White has taken a knight; Black has taken two pawns.
        let traded = rows([
            "r-bqkbnr", "pppppppp", "--------", "--------", "--------", "--------",
            "PPPPPP--", "RNBQKBNR",
        ]);
        assert_eq!(material_edge(&traded, true), (vec![b'n'], 1));
        assert_eq!(material_edge(&traded, false), (vec![b'P', b'P'], 0));
        // A promoted queen counts as a queen.
        let promoted = rows([
            "Q---k---", "--------", "--------", "--------", "--------", "--------",
            "--------", "----K---",
        ]);
        assert_eq!(material_edge(&promoted, true), (vec![b'q'], 9));
    }

    #[test]
    fn labels_moves() {
        assert_eq!(move_label(1), "1.");
        assert_eq!(move_label(2), "1...");
        assert_eq!(move_label(45), "23.");
    }

    #[test]
    fn formats_thousands() {
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1000), "1,000");
        assert_eq!(thousands(17234760), "17,234,760");
    }
}
