//! termpaint — a bitmap paint program for the terminal.
//!
//! * UI chrome (toolbar, layers, status, labels) is a ratatui text UI.
//! * The colour picker is a bitmap tiled under its text labels the same way.
//! * The canvas is a grid of Kitty graphics protocol tiles placed under the
//!   empty canvas cells with a z-index below non-default cell backgrounds,
//!   so ratatui popups naturally occlude it. Only dirty tiles are re-sent.
//! * Mouse input uses SGR-Pixels (DECSET 1016) for pixel-precise strokes.

mod app;
mod document;
mod history;
mod icon;
mod io;
mod kitty;
mod picker;
mod stroke;
mod tiles;
mod ui;
mod viewport;

use std::io::{Write, stdout};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyEventKind};
use ratatui::crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::crossterm::{cursor, execute};
use ratatui::layout::Rect;
use ratatui::Terminal;

use app::App;
use document::Document;
use kitty::Graphics;

const USAGE: &str = "\
termpaint — pixel-precise bitmap painting in Kitty-compatible terminals

USAGE:
    termpaint [FILE.png] [OPTIONS]

    FILE.png            Open this PNG (if it exists) and save to it (default: untitled.png).
                        Layers are kept in a private PNG chunk and restored on open.

OPTIONS:
    --size WxH          Canvas size for a new image (default: fits the window)
    --tile PX           Canvas tile size in pixels (default: 64)
    --no-compress       Disable zlib compression of image data
    --force             Run even if the terminal does not look Kitty-compatible
    -h, --help          Show this help
";

struct Options {
    path: PathBuf,
    size: Option<(u32, u32)>,
    tile_px: u32,
    compress: bool,
    force: bool,
}

fn parse_args() -> Result<Options> {
    let mut o = Options { path: PathBuf::from("untitled.png"), size: None, tile_px: 64, compress: true, force: false };
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "--size" => {
                let v = args.next().context("--size needs a value like 1024x768")?;
                let (w, h) = v.split_once(['x', 'X']).context("--size must look like WxH")?;
                let (w, h): (u32, u32) = (w.parse()?, h.parse()?);
                if !(1..=8192).contains(&w) || !(1..=8192).contains(&h) {
                    bail!("--size must be between 1 and 8192 in each dimension");
                }
                o.size = Some((w, h));
            }
            "--tile" => {
                let v: u32 = args.next().context("--tile needs a pixel size")?.parse()?;
                o.tile_px = v.clamp(16, 4096);
            }
            "--no-compress" => o.compress = false,
            "--force" => o.force = true,
            s if s.starts_with('-') => bail!("unknown option {s}\n\n{USAGE}"),
            s => o.path = PathBuf::from(s),
        }
    }
    Ok(o)
}

fn looks_like_kitty() -> bool {
    let term = std::env::var("TERM").unwrap_or_default();
    let prog = std::env::var("TERM_PROGRAM").unwrap_or_default().to_lowercase();
    term.contains("kitty")
        || term.contains("ghostty")
        || std::env::var_os("KITTY_WINDOW_ID").is_some()
        || prog == "ghostty"
        || prog == "wezterm"
}

fn cell_size() -> Result<(u32, u32)> {
    let ws = terminal::window_size().context("query window size")?;
    if ws.width == 0 || ws.height == 0 || ws.columns == 0 || ws.rows == 0 {
        bail!("the terminal does not report its pixel size (TIOCGWINSZ); a Kitty-compatible terminal is required");
    }
    Ok(((ws.width / ws.columns) as u32, (ws.height / ws.rows) as u32))
}

/// Restores the terminal on drop (including on panic unwinding).
struct TermGuard;

impl TermGuard {
    fn enter() -> Result<Self> {
        terminal::enable_raw_mode()?;
        let mut out = stdout();
        execute!(out, EnterAlternateScreen, cursor::Hide, EnableMouseCapture)?;
        // SGR-Pixels: report mouse positions in pixels instead of cells.
        out.write_all(b"\x1b[?1016h")?;
        out.flush()?;
        Ok(TermGuard)
    }
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        let mut out = stdout();
        let _ = out.write_all(b"\x1b[?1016l\x1b[?2026l");
        let _ = execute!(out, DisableMouseCapture, cursor::Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
    }
}

fn main() -> Result<()> {
    let opts = parse_args()?;
    if !opts.force && !looks_like_kitty() {
        bail!(
            "this terminal does not look Kitty-graphics capable (TERM={}).\n\
             Run inside kitty (or Ghostty/WezTerm) or pass --force to try anyway.",
            std::env::var("TERM").unwrap_or_default()
        );
    }
    let mut cell = cell_size()?;

    let (doc, load_note) = if opts.path.exists() {
        io::load_document(&opts.path)?
    } else {
        let (w, h) = opts.size.unwrap_or_else(|| {
            let (cols, rows) = terminal::size().unwrap_or((120, 40));
            let c = ui::areas(Rect::new(0, 0, cols, rows), cell).canvas;
            let w = (c.width as u32 * cell.0).saturating_sub(4 * cell.0).clamp(64, 4096);
            let h = (c.height as u32 * cell.1).saturating_sub(2 * cell.1).clamp(64, 4096);
            (w, h)
        });
        (Document::new(w, h), None)
    };

    let guard = TermGuard::enter()?;
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let mut out = stdout();
        let _ = out.write_all(b"\x1b[?1016l");
        let _ = execute!(out, DisableMouseCapture, cursor::Show, LeaveAlternateScreen);
        let _ = terminal::disable_raw_mode();
        prev_hook(info);
    }));

    let mut term = Terminal::new(CrosstermBackend::new(stdout()))?;
    term.clear()?;
    let mut app = App::new(doc, opts.path, cell, opts.tile_px);
    if let Some(note) = load_note {
        app.set_status(note);
    }
    let result = run(&mut term, &mut app, &mut cell, opts.compress);

    let mut g = Graphics::new(false);
    app.shutdown(&mut g);
    let mut out = stdout();
    let _ = g.flush_to(&mut out);
    let _ = out.flush();
    drop(guard);
    result
}

/// Minimum spacing between frames while input keeps streaming in. The
/// terminal repaints at most every few ms anyway, so uploading faster only
/// queues work there; when the queue is empty we render immediately.
const MIN_FRAME: Duration = Duration::from_millis(4);

fn run(term: &mut Terminal<CrosstermBackend<std::io::Stdout>>, app: &mut App, cell: &mut (u32, u32), compress: bool) -> Result<()> {
    let mut g = Graphics::new(compress);
    while !app.quit {
        // Batch UI text + image updates in one synchronized frame.
        let t0 = Instant::now();
        stdout().write_all(b"\x1b[?2026h")?;
        term.draw(|f| ui::draw(f, app))?;
        app.sync_graphics(&mut g);
        let bytes = g.len();
        let mut out = stdout().lock();
        g.flush_to(&mut out)?;
        out.write_all(b"\x1b[?2026l")?;
        out.flush()?;
        drop(out);
        let frame_done = Instant::now();
        if bytes > 0 {
            app.stats.ms = (frame_done - t0).as_secs_f32() * 1e3;
            app.stats.bytes = bytes;
        }

        // Wait for input, then drain everything pending so fast mouse
        // motion is coalesced into a single frame upload.
        if !event::poll(Duration::from_millis(500))? {
            continue;
        }
        loop {
            match event::read()? {
                Event::Key(k) if k.kind != KeyEventKind::Release => app.on_key(k),
                Event::Mouse(m) => app.on_mouse(m),
                Event::Resize(_, _) => {
                    *cell = cell_size().unwrap_or(*cell);
                    app.on_resize(*cell);
                    term.autoresize()?;
                }
                _ => {}
            }
            if app.quit || !event::poll(Duration::ZERO)? {
                break;
            }
            // Input is streaming faster than we render: keep coalescing
            // until the minimum frame interval has passed.
            if frame_done.elapsed() >= MIN_FRAME {
                break;
            }
        }
    }
    Ok(())
}
