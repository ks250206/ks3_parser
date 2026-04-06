use std::fs::File;
use std::io::{stdout, BufRead, BufReader, ErrorKind};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use ks2_parser::{load_config, save_config, run_pipeline, Config, Endianness};
use ratatui::DefaultTerminal;

use super::clipboard;
use super::ui;

pub const FIELD_COUNT: usize = 18;

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

const LABELS: [&str; FIELD_COUNT] = [
    "input_path",
    "output_dir",
    "output_file_name",
    "auto_detect_offsets",
    "header_byte",
    "variable_header_byte",
    "data_header_byte",
    "data_skip_byte",
    "footer_byte",
    "values_per_record",
    "endianness",
    "ADConverterScale",
    "ADRangeCoefficient",
    "ADCoefficient",
    "coefficient.CH1",
    "coefficient.CH2",
    "coefficient.CH3",
    "coefficient.CH4",
];

const HELP_TEXT: [&str; FIELD_COUNT] = [
    "入力 .ks2 ファイルのパス",
    "CSV の出力ディレクトリ",
    "出力 CSV ファイル名",
    "true: CRLF から variable/data/footer オフセットを自動判定",
    "データ開始計算の基準となるヘッダ長（バイト）",
    "可変ヘッダ長（バイト）",
    "データヘッダ長（バイト）",
    "データ本体直前の追加スキップ（バイト）",
    "ファイル末尾から除外するバイト数",
    "1 レコードあたりの値数（現在は 4 固定で検証あり）",
    "i32 のエンディアン: little または big",
    "AD 変換スケール（0 不可）",
    "AD レンジ係数",
    "AD 係数",
    "ch1 のチャンネル補正係数",
    "ch2 のチャンネル補正係数",
    "ch3 のチャンネル補正係数",
    "ch4 のチャンネル補正係数",
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
                self.push_log(format!(
                    "クリップボードにコピーしました（{n_lines} 行）"
                ));
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
        self.list_state
            .selected()
            .unwrap_or(0)
            .min(FIELD_COUNT - 1)
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
        self.push_undo_checkpoint();
        let mut cfg = self.config.clone();
        let before_run = cfg.clone();
        match run_pipeline(&mut cfg) {
            Ok(summary) => {
                self.status_error = false;
                for idx in [5usize, 6, 8] {
                    let b = display_value(&before_run, idx);
                    let a = display_value(&cfg, idx);
                    if b != a {
                        self.push_field_update_log(idx, &b, &a);
                    }
                }
                self.config = cfg;
                let msg = format!(
                    "完了: records={} variable_header={} data_header={} footer={}",
                    summary.records,
                    summary.variable_header_byte,
                    summary.data_header_byte,
                    summary.footer_byte
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
                self.undo_stack.pop();
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
                if self.edit_cursor > 0 && remove_char_before(&mut self.edit_buffer, self.edit_cursor)
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
        let i = self.selected_row();
        if i == 3 {
            self.push_undo_checkpoint();
            let before = display_value(&self.config, 3);
            self.config.auto_detect_offsets = !self.config.auto_detect_offsets;
            let after = display_value(&self.config, 3);
            self.push_field_update_log(3, &before, &after);
        } else if i == 10 {
            self.push_undo_checkpoint();
            let before = display_value(&self.config, 10);
            self.config.endianness = match self.config.endianness {
                Endianness::Little => Endianness::Big,
                Endianness::Big => Endianness::Little,
            };
            let after = display_value(&self.config, 10);
            self.push_field_update_log(10, &before, &after);
        }
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
                (
                    sel_bg,
                    val_style.patch(sel_bg),
                )
            } else {
                (Style::default(), val_style)
            };
            ratatui::text::Line::from(vec![
                Span::styled(prefix, p_sty),
                Span::styled(val, v_sty),
            ])
        }
    }
}

/// CSV プレビュー用プレーン行（スクロール・visual・クリップボード用）
fn read_output_csv_plain(config: &Config, max_lines: usize) -> Vec<String> {
    const MAX_BYTES: usize = 256 * 1024;
    let path = config.output_dir.join(&config.output_file_name);

    let file = match File::open(&path) {
        Ok(f) => f,
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

    let mut reader = BufReader::new(file);
    let mut out: Vec<String> = Vec::new();
    let mut buf = String::new();
    let mut total_bytes = 0usize;
    let limit = max_lines.max(1);

    if limit == 1 {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => return vec!["（中身が空です）".into()],
            Ok(_) => {
                if total_bytes + buf.len() > MAX_BYTES {
                    return vec!["… （バイト数の上限で省略）".into()];
                }
                let trimmed = buf.trim_end_matches(['\r', '\n']).to_string();
                return vec![trimmed];
            }
            Err(e) => return vec![format!("読み込みエラー: {e}")],
        }
    }

    let mut truncated_by_bytes = false;
    while out.len() < limit {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => break,
            Ok(_) => {
                if total_bytes + buf.len() > MAX_BYTES {
                    out.push("… （バイト数の上限で省略）".into());
                    truncated_by_bytes = true;
                    break;
                }
                total_bytes += buf.len();
                let trimmed = buf.trim_end_matches(['\r', '\n']).to_string();
                out.push(trimmed);
            }
            Err(e) => return vec![format!("読み込みエラー: {e}")],
        }
    }

    if !truncated_by_bytes {
        buf.clear();
        let has_more = reader.read_line(&mut buf).map(|n| n > 0).unwrap_or(false);
        if has_more && out.len() == limit {
            out.pop();
            out.push("… （表示行数の上限で省略）".into());
        }
    }

    if out.is_empty() {
        vec!["（中身が空です）".into()]
    } else {
        out
    }
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
    let start = s
        .char_indices()
        .nth(char_idx - 1)
        .map(|(b, _)| b)
        .unwrap();
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
        3 => current.auto_detect_offsets != saved.auto_detect_offsets,
        4 => current.header_byte != saved.header_byte,
        5 => current.variable_header_byte != saved.variable_header_byte,
        6 => current.data_header_byte != saved.data_header_byte,
        7 => current.data_skip_byte != saved.data_skip_byte,
        8 => current.footer_byte != saved.footer_byte,
        9 => current.values_per_record != saved.values_per_record,
        10 => current.endianness != saved.endianness,
        11 => current.ad_converter_scale != saved.ad_converter_scale,
        12 => current.ad_range_coefficient != saved.ad_range_coefficient,
        13 => current.ad_coefficient != saved.ad_coefficient,
        14 => current.coefficient.ch1 != saved.coefficient.ch1,
        15 => current.coefficient.ch2 != saved.coefficient.ch2,
        16 => current.coefficient.ch3 != saved.coefficient.ch3,
        17 => current.coefficient.ch4 != saved.coefficient.ch4,
        _ => false,
    }
}

fn display_value(c: &Config, i: usize) -> String {
    match i {
        0 => c.input_path.display().to_string(),
        1 => c.output_dir.display().to_string(),
        2 => c.output_file_name.clone(),
        3 => c.auto_detect_offsets.to_string(),
        4 => c.header_byte.to_string(),
        5 => c.variable_header_byte.to_string(),
        6 => c.data_header_byte.to_string(),
        7 => c.data_skip_byte.to_string(),
        8 => c.footer_byte.to_string(),
        9 => c.values_per_record.to_string(),
        10 => match c.endianness {
            Endianness::Little => "little".to_string(),
            Endianness::Big => "big".to_string(),
        },
        11 => format_f64(c.ad_converter_scale),
        12 => format_f64(c.ad_range_coefficient),
        13 => format_f64(c.ad_coefficient),
        14 => format_f64(c.coefficient.ch1),
        15 => format_f64(c.coefficient.ch2),
        16 => format_f64(c.coefficient.ch3),
        17 => format_f64(c.coefficient.ch4),
        _ => String::new(),
    }
}

fn format_f64(x: f64) -> String {
    if x.fract() == 0.0 {
        format!("{:.0}", x)
    } else {
        x.to_string()
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
        3 => config.auto_detect_offsets = parse_bool(t)?,
        4 => config.header_byte = t.parse().map_err(|_| format!("無効な usize: {t}"))?,
        5 => {
            config.variable_header_byte = t
                .parse()
                .map_err(|_| format!("無効な usize: {t}"))?
        }
        6 => {
            config.data_header_byte = t
                .parse()
                .map_err(|_| format!("無効な usize: {t}"))?
        }
        7 => config.data_skip_byte = t.parse().map_err(|_| format!("無効な usize: {t}"))?,
        8 => config.footer_byte = t.parse().map_err(|_| format!("無効な usize: {t}"))?,
        9 => {
            config.values_per_record = t
                .parse()
                .map_err(|_| format!("無効な usize: {t}"))?
        }
        10 => config.endianness = parse_endian(t)?,
        11 => {
            config.ad_converter_scale = t
                .parse()
                .map_err(|_| format!("無効な数値: {t}"))?
        }
        12 => {
            config.ad_range_coefficient = t
                .parse()
                .map_err(|_| format!("無効な数値: {t}"))?
        }
        13 => {
            config.ad_coefficient = t
                .parse()
                .map_err(|_| format!("無効な数値: {t}"))?
        }
        14 => {
            config.coefficient.ch1 = t
                .parse()
                .map_err(|_| format!("無効な数値: {t}"))?
        }
        15 => {
            config.coefficient.ch2 = t
                .parse()
                .map_err(|_| format!("無効な数値: {t}"))?
        }
        16 => {
            config.coefficient.ch3 = t
                .parse()
                .map_err(|_| format!("無効な数値: {t}"))?
        }
        17 => {
            config.coefficient.ch4 = t
                .parse()
                .map_err(|_| format!("無効な数値: {t}"))?
        }
        _ => return Err("不明なフィールド".into()),
    }
    Ok(())
}

fn parse_bool(t: &str) -> Result<bool, String> {
    match t.to_lowercase().as_str() {
        "true" | "1" | "yes" => Ok(true),
        "false" | "0" | "no" => Ok(false),
        _ => Err(format!("bool として解釈できません: {t}")),
    }
}

fn parse_endian(t: &str) -> Result<Endianness, String> {
    match t.to_lowercase().as_str() {
        "little" => Ok(Endianness::Little),
        "big" => Ok(Endianness::Big),
        _ => Err(format!("endianness は little または big: {t}")),
    }
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
        0 => "任意のパス文字列（入力 .ks2）",
        1 => "任意のディレクトリパス",
        2 => "空でない 1 行のファイル名",
        3 => "true / false（編集では 1/0, yes/no も可）",
        4 | 5 | 6 | 7 | 8 => "0 以上の整数",
        9 => "本プログラムは 4 固定で検証（変更は非推奨）",
        10 => "little または big（編集では小文字）",
        11 => "0 以外の浮動小数点数",
        12 | 13 | 14 | 15 | 16 | 17 => "浮動小数点数",
        _ => "",
    }
}

/// Space で切り替え可能な行だけヒントを返す（bool と endian を分けて表示）
pub fn field_space_hint(i: usize) -> Option<&'static str> {
    match i {
        3 => Some("Space で true / false を切り替え"),
        10 => Some("Space で little ↔ big を切り替え"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use ks2_parser::{ChannelCoefficient, Config, Endianness};
    use tempfile::tempdir;

    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    use super::*;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn press_ctrl_r() -> KeyEvent {
        KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)
    }

    fn base_config(dir: &std::path::Path) -> Config {
        Config {
            input_path: dir.join("i.ks2"),
            output_dir: dir.to_path_buf(),
            output_file_name: "p.csv".into(),
            auto_detect_offsets: false,
            header_byte: 0,
            variable_header_byte: 0,
            data_header_byte: 0,
            data_skip_byte: 0,
            footer_byte: 0,
            values_per_record: 4,
            endianness: Endianness::Little,
            ad_converter_scale: 1.0,
            ad_range_coefficient: 1.0,
            ad_coefficient: 1.0,
            coefficient: ChannelCoefficient {
                ch1: 1.0,
                ch2: 1.0,
                ch3: 1.0,
                ch4: 1.0,
            },
        }
    }

    #[test]
    fn apply_field_paths_and_numbers() {
        let dir = tempdir().unwrap();
        let mut c = base_config(dir.path());
        apply_field(&mut c, 0, " /tmp/in.ks2 ").unwrap();
        assert_eq!(c.input_path, PathBuf::from("/tmp/in.ks2"));
        apply_field(&mut c, 4, "99").unwrap();
        assert_eq!(c.header_byte, 99);
        assert!(apply_field(&mut c, 4, "x").is_err());
    }

    #[test]
    fn apply_field_bool_and_endian() {
        let dir = tempdir().unwrap();
        let mut c = base_config(dir.path());
        apply_field(&mut c, 3, "yes").unwrap();
        assert!(c.auto_detect_offsets);
        apply_field(&mut c, 10, "BIG").unwrap();
        assert_eq!(c.endianness, Endianness::Big);
        assert!(apply_field(&mut c, 10, "middle").is_err());
        assert!(apply_field(&mut c, 3, "maybe").is_err());
    }

    #[test]
    fn apply_field_unknown_index() {
        let dir = tempdir().unwrap();
        let mut c = base_config(dir.path());
        assert!(apply_field(&mut c, 99, "1").is_err());
    }

    #[test]
    fn apply_field_empty_output_name() {
        let dir = tempdir().unwrap();
        let mut c = base_config(dir.path());
        assert!(apply_field(&mut c, 2, "   ").is_err());
    }

    #[test]
    fn insert_remove_chars_utf8() {
        let mut s = String::from("ab");
        insert_char_at(&mut s, 1, 'é');
        assert_eq!(s, "aéb");
        assert!(!remove_char_before(&mut s, 0));
        assert!(remove_char_before(&mut s, 2));
        assert_eq!(s, "ab");
        assert!(remove_char_at(&mut s, 0));
        assert_eq!(s, "b");
    }

    #[test]
    fn csv_preview_not_found_is_soft_message() {
        let dir = tempdir().unwrap();
        let mut c = base_config(dir.path());
        c.output_file_name = "nope.csv".into();
        let lines = read_output_csv_plain(&c, 5);
        let joined = lines.join("");
        assert!(joined.contains("まだ"));
    }

    #[test]
    fn csv_preview_single_line_mode() {
        let dir = tempdir().unwrap();
        let mut c = base_config(dir.path());
        c.output_file_name = "one.csv".into();
        fs::write(dir.path().join("one.csv"), "only\nnext\n").unwrap();
        let lines = read_output_csv_plain(&c, 1);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("only"));
    }

    #[test]
    fn csv_preview_empty_file_message() {
        let dir = tempdir().unwrap();
        let mut c = base_config(dir.path());
        c.output_file_name = "empty.csv".into();
        fs::write(dir.path().join("empty.csv"), b"").unwrap();
        let lines = read_output_csv_plain(&c, 3);
        assert!(lines[0].contains("空"));
    }

    #[test]
    fn csv_preview_reads_lines() {
        let dir = tempdir().unwrap();
        let mut c = base_config(dir.path());
        c.output_file_name = "x.csv".into();
        fs::write(dir.path().join("x.csv"), "a,b\n1,2\n").unwrap();
        let lines = read_output_csv_plain(&c, 10);
        assert!(lines.iter().any(|l| l.contains("a,b")));
    }

    #[test]
    fn csv_preview_truncates_with_ellipsis() {
        let dir = tempdir().unwrap();
        let mut c = base_config(dir.path());
        c.output_file_name = "many.csv".into();
        let mut body = String::new();
        for i in 0..30 {
            body.push_str(&format!("row{i}\n"));
        }
        fs::write(dir.path().join("many.csv"), body).unwrap();
        let lines = read_output_csv_plain(&c, 5);
        let last = lines.last().unwrap();
        assert!(last.contains("省略"));
    }

    #[test]
    fn edit_value_spans_newline_and_tail_cursor() {
        let s = edit_value_spans("a\nb", 1);
        let flat: String = s.iter().map(|sp| sp.to_string()).collect();
        assert!(flat.contains('⏎'));
        let t = edit_value_spans("z", 1);
        assert_eq!(t.len(), 2);
        let tail: String = t.iter().map(|sp| sp.to_string()).collect();
        assert!(tail.contains('\u{258c}'), "末尾カーソルは ▌ で表示");
    }

    #[test]
    fn format_f64_fraction() {
        assert_eq!(format_f64(1.5), "1.5");
        assert_eq!(format_f64(2.0), "2");
    }

    #[test]
    fn field_label_help_bounds() {
        assert_eq!(field_label(0), "input_path");
        assert!(!field_help(FIELD_COUNT - 1).is_empty());
        assert!(!field_allowed_values(FIELD_COUNT - 1).is_empty());
        assert_eq!(field_label(999), "");
        assert_eq!(field_allowed_values(999), "");
        assert_eq!(field_space_hint(3).unwrap(), "Space で true / false を切り替え");
        assert_eq!(
            field_space_hint(10).unwrap(),
            "Space で little ↔ big を切り替え"
        );
        assert!(field_space_hint(0).is_none());
    }

    #[test]
    fn display_value_paths() {
        let dir = tempdir().unwrap();
        let c = base_config(dir.path());
        assert!(display_value(&c, 0).contains("i.ks2"));
    }

    fn write_app_config(dir: &std::path::Path) -> PathBuf {
        let ks2 = dir.join("x.ks2");
        let mut raw = vec![0u8; 4];
        for v in [1_i32, 2, 3, 4] {
            raw.extend(v.to_le_bytes());
        }
        fs::write(&ks2, &raw).unwrap();
        let out = dir.join("out");
        let path = dir.join("app.toml");
        let body = format!(
            r#"input_path = "{}"
output_dir = "{}"
output_file_name = "t.csv"
auto_detect_offsets = false
header_byte = 4
variable_header_byte = 0
data_header_byte = 0
data_skip_byte = 0
footer_byte = 0
values_per_record = 4
endianness = "little"
ADConverterScale = 1.0
ADRangeCoefficient = 1.0
ADCoefficient = 1.0

[coefficient]
CH1 = 1.0
CH2 = 1.0
CH3 = 1.0
CH4 = 1.0
"#,
            ks2.display(),
            out.display()
        );
        fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn draw_smoke_normal_help_and_editing() {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;

        use crate::tui::ui;

        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();

        let mut term = Terminal::new(TestBackend::new(100, 50)).unwrap();
        term.draw(|f| ui::draw(f, &mut app)).unwrap();

        app.show_help = true;
        term.draw(|f| ui::draw(f, &mut app)).unwrap();

        app.show_help = false;
        app.editing = true;
        app.edit_buffer = "ab".into();
        app.edit_cursor = 1;
        term.draw(|f| ui::draw(f, &mut app)).unwrap();
    }

    #[test]
    fn list_row_line_covers_all_fields() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let app = App::new(p).unwrap();
        for i in 0..FIELD_COUNT {
            let _ = app.list_row_line(i);
        }
    }

    #[test]
    fn handle_key_quit_and_help() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p.clone()).unwrap();
        assert!(app.handle_key(press(KeyCode::Char('q'))).unwrap());

        let mut app = App::new(p).unwrap();
        assert!(!app.handle_key(press(KeyCode::Char('?'))).unwrap());
        assert!(app.show_help);
        assert!(!app.handle_key(press(KeyCode::Esc)).unwrap());
        assert!(!app.show_help);
        assert!(!app.handle_key(press(KeyCode::Char('?'))).unwrap());
        assert!(app.show_help);
        assert!(!app.handle_key(press(KeyCode::Char('?'))).unwrap());
    }

    #[test]
    fn handle_key_move_save_run_space() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();

        assert!(!app.handle_key(press(KeyCode::Char('j'))).unwrap());
        assert!(!app.handle_key(press(KeyCode::Down)).unwrap());
        assert!(!app.handle_key(press(KeyCode::Char('k'))).unwrap());
        assert!(!app.handle_key(press(KeyCode::Up)).unwrap());

        app.list_state.select(Some(3));
        assert!(!app.handle_key(press(KeyCode::Char(' '))).unwrap());
        app.list_state.select(Some(10));
        assert!(!app.handle_key(press(KeyCode::Char(' '))).unwrap());

        assert!(!app.handle_key(press(KeyCode::Char('s'))).unwrap());
        assert!(!app.handle_key(press(KeyCode::Char('r'))).unwrap());
    }

    #[test]
    fn handle_key_undo_after_space_toggle() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();

        app.list_state.select(Some(3));
        let before = app.config.auto_detect_offsets;
        assert!(!app.handle_key(press(KeyCode::Char(' '))).unwrap());
        assert_ne!(before, app.config.auto_detect_offsets);
        assert!(!app.handle_key(press(KeyCode::Char('u'))).unwrap());
        assert_eq!(before, app.config.auto_detect_offsets);
    }

    #[test]
    fn field_update_log_scrolls_to_tail() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();
        app.logs = vec!["old".into(), "older".into()];
        app.log_cursor_line = 0;
        app.log_follow_tail = false;
        app.list_state.select(Some(3));
        assert!(!app.handle_key(press(KeyCode::Char(' '))).unwrap());
        assert!(app.log_follow_tail);
        assert_eq!(app.log_cursor_line, app.logs.len().saturating_sub(1));
    }

    #[test]
    fn handle_key_redo_ctrl_r_after_undo() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();

        app.list_state.select(Some(3));
        let before = app.config.auto_detect_offsets;
        assert!(!app.handle_key(press(KeyCode::Char(' '))).unwrap());
        assert_ne!(before, app.config.auto_detect_offsets);
        assert!(!app.handle_key(press(KeyCode::Char('u'))).unwrap());
        assert_eq!(before, app.config.auto_detect_offsets);
        assert!(!app.handle_key(press_ctrl_r()).unwrap());
        assert_ne!(before, app.config.auto_detect_offsets);
    }

    #[test]
    fn handle_key_redo_empty_stack_message() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();

        assert!(!app.handle_key(press_ctrl_r()).unwrap());
        assert!(app.status_line.contains("進めません"));
    }

    #[test]
    fn handle_key_undo_empty_stack_message() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();

        assert!(!app.handle_key(press(KeyCode::Char('u'))).unwrap());
        assert!(app.status_line.contains("取り消せません"));
    }

    #[test]
    fn handle_key_gg_and_goto_bottom() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();

        assert!(!app.handle_key(press(KeyCode::Char('G'))).unwrap());
        assert_eq!(app.selected_row(), FIELD_COUNT - 1);

        assert!(!app.handle_key(press(KeyCode::Char('g'))).unwrap());
        assert!(app.awaiting_second_g);
        assert!(!app.handle_key(press(KeyCode::Char('g'))).unwrap());
        assert_eq!(app.selected_row(), 0);
        assert!(!app.awaiting_second_g);
    }

    #[test]
    fn handle_key_gg_g_stays_on_focused_pane() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();

        app.focused_pane = FocusedPane::Log;
        app.logs = vec!["a".into(), "b".into(), "c".into()];
        app.log_cursor_line = 0;
        app.log_follow_tail = false;
        assert!(!app.handle_key(press(KeyCode::Char('G'))).unwrap());
        assert_eq!(app.focused_pane, FocusedPane::Log);
        assert_eq!(app.log_cursor_line, 2);
        assert!(app.log_follow_tail);

        app.focused_pane = FocusedPane::Detail;
        app.detail_cursor_line = 5;
        assert!(!app.handle_key(press(KeyCode::Char('g'))).unwrap());
        assert!(!app.handle_key(press(KeyCode::Char('g'))).unwrap());
        assert_eq!(app.focused_pane, FocusedPane::Detail);
        assert_eq!(app.detail_cursor_line, 0);
    }

    #[test]
    fn begin_edit_i_puts_cursor_at_start_a_at_end() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();
        app.list_state.select(Some(2));

        assert!(!app.handle_key(press(KeyCode::Char('i'))).unwrap());
        assert!(app.editing);
        assert_eq!(app.edit_cursor, 0);
        assert!(!app.handle_key(press(KeyCode::Esc)).unwrap());

        assert!(!app.handle_key(press(KeyCode::Char('a'))).unwrap());
        assert!(app.editing);
        assert_eq!(app.edit_cursor, app.edit_buffer.chars().count());
    }

    #[test]
    fn handle_key_edit_buffer_roundtrip() {
        let dir = tempdir().unwrap();
        let p = write_app_config(dir.path());
        let mut app = App::new(p).unwrap();
        app.list_state.select(Some(4));

        assert!(!app.handle_key(press(KeyCode::Enter)).unwrap());
        assert!(app.editing);
        assert!(!app.handle_key(press(KeyCode::Backspace)).unwrap());
        assert!(!app.handle_key(press(KeyCode::Char('9'))).unwrap());
        assert!(!app.handle_key(press(KeyCode::Left)).unwrap());
        assert!(!app.handle_key(press(KeyCode::Right)).unwrap());
        assert!(!app.handle_key(press(KeyCode::Home)).unwrap());
        assert!(!app.handle_key(press(KeyCode::End)).unwrap());
        assert!(!app.handle_key(press(KeyCode::Delete)).unwrap());
        assert!(!app.handle_key(press(KeyCode::Enter)).unwrap());
        assert!(!app.editing);
        assert_eq!(app.config.header_byte, 9);

        assert!(!app.handle_key(press(KeyCode::Enter)).unwrap());
        assert!(!app.handle_key(press(KeyCode::Char('x'))).unwrap());
        assert!(!app.handle_key(press(KeyCode::Esc)).unwrap());
        assert!(!app.editing);
        assert_eq!(app.config.header_byte, 9);

        assert!(!app.handle_key(press(KeyCode::Enter)).unwrap());
        assert!(!app.handle_key(press(KeyCode::Backspace)).unwrap());
        assert!(!app.handle_key(press(KeyCode::Char('x'))).unwrap());
        assert!(!app.handle_key(press(KeyCode::Enter)).unwrap());
        assert!(app.status_error);
        assert!(app.editing);
        assert!(app.status_line.starts_with("入力エラー:"));
    }
}
