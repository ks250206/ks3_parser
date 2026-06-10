use std::io::{ErrorKind, stdout};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use encoding_rs::SHIFT_JIS;
use ks3_parser::{Config, load_config, run_pipeline, save_config};
use ratatui::DefaultTerminal;

use super::clipboard;
use super::ui;

pub const FIELD_COUNT: usize = 3;

/// lazygit 風のペイン番号（1–4）
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub enum FocusedPane {
    #[default]
    ConfigList,
    Detail,
    Csv,
    Log,
}

impl FocusedPane {
    pub fn from_digit(c: char) -> Option<Self> {
        match c {
            '1' => Some(Self::ConfigList),
            '2' => Some(Self::Detail),
            '3' => Some(Self::Csv),
            '4' => Some(Self::Log),
            _ => None,
        }
    }

    pub fn title_prefix(self) -> &'static str {
        match self {
            FocusedPane::ConfigList => "1",
            FocusedPane::Detail => "2",
            FocusedPane::Csv => "3",
            FocusedPane::Log => "4",
        }
    }
}

#[derive(Clone, Copy)]
pub struct VisualSelection {
    pub pane: FocusedPane,
    pub anchor: usize,
    pub cursor: usize,
}

const LABELS: [&str; FIELD_COUNT] = ["input_path", "output_dir", "output_file_name"];

const HELP_TEXT: [&str; FIELD_COUNT] = [
    "入力 .KS3 ファイルのパス",
    "CSV の出力ディレクトリ",
    "出力 CSV ファイル名",
];

pub fn run_app(config_path: PathBuf) -> Result<()> {
    let mut terminal = ratatui::init();
    execute!(stdout(), event::EnableMouseCapture).context("EnableMouseCapture")?;
    let result = run_loop(&mut terminal, config_path);
    let _ = execute!(stdout(), event::DisableMouseCapture);
    ratatui::restore();
    result
}

fn run_loop(terminal: &mut DefaultTerminal, config_path: PathBuf) -> Result<()> {
    let mut app = App::new(config_path)?;

    loop {
        terminal.draw(|frame| ui::draw(frame, &mut app))?;

        if !event::poll(Duration::from_millis(250))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) => {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                if app.handle_key(key)? {
                    break;
                }
            }
            Event::Mouse(m) => {
                app.handle_mouse(m)?;
            }
            _ => {}
        }
    }

    Ok(())
}

pub struct App {
    pub config_path: PathBuf,
    pub config: Config,
    /// 最後にディスクへ保存した内容（未保存の変更は `config` と差分比較で色分け）
    saved_config: Config,
    pub list_state: ratatui::widgets::ListState,
    pub editing: bool,
    pub edit_buffer: String,
    /// 編集バッファ内のカーソル位置（文字インデックス、UTF-8 安全）
    pub edit_cursor: usize,
    pub logs: Vec<String>,
    pub status_line: String,
    /// 確定失敗・保存/変換エラーなどでメッセージ行を赤字にする
    pub status_error: bool,
    pub show_help: bool,
    /// `gg` 用（1 回目の `g` を待っている）
    pub awaiting_second_g: bool,
    /// 変更直前の `config`（`u` で復元）
    undo_stack: Vec<Config>,
    /// `u` で戻したあと `Ctrl+R` で進むための履歴
    redo_stack: Vec<Config>,
    pub focused_pane: FocusedPane,
    /// 行ビジュアル選択（`v` で開始、`y` でクリップボードへ）
    pub visual: Option<VisualSelection>,
    /// 詳細ペイン内の絶対行（0 始まり）
    pub detail_cursor_line: usize,
    /// CSV プレビュー内の絶対行
    pub csv_cursor_line: usize,
    /// ログの絶対行
    pub log_cursor_line: usize,
    /// 新規ログで末尾へ追従する（ログで k 等で上に動かしたら false）
    pub log_follow_tail: bool,
    /// 直近フレームのペイン領域（マウスヒットテスト用）
    pub hit_rects: Option<ui::PaneHitRects>,
}

impl App {
    fn new(config_path: PathBuf) -> Result<Self> {
        let config = load_config(&config_path)?;
        let saved_config = config.clone();
        let mut list_state = ratatui::widgets::ListState::default();
        list_state.select(Some(0));

        Ok(Self {
            config_path,
            config,
            saved_config,
            list_state,
            editing: false,
            edit_buffer: String::new(),
            edit_cursor: 0,
            logs: Vec::new(),
            status_line: String::new(),
            status_error: false,
            show_help: false,
            awaiting_second_g: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            focused_pane: FocusedPane::default(),
            visual: None,
            detail_cursor_line: 0,
            csv_cursor_line: 0,
            log_cursor_line: 0,
            log_follow_tail: true,
            hit_rects: None,
        })
    }

    fn handle_mouse(&mut self, ev: MouseEvent) -> Result<()> {
        if self.show_help || self.editing || self.visual.is_some() {
            return Ok(());
        }
        let MouseEventKind::Down(MouseButton::Left) = ev.kind else {
            return Ok(());
        };
        let Some(r) = self.hit_rects else {
            return Ok(());
        };
        let col = ev.column;
        let y = ev.row;
        self.awaiting_second_g = false;

        if ui::rect_contains(r.list, col, y) {
            self.focused_pane = FocusedPane::ConfigList;
            let inner_top = r.list.y.saturating_add(1);
            let inner_h = r.list.height.saturating_sub(2).max(1);
            if y >= inner_top && y < inner_top.saturating_add(inner_h) {
                let row_in_view = (y - inner_top) as usize;
                let idx = self.list_state.offset() + row_in_view;
                if idx < FIELD_COUNT {
                    self.list_state.select(Some(idx));
                    self.detail_cursor_line = 0;
                }
            }
            return Ok(());
        }
        if ui::rect_contains(r.detail, col, y) {
            self.focused_pane = FocusedPane::Detail;
            return Ok(());
        }
        if ui::rect_contains(r.csv, col, y) {
            self.focused_pane = FocusedPane::Csv;
            return Ok(());
        }
        if ui::rect_contains(r.log, col, y) {
            self.focused_pane = FocusedPane::Log;
        }
        Ok(())
    }

    /// 詳細ペイン用プレーン行（表示・スクロール・visual と一致させる）
    pub fn detail_plain_lines(&self) -> Vec<String> {
        let i = self.selected_row();
        let help = field_help(i);
        let allowed = field_allowed_values(i);
        let mut lines = vec![
            format!("説明: {help}"),
            String::new(),
            format!("有効な値: {allowed}"),
        ];
        if self.editing {
            lines.push(String::new());
            lines.push("[編集中] Enter=確定 Esc=取消 ←/→ Home/End".into());
        } else if let Some(h) = field_space_hint(i) {
            lines.push(String::new());
            lines.push(format!("状態: {h}"));
        }
        lines.push(String::new());
        lines.push(format!("メッセージ: {}", self.status_line));
        lines
    }

    /// CSV プレビュー用（最大行数・バイト上限あり）
    pub fn csv_plain_lines(&self) -> Vec<String> {
        read_output_csv_plain(&self.config, 8192)
    }

    pub fn config_row_plain(&self, row: usize) -> String {
        if row >= FIELD_COUNT {
            return String::new();
        }
        let label = LABELS[row];
        let prefix = format!(" {:<22} ", label);
        let val = display_value(&self.config, row);
        format!("{prefix}{val}")
    }

    fn clamp_detail_cursor(&mut self) {
        let n = self.detail_plain_lines().len().max(1);
        self.detail_cursor_line = self.detail_cursor_line.min(n - 1);
    }

    fn clamp_csv_cursor(&mut self) {
        let n = self.csv_plain_lines().len().max(1);
        self.csv_cursor_line = self.csv_cursor_line.min(n - 1);
    }

    fn clamp_log_cursor(&mut self) {
        let n = self.logs.len();
        if n == 0 {
            self.log_cursor_line = 0;
            return;
        }
        self.log_cursor_line = self.log_cursor_line.min(n - 1);
    }

    pub fn clamp_all_pane_cursors(&mut self) {
        self.clamp_detail_cursor();
        self.clamp_csv_cursor();
        self.clamp_log_cursor();
    }

    fn move_detail_cursor(&mut self, delta: isize) {
        self.clamp_detail_cursor();
        let n = self.detail_plain_lines().len().max(1) as isize;
        let i = self.detail_cursor_line as isize;
        self.detail_cursor_line = (i + delta).clamp(0, n - 1) as usize;
    }

    fn move_csv_cursor(&mut self, delta: isize) {
        self.clamp_csv_cursor();
        let n = self.csv_plain_lines().len().max(1) as isize;
        let i = self.csv_cursor_line as isize;
        self.csv_cursor_line = (i + delta).clamp(0, n - 1) as usize;
    }

    fn move_log_cursor(&mut self, delta: isize) {
        let n = self.logs.len();
        if n == 0 {
            return;
        }
        self.log_follow_tail = false;
        self.clamp_log_cursor();
        let i = self.log_cursor_line as isize;
        self.log_cursor_line = (i + delta).clamp(0, (n - 1) as isize) as usize;
    }

    /// `gg` — フォーカス中ペインの先頭へ（一覧は先頭項目、ログは先頭行で末尾追従オフ）
    fn goto_focused_pane_top(&mut self) {
        self.clear_status_if_error();
        match self.focused_pane {
            FocusedPane::ConfigList => {
                self.list_state.select(Some(0));
                self.detail_cursor_line = 0;
            }
            FocusedPane::Detail => {
                self.detail_cursor_line = 0;
            }
            FocusedPane::Csv => {
                self.csv_cursor_line = 0;
            }
            FocusedPane::Log => {
                self.log_follow_tail = false;
                self.log_cursor_line = 0;
            }
        }
    }

    /// `G` — フォーカス中ペインの末尾へ（ログは末尾行＋新規ログで末尾追従）
    fn goto_focused_pane_bottom(&mut self) {
        self.clear_status_if_error();
        match self.focused_pane {
            FocusedPane::ConfigList => {
                self.list_state.select(Some(FIELD_COUNT - 1));
                self.detail_cursor_line = 0;
            }
            FocusedPane::Detail => {
                let n = self.detail_plain_lines().len().max(1);
                self.detail_cursor_line = n - 1;
            }
            FocusedPane::Csv => {
                let n = self.csv_plain_lines().len().max(1);
                self.csv_cursor_line = n - 1;
            }
            FocusedPane::Log => {
                let n = self.logs.len();
                if n == 0 {
                    self.log_cursor_line = 0;
                } else {
                    self.log_cursor_line = n - 1;
                    self.log_follow_tail = true;
                }
            }
        }
    }

    fn visual_line_bounds(vis: VisualSelection) -> (usize, usize) {
        let lo = vis.anchor.min(vis.cursor);
        let hi = vis.anchor.max(vis.cursor);
        (lo, hi)
    }

    fn yank_visual_selection(&mut self) {
        let Some(vis) = self.visual else {
            return;
        };
        let (lo, hi) = Self::visual_line_bounds(vis);
        let n_lines = hi.saturating_sub(lo).saturating_add(1);
        let text = match vis.pane {
            FocusedPane::ConfigList => {
                if lo >= FIELD_COUNT {
                    String::new()
                } else {
                    let end = hi.min(FIELD_COUNT - 1);
                    (lo..=end)
                        .map(|i| self.config_row_plain(i))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            FocusedPane::Detail => {
                let lines = self.detail_plain_lines();
                if lines.is_empty() || lo >= lines.len() {
                    String::new()
                } else {
                    let end = hi.min(lines.len() - 1);
                    lines[lo..=end].join("\n")
                }
            }
            FocusedPane::Csv => {
                let lines = self.csv_plain_lines();
                if lines.is_empty() || lo >= lines.len() {
                    String::new()
                } else {
                    let end = hi.min(lines.len() - 1);
                    lines[lo..=end].join("\n")
                }
            }
            FocusedPane::Log => {
                if self.logs.is_empty() || lo >= self.logs.len() {
                    String::new()
                } else {
                    let end = hi.min(self.logs.len() - 1);
                    self.logs[lo..=end].join("\n")
                }
            }
        };
        match clipboard::set_clipboard_text(&text) {
            Ok(()) => {
                self.status_error = false;
                self.status_line.clear();
                self.push_log(format!("クリップボードにコピーしました（{n_lines} 行）"));
            }
            Err(e) => {
                self.status_error = true;
                self.status_line = format!("クリップボードエラー: {e}");
            }
        }
        self.visual = None;
    }

    fn handle_visual_key(&mut self, key: KeyEvent) {
        let no_ctrl = !key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(mut vis) = self.visual else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.visual = None;
            }
            KeyCode::Char('y') if no_ctrl => {
                self.yank_visual_selection();
            }
            KeyCode::Char('v') | KeyCode::Char('V') if no_ctrl => {
                self.visual = None;
            }
            KeyCode::Down | KeyCode::Char('j') if no_ctrl => {
                let max = match vis.pane {
                    FocusedPane::ConfigList => FIELD_COUNT.saturating_sub(1),
                    FocusedPane::Detail => self.detail_plain_lines().len().saturating_sub(1),
                    FocusedPane::Csv => self.csv_plain_lines().len().saturating_sub(1),
                    FocusedPane::Log => self.logs.len().saturating_sub(1),
                };
                vis.cursor = (vis.cursor + 1).min(max);
                self.visual = Some(vis);
            }
            KeyCode::Up | KeyCode::Char('k') if no_ctrl => {
                vis.cursor = vis.cursor.saturating_sub(1);
                self.visual = Some(vis);
            }
            _ => {}
        }
    }

    fn enter_visual_mode(&mut self) {
        self.awaiting_second_g = false;
        let pane = self.focused_pane;
        let anchor = match pane {
            FocusedPane::ConfigList => self.selected_row(),
            FocusedPane::Detail => {
                self.clamp_detail_cursor();
                self.detail_cursor_line
            }
            FocusedPane::Csv => {
                self.clamp_csv_cursor();
                self.csv_cursor_line
            }
            FocusedPane::Log => {
                self.clamp_log_cursor();
                self.log_cursor_line
            }
        };
        self.visual = Some(VisualSelection {
            pane,
            anchor,
            cursor: anchor,
        });
    }

    const MAX_UNDO: usize = 128;

    fn trim_undo_stack(&mut self) {
        while self.undo_stack.len() > Self::MAX_UNDO {
            self.undo_stack.remove(0);
        }
    }

    fn trim_redo_stack(&mut self) {
        while self.redo_stack.len() > Self::MAX_UNDO {
            self.redo_stack.remove(0);
        }
    }

    /// 現在の `config` を undo 用に積む（新しい編集の直前に呼ぶ）
    fn push_undo_checkpoint(&mut self) {
        self.redo_stack.clear();
        self.undo_stack.push(self.config.clone());
        self.trim_undo_stack();
    }

    /// `u` / `Ctrl+R` の連鎖用にだけ積む（`redo_stack` は触らない）
    fn push_onto_undo_only(&mut self, cfg: Config) {
        self.undo_stack.push(cfg);
        self.trim_undo_stack();
    }

    fn undo_last_change(&mut self) {
        self.awaiting_second_g = false;
        if let Some(prev) = self.undo_stack.pop() {
            let replaced = std::mem::replace(&mut self.config, prev);
            self.redo_stack.push(replaced);
            self.trim_redo_stack();
            self.status_error = false;
            let msg = "直前の変更を取り消しました";
            self.status_line = msg.into();
            self.push_log(msg.into());
        } else {
            self.status_error = false;
            let msg = "これ以上取り消せません";
            self.status_line = msg.into();
            self.push_log(msg.into());
        }
    }

    fn redo_last_change(&mut self) {
        self.awaiting_second_g = false;
        if let Some(next) = self.redo_stack.pop() {
            let prev = std::mem::replace(&mut self.config, next);
            self.push_onto_undo_only(prev);
            self.status_error = false;
            let msg = "やり直しました（Ctrl+R）";
            self.status_line = msg.into();
            self.push_log(msg.into());
        } else {
            self.status_error = false;
            let msg = "これ以上進めません";
            self.status_line = msg.into();
            self.push_log(msg.into());
        }
    }

    pub fn selected_row(&self) -> usize {
        self.list_state.selected().unwrap_or(0).min(FIELD_COUNT - 1)
    }

    fn move_selection(&mut self, delta: isize) {
        if self.editing {
            return;
        }
        self.clear_status_if_error();
        self.awaiting_second_g = false;
        let i = self.selected_row() as isize;
        let n = FIELD_COUNT as isize;
        let j = (i + delta).clamp(0, n - 1);
        if j != i {
            self.detail_cursor_line = 0;
        }
        self.list_state.select(Some(j as usize));
    }

    fn clear_status_if_error(&mut self) {
        if self.status_error {
            self.status_line.clear();
            self.status_error = false;
        }
    }

    /// `cursor_at_end`: true = Enter / a（末尾）, false = i（先頭）
    fn begin_edit(&mut self, cursor_at_end: bool) {
        self.awaiting_second_g = false;
        let i = self.selected_row();
        self.edit_buffer = display_value(&self.config, i);
        let len = self.edit_buffer.chars().count();
        self.edit_cursor = if cursor_at_end { len } else { 0 };
        self.editing = true;
        self.status_line.clear();
        self.status_error = false;
    }

    fn cancel_editing(&mut self) {
        self.editing = false;
        self.edit_buffer.clear();
        self.edit_cursor = 0;
        self.status_line.clear();
        self.status_error = false;
    }

    fn commit_editing(&mut self) -> bool {
        let i = self.selected_row();
        let before = display_value(&self.config, i);
        self.push_undo_checkpoint();
        match apply_field(&mut self.config, i, &self.edit_buffer) {
            Ok(()) => {
                let after = display_value(&self.config, i);
                self.editing = false;
                self.edit_buffer.clear();
                self.edit_cursor = 0;
                self.status_line.clear();
                self.status_error = false;
                self.push_field_update_log(i, &before, &after);
                true
            }
            Err(msg) => {
                self.undo_stack.pop();
                self.status_line = format!("入力エラー: {msg}");
                self.status_error = true;
                false
            }
        }
    }

    fn push_field_update_log(&mut self, i: usize, before: &str, after: &str) {
        self.log_follow_tail = true;
        self.push_log(format!("更新: {}  {} → {}", LABELS[i], before, after));
    }

    fn push_log(&mut self, line: String) {
        const MAX: usize = 200;
        self.logs.push(line);
        if self.logs.len() > MAX {
            self.logs.drain(0..self.logs.len() - MAX);
        }
        if self.log_follow_tail {
            self.log_cursor_line = self.logs.len().saturating_sub(1);
        }
    }

    fn save(&mut self) {
        match save_config(&self.config_path, &self.config) {
            Ok(()) => {
                self.saved_config = self.config.clone();
                self.status_error = false;
                self.status_line = format!("保存しました: {}", self.config_path.display());
                self.push_log(self.status_line.clone());
            }
            Err(e) => {
                self.status_error = true;
                self.status_line = format!("保存エラー: {e:#}");
                self.push_log(self.status_line.clone());
            }
        }
    }

    fn run_convert(&mut self) {
        let mut cfg = self.config.clone();
        match run_pipeline(&mut cfg) {
            Ok(summary) => {
                self.status_error = false;
                self.config = cfg;
                let msg = format!(
                    "完了: records={} channels={} sampling_frequency_hz={}",
                    summary.records, summary.channels, summary.sampling_frequency_hz
                );
                self.status_line = msg.clone();
                self.push_log(msg);
                self.push_log(format!(
                    "出力: {}",
                    self.config
                        .output_dir
                        .join(&self.config.output_file_name)
                        .display()
                ));
            }
            Err(e) => {
                self.status_error = true;
                self.status_line = format!("変換エラー: {e:#}");
                self.push_log(self.status_line.clone());
            }
        }
    }

    /// `true` = quit
    fn handle_key(&mut self, key: KeyEvent) -> Result<bool> {
        if self.show_help {
            return Ok(self.handle_help_key(key.code));
        }

        if self.editing {
            return Ok(self.handle_edit_key(key));
        }

        if self.visual.is_some() {
            self.handle_visual_key(key);
            return Ok(false);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('r' | 'R') => {
                    self.clear_status_if_error();
                    self.redo_last_change();
                    return Ok(false);
                }
                _ => {}
            }
        }

        let no_ctrl = !key.modifiers.contains(KeyModifiers::CONTROL);

        if let KeyCode::Char(c) = key.code {
            if no_ctrl {
                if let Some(pane) = FocusedPane::from_digit(c) {
                    self.focused_pane = pane;
                    self.awaiting_second_g = false;
                    return Ok(false);
                }
            }
        }

        match key.code {
            KeyCode::Char('g') if no_ctrl => {
                if self.awaiting_second_g {
                    self.goto_focused_pane_top();
                    self.awaiting_second_g = false;
                } else {
                    self.awaiting_second_g = true;
                }
            }
            KeyCode::Char('G') if no_ctrl => {
                self.awaiting_second_g = false;
                self.goto_focused_pane_bottom();
            }
            other => {
                self.awaiting_second_g = false;
                match other {
                    KeyCode::Char('q') | KeyCode::Char('Q') => return Ok(true),
                    KeyCode::Char('?') => {
                        self.show_help = true;
                    }
                    KeyCode::Char('s') | KeyCode::Char('S') => self.save(),
                    KeyCode::Char('r') | KeyCode::Char('R') => self.run_convert(),
                    KeyCode::Char('u') | KeyCode::Char('U') => self.undo_last_change(),
                    KeyCode::Down | KeyCode::Char('j') if no_ctrl => match self.focused_pane {
                        FocusedPane::ConfigList => self.move_selection(1),
                        FocusedPane::Detail => self.move_detail_cursor(1),
                        FocusedPane::Csv => self.move_csv_cursor(1),
                        FocusedPane::Log => self.move_log_cursor(1),
                    },
                    KeyCode::Up | KeyCode::Char('k') if no_ctrl => match self.focused_pane {
                        FocusedPane::ConfigList => self.move_selection(-1),
                        FocusedPane::Detail => self.move_detail_cursor(-1),
                        FocusedPane::Csv => self.move_csv_cursor(-1),
                        FocusedPane::Log => self.move_log_cursor(-1),
                    },
                    KeyCode::Enter | KeyCode::Char('a')
                        if no_ctrl && self.focused_pane == FocusedPane::ConfigList =>
                    {
                        self.begin_edit(true);
                    }
                    KeyCode::Char('i')
                        if no_ctrl && self.focused_pane == FocusedPane::ConfigList =>
                    {
                        self.begin_edit(false);
                    }
                    KeyCode::Char(' ') if self.focused_pane == FocusedPane::ConfigList => {
                        self.toggle_current_if_toggleable();
                    }
                    KeyCode::Char('v') | KeyCode::Char('V') if no_ctrl => {
                        self.enter_visual_mode();
                    }
                    _ => {}
                }
            }
        }

        Ok(false)
    }

    pub fn visual_abs_range_for_pane(&self, pane: FocusedPane) -> Option<(usize, usize)> {
        self.visual
            .filter(|&v| v.pane == pane)
            .map(|v| Self::visual_line_bounds(v))
    }

    fn handle_help_key(&mut self, code: KeyCode) -> bool {
        if matches!(code, KeyCode::Esc | KeyCode::Char('?')) {
            self.show_help = false;
        }
        false
    }

    fn handle_edit_key(&mut self, key: KeyEvent) -> bool {
        let len = self.edit_buffer.chars().count();
        match key.code {
            KeyCode::Esc => {
                self.cancel_editing();
            }
            KeyCode::Enter => {
                self.commit_editing();
            }
            KeyCode::Left => {
                self.edit_cursor = self.edit_cursor.saturating_sub(1);
            }
            KeyCode::Right => {
                self.edit_cursor = (self.edit_cursor + 1).min(len);
            }
            KeyCode::Home => {
                self.edit_cursor = 0;
            }
            KeyCode::End => {
                self.edit_cursor = len;
            }
            KeyCode::Backspace => {
                if self.edit_cursor > 0
                    && remove_char_before(&mut self.edit_buffer, self.edit_cursor)
                {
                    self.edit_cursor -= 1;
                }
            }
            KeyCode::Delete => {
                let _ = remove_char_at(&mut self.edit_buffer, self.edit_cursor);
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                insert_char_at(&mut self.edit_buffer, self.edit_cursor, c);
                self.edit_cursor += 1;
            }
            _ => {}
        }
        false
    }

    fn toggle_current_if_toggleable(&mut self) {
        self.status_error = false;
        self.status_line = "この設定には Space で切り替える項目はありません".into();
    }

    pub fn list_row_line(&self, i: usize) -> ratatui::text::Line<'static> {
        use ratatui::style::{Color, Modifier, Style};
        use ratatui::text::Span;

        let label = LABELS[i];
        let prefix = format!(" {:<22} ", label);
        let sel = self.selected_row();
        if self.editing && i == sel {
            let mut spans = vec![Span::raw(prefix)];
            spans.extend(edit_value_spans(&self.edit_buffer, self.edit_cursor));
            ratatui::text::Line::from(spans)
        } else {
            let val = display_value(&self.config, i);
            let modified = field_modified_from_saved(&self.config, &self.saved_config, i);
            let val_style = if modified {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default()
            };
            let sel_bg = Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD);
            let (p_sty, v_sty) = if i == sel {
                (sel_bg, val_style.patch(sel_bg))
            } else {
                (Style::default(), val_style)
            };
            ratatui::text::Line::from(vec![Span::styled(prefix, p_sty), Span::styled(val, v_sty)])
        }
    }
}

/// CSV プレビュー用プレーン行（スクロール・visual・クリップボード用）
fn read_output_csv_plain(config: &Config, max_lines: usize) -> Vec<String> {
    const MAX_BYTES: usize = 256 * 1024;
    let path = config.output_dir.join(&config.output_file_name);

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => {
            return if e.kind() == ErrorKind::NotFound {
                vec![
                    "このパスにはまだ CSV がありません。".into(),
                    String::new(),
                    "ヒント: r を押して変換すると、ここに内容が表示されます。".into(),
                    String::new(),
                    path.display().to_string(),
                ]
            } else {
                vec![
                    "ファイルを開けませんでした。".into(),
                    path.display().to_string(),
                    e.to_string(),
                ]
            };
        }
    };

    if bytes.is_empty() {
        return vec!["（中身が空です）".into()];
    }

    let bytes = if bytes.len() > MAX_BYTES {
        &bytes[..MAX_BYTES]
    } else {
        &bytes
    };
    let (decoded, _, _) = SHIFT_JIS.decode(bytes);
    let mut lines = decoded
        .lines()
        .map(|line| line.trim_end_matches('\r').to_string())
        .collect::<Vec<_>>();
    if lines.is_empty() {
        return vec!["（中身が空です）".into()];
    }

    let limit = max_lines.max(1);
    if lines.len() > limit {
        lines.truncate(limit);
        if let Some(last) = lines.last_mut() {
            *last = "… （表示行数の上限で省略）".into();
        }
    }
    if bytes.len() == MAX_BYTES {
        lines.push("… （バイト数の上限で省略）".into());
    }
    lines
}

fn insert_char_at(s: &mut String, char_idx: usize, ch: char) {
    let byte = s
        .char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    s.insert(byte, ch);
}

fn remove_char_before(s: &mut String, char_idx: usize) -> bool {
    if char_idx == 0 {
        return false;
    }
    let start = s.char_indices().nth(char_idx - 1).map(|(b, _)| b).unwrap();
    let end = s
        .char_indices()
        .nth(char_idx)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    s.replace_range(start..end, "");
    true
}

fn remove_char_at(s: &mut String, char_idx: usize) -> bool {
    let len = s.chars().count();
    if char_idx >= len {
        return false;
    }
    let start = s.char_indices().nth(char_idx).map(|(b, _)| b).unwrap();
    let end = s
        .char_indices()
        .nth(char_idx + 1)
        .map(|(b, _)| b)
        .unwrap_or(s.len());
    s.replace_range(start..end, "");
    true
}

fn edit_value_spans(buffer: &str, cursor_char: usize) -> Vec<ratatui::text::Span<'static>> {
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::Span;

    // 選択行の DarkGray ハイライトと重なると、緑背景が端末によってはセルに乗らず「透明」に見える。
    // 黄×黒は互換性が高く、カーソル位置が常に判別しやすい。
    let cursor_style = Style::default()
        .fg(Color::Black)
        .bg(Color::Yellow)
        .add_modifier(Modifier::BOLD);

    let chars: Vec<char> = buffer.chars().collect();
    let len = chars.len();
    let cursor = cursor_char.min(len);

    let mut out = Vec::new();
    let before: String = chars[..cursor].iter().collect();
    out.push(Span::raw(before));

    if cursor < len {
        let c = chars[cursor];
        let sym = if c == '\n' {
            "⏎".to_string()
        } else {
            c.to_string()
        };
        out.push(Span::styled(sym, cursor_style));
        let after: String = chars[cursor + 1..].iter().collect();
        out.push(Span::raw(after));
    } else {
        // 末尾カーソルは半角スペースだと見えにくいのでブロックを表示
        out.push(Span::styled("\u{258c}", cursor_style));
    }

    out
}

fn field_modified_from_saved(current: &Config, saved: &Config, i: usize) -> bool {
    match i {
        0 => current.input_path != saved.input_path,
        1 => current.output_dir != saved.output_dir,
        2 => current.output_file_name != saved.output_file_name,
        _ => false,
    }
}

fn display_value(c: &Config, i: usize) -> String {
    match i {
        0 => c.input_path.display().to_string(),
        1 => c.output_dir.display().to_string(),
        2 => c.output_file_name.clone(),
        _ => String::new(),
    }
}

fn apply_field(config: &mut Config, i: usize, raw: &str) -> Result<(), String> {
    let t = raw.trim();
    match i {
        0 => config.input_path = PathBuf::from(t),
        1 => config.output_dir = PathBuf::from(t),
        2 => {
            if t.is_empty() {
                return Err("output_file_name が空です".into());
            }
            config.output_file_name = t.to_string();
        }
        _ => return Err("不明なフィールド".into()),
    }
    Ok(())
}

pub fn field_label(i: usize) -> &'static str {
    LABELS.get(i).copied().unwrap_or("")
}

pub fn field_help(i: usize) -> &'static str {
    HELP_TEXT.get(i).copied().unwrap_or("")
}

/// 右ペイン「有効な値」用（編集入力・TOML の解釈に合わせる）
pub fn field_allowed_values(i: usize) -> &'static str {
    match i {
        0 => "任意のパス文字列（入力 .KS3）",
        1 => "任意のディレクトリパス",
        2 => "空でない 1 行のファイル名",
        _ => "",
    }
}

/// Space で切り替え可能な行だけヒントを返す（bool と endian を分けて表示）
pub fn field_space_hint(i: usize) -> Option<&'static str> {
    let _ = i;
    None
}
