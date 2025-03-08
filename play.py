import argparse
import curses
import logging
import os
import shutil
import tempfile
from pathlib import Path
from typing import Optional

os.environ["PYGAME_HIDE_SUPPORT_PROMPT"] = "1"

import pygame
import pygame.mixer_music as pgm
import sox
import yt_dlp

AUDIO_FORMAT = "flac"
SEEK_DISTANCE_SECONDS = 5
PROGRESS_BAR_WIDTH = 60


def main(
    stdscr: curses.window,
    file_or_url: str,
    speed: float = 1.0,
    start: float = 0.0,
    end: Optional[float] = None,
    loop: bool = False,
    save: Optional[str | Path] = None,
) -> None:
    curses.start_color()
    curses.use_default_colors()
    curses.curs_set(0)

    stdscr.nodelay(True)

    with tempfile.TemporaryDirectory() as work_dir:
        if file_or_url.startswith("http"):
            # download from youtube
            stdscr.clear()
            stdscr.addstr(0, 0, "Downloading song...")
            stdscr.refresh()

            source_file = Path(work_dir, f"audio.{AUDIO_FORMAT}")

            options = {
                "format": "bestaudio/best",
                "outtmpl": str(source_file.with_suffix("")),
                "postprocessors": [
                    {
                        "key": "FFmpegExtractAudio",
                        "preferredcodec": AUDIO_FORMAT,
                        "preferredquality": "0",
                    }
                ],
                "logger": logging.getLogger(),
            }

            with yt_dlp.YoutubeDL(options) as ydl:
                try:
                    ydl.download([file_or_url])
                except yt_dlp.utils.DownloadError as e:
                    raise SystemExit(
                        "ERROR: Failed to download audio (invalid URL?)"
                    ) from e

            # copy the downloaded file if the user wants to keep it
            if save is not None:
                try:
                    shutil.copy(source_file, Path(save).with_suffix(f".{AUDIO_FORMAT}"))
                except OSError:
                    # user probably passed a directory or some other stupid path, oh well, we tried
                    pass
        else:
            # play from file
            source_file = Path(file_or_url)
            if not source_file.exists():
                raise SystemExit("ERROR: File not found")

        stdscr.clear()
        stdscr.addstr(0, 0, "Loading...")
        stdscr.refresh()

        # generate the playback file by adjusting the tempo
        playback_file = Path(work_dir, f"playback.{AUDIO_FORMAT}")
        transformer = sox.Transformer()
        if start != 0.0 or end is not None:
            transformer.trim(start, end)
        if speed != 1.0:
            transformer.tempo(speed, audio_type="m")
        if len(transformer.effects) > 0:
            transformer.build(source_file, playback_file)
        else:
            playback_file = source_file
        audio_length = sox.file_info.duration(playback_file)

        # load the playback file
        pygame.mixer.init()
        pgm.load(playback_file)

        # pygame does not really support seeking. all we have to work with is the absolute time
        # since `play` was called, and the ability to call `play` with a start offset. therefore, we
        # need to do some clever state tracking to emulate seeking
        time_since_play = start_offset = 0
        paused = False
        pgm.play(start=start_offset)

        while True:
            try:
                if pgm.get_busy():
                    time_since_play = pgm.get_pos() / 1000

                # handle key presses
                key = stdscr.getch()
                if key != -1:
                    # quit
                    if key == pygame.K_q or key == pygame.K_ESCAPE:
                        break

                    # pause/unpause
                    elif key == pygame.K_SPACE:
                        if paused:
                            pgm.play(start=start_offset)
                        else:
                            start_offset = min(
                                start_offset + time_since_play, audio_length
                            )
                            time_since_play = 0
                            pgm.stop()
                        paused = not paused

                    # restart
                    elif key == pygame.K_r:
                        time_since_play = start_offset = 0
                        pgm.stop()
                        if not paused:
                            pgm.play(start=start_offset)

                    # seek backward
                    elif key == curses.KEY_LEFT:
                        start_offset = max(
                            0, start_offset + time_since_play - SEEK_DISTANCE_SECONDS
                        )
                        time_since_play = 0
                        pgm.stop()
                        if not paused:
                            pgm.play(start=start_offset)

                    # seek forward
                    elif key == curses.KEY_RIGHT:
                        start_offset = min(
                            start_offset + time_since_play + SEEK_DISTANCE_SECONDS,
                            audio_length,
                        )
                        time_since_play = 0
                        pgm.stop()
                        if not paused:
                            pgm.play(start=start_offset)

                # loop
                if loop and not paused and not pgm.get_busy():
                    time_since_play = start_offset = 0
                    pgm.play(start=start_offset)

                t = start_offset + time_since_play
                height, width = stdscr.getmaxyx()
                center_y = height // 2
                max_width = width - 2
                strings_to_draw = []

                # draw progress bar
                progress = min(t / audio_length, 1)
                max_bars = min(max_width - 2, PROGRESS_BAR_WIDTH)
                n_bars = round(progress * max_bars)
                progress_bar = "[" + "=" * n_bars + " " * (max_bars - n_bars) + "]"
                strings_to_draw.append(
                    (center_y, (width - len(progress_bar)) // 2, progress_bar)
                )

                # draw info
                if center_y > 0:
                    formatted_speed = f"{speed:.2f}x speed"

                    minutes, seconds = divmod(int(speed * t), 60)
                    hours, minutes = divmod(minutes, 60)
                    formatted_time = (
                        f"{hours}:{minutes:02d}:{seconds:02d}"
                        if hours
                        else f"{minutes}:{seconds:02d}"
                    )

                    info_string = f"{formatted_speed} - {formatted_time}"
                    if len(info_string) < max_width:
                        strings_to_draw.append(
                            (
                                center_y - 1,
                                (width - len(info_string)) // 2,
                                info_string,
                            )
                        )

                # draw help
                if center_y < height - 2:
                    help_string = (
                        "space: pause, r: restart, left/right: seek, q/esc: quit"
                    )
                    if len(help_string) < max_width:
                        strings_to_draw.append(
                            (center_y + 2, (width - len(help_string)) // 2, help_string)
                        )

                stdscr.clear()
                for string in strings_to_draw:
                    stdscr.addstr(*string)
                stdscr.refresh()

                curses.napms(10)

            except KeyboardInterrupt:
                # quit
                break

        pgm.unload()


# suppress third party library logging
logging.disable(logging.WARNING)

parser = argparse.ArgumentParser(
    usage=f"python {Path(__file__).name} <file_or_youtube_url> <options>",
    description="Play an audio file or audio from YouTube URL with a given speed multiplier.",
)
parser.add_argument("file_or_url", help="Path to audio file or YouTube URL")
parser.add_argument("--speed", type=float, default=1.0, help="Speed multiplier")
parser.add_argument(
    "--start", type=float, default=0, help="Track start time in seconds"
)
parser.add_argument("--end", type=float, help="Track end time in seconds")
parser.add_argument("--loop", action="store_true", help="Loop the track")
parser.add_argument(
    "--save",
    help="Save the downloaded audio file to the given path. Do not include an extension.",
)
args = parser.parse_args()

curses.wrapper(main, **vars(args))
