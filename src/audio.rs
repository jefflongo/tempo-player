use std::collections::VecDeque;
use std::ffi::OsStr;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use anyhow::{Result, ensure};
use bounded_integer::bounded_integer;
use rodio::source::SeekError;
use rodio::{ChannelCount, SampleRate};
use rodio::{Sample, Source};
use rubberband::Stretcher;
use tokio::sync::Notify;

/// Simple structure which groups metadata unavailable from the [`rodio::Player`] API.
pub struct TrackMetadata {
    pub length: Duration,
    pub title: String,
    pub track_ended: Arc<Notify>,
    pub controller: Arc<ElasticController>,
    pub loop_track: bool,
}

impl TrackMetadata {
    /// Get the track's length at the current tempo.
    #[inline]
    pub fn length_with_tempo(&self) -> Duration {
        self.controller.duration_at_tempo(self.length)
    }
}

bounded_integer! {
    /// Transposition of pitch in positive or negative semitones.
    pub struct PitchTranspose(-12, 12);
}

impl Deref for PitchTranspose {
    type Target = i8;

    fn deref(&self) -> &Self::Target {
        self.as_ref()
    }
}

impl TryFrom<f64> for PitchTranspose {
    type Error = bounded_integer::TryFromError;

    fn try_from(pitch: f64) -> Result<PitchTranspose, Self::Error> {
        let semitones = (12.0 * pitch.log2()).round() as i8;
        semitones.try_into()
    }
}

impl From<PitchTranspose> for f64 {
    fn from(semitones: PitchTranspose) -> Self {
        (f64::from(semitones.get()) / 12.0).exp2()
    }
}

/// A handle for controlling the tempo of a [`Elastic`].
pub struct ElasticController {
    tempo_bits: AtomicU64,
    pitch_bits: AtomicU64,
}

impl ElasticController {
    /// Set the tempo of the [`Elastic`]. A tempo of 2.0 means the source will play at double the
    /// original speed. A tempo of 0.5 means the source will play at half the original speed.
    #[inline]
    pub fn set_tempo(&self, tempo: f64) {
        self.tempo_bits.store(tempo.to_bits(), Ordering::Relaxed);
    }

    /// Get the current tempo of the [`Elastic`]. A tempo of 2.0 means the source will play at
    /// double the original speed. A tempo of 0.5 means the source will play at half the original
    /// speed.
    #[inline]
    pub fn tempo(&self) -> f64 {
        f64::from_bits(self.tempo_bits.load(Ordering::Relaxed))
    }

    /// Set the pitch of the [`Elastic`] in positive or negative semitones.
    #[inline]
    pub fn set_pitch(&self, transpose: PitchTranspose) {
        self.pitch_bits
            .store(Into::<f64>::into(transpose).to_bits(), Ordering::Relaxed);
    }

    #[inline]
    pub fn pitch(&self) -> PitchTranspose {
        f64::from_bits(self.pitch_bits.load(Ordering::Relaxed))
            .try_into()
            .expect("Pitch did not produce valid PitchTranspose")
    }

    /// Given a [`Duration`] at tempo, compute the position at real-time.
    #[inline]
    pub fn duration_from_tempo(&self, pos: Duration) -> Duration {
        pos.mul_f64(self.tempo())
    }

    /// Given a [`Duration`] at real-time, compute the position at tempo.
    #[inline]
    pub fn duration_at_tempo(&self, pos: Duration) -> Duration {
        pos.div_f64(self.tempo())
    }

    /// Constructs a new [`ElasticController`] with initial tempo multiplier `tempo` and pitch
    /// transposition `pitch` in semitones.
    fn new(tempo: f64, transpose: PitchTranspose) -> Self {
        Self {
            tempo_bits: AtomicU64::new(tempo.to_bits()),
            pitch_bits: AtomicU64::new(Into::<f64>::into(transpose).to_bits()),
        }
    }
}

/// A [`Source`] which can have its tempo dynamically controlled.
pub struct Elastic<S: Source> {
    inner: S,
    stretcher: Stretcher,
    controller: Arc<ElasticController>,

    in_buffers: Vec<Vec<Sample>>,
    out_buffers: Vec<Vec<Sample>>,
    out_queue: VecDeque<Sample>,

    source_drained: bool,
    stretcher_drained: bool,

    pad_remaining: usize,
    discard_remaining: usize,
}

impl<S: Source> Elastic<S> {
    /// Constructs a new [`Elastic<S>`] with underlying [`Source`] `inner` and initial tempo
    /// `initial_tempo`. Returns both the new [`Elastic<S>`] and an [`Arc<ElasticController>`] for controlling the tempo after the source is consumed by a sink.
    pub fn new(
        inner: S,
        initial_tempo: f64,
        initial_pitch: PitchTranspose,
    ) -> (Self, Arc<ElasticController>) {
        use rubberband::Options;

        let controller = Arc::new(ElasticController::new(initial_tempo, initial_pitch));
        let user_controller = Arc::clone(&controller);

        let stretcher = Stretcher::new(
            inner.sample_rate().get(),
            inner.channels().get().into(),
            Options::PROCESS_REALTIME | Options::ENGINE_FINER,
            1.0 / controller.tempo(),
            initial_pitch.into(),
        );

        let start_pad = stretcher.preferred_start_pad().try_into().unwrap();
        let start_delay = stretcher.start_delay().try_into().unwrap();
        let num_buffers = stretcher.channel_count().try_into().unwrap();
        let buffer_size = stretcher.samples_required().try_into().unwrap();

        let source = Self {
            inner,
            stretcher,
            controller,

            in_buffers: (0..num_buffers)
                .map(|_| Vec::with_capacity(buffer_size))
                .collect(),
            out_buffers: (0..num_buffers)
                .map(|_| Vec::with_capacity(buffer_size))
                .collect(),
            out_queue: VecDeque::with_capacity(num_buffers * buffer_size),

            source_drained: false,
            stretcher_drained: false,

            pad_remaining: start_pad,
            discard_remaining: start_delay,
        };

        (source, user_controller)
    }

    /// Pull samples from [`Self::inner`], feed them to [`Self::stretcher`], and move any newly
    /// available output into [`Self::out_queue`]. Called from [`Self::next`] whenever
    /// [`Self::out_queue`] is exhausted.
    fn pump(&mut self) {
        self.stretcher.set_time_ratio(1.0 / self.controller.tempo());
        self.stretcher
            .set_pitch_scale(self.controller.pitch().into());

        if !self.source_drained {
            let samples_required = self.stretcher.samples_required().try_into().unwrap();

            // reset input buffers
            let pad = self.pad_remaining.min(samples_required);
            self.pad_remaining -= pad;
            for ch in self.in_buffers.iter_mut() {
                ch.clear();
                ch.resize(pad, 0.0);
            }

            // load input buffers (after any pad samples), deinterleaving samples from inner
            'outer: for _ in pad..samples_required {
                for ch in self.in_buffers.iter_mut() {
                    match self.inner.next() {
                        Some(s) => ch.push(s),
                        None => {
                            self.source_drained = true;
                            break 'outer;
                        }
                    }
                }
            }

            // submit input buffers for processing
            let samples_refs = self
                .in_buffers
                .iter()
                .map(|v| v.as_slice())
                .collect::<Vec<_>>();
            self.stretcher.process(&samples_refs, self.source_drained);
        }

        match self.stretcher.available().map(|n| n.try_into().unwrap()) {
            // busy processing
            Some(0) => {}

            // output samples are ready
            Some(n) if n > 0 => {
                // reset output buffers
                for ch in self.out_buffers.iter_mut() {
                    ch.clear();
                    ch.resize(n, 0.0);
                }

                // retrieve output samples
                let mut samples_refs = self
                    .out_buffers
                    .iter_mut()
                    .map(|v| v.as_mut_slice())
                    .collect::<Vec<_>>();
                let n = self
                    .stretcher
                    .retrieve(&mut samples_refs)
                    .try_into()
                    .unwrap();

                // load output queue (after any discard samples)
                let discard = self.discard_remaining.min(n);
                self.discard_remaining -= discard;
                for i in discard..n {
                    for ch in self.out_buffers.iter() {
                        self.out_queue.push_back(ch[i]);
                    }
                }
            }

            // stretcher is finished
            _ => self.stretcher_drained = true,
        }
    }
}

impl<S: Source> Iterator for Elastic<S> {
    type Item = Sample;

    fn next(&mut self) -> Option<Sample> {
        loop {
            if let Some(s) = self.out_queue.pop_front() {
                return Some(s);
            }
            if self.stretcher_drained {
                return None;
            }
            self.pump();
        }
    }
}

impl<S: Source> Source for Elastic<S> {
    fn current_span_len(&self) -> Option<usize> {
        // channel count / sample rate are fixed for the lifetime of this source
        None
    }

    fn is_exhausted(&self) -> bool {
        self.stretcher_drained && self.out_queue.is_empty()
    }

    fn channels(&self) -> ChannelCount {
        self.inner.channels()
    }

    fn sample_rate(&self) -> SampleRate {
        self.inner.sample_rate()
    }

    fn total_duration(&self) -> Option<Duration> {
        self.inner
            .total_duration()
            .map(|d| self.controller.duration_at_tempo(d))
    }

    fn try_seek(&mut self, pos: Duration) -> Result<(), SeekError> {
        self.inner
            .try_seek(self.controller.duration_from_tempo(pos))?;

        // seeking is a discontinuity - reset stretcher state
        self.stretcher.reset();
        self.out_queue.clear();
        self.source_drained = false;
        self.stretcher_drained = false;
        self.pad_remaining = self.stretcher.preferred_start_pad().try_into().unwrap();
        self.discard_remaining = self.stretcher.start_delay().try_into().unwrap();

        Ok(())
    }
}

/// [`Source`] that plays zeroed samples after the inner source ends such that it never gets removed
/// from the queue of a [`rodio::Player`].
pub struct NeverStop<S, F>
where
    S: Source,
    F: Fn(),
{
    inner: S,
    on_end: F,
    ended: bool,
}

impl<S, F> NeverStop<S, F>
where
    S: Source,
    F: Fn(),
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
    F: Fn(),
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
    F: Fn(),
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
        let duration = self.total_duration();
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
    start: Option<f64>,
    end: Option<f64>,
) -> Result<PathBuf> {
    ensure!(src.is_file(), "Invalid path to source audio");
    let stem = src.file_stem().unwrap_or_else(|| OsStr::new(""));
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
    ffmpeg_cmd.arg("-i").arg(src).arg(&wav);
    let status = ffmpeg_cmd.status()?;
    ensure!(status.success(), "ffmpeg exited with status {status}");
    Ok(wav)
}
