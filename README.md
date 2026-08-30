# Tempo Player

A simple command-line audio player designed to help with instrument practice. It allows playback from audio files or YouTube links, and supports tempo adjustment, start/end cropping, and looping.

## Dependencies

Windows:
- Download [Rubber Band Library](https://breakfastquay.com/rubberband/)
- Extract and add the directory to the system PATH

Mac:
```bash
brew install rubberband
```

Linux:
```bash
sudo apt install rubberband-cli
```

## Install

```bash
cargo install --path .
```

## Usage

```bash
tempo-player <file_or_youtube_url> <options>
```
