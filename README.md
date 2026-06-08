# Quick JSON Viewer

A fast, native macOS app for viewing and navigating large JSON and NDJSON files.

Built with Rust + [egui](https://github.com/emilk/egui). Uses memory-mapped file I/O so even multi-GB files open instantly.

## Installation

```sh
brew install --cask evyatar/tap/quick-json-viewer
```

Installs to `/Applications` — find it in Launchpad or open files with right-click → Open With.

## Features

- **JSON & NDJSON** — single-document JSON and newline-delimited JSON both supported
- **Large file support** — memory-mapped I/O; the file is never fully loaded into RAM
- **Full-text & regex search** — highlights all matches, navigate with ⌘G / ⌘⇧G
- **Keyboard-driven navigation** — arrow keys, Page Up/Down, Home/End
- **BiDi text** — correct display of Hebrew, Arabic, and other RTL content
- **Dark / light / auto themes**
- **Native macOS menu bar**

## Requirements

- macOS 12.0 or later (Apple Silicon or Intel via Rosetta)

## Building

```sh
# Development build
cargo run

# Production .app bundle (Apple Silicon)
./build-app.sh
open quick-json-viewer.app
```

## Testing

```sh
cargo test
```

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| ⌘O | Open file |
| ⌘F | Focus search |
| ⌘, | Settings |
| ↑ / ↓ | Select previous / next row |
| ← / → | Collapse / expand node |
| ⌥C | Collapse all |
| ⌥X | Expand all |
| ⌘G | Next search result |
| ⌘⇧G | Previous search result |
| Page Up/Down | Jump 20 rows |
| Home / End | First / last row |

Right-click any row to copy its JSON path, key, or value.

## Architecture

| File | Purpose |
|------|---------|
| `src/main.rs` | UI layout, keyboard handling, app state |
| `src/parser.rs` | Hand-written JSON / NDJSON parser |
| `src/index.rs` | Flat node array and memory-mapped file handle |
| `src/tree.rs` | Tree expansion, selection, and search state |
| `src/search.rs` | Full-text and regex search over node keys/values |
| `src/loader.rs` | Background file loading via message-passing channel |
| `src/settings.rs` | Persistent user preferences |
| `src/macos_menu.rs` | Native macOS menu bar via Objective-C FFI |
