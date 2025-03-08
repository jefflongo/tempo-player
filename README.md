# Tempo Player

A simple command-line audio player designed to help with instrument practice. It allows playback from audio files or YouTube links, and supports tempo adjustment, start/end cropping, and looping. Currently, only MP3 and FLAC formats are supported.

## Installation

```bash
# Install Python requirements
pip install -r requirements.txt
```

Linux:
```bash
sudo apt install -y ffmpeg sox libsox-fmt-all
```

Windows:
```
winget install ffmpeg sox
```
- Add `C:\Program Files (x86)\sox-x-y-z` to `PATH` (replace `x-y-z` with version)

## Usage

```bash
python play.py <file_or_youtube_url> <options>
```
