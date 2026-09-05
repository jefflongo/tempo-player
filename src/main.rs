mod audio;
mod cli_player;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail, ensure};
use clap::Parser;
use rodio::{DeviceSinkBuilder, Player, Source};
use tokio::sync::Notify;
use url::Url;
use which::which;
use yt_dlp::client::LibraryInstaller;
use yt_dlp::client::deps::Libraries;
use yt_dlp::model::Video;
use yt_dlp::{Downloader, VideoSelection};

use crate::audio::{NeverStop, TrackMetadata, convert_to_wav, get_audio_with_tempo};
use crate::cli_player::cli_player;

/// Play music at a desired tempo locally or from URL.
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    /// Path to file, URL, or YouTube search query
    query: String,

    /// Tempo multiplier
    #[arg(short, long, default_value_t = 1.0)]
    tempo: f64,

    /// Start time of track
    #[arg(short, long, value_parser = parse_time)]
    start: Option<f32>,

    /// End time of track
    #[arg(short, long, value_parser = parse_time)]
    end: Option<f32>,

    /// Loop the track
    #[arg(short, long = "loop")]
    loop_track: bool,

    /// Save the downloaded audio to the specified directory
    #[arg(long, value_parser = |s: &str| -> Result<PathBuf> {
        let path = PathBuf::from(s);
        path.is_dir().then_some(path).context("Path must be a directory")
    })]
    save: Option<PathBuf>,
}

/// Convert a timestamp in the format HH:MM:SS.XXX or MM:SS.XXX to fractional seconds.
fn parse_time(s: &str) -> Result<f32> {
    fn seconds(s: &str) -> Result<f32> {
        let seconds = s.parse::<f32>().context("Invalid seconds value")?;
        ensure!(seconds >= 0.0, "Seconds must be positive");
        Ok(seconds)
    }

    fn minutes(s: &str) -> Result<f32> {
        let minutes = s.parse::<u8>().context("Invalid minutes value")?;
        ensure!(minutes < 60, "Minutes must be less than 60");
        Ok(minutes as f32)
    }

    fn hours(s: &str) -> Result<f32> {
        let minutes = s.parse::<usize>().context("Invalid hours value")?;
        Ok(minutes as f32)
    }

    let s = s.trim();
    let parts: Vec<_> = s.split(':').collect();

    match parts.as_slice() {
        [ss] => seconds(ss),
        [mm, ss] => Ok(60.0 * minutes(mm)? + seconds(ss)?),
        [hh, mm, ss] => Ok(3600.0 * hours(hh)? + 60.0 * minutes(mm)? + seconds(ss)?),
        _ => bail!("Invalid timestamp"),
    }
}

enum VideoQuery {
    Url(Url),
    Search(String),
}

async fn download_audio(
    installer: &LibraryInstaller,
    ffmpeg: &Path,
    out_dir: &Path,
    query: VideoQuery,
) -> Result<PathBuf> {
    println!("Downloading from YouTube..");

    let yt_dlp = match which("yt-dlp") {
        Ok(path) => path,
        _ => installer.install_youtube(None).await?,
    };
    let executables = Libraries::new(yt_dlp, ffmpeg.to_path_buf());
    let downloader = Downloader::builder(executables, out_dir).build().await?;

    async fn fetch(
        downloader: &Downloader,
        query: &VideoQuery,
    ) -> Result<Video, yt_dlp::error::Error> {
        match query {
            VideoQuery::Url(url) => downloader.fetch_video_infos(url).await,
            VideoQuery::Search(search) => downloader.youtube_extractor().search_first(search).await,
        }
    }

    let video = match fetch(&downloader, &query).await {
        Ok(v) => v,
        Err(_) => {
            println!("Failed to download video, updating yt-dlp and trying again..");
            let _ = downloader.update_downloader().await;
            fetch(&downloader, &query).await?
        }
    };

    let file_name = format!(
        "{}.{}",
        video.title,
        video.best_audio_format().unwrap().codec_info.audio_ext
    );
    let file_path = downloader.download_audio_stream(&video, &file_name).await?;
    Ok(file_path)
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let executables_dir = dirs::cache_dir()
        .expect("Couldn't determine native cache directory")
        .join(env!("CARGO_PKG_NAME"));
    let temp_dir = tempfile::tempdir()?;

    println!("Checking dependencies..");
    let installer = LibraryInstaller::new(executables_dir);
    let ffmpeg = match which("ffmpeg") {
        Ok(path) => path,
        _ => installer.install_ffmpeg(None).await?,
    };

    let source = if Path::new(&cli.query).exists() {
        PathBuf::from(&cli.query)
    } else {
        let query = match Url::parse(&cli.query) {
            Ok(url) => VideoQuery::Url(url),
            _ => VideoQuery::Search(cli.query),
        };
        download_audio(&installer, &ffmpeg, temp_dir.path(), query).await?
    };

    println!("Processing..");
    let out_dir = cli.save.as_deref().unwrap_or(temp_dir.as_ref());
    let wav = convert_to_wav(&ffmpeg, &source, out_dir, cli.start, cli.end)?;
    let audio = get_audio_with_tempo(&wav, cli.tempo, &temp_dir.path())?;
    let audio_length = audio
        .total_duration()
        .expect("Couldn't retrieve track length");

    let mut sink = DeviceSinkBuilder::open_default_sink()?;
    sink.log_on_drop(false);
    let player = Player::connect_new(&sink.mixer());

    let track_ended = Arc::new(Notify::new());
    let track_ended_listener = track_ended.clone();
    let metadata = TrackMetadata {
        length: audio_length,
        title: source
            .file_stem()
            .map(|os| os.to_string_lossy().into_owned())
            .unwrap_or(String::new()),
        track_ended: track_ended_listener,
        loop_track: cli.loop_track,
    };

    player.append(NeverStop::new(audio, move || track_ended.notify_one()));

    cli_player(player, metadata).await
}
