# md

A terminal markdown reader. Built with Rust + [ratatui](https://ratatui.rs).

## Install

```sh
cargo install md-tui-rs
```

Or from a local checkout:

```sh
cargo install --path .
```

The binary is named `md`.

## Use

```sh
md                # browse the current directory
md README.md      # open a file
md docs/          # browse a directory
cat NOTES.md | md # read from stdin
```

## Features

- **Mouse-aware** — wheel to scroll, click links to follow, hover for highlight.
- **Clickable task lists** — click `- [ ]` / `- [x]` to toggle; the file is rewritten in place.
- **Native text selection** — press `m` to drop mouse capture and drag-select with the terminal.
- **Proportional scrollbar** — thumb size reflects how much of the document is visible.
- **Fuzzy file search** — `/` opens an overlay that walks the launch directory.
- **Directory browser** — only directories and `.md` files; `..` to go up.
- **History** — `h`/`l` (or `b`/`f`) walk back and forward; cursor and scroll position are remembered.
- **Themes** — Catppuccin Mocha / Latte; auto-selects from `COLORFGBG`, override with `-s dark|light`.
- **GitHub-flavored** — tables, strikethrough, footnotes, task lists, code-block syntax highlighting (syntect), heading anchors.

## Keys

| Key            | Action                               |
| -------------- | ------------------------------------ |
| `j` / `k`      | Scroll down / up                     |
| `d` / `u`      | Half page down / up                  |
| `g` / `G`      | Top / bottom                         |
| `Tab` / `S-Tab`| Next / previous link                 |
| `Enter`        | Open selected entry / focused link   |
| `o`            | Open focused link in system browser  |
| `/`            | Fuzzy search                         |
| `h` / `l`      | Back / forward in history            |
| `m`            | Toggle mouse capture (drag-to-select)|
| `?`            | Help                                 |
| `q` / `Esc`    | Quit / go back                       |

## Config

Optional `md/config.toml` under your platform config dir
(`~/.config/md/config.toml` on Linux, `~/Library/Application Support/md/config.toml`
on macOS):

```toml
theme = "dark"        # "dark" | "light" | "auto"
width = 0             # word-wrap width; 0 = terminal width
line_numbers = false
```

CLI flags override the config: `-s dark|light|auto`, `-w 100`, `-l`.

## License

[MIT](LICENSE)
