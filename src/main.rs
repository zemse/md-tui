mod app;
mod config;
mod events;
mod links;
mod markdown;
mod palette;
mod syntax;
mod theme;
mod ui;

use anyhow::Result;
use clap::Parser;
use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "md",
    version,
    about = "Markdown reader TUI with mouse + clickable links"
)]
struct Cli {
    /// File or directory to view. If omitted and stdin is piped, reads stdin.
    /// If omitted with a TTY, browses the current directory.
    path: Option<PathBuf>,

    /// Word-wrap width (0 = use terminal width)
    #[arg(short = 'w', long, default_value_t = 0)]
    width: u16,

    /// Show line numbers
    #[arg(short = 'l', long)]
    line_numbers: bool,

    /// Theme: dark | light | auto
    #[arg(short = 's', long, default_value = "auto")]
    style: String,

    /// Pager mode (kept for glow CLI parity)
    #[arg(short = 'p', long)]
    pager: bool,

    /// TUI mode (kept for glow CLI parity)
    #[arg(short = 't', long)]
    tui: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = config::load();
    let theme = theme::resolve(&cli.style, &cfg);

    let source = match cli.path.as_deref() {
        Some(p) if p.is_dir() => app::Source::Directory(p.to_path_buf()),
        Some(p) => app::Source::File(p.to_path_buf()),
        None => {
            if io::stdin().is_terminal() {
                app::Source::Directory(std::env::current_dir()?)
            } else {
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf)?;
                app::Source::Stdin(buf)
            }
        }
    };

    let opts = app::Options {
        width: cli.width,
        line_numbers: cli.line_numbers,
        theme,
    };
    let _ = (cli.pager, cli.tui);

    let mut app = app::App::new(source, opts)?;
    let mut term = ui::setup_terminal()?;
    // Probe for graphics support only after entering the alt screen, so any
    // unrecognized response bytes don't pollute the user's shell on exit.
    app.init_image_picker();

    let panic_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = ui::restore_raw();
        panic_hook(info);
    }));

    let res = events::run(&mut term, &mut app);
    ui::restore_terminal(&mut term)?;
    res
}
