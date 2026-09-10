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

/// Describes what UI element is current selected by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiSelection {
    Tempo,
    Volume,
}

/// Describes the result of a key press.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InputResult {
    Unhandled,
    Handled,
    Quit,
}

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

/// Truncates `text` to fit within `max_width` terminal cells, appending an
/// ellipsis if it had to cut anything.
fn truncate_with_ellipsis(text: &str, max_width: usize) -> String {
    if text.chars().count() <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::default();
    }
    let truncated = text.chars().take(max_width - 1).collect::<String>();
    format!("{truncated}…")
}

fn draw(f: &mut Frame, player: &Player, metadata: &TrackMetadata, selected: &UiSelection) {
    let box_area = centered_rect(60, 5, f.area());

    // draw the program title over the top of the border
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
            Constraint::Length(1), // seekbar
            Constraint::Length(1), // tempo / status / volume
        ])
        .split(inner);

    // draw the track title in the first row
    let title = Paragraph::new(Line::from(Span::styled(
        truncate_with_ellipsis(&metadata.title, inner.width.into()),
        Style::default().add_modifier(Modifier::BOLD),
    )))
    .alignment(Alignment::Center);
    f.render_widget(title, rows[0]);

    // draw the seekbar in the second row
    let pos = metadata.tempo_control.duration_from_tempo(player.get_pos());
    let elapsed = format_time(pos.as_secs());
    let total = format_time(metadata.length.as_secs());

    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio((pos.as_secs_f64() / metadata.length.as_secs_f64()).min(1.0))
        .label(format!("{elapsed} / {total}"));
    f.render_widget(gauge, rows[1]);

    // draw status in the third row
    let status = if player.is_paused() {
        "paused"
    } else {
        "playing"
    };
    let tempo_text = format!("Tempo: {:.2}x", metadata.tempo_control.tempo());
    let volume_text = format!("Volume: {:<3}", (player.volume() * 100.0).round() as u8);
    let side_width = tempo_text.len().max(volume_text.len()).try_into().unwrap();

    let footer = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(side_width),
            Constraint::Fill(1),
            Constraint::Length(side_width),
        ])
        .split(rows[2]);

    let unselected_style = Style::default().fg(Color::DarkGray);
    let selected_style = Style::default().fg(Color::Cyan);

    let tempo_style = if *selected == UiSelection::Tempo {
        selected_style
    } else {
        unselected_style
    };
    let volume_style = if *selected == UiSelection::Volume {
        selected_style
    } else {
        unselected_style
    };

    f.render_widget(
        Paragraph::new(tempo_text)
            .style(tempo_style)
            .alignment(Alignment::Left),
        footer[0],
    );
    f.render_widget(
        Paragraph::new(status)
            .fg(Color::DarkGray)
            .alignment(Alignment::Center),
        footer[1],
    );
    f.render_widget(
        Paragraph::new(volume_text)
            .style(volume_style)
            .alignment(Alignment::Right),
        footer[2],
    );
}

fn handle_input(
    event: Event,
    player: &Player,
    metadata: &TrackMetadata,
    selected: &mut UiSelection,
) -> InputResult {
    const SEEK_INC: Duration = Duration::from_secs(5);
    const VOLUME_INC: f32 = 0.1;
    const TEMPO_INC: f64 = 0.05;

    let key = match event {
        Event::Key(key) if key.kind.is_press() => key,
        _ => return InputResult::Unhandled,
    };

    match key.code {
        KeyCode::Char(' ') => {
            if player.is_paused() {
                // restart if at the end of the track
                if player.get_pos() >= metadata.length_with_tempo() {
                    let _ = player.try_seek(Duration::ZERO);
                }
                player.play()
            } else {
                player.pause()
            }
        }
        KeyCode::Char('t') => {
            *selected = UiSelection::Tempo;
        }
        KeyCode::Char('v') => {
            *selected = UiSelection::Volume;
        }
        KeyCode::Home | KeyCode::Backspace => {
            let _ = player.try_seek(Duration::ZERO);
        }
        KeyCode::End => {
            let _ = player.try_seek(metadata.length_with_tempo());
        }
        KeyCode::Left => {
            let pos = player.get_pos().saturating_sub(SEEK_INC);
            let _ = player.try_seek(pos);
        }
        KeyCode::Right => {
            let pos = (player.get_pos() + SEEK_INC).min(metadata.length_with_tempo());
            let _ = player.try_seek(pos);
        }
        KeyCode::Up if *selected == UiSelection::Tempo && metadata.tempo_control.tempo() < 2.0 => {
            let old_tempo = metadata.tempo_control.tempo();
            let new_tempo = old_tempo + TEMPO_INC;
            metadata.tempo_control.set_tempo(new_tempo);
            // increasing tempo, playback moves backward
            let _ = player.try_seek(player.get_pos().mul_f64(old_tempo / new_tempo));
        }
        KeyCode::Down
            if *selected == UiSelection::Tempo && metadata.tempo_control.tempo() > 0.15 =>
        {
            let old_tempo = metadata.tempo_control.tempo();
            let new_tempo = old_tempo - TEMPO_INC;
            metadata.tempo_control.set_tempo(new_tempo);
            // decreasing tempo, playback moves forward
            let _ = player.try_seek(player.get_pos().mul_f64(old_tempo / new_tempo));
        }
        KeyCode::Up if *selected == UiSelection::Volume && player.volume() < 1.0 => {
            player.set_volume(player.volume() + VOLUME_INC);
        }
        KeyCode::Down if *selected == UiSelection::Volume && player.volume() > 0.0 => {
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
    let mut selected = UiSelection::Tempo;

    loop {
        let redraw = tokio::select! {
            maybe_event = reader.next() => {
                let Some(event) = maybe_event else {
                    break;
                };

                match handle_input(event?, &player, &metadata, &mut selected) {
                    InputResult::Quit => break,
                    InputResult::Handled => true,
                    InputResult::Unhandled => false,
                }
            },
            _ = interval.tick() => {
                let pos = metadata.tempo_control.duration_from_tempo(player.get_pos()).as_secs();
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
            terminal.draw(|f| draw(f, &player, &metadata, &selected))?;
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
