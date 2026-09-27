//! Text UI (toolbar, colour picker labels, layer list, status bar, popups)
//! built with ratatui. The canvas and the colour picker's interior are left
//! as default-background cells; the Kitty images placed underneath show
//! through there.

use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph, Wrap};

use crate::app::{ALPHA_PRESETS, Action, App, Popup, Tool};
use crate::picker;

const BG: Color = Color::Rgb(24, 25, 31);
pub const PANEL: Color = Color::Rgb(32, 34, 42);
const BTN: Color = Color::Rgb(52, 55, 68);
pub const ACCENT: Color = Color::Rgb(94, 129, 244);
const FG: Color = Color::Rgb(222, 224, 232);
const DIM: Color = Color::Rgb(135, 140, 158);
const ROW_SEL: Color = Color::Rgb(56, 64, 96);
const POPUP_BG: Color = Color::Rgb(40, 42, 54);
const STRIP: Color = Color::Rgb(38, 40, 51);
const SEP: Color = Color::Rgb(72, 76, 94);
const HOVER: Color = Color::Rgb(74, 80, 104);

const COPYRIGHT: &str = "(c) Shuichi Kurabayashi";

const RIGHT_W: u16 = 36;
/// The layers panel keeps at least this many rows (3 list rows); the
/// colour panel above it gets the rest, up to what the picker wants.
const LAYERS_MIN_H: u16 = 10;
const TOOLBAR_H: u16 = 2;
/// Height of the About icon in text rows, so it scales with the font.
const ICON_ROWS: u16 = 6;

pub struct Areas {
    pub toolbar: Rect,
    pub canvas_block: Rect,
    pub canvas: Rect,
    pub colors: Rect,
    pub layers: Rect,
    pub status: Rect,
}

/// Screen layout; `cell` (pixels) decides how tall the colour picker is.
pub fn areas(full: Rect, cell: (u32, u32)) -> Areas {
    let [toolbar, main, status] =
        Layout::vertical([Constraint::Length(TOOLBAR_H), Constraint::Min(3), Constraint::Length(1)]).areas(full);
    let [canvas_block, right] = Layout::horizontal([Constraint::Min(4), Constraint::Length(RIGHT_W)]).areas(main);
    let want = picker::rows_for(RIGHT_W - 2, cell) + 2;
    let colors_h = want.min(right.height.saturating_sub(LAYERS_MIN_H));
    let [colors, layers] = Layout::vertical([Constraint::Length(colors_h), Constraint::Min(0)]).areas(right);
    let canvas = Block::bordered().inner(canvas_block);
    Areas { toolbar, canvas_block, canvas, colors, layers, status }
}

/// Inline element of the property strip.
enum Item {
    Link(&'static str, Action, bool),
    Text(String),
}

impl Item {
    fn width(&self) -> u16 {
        match self {
            Item::Link(s, ..) => s.chars().count() as u16,
            Item::Text(s) => s.chars().count() as u16,
        }
    }
}

/// Cursor that lays out inline widgets left to right and records hit boxes.
struct Row<'a> {
    buf: &'a mut Buffer,
    hits: &'a mut Vec<(Rect, Action)>,
    x: u16,
    y: u16,
    end: u16,
}

impl<'a> Row<'a> {
    fn new(buf: &'a mut Buffer, hits: &'a mut Vec<(Rect, Action)>, area: Rect, y: u16) -> Self {
        Self { buf, hits, x: area.x, y, end: area.x + area.width }
    }
    fn text(&mut self, s: &str, style: Style) -> Rect {
        let w = (s.chars().count() as u16).min(self.end.saturating_sub(self.x));
        let r = Rect::new(self.x, self.y, w, 1);
        if w > 0 {
            self.buf.set_stringn(self.x, self.y, s, w as usize, style);
        }
        self.x += w;
        r
    }
    fn button(&mut self, s: &str, active: bool, action: Action) {
        let st = if active {
            Style::new().bg(ACCENT).fg(Color::White).add_modifier(Modifier::BOLD)
        } else {
            Style::new().bg(BTN).fg(FG)
        };
        let r = self.text(s, st);
        if r.width > 0 {
            self.hits.push((r, action));
        }
    }
    /// Clickable text without a box (style is patched onto the row bg).
    fn link(&mut self, s: &str, style: Style, action: Action) {
        let r = self.text(s, style);
        if r.width > 0 {
            self.hits.push((r, action));
        }
    }
    fn gap(&mut self, n: u16) {
        self.x = (self.x + n).min(self.end);
    }
    fn remaining(&self) -> u16 {
        self.end.saturating_sub(self.x)
    }
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let a = areas(f.area(), app.cell);
    app.canvas_cells = a.canvas;
    app.about_icon = None;
    app.hits.clear();
    let buf = f.buffer_mut();

    draw_toolbar(buf, app, a.toolbar);
    draw_colors(buf, app, a.colors);
    draw_layers(buf, app, a.layers);
    draw_canvas_frame(buf, app, a.canvas_block);
    draw_status(buf, app, a.status);
    if app.popup.is_some() {
        draw_popup(f, app);
    }
    draw_hover(f.buffer_mut(), app);
}

/// Highlight the clickable element under the mouse pointer.
fn draw_hover(buf: &mut Buffer, app: &App) {
    let Some(pos) = app.hover else { return };
    let Some((r, a)) = app.hits.iter().rev().find(|(r, _)| r.contains(pos)) else { return };
    let skip = match a {
        // The picker highlights its own buttons in its bitmap.
        Action::LayerOpacity | Action::LayerSelect(_) | Action::Picker => true,
        Action::Tool(t) => *t == app.tool,
        _ => false,
    };
    if !skip {
        buf.set_style(*r, Style::new().bg(HOVER));
    }
}

fn draw_toolbar(buf: &mut Buffer, app: &mut App, area: Rect) {
    buf.set_style(area, Style::new().bg(BG).fg(FG));
    let mut hits = std::mem::take(&mut app.hits);
    let tool = app.tool;
    let mut row = Row::new(buf, &mut hits, area, area.y);
    row.link(" ▞ termpaint ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD), Action::About);
    for t in Tool::ALL {
        let label = format!(" {} {} ", t.key().to_ascii_uppercase(), t.label());
        row.button(&label, t == tool, Action::Tool(t));
        row.gap(1);
    }

    // Second row: a property strip with its own background and box-less
    // inline controls. Boxed buttons here would sit flush against the tool
    // buttons above (no vertical gap between cell rows) and read as ragged
    // protrusions, so boxes are reserved for the tool row.
    let y = area.y + 1;
    buf.set_style(Rect::new(area.x, y, area.width, 1), Style::new().bg(STRIP).fg(FG));
    let mut row = Row::new(buf, &mut hits, area, y);
    let sep = Style::new().fg(SEP);
    row.gap(1);
    let params = app.tool.params();
    if params.is_empty() {
        let hint = match app.tool {
            Tool::Picker => "Left: pick primary · Right: pick secondary",
            _ => "Drag to pan · Wheel to zoom · Middle-drag pans with any tool",
        };
        row.text(hint, Style::new().fg(DIM));
    }
    for (k, &p) in params.iter().enumerate() {
        if k > 0 {
            row.text(" │ ", sep);
        }
        row.text(&format!("{} ", p.label()), Style::new().fg(DIM));
        row.link("◂", Style::new().fg(ACCENT), Action::Param(p, -1));
        let val = format!(" {:^5} ", app.param_value(p));
        row.link(&val, Style::new().fg(Color::White).add_modifier(Modifier::BOLD), Action::Param(p, 1));
        row.link("▸", Style::new().fg(ACCENT), Action::Param(p, 1));
    }

    // Command groups, right-aligned; lower-priority groups are dropped when
    // the terminal is too narrow (everything also has a keyboard shortcut).
    let zoom = format!("{:>4}", format!("{}%", (app.view.zoom * 100.0).round()));
    let groups: [(u8, Vec<Item>); 4] = [
        (0, vec![Item::Link(" Undo ", Action::Undo, app.can_undo()), Item::Link(" Redo ", Action::Redo, app.can_redo())]),
        (3, vec![Item::Link(" Clear ", Action::ClearLayer, true)]),
        (
            1,
            vec![
                Item::Link(" - ", Action::ZoomOut, true),
                Item::Text(zoom),
                Item::Link(" + ", Action::ZoomIn, true),
                Item::Link(" Fit ", Action::ZoomFit, true),
            ],
        ),
        (2, vec![Item::Link(" Save ", Action::Save, true), Item::Link(" ? ", Action::Help, true), Item::Link(" Quit ", Action::Quit, true)]),
    ];
    // Links carry their own padding, so a bare "│" keeps single spacing.
    let width = |g: &[Item]| g.iter().map(Item::width).sum::<u16>() + 1;
    let avail = row.remaining().saturating_sub(2);
    let mut keep = [false; 4];
    let mut used = 0;
    let mut order: Vec<usize> = (0..groups.len()).collect();
    order.sort_by_key(|&i| groups[i].0);
    for i in order {
        let w = width(&groups[i].1);
        if used + w <= avail {
            keep[i] = true;
            used += w;
        }
    }
    row.x = row.end - used - 1;
    for (_, items) in groups.iter().zip(keep).filter(|(_, k)| *k).map(|(g, _)| g) {
        row.text("│", sep);
        for it in items {
            match it {
                Item::Link(label, action, true) => row.link(label, Style::new().fg(FG), *action),
                Item::Link(label, _, false) => {
                    row.text(label, Style::new().fg(DIM));
                }
                Item::Text(t) => {
                    row.text(t, Style::new().fg(FG));
                }
            }
        }
    }
    app.hits = hits;
}

fn panel_block(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .title(Span::styled(format!(" {title} "), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
        .border_style(Style::new().fg(DIM))
        .style(Style::new().bg(PANEL).fg(FG))
}

/// The colour picker: a Kitty bitmap under default-background cells (see
/// `picker`), with the numbers drawn here as text on top of it.
fn draw_colors(buf: &mut Buffer, app: &mut App, area: Rect) {
    use ratatui::widgets::Widget;
    let block = panel_block("Colors");
    let inner = block.inner(area);
    block.render(area, buf);
    app.picker = picker::Layout::new(inner, app.cell);
    let Some(l) = app.picker else {
        // Too small for the picker: a hidden hex field must not keep the keyboard.
        app.hex_edit = None;
        return;
    };
    buf.set_style(inner, Style::reset());
    app.hits.push((inner, Action::Picker));

    let label = Style::new().fg(FG);
    let (h, s, lum) = app.hsl_readout();
    for (r, t) in l.hsl.iter().zip([format!("H: {h}"), format!("S: {s}"), format!("L: {lum}")]) {
        buf.set_stringn(r.x, r.y, t, r.width as usize, label);
    }
    buf.set_string(l.hex_label.x, l.hex_label.y, "#:", label);
    let (x, y, n) = (l.hex.x + 1, l.hex.y, l.hex.width as usize - 1);
    let value = Style::new().fg(Color::White);
    match &app.hex_edit {
        // Nothing typed yet: the current value is a placeholder.
        Some(t) if t.is_empty() => {
            buf.set_string(x, y, "▏", value);
            buf.set_stringn(x + 1, y, &app.primary.hex()[1..7], n - 1, Style::new().fg(DIM));
        }
        Some(t) => {
            buf.set_stringn(x, y, format!("{t}▏"), n, value);
        }
        None => {
            buf.set_stringn(x, y, &app.primary.hex()[1..7], n, value);
        }
    }
    buf.set_string(l.alpha_label.x, l.alpha_label.y, "Opacity", label);
    buf.set_string(l.alpha.x + 1, l.alpha.y, format!("{:>3} %", app.alpha_percent()), value);
}

/// Layer opacity bar: filled up to the value, with a marker.
fn draw_opacity_bar(buf: &mut Buffer, value: f32, bar: Rect) {
    let n = bar.width.max(1);
    let marker = ((value * n as f32) as u16).min(n - 1);
    for i in 0..n {
        let t = (i as f32 + 0.5) / n as f32;
        let color = if t <= value { ACCENT } else { BTN };
        let sym = if i == marker { "┃" } else { " " };
        buf.set_string(bar.x + i, bar.y, sym, Style::new().bg(color).fg(Color::White));
    }
}

fn draw_layers(buf: &mut Buffer, app: &mut App, area: Rect) {
    use ratatui::widgets::Widget;
    let block = panel_block("Layers");
    let inner = block.inner(area);
    block.render(area, buf);
    if inner.height < 6 {
        return;
    }
    let controls_h = 5;
    let list_h = inner.height - controls_h;
    let n = app.doc.layers.len();
    // Top-most layer first; scroll so the active layer stays visible.
    let active_row = n - 1 - app.doc.active;
    let offset = active_row.saturating_sub(list_h as usize - 1);
    for (row_i, idx) in (0..n).rev().enumerate().skip(offset).take(list_h as usize) {
        let y = inner.y + (row_i - offset) as u16;
        let l = &app.doc.layers[idx];
        let sel = idx == app.doc.active;
        let bg = if sel { ROW_SEL } else { PANEL };
        let row_rect = Rect::new(inner.x, y, inner.width, 1);
        buf.set_style(row_rect, Style::new().bg(bg));
        let eye = if l.visible { " ● " } else { " ○ " };
        buf.set_string(inner.x, y, eye, Style::new().bg(bg).fg(if l.visible { ACCENT } else { DIM }));
        let name_w = inner.width.saturating_sub(3 + 5) as usize;
        let name: String = l.name.chars().take(name_w).collect();
        let st = if sel { Style::new().bg(bg).fg(Color::White).add_modifier(Modifier::BOLD) } else { Style::new().bg(bg).fg(FG) };
        buf.set_string(inner.x + 3, y, &name, st);
        buf.set_string(inner.x + inner.width - 5, y, format!("{:>4}%", l.opacity), Style::new().bg(bg).fg(DIM));
        app.hits.push((row_rect, Action::LayerSelect(idx)));
        app.hits.push((Rect::new(inner.x, y, 3, 1), Action::LayerToggle(idx)));
    }

    let y0 = inner.y + list_h;
    buf.set_string(inner.x, y0, "─".repeat(inner.width as usize), Style::new().fg(DIM));
    buf.set_string(inner.x + 1, y0 + 1, "Opacity", Style::new().fg(DIM));
    let bar = Rect::new(inner.x + 9, y0 + 1, inner.width.saturating_sub(15), 1);
    draw_opacity_bar(buf, app.layer_opacity(), bar);
    app.hits.push((bar, Action::LayerOpacity));
    let op = app.doc.layers[app.doc.active].opacity;
    buf.set_string(bar.x + bar.width, bar.y, format!("{op:>5}%"), Style::new().fg(FG));

    let mut row = Row::new(buf, &mut app.hits, inner, y0 + 3);
    row.gap(1);
    for (label, a) in [(" + New ", Action::LayerAdd), (" Dup ", Action::LayerDup), (" Del ", Action::LayerDelete)] {
        row.button(label, false, a);
        row.gap(1);
    }
    let mut row = Row::new(buf, &mut app.hits, inner, y0 + 4);
    row.gap(1);
    for (label, a) in [(" ▲ ", Action::LayerUp), (" ▼ ", Action::LayerDown), (" Merge ", Action::LayerMerge), (" Name ", Action::LayerRename)] {
        row.button(label, false, a);
        row.gap(1);
    }
}

fn draw_canvas_frame(buf: &mut Buffer, app: &App, area: Rect) {
    use ratatui::widgets::Widget;
    let title = format!(
        " {}{} · {}×{} ",
        app.path.file_name().map(|s| s.to_string_lossy()).unwrap_or_default(),
        if app.modified { " ●" } else { "" },
        app.doc.width,
        app.doc.height
    );
    // No background style: the inner cells must keep the terminal's default
    // background so the Kitty image (z below non-default bg) is visible.
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(DIM))
        .title(Span::styled(title, Style::new().fg(FG)))
        .render(area, buf);
}

fn draw_status(buf: &mut Buffer, app: &App, area: Rect) {
    buf.set_style(area, Style::new().bg(ACCENT).fg(Color::White));
    let pos = match app.mouse_doc {
        Some((x, y)) => format!("{x:>4},{y:<4}"),
        None => "    –    ".into(),
    };
    let perf = if app.perf {
        let (tx, ty) = app.tile_grid();
        format!(
            "{:.1}ms {}KB {}kpx · tiles {tx}×{ty} │ ",
            app.stats.ms,
            app.stats.bytes.div_ceil(1024),
            app.stats.px.div_ceil(1000)
        )
    } else {
        String::new()
    };
    let right = format!(
        " {perf}{pos} │ {} │ {}% │ Layer {}/{} ",
        app.tool.label(),
        (app.view.zoom * 100.0).round(),
        app.doc.active + 1,
        app.doc.layers.len(),
    );
    let rw = right.chars().count() as u16;
    let msg = app.status();
    buf.set_stringn(area.x + 1, area.y, msg, area.width.saturating_sub(rw + 2) as usize, Style::new());
    if area.width > rw {
        buf.set_string(area.x + area.width - rw, area.y, right, Style::new().add_modifier(Modifier::BOLD));
    }
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

fn draw_popup(f: &mut Frame, app: &mut App) {
    // Only popup controls are clickable while it is open.
    app.hits.clear();
    let full = f.area();
    let st = Style::new().bg(POPUP_BG).fg(FG);
    let block = |t: &'static str| {
        Block::bordered()
            .border_type(BorderType::Double)
            .title(Span::styled(format!(" {t} "), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
            .border_style(Style::new().fg(ACCENT))
            .style(st)
    };
    match &app.popup {
        Some(Popup::Help) => {
            let rows: &[(&str, &str)] = &[
                ("P B E", "Pencil / Brush / Eraser"),
                ("L R O", "Line / Rectangle / Ellipse"),
                ("F I H", "Fill / Color picker / Hand (pan)"),
                ("[ ]", "Brush size −/+ (Ctrl+wheel on canvas)"),
                ("{ }", "Hardness −/+"),
                ("< >", "Opacity −/+"),
                ("s S", "Stabilizer +/−"),
                ("Left/Right btn", "Paint with primary / secondary"),
                ("Wheel / + − 0 1", "Zoom at pointer / in / out / fit / 100%"),
                ("Middle drag, ←↑↓→", "Pan"),
                ("x", "Swap colors"),
                ("Ctrl+Z / Ctrl+Y", "Undo / Redo (also u / U)"),
                ("n N D M v", "New / rename / delete / merge / hide layer"),
                ("j k  J K", "Select layer below/above · move layer"),
                ("Delete", "Clear current layer"),
                ("Ctrl+S", "Save PNG"),
                ("F3", "Show frame time / upload size"),
                ("q / Ctrl+C", "Quit"),
            ];
            let lines: Vec<Line> = rows
                .iter()
                .map(|(k, d)| {
                    Line::from(vec![
                        Span::styled(format!("  {k:>18}  "), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)),
                        Span::raw(*d),
                    ])
                })
                .chain([
                    Line::raw(""),
                    Line::styled(
                        "  Canvas: tiled Kitty images (z below text bg) · Mouse: SGR-Pixels",
                        Style::new().fg(DIM),
                    ),
                    Line::styled("  Press any key or click to close", Style::new().fg(DIM)),
                ])
                .collect();
            let r = centered(full, 72, lines.len() as u16 + 2);
            f.render_widget(Clear, r);
            f.render_widget(Paragraph::new(lines).block(block("Help")), r);
        }
        Some(Popup::About) => {
            // The icon is a Kitty image placed over blank cells reserved here
            // (see App::sync_icon); it is left out if the screen is too small.
            let (cw, ch) = app.cell;
            let rows = ICON_ROWS.min(full.height.saturating_sub(10));
            let cols = (rows as u32 * ch).div_ceil(cw) as u16;
            let icon = rows >= 3 && cols + 4 <= full.width;
            let icon_h = if icon { rows + 1 } else { 0 };
            let lines = vec![
                Line::raw(""),
                Line::styled(
                    if icon { "termpaint" } else { "▞ termpaint" },
                    Style::new().fg(ACCENT).add_modifier(Modifier::BOLD),
                ),
                Line::raw(format!("Version {}", env!("CARGO_PKG_VERSION"))),
                Line::styled(COPYRIGHT, Style::new().fg(DIM)),
            ];
            let r = centered(full, if icon { 36.max(cols + 4) } else { 36 }, 9 + icon_h);
            f.render_widget(Clear, r);
            let b = block("About");
            let inner = b.inner(r);
            f.render_widget(b, r);
            if icon {
                app.about_icon = Some(Rect::new(inner.x + (inner.width - cols) / 2, inner.y + 1, cols, rows));
            }
            let text = Rect { y: inner.y + icon_h, height: inner.height - icon_h, ..inner };
            f.render_widget(Paragraph::new(lines).alignment(ratatui::layout::Alignment::Center).style(st), text);
            let buf = f.buffer_mut();
            let mut row = Row::new(buf, &mut app.hits, inner, inner.y + inner.height.saturating_sub(2));
            row.x = inner.x + inner.width.saturating_sub(8) / 2;
            row.button("   OK   ", true, Action::PopupClose);
        }
        Some(Popup::Rename(name)) => {
            let r = centered(full, 44, 7);
            f.render_widget(Clear, r);
            let b = block("Rename layer");
            let inner = b.inner(r);
            f.render_widget(b, r);
            let buf = f.buffer_mut();
            let field = Rect::new(inner.x + 1, inner.y + 1, inner.width - 2, 1);
            buf.set_style(field, Style::new().bg(BG));
            buf.set_stringn(field.x + 1, field.y, format!("{name}▏"), field.width as usize - 1, Style::new().bg(BG).fg(Color::White));
            let mut row = Row::new(buf, &mut app.hits, inner, inner.y + 3);
            row.x = inner.x + inner.width.saturating_sub(22);
            row.button("   OK   ", true, Action::PopupOk);
            row.gap(2);
            row.button(" Cancel ", false, Action::PopupClose);
        }
        Some(Popup::ConfirmQuit) => {
            let r = centered(full, 50, 7);
            f.render_widget(Clear, r);
            let b = block("Unsaved changes");
            let inner = b.inner(r);
            f.render_widget(b, r);
            let msg = format!("Save changes to {} before quitting?", app.path.display());
            f.render_widget(
                Paragraph::new(msg).wrap(Wrap { trim: true }).style(st),
                Rect::new(inner.x + 1, inner.y, inner.width - 2, 2),
            );
            let buf = f.buffer_mut();
            let mut row = Row::new(buf, &mut app.hits, inner, inner.y + 3);
            row.gap(1);
            row.button(" Save & quit (s) ", true, Action::SaveAndQuit);
            row.gap(1);
            row.button(" Quit (q) ", false, Action::QuitNoSave);
            row.gap(1);
            row.button(" Cancel ", false, Action::PopupClose);
        }
        Some(Popup::AlphaMenu) => {
            // Dropped down from (or up over) the picker's menu button.
            let Some(button) = app.picker.map(|l| l.menu) else { return };
            let (w, h) = (9, ALPHA_PRESETS.len() as u16 + 2);
            let x = button.right().saturating_sub(w);
            let y = if button.bottom() + h <= full.bottom() { button.bottom() } else { button.y.saturating_sub(h) };
            let r = Rect::new(x, y, w, h).intersection(full);
            f.render_widget(Clear, r);
            let b = Block::bordered().border_type(BorderType::Rounded).border_style(Style::new().fg(ACCENT)).style(st);
            let inner = b.inner(r);
            f.render_widget(b, r);
            let current = app.alpha_percent();
            let buf = f.buffer_mut();
            for (i, p) in ALPHA_PRESETS.into_iter().enumerate() {
                let mut row = Row::new(buf, &mut app.hits, inner, inner.y + i as u16);
                row.button(&format!(" {p:>3} % "), p == current, Action::Alpha(p));
            }
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(width: u16, tool: Tool) -> Buffer {
        let mut app = App::new(Document::new(64, 64), "t.png".into(), (19, 42), 64);
        app.tool = tool;
        let mut term = Terminal::new(TestBackend::new(width, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        term.backend().buffer().clone()
    }

    fn row_text(b: &Buffer, y: u16) -> String {
        (0..b.area.width).map(|x| b[(x, y)].symbol().to_string()).collect()
    }

    /// Print the toolbar as text plus a background map (`#` = boxed button,
    /// `-` = strip, `.` = toolbar bg): `cargo test show_toolbar -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn show_toolbar() {
        for w in [104, 160] {
            for tool in [Tool::Brush, Tool::Fill] {
                let b = render(w, tool);
                for y in 0..2 {
                    println!("{}", row_text(&b, y));
                    let map: String = (0..w)
                        .map(|x| match b[(x, y)].bg {
                            BTN => '#',
                            ACCENT => '@',
                            STRIP => '-',
                            BG => '.',
                            _ => '?',
                        })
                        .collect();
                    println!("{map}");
                }
                println!();
            }
        }
    }

    /// Print the About dialog, with `░` for the cells reserved for the icon:
    /// `cargo test show_about -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn show_about() {
        let mut app = App::new(Document::new(64, 64), "t.png".into(), (19, 42), 64);
        app.perform(Action::About);
        let mut term = Terminal::new(TestBackend::new(104, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let mut b = term.backend().buffer().clone();
        if let Some(r) = app.about_icon {
            for p in r.positions() {
                b[p].set_symbol("░");
            }
        }
        for y in 6..24 {
            println!("{}", row_text(&b, y).chars().skip(30).take(44).collect::<String>());
        }
    }

    fn open_about(width: u16, height: u16) -> (App, Buffer) {
        let mut app = App::new(Document::new(64, 64), "t.png".into(), (19, 42), 64);
        app.perform(Action::About);
        let mut term = Terminal::new(TestBackend::new(width, height)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let b = term.backend().buffer().clone();
        (app, b)
    }

    #[test]
    fn about_reserves_blank_popup_cells_for_the_icon() {
        let (app, b) = open_about(104, 30);
        let r = app.about_icon.expect("icon area");
        // 6 rows of 42px = 252px → 14 columns of 19px.
        assert_eq!((r.width, r.height), (14, ICON_ROWS));
        for p in r.positions() {
            assert_eq!(b[p].symbol(), " ", "icon cell {p:?} must be blank");
            assert_eq!(b[p].bg, POPUP_BG, "icon cell {p:?} must be inside the popup");
        }
        // Centred horizontally; the title follows one blank row below it.
        assert_eq!(r.x * 2 + r.width, 104);
        assert!(row_text(&b, r.y + r.height + 1).contains("termpaint"));
    }

    #[test]
    fn about_drops_the_icon_on_a_short_terminal() {
        let (app, b) = open_about(104, 12);
        assert_eq!(app.about_icon, None);
        let screen: String = (0..b.area.height).map(|y| row_text(&b, y) + "\n").collect();
        assert!(screen.contains("▞ termpaint"));
        assert!(screen.contains(&format!("Version {}", env!("CARGO_PKG_VERSION"))));
    }

    #[test]
    fn closing_about_clears_the_icon_area() {
        let (mut app, _) = open_about(104, 30);
        assert!(app.about_icon.is_some());
        app.perform(Action::PopupClose);
        let mut term = Terminal::new(TestBackend::new(104, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.about_icon, None);
    }

    #[test]
    fn clicking_the_app_name_opens_about() {
        let mut app = App::new(Document::new(64, 64), "t.png".into(), (19, 42), 64);
        let mut term = Terminal::new(TestBackend::new(104, 30)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let logo = app.hits.iter().find(|(_, a)| *a == Action::About).map(|(r, _)| *r);
        assert_eq!(logo, Some(Rect::new(0, 0, 13, 1)), "logo ' ▞ termpaint ' should be clickable");

        app.perform(Action::About);
        term.draw(|f| draw(f, &mut app)).unwrap();
        let b = term.backend().buffer().clone();
        let screen: String = (0..b.area.height).map(|y| row_text(&b, y) + "\n").collect();
        assert!(screen.contains(&format!("Version {}", env!("CARGO_PKG_VERSION"))));
        assert!(screen.contains("(c) Shuichi Kurabayashi"));
        assert!(app.hits.iter().any(|(_, a)| *a == Action::PopupClose), "OK button");
    }

    #[test]
    fn right_pane_stacks_colors_over_layers() {
        let a = areas(Rect::new(0, 0, 181, 49), (19, 42));
        assert_eq!((a.colors.x, a.colors.width), (a.layers.x, RIGHT_W));
        assert_eq!(a.colors.bottom(), a.layers.y);
        assert_eq!(a.canvas_block.right(), a.colors.x, "no left pane");
        assert_eq!(a.colors.height, picker::rows_for(RIGHT_W - 2, (19, 42)) + 2);
        // Short terminals keep a usable layers panel.
        let a = areas(Rect::new(0, 0, 100, 24), (19, 42));
        assert!(a.layers.height >= LAYERS_MIN_H);
    }

    #[test]
    fn picker_cells_let_the_bitmap_show_through() {
        let mut app = App::new(Document::new(64, 64), "t.png".into(), (19, 42), 64);
        let mut term = Terminal::new(TestBackend::new(140, 40)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let b = term.backend().buffer().clone();
        let l = app.picker.expect("picker");
        for p in l.area.positions() {
            assert_eq!(b[p].bg, Color::Reset, "picker cell {p:?} must keep the default background");
        }
        let text = |r: Rect| (r.x..r.right()).map(|x| b[(x, r.y)].symbol().to_string()).collect::<String>();
        assert_eq!(text(l.hsl[0]).trim_end(), "H: 0");
        assert_eq!(text(l.hsl[2]).trim_end(), "L: 0");
        assert_eq!(text(l.hex_label), "#:");
        assert_eq!(text(l.hex).trim(), "000000");
        assert_eq!(text(l.alpha).trim(), "100 %");
        assert_eq!(text(l.alpha_label), "Opacity");
        let screen: String = (0..b.area.height).map(|y| row_text(&b, y) + "\n").collect();
        assert!(screen.contains(" Colors ") && screen.contains(" Layers ") && screen.contains(" + New "));
    }

    #[test]
    fn strip_has_no_boxes_under_tool_buttons() {
        for w in [80, 104, 200] {
            let b = render(w, Tool::Brush);
            for x in 0..w {
                assert_ne!(b[(x, 1)].bg, BTN, "boxed element on row 2 at x={x} (width {w})");
            }
        }
    }
}
