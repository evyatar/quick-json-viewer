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
- **Paste to view** — ⌘V pastes JSON straight from the clipboard; JWT tokens are decoded into header / payload / signature
- **Advanced search** — `key:name`, `value:err`, `age > 30`, operators `= != < <= > >=`, space-ANDed clauses, regex mode; all matches highlighted and navigable with ⌘G / ⌘⇧G
- **Compare two documents** — side-by-side semantic diff; additions, removals, and changes colour-coded; configurable ignore options; ▲/▼ or ⌘G / ⌘⇧G to jump between differences
- **Breadcrumbs bar** — shows the JSON path of the selected node; click any segment to jump to an ancestor
- **Keyboard-driven navigation** — arrow keys, Page Up/Down, Home/End
- **BiDi text** — correct display of Hebrew, Arabic, and other RTL content
- **Dark / light / auto themes**
- **Set as default JSON viewer** — register the app as the system-wide handler for `.json` files from Settings

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
| ⌘V | Paste JSON / JWT from clipboard |
| ⌘F | Focus search |
| ⌘, | Settings |
| ↑ / ↓ | Select previous / next row |
| ← / → | Collapse / expand node |
| ⌥C | Collapse all |
| ⌥X | Expand all |
| ⌘G | Next search result / next difference (Compare) |
| ⌘⇧G | Previous search result / previous difference (Compare) |
| Page Up/Down | Jump 20 rows |
| Home / End | First / last row |

Right-click any row to copy its JSON path, key, or value. Right-clicking a container also offers **Expand All** / **Collapse All** for that subtree. In Compare mode the context menu provides **Copy Left Value**, **Copy Right Value**, and **Copy Path**.

## Architecture

| File | Purpose |
|------|---------|
| `src/main.rs` | UI layout, keyboard handling, app state |
| `src/parser.rs` | Hand-written JSON / NDJSON parser |
| `src/index.rs` | Flat node array over the backing data (mmap or pasted buffer) |
| `src/tree.rs` | Tree expansion, selection, and search state |
| `src/search.rs` | Full-text, structured-query, and regex search over node keys/values |
| `src/diff.rs` | Semantic JSON diff engine; merged tree model for the Compare view |
| `src/loader.rs` | Background file loading via message-passing channel |
| `src/paste.rs` | Pasted-text handling and JWT decoding |
| `src/settings.rs` | Persistent user preferences; set-as-default JSON viewer |
| `src/macos_menu.rs` | Native macOS menu bar via Objective-C FFI |
