//! Reusable TUI widgets: panes, status/input bars, lobby, moves, chat, overlays.

use crate::session::store::{
    ChatKind, ChatScope, ConnectionState, GameView, LogLevel, Store,
};
use crate::ui::tui::board_render;
use crate::ui::tui::theme::Theme;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// Standard Zellij-style pane frame. The hue identifies the module; focus
/// raises brightness and border weight (design §9.5).
pub fn pane_block(title: &str, color: Color, theme: &Theme, focused: bool) -> Block<'static> {
    let (border_color, border_type) = if focused {
        (theme.focus(color), BorderType::Thick)
    } else {
        (theme.dim(color), BorderType::Rounded)
    };
    let title_style = Style::default()
        .fg(border_color)
        .add_modifier(Modifier::BOLD);
    let title_text = if focused {
        format!(" ▶ {title} ")
    } else {
        format!(" {title} ")
    };
    Block::default()
        .title(Span::styled(title_text, title_style))
        .borders(Borders::ALL)
        .border_type(border_type)
        .border_style(Style::default().fg(border_color))
}

/// Convenience wrapper that draws a pane and its inner content.
pub fn render_pane(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    color: Color,
    theme: &Theme,
    focused: bool,
    inner: impl FnOnce(&mut Frame, Rect),
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = pane_block(title, color, theme, focused);
    let inner_area = block.inner(area);
    frame.render_widget(block, area);
    if inner_area.width == 0 || inner_area.height == 0 {
        return;
    }
    inner(frame, inner_area);
}

/// Top status bar.
pub fn draw_status(
    frame: &mut Frame,
    area: Rect,
    store: &Store,
    theme: &Theme,
    notice: Option<&str>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (connection_color, connection_text) = match store.connection {
        ConnectionState::Authenticated => (theme.success, "authenticated"),
        ConnectionState::Connected => (theme.warning, "connected"),
        ConnectionState::Connecting => (theme.warning, "connecting"),
        ConnectionState::Error => (theme.error, "error"),
        ConnectionState::Offline => (theme.muted, "offline"),
    };
    let user = store
        .username
        .as_deref()
        .map(|name| format!(" · {name}"))
        .unwrap_or_default();
    let mut spans = vec![
        Span::styled(" ● ", Style::default().fg(connection_color)),
        Span::styled(
            format!("{}{}", connection_text, user),
            Style::default().fg(connection_color),
        ),
    ];
    if let Some(message) = notice
        .filter(|message| !message.is_empty())
        .or(store.connection_message.as_deref())
    {
        spans.push(Span::styled(
            format!("  {message}"),
            Style::default().fg(theme.error),
        ));
    }
    let unread = store.unread_total();
    let notifications = store.notifications.len();
    let scope = store.chat_scope.label();
    spans.push(Span::styled(
        format!(
            "   | {} | {} game(s) | {} | unread {}{}",
            scope,
            store.my_games.len(),
            store
                .online
                .map(|online| format!("{online} online"))
                .unwrap_or_else(|| "online ?".to_string()),
            unread,
            if notifications > 0 {
                format!(" | notes {notifications}")
            } else {
                String::new()
            }
        ),
        Style::default().fg(theme.muted),
    ));
    if let Some(summary) = store
        .focused_game()
        .and_then(|game| game.estimate_summary.as_ref())
        .filter(|_| {
            store
                .focused_game()
                .map(|game| game.estimate_active)
                .unwrap_or(false)
        })
    {
        spans.push(Span::styled(
            format!("   EST {} (local)", summary.result()),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default()),
        area,
    );
}

/// Bottom input line with a block cursor.
pub fn draw_input(
    frame: &mut Frame,
    area: Rect,
    prompt: &str,
    buffer: &str,
    cursor: usize,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let chars: Vec<char> = buffer.chars().collect();
    let cursor = cursor.min(chars.len());
    let before: String = chars[..cursor].iter().collect();
    let at = chars.get(cursor).copied().unwrap_or(' ');
    let after: String = if cursor < chars.len() {
        chars[cursor + 1..].iter().collect()
    } else {
        String::new()
    };
    let line = Line::from(vec![
        Span::styled(prompt, Style::default().fg(theme.board)),
        Span::raw(before),
        Span::styled(
            at.to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(after),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Centered login screen used before the store has a user.
pub fn draw_login(
    frame: &mut Frame,
    area: Rect,
    username: &str,
    password: &str,
    focus_password: bool,
    message: &str,
    theme: &Theme,
) {
    let area = centered_rect(70, 55, area);
    let block = pane_block("OGS Login", theme.lobby, theme, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    let lines = vec![
        Line::from(Span::styled(
            "Sign in to online-go.com",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        field_line("Username", username, !focus_password, theme),
        Line::from(""),
        field_line("Password", &"*".repeat(password.chars().count()), focus_password, theme),
        Line::from(""),
        Line::from(Span::styled(message, Style::default().fg(theme.warning))),
        Line::from(""),
        Line::from(Span::styled(
            "Tab: next field · Enter: submit · Ctrl+C: quit",
            Style::default().fg(theme.muted),
        )),
        Line::from(Span::styled(
            "Credentials are stored 0600 in your config directory; use `logout` to clear them.",
            Style::default().fg(theme.muted),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false }),
        inner,
    );
}

fn field_line<'a>(label: &str, value: &str, focused: bool, theme: &Theme) -> Line<'a> {
    let marker = if focused { "›" } else { " " };
    Line::from(vec![
        Span::styled(
            format!("{marker} {label:<9}"),
            Style::default().fg(if focused {
                theme.focus(theme.board)
            } else {
                theme.muted
            }),
        ),
        Span::styled(
            if value.is_empty() { " " } else { value }.to_string(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(if focused {
                    Modifier::UNDERLINED
                } else {
                    Modifier::empty()
                }),
        ),
    ])
}

/// A message overlay for help, version, notifications, or score details.
#[derive(Debug, Clone)]
pub struct Overlay {
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: u16,
}

impl Overlay {
    pub fn new(title: impl Into<String>, text: impl Into<String>) -> Self {
        let text = text.into();
        let lines = text.lines().map(str::to_string).collect();
        Self {
            title: title.into(),
            lines,
            scroll: 0,
        }
    }

    pub fn scroll_by(&mut self, delta: i32) {
        let max = self.lines.len().saturating_sub(1) as i32;
        self.scroll = (i32::from(self.scroll) + delta).clamp(0, max) as u16;
    }
}

pub fn draw_overlay(frame: &mut Frame, area: Rect, overlay: &Overlay, theme: &Theme) {
    let area = centered_rect(80, 75, area);
    frame.render_widget(Clear, area);
    let block = pane_block(&overlay.title, theme.accent, theme, true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let lines: Vec<Line> = overlay
        .lines
        .iter()
        .map(|line| Line::from(Span::raw(line.clone())))
        .collect();
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((overlay.scroll, 0))
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(theme.accent)),
        inner,
    );
}

/// Minimum-size warning (design §9.3).
pub fn draw_too_small(frame: &mut Frame, area: Rect, current: (u16, u16), min: (u16, u16), theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = vec![
        Line::from(Span::styled(
            "Terminal too small",
            Style::default()
                .fg(theme.error)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("current: ", Style::default().fg(theme.muted)),
            Span::styled(
                format!("{}x{}", current.0, current.1),
                Style::default().fg(theme.accent),
            ),
        ]),
        Line::from(vec![
            Span::styled("need:    ", Style::default().fg(theme.muted)),
            Span::styled(
                format!("{}x{}", min.0, min.1),
                Style::default().fg(theme.success),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Resize the window (or use full screen); this screen updates automatically.",
            Style::default().fg(theme.warning),
        )),
    ];
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL)),
        area,
    );
}

/// Game info + move list pane.
pub fn draw_moves(
    frame: &mut Frame,
    area: Rect,
    game: &GameView,
    theme: &Theme,
    scroll_back: u16,
) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        game.title(),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    )));
    let our = match game.our_color {
        Some(crate::engine::board::Color::Black) => "you are Black",
        Some(crate::engine::board::Color::White) => "you are White",
        None => "spectating",
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("● {}", game.black),
            Style::default().fg(theme.stone_black),
        ),
        Span::raw("  vs  "),
        Span::styled(
            format!("○ {}", game.white),
            Style::default().fg(theme.stone_white),
        ),
        Span::styled(format!("   ({our})"), Style::default().fg(theme.muted)),
    ]));
    lines.push(Line::from(Span::styled(
        format!(
            "rules {} · komi {} · {} · move {}",
            game.rules,
            game.komi,
            game.phase.label(),
            game.move_number()
        ),
        Style::default().fg(theme.muted),
    )));
    let active = game.clock.active();
    lines.push(Line::from(vec![
        Span::styled(
            format!(
                "● {}{}",
                game.clock.player(crate::engine::board::Color::Black).display(game.clock.control()),
                if active == Some(crate::engine::board::Color::Black) {
                    " *"
                } else {
                    ""
                }
            ),
            Style::default().fg(if active == Some(crate::engine::board::Color::Black) {
                theme.focus(theme.moves)
            } else {
                theme.muted
            }),
        ),
        Span::raw("   "),
        Span::styled(
            format!(
                "○ {}{}",
                game.clock.player(crate::engine::board::Color::White).display(game.clock.control()),
                if active == Some(crate::engine::board::Color::White) {
                    " *"
                } else {
                    ""
                }
            ),
            Style::default().fg(if active == Some(crate::engine::board::Color::White) {
                theme.focus(theme.moves)
            } else {
                theme.muted
            }),
        ),
    ]));
    lines.push(Line::from(Span::styled(
        format!(
            "captures B {} · W {}   last {}",
            game.game.captures(crate::engine::board::Color::Black),
            game.game.captures(crate::engine::board::Color::White),
            game.last_move()
                .map(|coord| coord.to_gtp())
                .unwrap_or_else(|| "-".to_string())
        ),
        Style::default().fg(theme.muted),
    )));
    if let Some(result) = &game.result {
        lines.push(Line::from(Span::styled(
            format!("result: {result}"),
            Style::default().fg(theme.success),
        )));
    }
    if game.phase == crate::session::store::GamePhase::StoneRemoval {
        lines.push(Line::from(Span::styled(
            format!(
                "OFFICIAL SCORING · marked dead {} · `dead`/`dame`, `score accept|reject`",
                game.scoring_dead.len()
            ),
            Style::default().fg(theme.warning),
        )));
    }
    if game.estimate_active {
        lines.push(Line::from(Span::styled(
            format!(
                "LOCAL ESTIMATE · dead {} · `[a10]` toggles, `estimate confirm|exit`",
                game.estimate_dead.len()
            ),
            Style::default().fg(theme.warning),
        )));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        format!("Moves ({})", game.game.history().len()),
        Style::default()
            .fg(theme.focus(theme.moves))
            .add_modifier(Modifier::BOLD),
    )));
    for mv in game.game.history() {
        let number = format!("{:>3}", mv.number);
        let color = match mv.color {
            crate::engine::board::Color::Black => "B",
            crate::engine::board::Color::White => "W",
        };
        let coord = mv
            .coord
            .map(|coord| coord.to_gtp())
            .unwrap_or_else(|| "pass".to_string());
        let captured = if mv.captured.is_empty() {
            String::new()
        } else {
            format!("  ×{}", mv.captured.len())
        };
        let style = if mv.number == game.move_number() {
            Style::default()
                .fg(theme.last_move)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.accent)
        };
        lines.push(Line::from(Span::styled(
            format!("{number} {color} {coord}{captured}"),
            style,
        )));
    }
    let total = lines.len() as u16;
    let view_height = area.height.saturating_sub(1);
    let offset = total
        .saturating_sub(view_height)
        .saturating_sub(scroll_back);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((offset, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Chat pane.
pub fn draw_chat(
    frame: &mut Frame,
    area: Rect,
    store: &Store,
    channel: &str,
    theme: &Theme,
    scroll_back: u16,
) {
    let channel_state = store.channels.get(channel);
    let mut lines: Vec<Line> = Vec::new();
    if let Some(topic) = channel_state.and_then(|state| state.topic.clone()) {
        if !topic.is_empty() {
            lines.push(Line::from(Span::styled(
                format!("topic: {topic}"),
                Style::default().fg(theme.muted),
            )));
        }
    }
    if let Some(state) = channel_state {
        for message in &state.messages {
            let line = match message.kind {
                ChatKind::System => Line::from(Span::styled(
                    format!("· {}", message.text),
                    Style::default().fg(theme.warning),
                )),
                ChatKind::Chat => Line::from(vec![
                    Span::styled(
                        format!("{}: ", message.author),
                        Style::default()
                            .fg(theme.focus(theme.chat))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(message.text.clone()),
                ]),
            };
            lines.push(line);
        }
    } else {
        lines.push(Line::from(Span::styled(
            "no messages yet",
            Style::default().fg(theme.muted),
        )));
    }
    let total = lines.len() as u16;
    let view_height = area.height.saturating_sub(1);
    let offset = total
        .saturating_sub(view_height)
        .saturating_sub(scroll_back);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((offset, 0))
            .wrap(Wrap { trim: true }),
        area,
    );
}

/// Lobby left-hand content (single outer pane, design §9.5).
pub fn draw_lobby_games(
    frame: &mut Frame,
    area: Rect,
    store: &Store,
    theme: &Theme,
    scroll_back: u16,
) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!(
            "online {} · active {} · your games {} · challenges {}",
            store
                .online
                .map(|online| online.to_string())
                .unwrap_or_else(|| "?".to_string()),
            store.lobby_games.len(),
            store.my_games.len(),
            store.my_challenges.len()
        ),
        Style::default().fg(theme.muted),
    )));
    lines.push(Line::from(""));
    let section = |title: &str, style: Style, lines_out: &mut Vec<Line>| {
        lines_out.push(Line::from(Span::styled(title.to_string(), style)));
    };
    section(
        "Active games",
        Style::default()
            .fg(theme.focus(theme.lobby))
            .add_modifier(Modifier::BOLD),
        &mut lines,
    );
    for game in store.lobby_games.iter().take(20) {
        lines.push(Line::from(Span::styled(
            format!("  {}", game.label()),
            Style::default().fg(theme.accent),
        )));
    }
    if store.lobby_games.is_empty() {
        lines.push(Line::from(Span::styled(
            "  no games visible",
            Style::default().fg(theme.muted),
        )));
    }
    lines.push(Line::from(""));
    section(
        "Your games",
        Style::default()
            .fg(theme.focus(theme.lobby))
            .add_modifier(Modifier::BOLD),
        &mut lines,
    );
    for game in store.my_games.iter().take(10) {
        lines.push(Line::from(Span::styled(
            format!("  {}", game.label()),
            Style::default().fg(theme.accent),
        )));
    }
    if store.my_games.is_empty() {
        lines.push(Line::from(Span::styled(
            "  none",
            Style::default().fg(theme.muted),
        )));
    }
    lines.push(Line::from(""));
    section(
        "Challenges",
        Style::default()
            .fg(theme.focus(theme.lobby))
            .add_modifier(Modifier::BOLD),
        &mut lines,
    );
    for challenge in store.my_challenges.iter().take(10) {
        lines.push(Line::from(Span::styled(
            format!(
                "  #{} from {} ({}x{}, {})",
                challenge.id,
                challenge.challenger,
                challenge.board_size,
                challenge.board_size,
                challenge.rules
            ),
            Style::default().fg(theme.accent),
        )));
    }
    if store.my_challenges.is_empty() {
        lines.push(Line::from(Span::styled(
            "  none",
            Style::default().fg(theme.muted),
        )));
    }
    if let Some(counts) = store
        .lobby_counts
        .iter()
        .map(|(list, count)| format!("{list} {count}"))
        .reduce(|left, right| format!("{left} · {right}"))
    {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("game counts: {counts}"),
            Style::default().fg(theme.muted),
        )));
    }
    if !store.notifications.is_empty() {
        lines.push(Line::from(""));
        section(
            "Notifications",
            Style::default()
                .fg(theme.focus(theme.warning))
                .add_modifier(Modifier::BOLD),
            &mut lines,
        );
        for note in store.notifications.iter().rev().take(3) {
            lines.push(Line::from(Span::styled(
                format!("  {}", note.text),
                Style::default().fg(theme.warning),
            )));
        }
    }
    let lines: Vec<Line> = lines;
    let offset = (lines.len() as u16)
        .saturating_sub(area.height.saturating_sub(1))
        .saturating_sub(scroll_back);
    frame.render_widget(
        Paragraph::new(lines)
            .scroll((offset, 0))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Recent log lines, used at the bottom of the lobby and in overlays.
pub fn recent_log_lines(store: &Store, count: usize) -> Vec<String> {
    store
        .logs
        .iter()
        .rev()
        .take(count)
        .rev()
        .map(|line| match line.level {
            LogLevel::Info => line.text.clone(),
            LogLevel::Success => format!("✓ {}", line.text),
            LogLevel::Warning => format!("! {}", line.text),
            LogLevel::Error => format!("✗ {}", line.text),
        })
        .collect()
}

/// Help overlay text.
pub fn help_overlay() -> Overlay {
    Overlay::new("Help", crate::ui::commands::help_text())
}

/// Version overlay text.
pub fn version_overlay(persist_path: &str) -> Overlay {
    Overlay::new(
        "Version",
        format!(
            "ogs-tui {}\n\nBuild: {}\nCredentials: {}\n\nLayers:\n  Transport → Session → Engine/Store → Presentation\n  Engine is IO-free and unit-tested offline.",
            env!("CARGO_PKG_VERSION"),
            option_env!("TARGET").unwrap_or("runtime"),
            persist_path
        ),
    )
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

/// Draws a vertical divider between the lobby's two halves.
pub fn draw_divider(frame: &mut Frame, area: Rect, x: u16, theme: &Theme) {
    let buffer = frame.buffer_mut();
    for y in area.y..area.y + area.height {
        if x < buffer.area.x + buffer.area.width && y < buffer.area.y + buffer.area.height {
            buffer[(x, y)].set_symbol("│");
            buffer[(x, y)].set_style(Style::default().fg(theme.dim(theme.lobby)));
        }
    }
}

/// Helper used by the app for the board pane title.
pub fn board_title(game: &GameView) -> String {
    let status = if game.estimate_active {
        "estimate"
    } else if game.phase == crate::session::store::GamePhase::StoneRemoval {
        "scoring"
    } else {
        "board"
    };
    format!("Board · {status} · {}", game.size)
}

/// Board rendering delegated here so the app only imports one module.
pub fn draw_board(
    frame: &mut Frame,
    area: Rect,
    game: &GameView,
    theme: &Theme,
    focused: bool,
) {
    board_render::draw_board(frame.buffer_mut(), area, game, theme, focused);
}

/// Chat pane title with unread count.
pub fn chat_title(store: &Store) -> String {
    let channel = store.current_chat_channel();
    let unread = store
        .channels
        .get(&channel)
        .map(|state| state.unread)
        .unwrap_or(0);
    let scope = match &store.chat_scope {
        ChatScope::World { .. } => format!("#{channel}"),
        ChatScope::Game { game_id } => format!("game {game_id}"),
    };
    if unread > 0 {
        format!("Chat {scope} ({unread} new)")
    } else {
        format!("Chat {scope}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_scroll_is_clamped() {
        let mut overlay = Overlay::new("t", "a\nb\nc");
        overlay.scroll_by(-5);
        assert_eq!(overlay.scroll, 0);
        overlay.scroll_by(100);
        assert_eq!(overlay.scroll, 2);
    }

    #[test]
    fn recent_logs_are_in_order_and_prefixed() {
        let mut store = Store::new("en");
        store.push_log(LogLevel::Info, "one");
        store.push_log(LogLevel::Error, "two");
        let lines = recent_log_lines(&store, 10);
        assert_eq!(lines, vec!["one".to_string(), "✗ two".to_string()]);
    }

    #[test]
    fn chat_title_reports_unread_only_for_the_current_channel() {
        let mut store = Store::new("en");
        assert_eq!(chat_title(&store), "Chat #en");
        store.switch_to_game(3);
        store.push_chat("game/3", "b", "hi");
        assert_eq!(chat_title(&store), "Chat game 3");
        store.push_chat("en", "a", "away message");
        assert_eq!(store.channels["en"].unread, 1);
        assert_eq!(chat_title(&store), "Chat game 3");
        store.ensure_channel("game/3").unread = 1;
        assert_eq!(chat_title(&store), "Chat game 3 (1 new)");
    }

    #[test]
    fn board_title_reflects_mode() {
        let mut game = GameView::empty(1, 9);
        assert!(board_title(&game).contains("board"));
        game.enter_estimate();
        assert!(board_title(&game).contains("estimate"));
    }
}
