use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Wrap};

use super::app::{App, FIELD_COUNT, FocusedPane, field_label};

/// マウスヒットテスト用（`draw` と同一の分割）
#[derive(Clone, Copy)]
pub struct PaneHitRects {
    pub list: Rect,
    pub detail: Rect,
    pub csv: Rect,
    pub log: Rect,
}

pub struct MainLayout {
    pub panes: PaneHitRects,
    pub footer: Rect,
}

pub fn layout_main(area: Rect) -> MainLayout {
    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(8),
            Constraint::Length(6),
            Constraint::Length(1),
        ])
        .split(area);

    let row = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(42), Constraint::Percentage(58)])
        .split(main_chunks[0]);

    let right_col = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
        .split(row[1]);

    MainLayout {
        panes: PaneHitRects {
            list: row[0],
            detail: right_col[0],
            csv: right_col[1],
            log: main_chunks[1],
        },
        footer: main_chunks[2],
    }
}

pub fn rect_contains(r: Rect, col: u16, row: u16) -> bool {
    col >= r.x
        && col < r.x.saturating_add(r.width)
        && row >= r.y
        && row < r.y.saturating_add(r.height)
}

fn scroll_first_visible(cursor: usize, viewport: usize, total: usize) -> usize {
    if total == 0 || viewport == 0 {
        return 0;
    }
    let max_first = total.saturating_sub(viewport);
    cursor
        .saturating_sub(viewport.saturating_sub(1))
        .min(max_first)
}

/// 行ビジュアル選択（一覧の選択行ハイライトより手前に出すため黄×黒）
fn merge_visual_line(line: Line<'static>, highlight: bool) -> Line<'static> {
    if !highlight {
        return line;
    }
    let patch = Style::default()
        .bg(Color::Yellow)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    Line::from(
        line.spans
            .into_iter()
            .map(|sp| {
                let t = sp.content.to_string();
                Span::styled(t, sp.style.patch(patch))
            })
            .collect::<Vec<_>>(),
    )
}

fn pane_title_cursor_suffix(app: &App, pane: FocusedPane, total_lines: usize) -> String {
    if total_lines == 0 && matches!(pane, FocusedPane::Log) {
        return "· (空)".to_string();
    }
    let n = total_lines.max(1);
    let (cursor0, vis) = match pane {
        FocusedPane::Detail => (
            app.detail_cursor_line,
            app.visual.filter(|&v| v.pane == FocusedPane::Detail),
        ),
        FocusedPane::Csv => (
            app.csv_cursor_line,
            app.visual.filter(|&v| v.pane == FocusedPane::Csv),
        ),
        FocusedPane::Log => (
            app.log_cursor_line,
            app.visual.filter(|&v| v.pane == FocusedPane::Log),
        ),
        FocusedPane::ConfigList => return String::new(),
    };
    let max0 = n.saturating_sub(1);
    let c = cursor0.min(max0) + 1;
    let mut out = format!("· 行{c}/{n}");
    if let Some(v) = vis {
        out.push_str(&format!(" · v{}", v.cursor + 1));
    }
    out
}

/// 詳細・CSV 用: visual 優先、次にカーソル行の薄い背景
fn text_pane_line(
    s: String,
    abs: usize,
    visual_range: Option<(usize, usize)>,
    cursor_line: usize,
) -> Line<'static> {
    let in_vis = visual_range
        .map(|(lo, hi)| abs >= lo && abs <= hi)
        .unwrap_or(false);
    let is_cur = abs == cursor_line;
    let st = if in_vis {
        Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else if is_cur {
        Style::default().bg(Color::Rgb(58, 58, 58))
    } else {
        Style::default()
    };
    Line::from(Span::styled(s, st))
}

/// ログ用: 左に絶対インデックス、本文は visual / カーソルで強調
fn log_pane_line(
    s: String,
    abs: usize,
    visual_range: Option<(usize, usize)>,
    cursor_line: usize,
) -> Line<'static> {
    let in_vis = visual_range
        .map(|(lo, hi)| abs >= lo && abs <= hi)
        .unwrap_or(false);
    let is_cur = abs == cursor_line;
    let idx_style = Style::default().fg(Color::White);
    let body_style = if in_vis {
        Style::default()
            .bg(Color::Yellow)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD)
    } else if is_cur {
        Style::default().bg(Color::Rgb(58, 58, 58))
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::styled(format!("{:>4} │ ", abs + 1), idx_style),
        Span::styled(s, body_style),
    ])
}

/// 非フォーカスは白枠、フォーカス中はタイトルと同じテーマ色
fn pane_border_style(pane: FocusedPane, focused: bool) -> Style {
    if !focused {
        return Style::default().fg(Color::White);
    }
    let c = match pane {
        FocusedPane::ConfigList => Color::Cyan,
        FocusedPane::Detail => Color::Green,
        FocusedPane::Csv => Color::Blue,
        FocusedPane::Log => Color::Magenta,
    };
    Style::default().fg(c)
}

pub fn draw(frame: &mut Frame<'_>, app: &mut App) {
    app.clamp_all_pane_cursors();

    let area = frame.area();
    let lay = layout_main(area);
    app.hit_rects = Some(lay.panes);

    let list_vr = app.visual_abs_range_for_pane(FocusedPane::ConfigList);
    let items: Vec<ListItem> = (0..FIELD_COUNT)
        .map(|i| {
            let mut line = app.list_row_line(i);
            if let Some((lo, hi)) = list_vr {
                if i >= lo && i <= hi {
                    line = merge_visual_line(line, true);
                }
            }
            ListItem::new(line)
        })
        .collect();

    let list_focus = app.focused_pane == FocusedPane::ConfigList;
    let list = List::new(items)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_border_style(FocusedPane::ConfigList, list_focus))
                .title(format!(
                    "[{}] config.toml ",
                    FocusedPane::ConfigList.title_prefix()
                ))
                .title_style(
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
        )
        // 選択行の背景は list_row_line 内の Span で付与（visual の黄背景が負けないように）
        .highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

    frame.render_stateful_widget(list, lay.panes.list, &mut app.list_state);

    let i = app.selected_row();
    let detail_plain = app.detail_plain_lines();
    let detail_title = format!(
        "[{}] {} {}",
        FocusedPane::Detail.title_prefix(),
        field_label(i),
        pane_title_cursor_suffix(app, FocusedPane::Detail, detail_plain.len()),
    );
    let detail_h = lay.panes.detail.height.saturating_sub(2).max(1) as usize;
    let detail_first = scroll_first_visible(app.detail_cursor_line, detail_h, detail_plain.len());
    let detail_vr = app.visual_abs_range_for_pane(FocusedPane::Detail);

    let detail_lines: Vec<Line> = detail_plain
        .iter()
        .enumerate()
        .skip(detail_first)
        .take(detail_h)
        .map(|(abs, s)| text_pane_line(s.clone(), abs, detail_vr, app.detail_cursor_line))
        .collect();

    let detail_focus = app.focused_pane == FocusedPane::Detail;
    let detail = Paragraph::new(detail_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_border_style(FocusedPane::Detail, detail_focus))
                .title(detail_title)
                .title_style(Style::default().fg(Color::Green)),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(detail, lay.panes.detail);

    let csv_plain = app.csv_plain_lines();
    let csv_h = lay.panes.csv.height.saturating_sub(2).max(1) as usize;
    let csv_first = scroll_first_visible(app.csv_cursor_line, csv_h, csv_plain.len());
    let csv_vr = app.visual_abs_range_for_pane(FocusedPane::Csv);

    let csv_lines: Vec<Line> = csv_plain
        .iter()
        .enumerate()
        .skip(csv_first)
        .take(csv_h)
        .map(|(abs, s)| text_pane_line(s.clone(), abs, csv_vr, app.csv_cursor_line))
        .collect();

    let csv_name = app.config.output_file_name.clone();
    let csv_focus = app.focused_pane == FocusedPane::Csv;
    let csv_title = format!(
        "[{}] output CSV ({}) {}",
        FocusedPane::Csv.title_prefix(),
        csv_name,
        pane_title_cursor_suffix(app, FocusedPane::Csv, csv_plain.len()),
    );
    let csv_para = Paragraph::new(csv_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(pane_border_style(FocusedPane::Csv, csv_focus))
                .title(csv_title)
                .title_style(
                    Style::default()
                        .fg(Color::Blue)
                        .add_modifier(Modifier::BOLD),
                ),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(csv_para, lay.panes.csv);

    let n_log = app.logs.len();
    let log_title = format!(
        "[{}] ログ {}",
        FocusedPane::Log.title_prefix(),
        pane_title_cursor_suffix(app, FocusedPane::Log, n_log),
    );
    let log_block = Block::default()
        .borders(Borders::ALL)
        .border_style(pane_border_style(
            FocusedPane::Log,
            app.focused_pane == FocusedPane::Log,
        ))
        .title(log_title)
        .title_style(Style::default().fg(Color::Magenta));
    let inner = log_block.inner(lay.panes.log);
    frame.render_widget(log_block, lay.panes.log);

    let log_h = inner.height as usize;
    let log_first = scroll_first_visible(app.log_cursor_line, log_h, n_log);
    let log_vr = app.visual_abs_range_for_pane(FocusedPane::Log);

    let log_text: Vec<Line> = app
        .logs
        .iter()
        .enumerate()
        .skip(log_first)
        .take(log_h)
        .map(|(abs, s)| log_pane_line(s.clone(), abs, log_vr, app.log_cursor_line))
        .collect();

    let log_para = Paragraph::new(log_text).wrap(Wrap { trim: false });
    frame.render_widget(
        log_para,
        inner.inner(Margin {
            horizontal: 1,
            vertical: 0,
        }),
    );

    let key = |s: &'static str| {
        Span::styled(
            format!(" {s} "),
            Style::default().fg(Color::Black).bg(Color::Gray),
        )
    };
    let ctrl_r = Span::styled(
        " Ctrl+R ",
        Style::default().fg(Color::Black).bg(Color::Gray),
    );
    let footer = Paragraph::new(Line::from(vec![
        key("1-4"),
        Span::raw("ペイン "),
        key("j/k"),
        Span::raw("移動 "),
        key("v"),
        Span::raw("選択 "),
        key("y"),
        Span::raw("コピー "),
        key("gg"),
        Span::raw("先頭 "),
        key("G"),
        Span::raw("末尾 "),
        key("i"),
        Span::raw("/"),
        key("a"),
        Span::raw("/"),
        key("Enter"),
        Span::raw("編集 "),
        key("Space"),
        Span::raw("切替 "),
        key("s"),
        Span::raw("保存 "),
        key("r"),
        Span::raw("実行 "),
        key("u"),
        Span::raw("戻す "),
        ctrl_r,
        Span::raw("進む "),
        key("?"),
        Span::raw("ヘルプ "),
        key("q"),
        Span::raw("終了"),
    ]));

    frame.render_widget(footer, lay.footer);

    if app.show_help {
        draw_help(frame, area);
    }
}

fn draw_help(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" キーバインド ")
        .title_style(Style::default().fg(Color::Cyan));

    let text = vec![
        Line::from(""),
        Line::from(" 1–4             ペイン切替（1=設定一覧 2=詳細 3=CSV 4=ログ）"),
        Line::from(" マウス           各ペイン内を左クリックでフォーカス（一覧は行選択）"),
        Line::from(" j / k            フォーカス中のペインで移動（一覧=項目、他=スクロール）"),
        Line::from(" v                行ビジュアル選択開始（再押しで解除）"),
        Line::from(" y                選択範囲を OS クリップボードへ（Mac/Windows 対応）"),
        Line::from(" Esc              ビジュアル選択を解除"),
        Line::from(""),
        Line::from(" Enter           編集中は値の確定"),
        Line::from(" Esc             編集取消 / このヘルプを閉じる"),
        Line::from(" ← / →           編集中のカーソル移動"),
        Line::from(" Home / End      編集中の行頭・行末"),
        Line::from(" Backspace       カーソル前の文字を削除"),
        Line::from(" Delete          カーソル位置の文字を削除"),
        Line::from(
            " g (続けて g)    フォーカス中ペインの先頭へ（一覧=先頭項目 詳細/CSV/ログ=先頭行）",
        ),
        Line::from(" G               フォーカス中ペインの末尾へ（ログ末尾では新規ログに追従）"),
        Line::from(
            " u               直前の変更を取り消し（編集確定・Space 切替・変換成功のたびに履歴へ積む）",
        ),
        Line::from(" Ctrl+R          やり直し（u のあと。Mac/Windows とも端末の Ctrl 修飾キー）"),
        Line::from(""),
        Line::from(vec![Span::styled(
            "確定時に値が不正なときはメッセージ欄が赤字になります。",
            Style::default().fg(Color::Red),
        )]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "保存時は serde による TOML 再生成のため、元ファイルのコメントは失われます。",
            Style::default().fg(Color::Yellow),
        )]),
        Line::from(""),
    ];

    let help_area = centered_rect(62, 62, area);
    let inner = block.inner(help_area);
    frame.render_widget(block, help_area);
    let p = Paragraph::new(text).wrap(Wrap { trim: true });
    frame.render_widget(
        p,
        inner.inner(Margin {
            horizontal: 1,
            vertical: 0,
        }),
    );
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(popup_layout[1])[1]
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::centered_rect;
    use super::rect_contains;
    use super::scroll_first_visible;

    #[test]
    fn centered_rect_inside_parent() {
        let parent = Rect::new(10, 20, 100, 50);
        let c = centered_rect(60, 40, parent);
        assert!(c.x >= parent.x);
        assert!(c.y >= parent.y);
        assert!(c.x + c.width <= parent.x + parent.width);
        assert!(c.y + c.height <= parent.y + parent.height);
        assert!(c.width > 0 && c.height > 0);
    }

    #[test]
    fn scroll_first_visible_keeps_cursor_in_view() {
        assert_eq!(scroll_first_visible(0, 5, 10), 0);
        assert_eq!(scroll_first_visible(9, 5, 10), 5);
        assert_eq!(scroll_first_visible(5, 5, 10), 1);
    }

    #[test]
    fn rect_contains_bounds() {
        let r = Rect::new(5, 10, 20, 8);
        assert!(rect_contains(r, 5, 10));
        assert!(rect_contains(r, 24, 17));
        assert!(!rect_contains(r, 4, 10));
        assert!(!rect_contains(r, 25, 10));
        assert!(!rect_contains(r, 5, 18));
    }
}
