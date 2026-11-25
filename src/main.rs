//! radu: a terminal disk usage analyzer with a file-type treemap.

mod app;
mod cells;
mod color;
mod fmt;
mod scan;
mod tree;
mod treemap;
mod ui;

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture};
use ratatui::crossterm::{execute, terminal};
use ratatui::DefaultTerminal;

use app::{App, Config};
use scan::{Options, Scanner};
use tree::Tree;

const HELP: &str = "\
radu - see what fills a disk, as a treemap by file type

Usage: radu [OPTIONS] [PATH]

  PATH                   Folder to scan (default: current folder)

Options:
  -x, --one-file-system  Do not cross into other mounted filesystems
  -a, --apparent-size    Count file lengths instead of space used on disk
  -j, --threads N        Scanner threads (default: up to 8)
      --dry-run          Deleting only removes entries from the view
      --no-motion        No animations
      --color MODE       auto, truecolor or 256 (default: auto)
  -h, --help             Show this help
  -V, --version          Show the version

Keys: arrows or hjkl move, Enter opens, Esc goes up, Tab switches pane,
Backspace deletes after a y/n prompt, r rescans, ? lists keys, q quits.";

struct Args {
    path: PathBuf,
    one_fs: bool,
    apparent: bool,
    threads: Option<usize>,
    dry_run: bool,
    no_motion: bool,
    color: String,
}

enum Parsed {
    Run(Args),
    Print(String),
}

fn parse(args: impl Iterator<Item = std::ffi::OsString>) -> Result<Parsed, String> {
    let mut a = Args {
        path: PathBuf::from("."),
        one_fs: false,
        apparent: false,
        threads: None,
        dry_run: false,
        no_motion: false,
        color: "auto".into(),
    };
    let mut have_path = false;
    let mut only_paths = false;
    let mut it = args;
    while let Some(arg) = it.next() {
        let text = arg.to_string_lossy().into_owned();
        let mut value = |name: &str| it.next().map(|v| v.to_string_lossy().into_owned()).ok_or(format!("{name} needs a value"));
        match text.as_str() {
            _ if only_paths || !text.starts_with('-') || text == "-" => {
                if have_path {
                    return Err("only one path can be scanned at a time".into());
                }
                a.path = PathBuf::from(arg);
                have_path = true;
            }
            "--" => only_paths = true,
            "-h" | "--help" => return Ok(Parsed::Print(HELP.into())),
            "-V" | "--version" => return Ok(Parsed::Print(format!("radu {}", env!("CARGO_PKG_VERSION")))),
            "-x" | "--one-file-system" => a.one_fs = true,
            "-a" | "--apparent-size" => a.apparent = true,
            "--dry-run" => a.dry_run = true,
            "--no-motion" => a.no_motion = true,
            "-j" | "--threads" => {
                let v = value("--threads")?;
                a.threads = Some(
                    v.parse::<usize>()
                        .ok()
                        .filter(|n| (1..=256).contains(n))
                        .ok_or(format!("--threads: {v:?} is not a number from 1 to 256"))?,
                );
            }
            "--color" => {
                a.color = value("--color")?;
                if !["auto", "truecolor", "256"].contains(&a.color.as_str()) {
                    return Err(format!("--color: {:?} is not auto, truecolor or 256", a.color));
                }
            }
            _ => return Err(format!("unknown option {text}")),
        }
    }
    Ok(Parsed::Run(a))
}

/// Height of a terminal cell over its width, when the terminal reports its
/// pixel size. Most fonts land near 2.
fn cell_aspect() -> f64 {
    match terminal::window_size() {
        Ok(s) if s.width > 0 && s.height > 0 && s.columns > 0 && s.rows > 0 => {
            let a = (s.height as f64 / s.rows as f64) / (s.width as f64 / s.columns as f64);
            if (1.4..=3.0).contains(&a) {
                a
            } else {
                2.0
            }
        }
        _ => 2.0,
    }
}

/// Frame spacing while something is moving: about 30 frames a second.
const FRAME_MS: f64 = 33.0;

fn run(terminal: &mut DefaultTerminal, mut app: App) -> io::Result<()> {
    let epoch = Instant::now();
    let clock = || epoch.elapsed().as_secs_f64() * 1000.0;
    let mut last_draw = f64::NEG_INFINITY;
    // `redraw`: the screen is stale. `urgent`: it is stale because of input,
    // so draw now instead of waiting for the next frame slot.
    let (mut redraw, mut urgent) = (true, true);
    let mut was_moving = false;
    loop {
        app.now = clock();
        let (changed, backlog) = app.pump();
        redraw |= changed;
        let moving = app.busy();
        // When motion ends, draw once more so the settled state is what
        // stays on screen, however the last animation frame was timed.
        redraw |= was_moving && !moving;
        if urgent || ((redraw || moving) && app.now - last_draw >= FRAME_MS) {
            terminal.draw(|f| ui::draw(f, &mut app))?;
            last_draw = app.now;
            (redraw, urgent) = (false, false);
        }
        if app.quit {
            return Ok(());
        }
        // Asked again after drawing: a resize is noticed while drawing and
        // starts a transition of its own.
        was_moving = app.busy();
        // With scan results waiting, go straight back to applying them.
        // Otherwise sleep until the next frame is due, or until input when
        // nothing is moving.
        let wait = if backlog {
            Duration::ZERO
        } else if redraw || was_moving {
            Duration::from_secs_f64((FRAME_MS - (clock() - last_draw)).clamp(1.0, FRAME_MS) / 1000.0)
        } else {
            Duration::from_millis(1000)
        };
        if event::poll(wait)? {
            loop {
                let ev = event::read()?;
                app.now = clock();
                if app.on_event(ev) {
                    (redraw, urgent) = (true, true);
                }
                if app.quit || !event::poll(Duration::ZERO)? {
                    break;
                }
            }
        }
    }
}

fn main() -> ExitCode {
    let args = match parse(std::env::args_os().skip(1)) {
        Ok(Parsed::Run(a)) => a,
        Ok(Parsed::Print(text)) => {
            println!("{text}");
            return ExitCode::SUCCESS;
        }
        Err(e) => {
            eprintln!("radu: {e}\nTry 'radu --help'.");
            return ExitCode::from(2);
        }
    };
    let root = match std::fs::canonicalize(&args.path) {
        Ok(p) if p.is_dir() => p,
        Ok(p) => {
            eprintln!("radu: {} is not a folder", p.display());
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("radu: {}: {e}", args.path.display());
            return ExitCode::from(2);
        }
    };
    if !io::stdout().is_terminal() || !io::stdin().is_terminal() {
        eprintln!("radu: needs an interactive terminal");
        return ExitCode::from(2);
    }
    let truecolor = match args.color.as_str() {
        "truecolor" => true,
        "256" => false,
        _ => std::env::var("COLORTERM").is_ok_and(|v| v.contains("truecolor") || v.contains("24bit")),
    };
    let threads = args.threads.unwrap_or_else(|| std::thread::available_parallelism().map_or(4, |n| n.get()).min(8));
    let own = std::fs::symlink_metadata(&root).map(|m| scan::entry_size(&m, args.apparent)).unwrap_or(0);
    let scanner = Scanner::new(Options { one_fs: args.one_fs, apparent: args.apparent, threads });
    let cfg = Config { reduced: args.no_motion, dry_run: args.dry_run, truecolor, aspect: cell_aspect() };
    let mut app = App::new(Tree::new(root, own), Some(scanner), cfg);

    let mut terminal = match ratatui::try_init() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("radu: could not set up the terminal: {e}");
            return ExitCode::FAILURE;
        }
    };
    let _ = execute!(io::stdout(), EnableMouseCapture);
    // Mouse reporting must also be switched off if we panic, or the shell
    // is left printing escape codes on every click.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = execute!(io::stdout(), DisableMouseCapture);
        hook(info);
    }));
    app.start_scan();
    let result = run(&mut terminal, app);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    ratatui::restore();
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("radu: {e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Parsed, String> {
        parse(list.iter().map(std::ffi::OsString::from))
    }

    #[test]
    fn parses_flags_and_path() {
        let Ok(Parsed::Run(a)) = args(&["-x", "--apparent-size", "-j", "3", "--dry-run", "--no-motion", "--color", "256", "/var"]) else {
            panic!()
        };
        assert!(a.one_fs && a.apparent && a.dry_run && a.no_motion);
        assert_eq!((a.threads, a.color.as_str(), a.path), (Some(3), "256", PathBuf::from("/var")));
        let Ok(Parsed::Run(a)) = args(&[]) else { panic!() };
        assert_eq!(a.path, PathBuf::from("."));
        let Ok(Parsed::Run(a)) = args(&["--", "-odd-name"]) else { panic!() };
        assert_eq!(a.path, PathBuf::from("-odd-name"));
    }

    #[test]
    fn rejects_bad_input() {
        for bad in [&["--nope"][..], &["-j"], &["-j", "0"], &["--color", "pink"], &["a", "b"]] {
            assert!(args(bad).is_err(), "{bad:?}");
        }
        assert!(matches!(args(&["--help"]), Ok(Parsed::Print(t)) if t.contains("Usage: radu")));
        assert!(matches!(args(&["-V"]), Ok(Parsed::Print(t)) if t.starts_with("radu ")));
    }
}
