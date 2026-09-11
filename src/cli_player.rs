use std::time::Duration;

use anyhow::Result;
use crossterm::event::{Event, KeyCode};
use crossterm::event::{EventStream, KeyEvent};
use futures::StreamExt;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};
use ratatui::{DefaultTerminal, Frame};
use rodio::Player;
use tokio::time;

use crate::audio::{PitchTranspose, TrackMetadata};

/// Describes what UI element is current selected by the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UiSelection {
    Tempo,
    Pitch,
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
        .horizontal_margin(1)
        .constraints([
            Constraint::Length(1), // track name / play status
            Constraint::Length(1), // seekbar
            Constraint::Length(1), // tempo / status / volume
        ])
        .split(inner);

    const STATUS_WIDTH: u16 = "playing".len() as u16;

    // draw the title in the first row
    let title_max_width = rows[0].width.saturating_sub(STATUS_WIDTH + 1);
    let title = truncate_with_ellipsis(&metadata.title, title_max_width.into());
    let title_len: u16 = title.chars().count().try_into().unwrap();

    let title_start = (rows[0].width.saturating_sub(title_len) / 2).max(STATUS_WIDTH + 1);
    let title_rect = Rect {
        x: rows[0].x + title_start,
        y: rows[0].y,
        width: rows[0].width.saturating_sub(title_start),
        height: rows[0].height,
    };

    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Left),
        title_rect,
    );

    // draw playing / paused status in the upper left corner
    let status = if player.is_paused() {
        "paused"
    } else {
        "playing"
    };

    let status_rect = Rect {
        x: rows[0].x,
        y: rows[0].y,
        width: STATUS_WIDTH,
        height: rows[0].height,
    };
    f.render_widget(
        Paragraph::new(status)
            .fg(Color::DarkGray)
            .alignment(Alignment::Left),
        status_rect,
    );

    // draw the seekbar in the second row
    let pos = metadata
        .controller
        .duration_from_tempo(player.get_pos())
        .min(metadata.length);
    let elapsed = format_time(pos.as_secs());
    let total = format_time(metadata.length.as_secs());

    let gauge = Gauge::default()
        .gauge_style(Style::default().fg(Color::Cyan))
        .ratio(pos.as_secs_f64() / metadata.length.as_secs_f64())
        .label(format!("{elapsed} / {total}"));
    f.render_widget(gauge, rows[1]);

    // draw settings in the third row
    let tempo = format!("Tempo: {:.2}x", metadata.controller.tempo());
    let pitch = format!("Pitch: {:<+3}", metadata.controller.pitch());
    let volume = format!("Volume: {:>3}%", (player.volume() * 100.0).round() as u8);

    let side_width = tempo.len().max(volume.len()).try_into().unwrap();
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
    let pitch_style = if *selected == UiSelection::Pitch {
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
        Paragraph::new(tempo)
            .style(tempo_style)
            .alignment(Alignment::Left),
        footer[0],
    );
    f.render_widget(
        Paragraph::new(pitch)
            .style(pitch_style)
            .alignment(Alignment::Center),
        footer[1],
    );
    f.render_widget(
        Paragraph::new(volume)
            .style(volume_style)
            .alignment(Alignment::Right),
        footer[2],
    );
}

fn handle_input(
    event: KeyEvent,
    player: &Player,
    metadata: &TrackMetadata,
    selected: &mut UiSelection,
) -> InputResult {
    const SEEK_INC: Duration = Duration::from_secs(5);
    const VOLUME_INC: f32 = 0.1;
    const TEMPO_INC: f64 = 0.05;

    if !event.kind.is_press() {
        return InputResult::Unhandled;
    }

    match event.code {
        // pause / unpause
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

        // cycle selection forward
        KeyCode::Tab => {
            *selected = match *selected {
                UiSelection::Tempo => UiSelection::Pitch,
                UiSelection::Pitch => UiSelection::Volume,
                UiSelection::Volume => UiSelection::Tempo,
            };
        }

        // cycle selection backward
        KeyCode::BackTab => {
            *selected = match *selected {
                UiSelection::Tempo => UiSelection::Volume,
                UiSelection::Pitch => UiSelection::Tempo,
                UiSelection::Volume => UiSelection::Pitch,
            };
        }

        // select tempo
        KeyCode::Char('t') => {
            *selected = UiSelection::Tempo;
        }

        // select pitch
        KeyCode::Char('p') => {
            *selected = UiSelection::Pitch;
        }

        // select volume
        KeyCode::Char('v') => {
            *selected = UiSelection::Volume;
        }

        // seek to beginning
        KeyCode::Home | KeyCode::Backspace => {
            let _ = player.try_seek(Duration::ZERO);
        }

        // seek to end
        KeyCode::End => {
            let _ = player.try_seek(metadata.length_with_tempo());
        }

        // seek backward
        KeyCode::Left => {
            let pos = player.get_pos().saturating_sub(SEEK_INC);
            let _ = player.try_seek(pos);
        }

        // seek forward
        KeyCode::Right => {
            let pos = (player.get_pos() + SEEK_INC).min(metadata.length_with_tempo());
            let _ = player.try_seek(pos);
        }

        // increase tempo
        KeyCode::Up if *selected == UiSelection::Tempo && metadata.controller.tempo() < 2.0 => {
            let old_tempo = metadata.controller.tempo();
            let new_tempo = old_tempo + TEMPO_INC;
            metadata.controller.set_tempo(new_tempo);
            // increasing tempo, playback moves backward
            let _ = player.try_seek(player.get_pos().mul_f64(old_tempo / new_tempo));
        }

        // decrease tempo
        KeyCode::Down if *selected == UiSelection::Tempo && metadata.controller.tempo() > 0.15 => {
            let old_tempo = metadata.controller.tempo();
            let new_tempo = old_tempo - TEMPO_INC;
            metadata.controller.set_tempo(new_tempo);
            // decreasing tempo, playback moves forward
            let _ = player.try_seek(player.get_pos().mul_f64(old_tempo / new_tempo));
        }

        // increase pitch
        KeyCode::Up
            if *selected == UiSelection::Pitch
                && metadata.controller.pitch() < PitchTranspose::MAX =>
        {
            metadata
                .controller
                .set_pitch(metadata.controller.pitch() + 1);
        }

        // decrease pitch
        KeyCode::Down
            if *selected == UiSelection::Pitch
                && metadata.controller.pitch() > PitchTranspose::MIN =>
        {
            metadata
                .controller
                .set_pitch(metadata.controller.pitch() - 1);
        }

        // increase volume
        KeyCode::Up if *selected == UiSelection::Volume && player.volume() < 2.0 => {
            player.set_volume(player.volume() + VOLUME_INC);
        }

        // decrease volume
        KeyCode::Down if *selected == UiSelection::Volume && player.volume() > 0.0 => {
            player.set_volume(player.volume() - VOLUME_INC);
        }

        // quit
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

                match event? {
                    Event::Key(e) => {
                        match handle_input(e, &player, &metadata, &mut selected) {
                            InputResult::Quit => break,
                            InputResult::Handled => true,
                            InputResult::Unhandled => false,
                        }
                    }
                    Event::Resize(_, _) => true,
                    _ => false
                }
            },
            _ = interval.tick() => {
                let pos = metadata.controller.duration_from_tempo(player.get_pos()).as_secs();
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
