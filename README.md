# Elastic Player

A simple command-line audio player designed to help with instrument practice. It supports playback from audio files or YouTube, along with tempo adjustment, pitch adjustment, start/end cropping, and looping.

## Install

This application will not be available on until [crates.io](https://crates.io/) until [yt-dlp video searching](https://github.com/boul2gom/yt-dlp/issues/314) is fixed. Until then, build and install locally with `cargo`:
```bash
cargo install --path .
```
Or download a binary from the published releases. If downloading a binary on Mac, `xz` is required: `brew install xz`.

## Usage

```bash
elastic-player <path, query, or URL> <options>
```
