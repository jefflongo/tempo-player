use std::time::Duration;

use anyhow::Result;
use crossterm::event::EventStream;
use crossterm::event::{Event, KeyCode};
use futures::StreamExt;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use rodio::Player;
use tokio::time;

use crate::audio::TrackMetadata;

/// Converts time in seconds to the format HH:MM:SS or MM:SS.
fn format_time(seconds: u64) -> String {
    let ss = seconds % 60;
    let mm = seconds / 60;
    let hh = seconds / 3600;
    if hh > 0 {
        format!("{hh}:{mm:02}:{ss:02}")
    } else {
        format!("{mm}:{ss:02}")
    }
}

/// Carves a fixed-size rectangle out of the middle of `area`.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(height),
            Constraint::Fill(1),
        ])
        .split(area);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Fill(1),
            Constraint::Length(width),
            Constraint::Fill(1),
        ])
        .split(vertical[1]);

    horizontal[1]
}

fn draw(f: &mut Frame, player: &Player, metadata: &TrackMetadata) {
    let box_area = centered_rect(60, 5, f.area());

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Tempo Player ")
        .title_alignment(Alignment::Center);
    let inner = block.inner(box_area);
    f.render_widget(block, box_area);

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // track name
            Constraint::Length(1), // gauge / seekbar
            Constraint::Length(1), // status
        ])
        .split(inner);

    let title = Paragraph::new(Line::from(Span::styled(
        &metadata.title,
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    f.render_widget(title, rows[0]);

    let pos = player.get_pos();
    let elapsed = format_time(pos.as_secs());
    let remaining = format_time(metadata.length.as_secs());

    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio((pos.as_secs_f64() / metadata.length.as_secs_f64()).min(1.0))
        .label(format!("{elapsed} / {remaining}"));
    f.render_widget(gauge, rows[1]);

    let status = if !player.is_paused() {
        "playing"
    } else {
        "paused"
    };
    let footer = Paragraph::new(status)
        .fg(Color::DarkGray)
        .alignment(Alignment::Center);
    f.render_widget(footer, rows[2]);
}

enum InputResult {
    Unhandled,
    Handled,
    Quit,
}

fn handle_input(event: Event, player: &Player, metadata: &TrackMetadata) -> InputResult {
    const SEEK_INC: Duration = Duration::from_secs(5);
    const VOLUME_INC: f32 = 0.1;

    let key = match event {
        Event::Key(key) if key.kind.is_press() => key,
        _ => return InputResult::Unhandled,
    };

    match key.code {
        KeyCode::Char(' ') => {
            if player.is_paused() {
                // restart if at the end of the track
                if player.get_pos() >= metadata.length {
                    let _ = player.try_seek(Duration::ZERO);
                }
                player.play()
            } else {
                player.pause()
            }
        }
        KeyCode::Backspace => {
            let _ = player.try_seek(Duration::ZERO);
        }
        KeyCode::Left => {
            let _ = player.try_seek(player.get_pos().saturating_sub(SEEK_INC));
        }
        KeyCode::Right => {
            let pos = (player.get_pos() + SEEK_INC).min(metadata.length);
            let _ = player.try_seek(pos);
        }
        KeyCode::Up if player.volume() + VOLUME_INC <= 1.0 => {
            player.set_volume(player.volume() + VOLUME_INC);
        }
        KeyCode::Down if player.volume() > VOLUME_INC => {
            player.set_volume(player.volume() - VOLUME_INC);
        }
        KeyCode::Char('q') | KeyCode::Esc => return InputResult::Quit,
        _ => return InputResult::Unhandled,
    }
    InputResult::Handled
}

async fn cli_player_main(
    terminal: &mut DefaultTerminal,
    player: Player,
    metadata: TrackMetadata,
) -> Result<()> {
    let mut reader = EventStream::new();
    let mut last_pos = None;
    let mut interval = time::interval(Duration::from_millis(50));

    loop {
        let redraw = tokio::select! {
            maybe_event = reader.next() => {
                let Some(event) = maybe_event else {
                    break;
                };

                match handle_input(event?, &player, &metadata) {
                    InputResult::Quit => break,
                    InputResult::Handled => true,
                    InputResult::Unhandled => false,
                }
            },
            _ = interval.tick() => {
                let pos = player.get_pos().as_secs();
                if Some(pos) != last_pos {
                    last_pos = Some(pos);
                    true
                } else {
                    false
                }
            }
            _ = metadata.track_ended.notified() => {
                if metadata.loop_track {
                    let _ = player.try_seek(Duration::ZERO);
                } else {
                    player.pause();
                }
                true
            }
        };

        if redraw {
            terminal.draw(|f| draw(f, &player, &metadata))?;
        }
    }
    Ok(())
}

pub async fn cli_player(player: Player, metadata: TrackMetadata) -> Result<()> {
    let mut terminal = ratatui::init();
    let result = cli_player_main(&mut terminal, player, metadata).await;
    ratatui::restore();
    result
}
