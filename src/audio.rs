use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate};
use rodio::{Decoder, Sample, Source};
use tokio::sync::Notify;

pub struct TrackMetadata {
    pub length: Duration,
    pub title: String,
    pub track_ended: Arc<Notify>,
    pub loop_track: bool,
}

/// rodio `Source` that plays zeroed samples after the inner source ends such that it never gets
/// removed from the player queue.
pub struct NeverStop<S, F>
where
    S: Source,
    F: Fn() -> (),
{
    inner: S,
    on_end: F,
    ended: bool,
}

impl<S, F> NeverStop<S, F>
where
    S: Source,
    F: Fn() -> (),
{
    pub fn new(inner: S, on_end: F) -> Self {
        Self {
            inner,
            on_end,
            ended: false,
        }
    }
}

impl<S, F> Iterator for NeverStop<S, F>
where
    S: Source,
    F: Fn() -> (),
{
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        if self.ended {
            return Some(0.0);
        }
        self.inner.next().or_else(|| {
            self.ended = true;
            (self.on_end)();
            Some(0.0)
        })
    }
}

impl<S, F> Source for NeverStop<S, F>
where
    S: Source,
    F: Fn() -> (),
{
    fn current_span_len(&self) -> Option<usize> {
        if self.ended {
            None
        } else {
            self.inner.current_span_len()
        }
    }

    fn is_exhausted(&self) -> bool {
        false // hold instead of ending
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner.total_duration()
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        let duration = self.inner.total_duration();
        let clamped = duration.map_or(pos, |d| pos.min(d));

        self.inner.try_seek(clamped)?;

        self.ended = duration.is_some_and(|d| pos >= d);
        if self.ended {
            (self.on_end)();
        }
        Ok(())
    }
}

/// Use `ffmpeg` to convert `src` to WAV format in `out_dir` with optional start time `start` and
/// end time `end`.
pub fn convert_to_wav(
    ffmpeg: &Path,
    src: &Path,
    out_dir: &Path,
    start: Option<f32>,
    end: Option<f32>,
) -> Result<PathBuf> {
    ensure!(src.is_file(), "Invalid path to source audio");
    let stem = src.file_stem().unwrap();
    let wav = out_dir.join(stem).with_extension("wav");
    if src == wav && start.is_none() && end.is_none() {
        return Ok(src.to_path_buf());
    }

    let mut ffmpeg_cmd = Command::new(ffmpeg);
    ffmpeg_cmd.args(["-hide_banner", "-v", "error", "-y"]);
    if let Some(start) = start {
        ffmpeg_cmd.args(["-ss", &start.to_string()]);
    }
    if let Some(end) = end {
        ffmpeg_cmd.args(["-to", &end.to_string()]);
    }
    ffmpeg_cmd.arg("-i").arg(&src).arg(&wav);
    ffmpeg_cmd.status()?;
    Ok(wav)
}

/// Produce a rodio `Source` with tempo multiplier `tempo` from the WAV file `wav`.
pub fn get_audio_with_tempo(
    wav: &Path,
    tempo: f64,
    work_dir: &Path,
) -> Result<impl Source + use<>> {
    let mut wav_out = wav.to_path_buf();

    if tempo != 1.0 {
        wav_out = work_dir.join("stretched.wav");
        let output = Command::new("rubberband")
            .args(["-q", "-2", "--tempo"])
            .arg(tempo.to_string())
            .arg(&wav)
            .arg(&wav_out)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .context("Failed to spawn rubberband. Is it installed?")?;

        ensure!(
            output.status.success(),
            "rubberband exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let file = File::open(wav_out)?;
    let len = file.metadata()?.len();
    let decoder = Decoder::builder()
        .with_data(file)
        .with_byte_len(len)
        .build()?;

    Ok(decoder)
}
