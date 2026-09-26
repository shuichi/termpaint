//! Application state, input handling and canvas/graphics synchronisation.

use std::path::PathBuf;
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::document::{Document, PxRect, Rgba};
use crate::history::{Changed, Edit, History};
use crate::icon;
use crate::kitty::{Graphics, Z_BELOW_BG};
use crate::stroke::{self, BrushSpec, Mode, Operation};
use crate::tiles::Tiles;
use crate::viewport::Viewport;

pub const CURSOR_IMAGE: u32 = 0x7470_0002;
pub const ICON_IMAGE: u32 = 0x7470_0003;
/// Canvas tiles and the cursor live below cells with a non-default background, so any
/// ratatui widget with a background colour (panels, popups) occludes them,
/// while the empty default-background cells of the canvas area show them.
pub const CANVAS_Z: i32 = Z_BELOW_BG - 2;
pub const CURSOR_Z: i32 = Z_BELOW_BG - 1;
/// The About icon must show *over* the popup background, so it is drawn
/// above text; the UI leaves the cells under it blank.
pub const ICON_Z: i32 = 0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tool {
    Pencil,
    Brush,
    Eraser,
    Line,
    Rect,
    Ellipse,
    Fill,
    Picker,
    Hand,
}

impl Tool {
    pub const ALL: [Tool; 9] = [
        Tool::Pencil,
        Tool::Brush,
        Tool::Eraser,
        Tool::Line,
        Tool::Rect,
        Tool::Ellipse,
        Tool::Fill,
        Tool::Picker,
        Tool::Hand,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Tool::Pencil => "Pencil",
            Tool::Brush => "Brush",
            Tool::Eraser => "Eraser",
            Tool::Line => "Line",
            Tool::Rect => "Rect",
            Tool::Ellipse => "Ellipse",
            Tool::Fill => "Fill",
            Tool::Picker => "Picker",
            Tool::Hand => "Hand",
        }
    }
    pub fn key(self) -> char {
        match self {
            Tool::Pencil => 'p',
            Tool::Brush => 'b',
            Tool::Eraser => 'e',
            Tool::Line => 'l',
            Tool::Rect => 'r',
            Tool::Ellipse => 'o',
            Tool::Fill => 'f',
            Tool::Picker => 'i',
            Tool::Hand => 'h',
        }
    }
    pub fn params(self) -> &'static [Param] {
        use Param::*;
        match self {
            Tool::Pencil => &[Size, Opacity, Stabilizer],
            Tool::Brush | Tool::Eraser => &[Size, Hardness, Opacity, Stabilizer],
            Tool::Line | Tool::Rect | Tool::Ellipse => &[Size, Hardness, Opacity],
            Tool::Fill => &[Tolerance, Opacity],
            Tool::Picker | Tool::Hand => &[],
        }
    }
    fn draws_brush_cursor(self) -> bool {
        matches!(self, Tool::Pencil | Tool::Brush | Tool::Eraser | Tool::Line | Tool::Rect | Tool::Ellipse)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Param {
    Size,
    Hardness,
    Opacity,
    Stabilizer,
    Tolerance,
}

impl Param {
    pub fn label(self) -> &'static str {
        match self {
            Param::Size => "Size",
            Param::Hardness => "Hard",
            Param::Opacity => "Opac",
            Param::Stabilizer => "Stab",
            Param::Tolerance => "Tol",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Slider {
    R,
    G,
    B,
    A,
    H,
    S,
    V,
    LayerOpacity,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Tool(Tool),
    Param(Param, i32),
    Undo,
    Redo,
    Save,
    Help,
    About,
    Quit,
    ClearLayer,
    ZoomIn,
    ZoomOut,
    ZoomFit,
    Swatch(usize),
    Swap,
    Slider(Slider),
    LayerSelect(usize),
    LayerToggle(usize),
    LayerAdd,
    LayerDup,
    LayerDelete,
    LayerUp,
    LayerDown,
    LayerMerge,
    LayerRename,
    PopupClose,
    PopupOk,
    SaveAndQuit,
    QuitNoSave,
}

pub enum Popup {
    Help,
    About,
    Rename(String),
    ConfirmQuit,
}

pub const PALETTE: [u32; 32] = [
    0x000000, 0x3f3f46, 0x71717a, 0xa1a1aa, 0xd4d4d8, 0xffffff, 0x7f1d1d, 0xef4444, //
    0xf97316, 0xfbbf24, 0xfde047, 0xa3e635, 0x22c55e, 0x14532d, 0x2dd4bf, 0x0e7490, //
    0x38bdf8, 0x3b82f6, 0x1e3a8a, 0x6366f1, 0xa855f7, 0x6b21a8, 0xec4899, 0xf9a8d4, //
    0xfecaca, 0xfed7aa, 0xfef3c7, 0xd9f99d, 0xbbf7d0, 0xbae6fd, 0xddd6fe, 0x78350f,
];

pub fn palette_color(i: usize) -> Rgba {
    let c = PALETTE[i];
    Rgba([(c >> 16) as u8, (c >> 8) as u8, c as u8, 255])
}

/// Timing / bandwidth of the last rendered frame.
#[derive(Default, Clone, Copy)]
pub struct FrameStats {
    /// Time spent building the frame (UI + canvas + encoding), ms.
    pub ms: f32,
    /// Bytes written to the terminal.
    pub bytes: usize,
    /// Canvas pixels re-sent.
    pub px: usize,
}

enum ActiveOp {
    Freehand { op: Operation, smooth: (f32, f32) },
    Shape { op: Operation, start: (f32, f32), tool: Tool },
    Pan { from: (i32, i32), origin: (f32, f32) },
}

#[derive(Clone, Copy, PartialEq)]
enum CursorShape {
    Ring(u32),
    Cross,
}

pub struct App {
    pub doc: Document,
    pub history: History,
    pub tool: Tool,
    pub primary: Rgba,
    pub secondary: Rgba,
    /// HSV of the primary colour, kept separately so hue survives grey.
    pub hsv: (f32, f32, f32),
    pub size: u32,
    pub hardness: u8,
    pub opacity: u8,
    pub stabilizer: u8,
    pub tolerance: u8,
    pub view: Viewport,
    /// Cell size in pixels.
    pub cell: (u32, u32),
    /// Canvas area in cells (set by the UI each frame).
    pub canvas_cells: Rect,
    /// Blank cells reserved for the icon in the About dialog (set by the UI
    /// each frame; `None` when the dialog is closed or too small for it).
    pub about_icon: Option<Rect>,
    pub hits: Vec<(Rect, Action)>,
    pub popup: Option<Popup>,
    pub path: PathBuf,
    pub modified: bool,
    pub quit: bool,
    pub mouse_doc: Option<(i32, i32)>,
    /// Cell under the mouse pointer (for hover highlighting).
    pub hover: Option<Position>,
    /// Target canvas tile edge in pixels.
    pub tile_px: u32,
    /// Show frame statistics in the status bar (F3).
    pub perf: bool,
    pub stats: FrameStats,
    status: String,
    status_at: Instant,
    op: Option<ActiveOp>,
    slider_drag: Option<(Slider, Rect)>,
    mouse_px: Option<(i32, i32)>,
    /// Document regions changed since the last frame.
    doc_dirty: Vec<PxRect>,
    view_full: bool,
    tiles: Option<Tiles>,
    force_recreate: bool,
    view_initialized: bool,
    cursor_image: Option<(CursorShape, u32)>,
    cursor_placed: Option<(PxRect, (i32, i32))>,
    /// Edge length of the icon image last sent to the terminal.
    icon_image: Option<u32>,
    /// Anchor cell and pixel offset of the current icon placement.
    icon_placed: Option<(u16, u16, (u32, u32))>,
}

impl App {
    pub fn new(doc: Document, path: PathBuf, cell: (u32, u32), tile_px: u32) -> Self {
        let primary = Rgba::BLACK;
        Self {
            doc,
            history: History::new(512 << 20),
            tool: Tool::Brush,
            primary,
            secondary: Rgba::WHITE,
            hsv: rgb_to_hsv(primary),
            size: 8,
            hardness: 80,
            opacity: 100,
            stabilizer: 0,
            tolerance: 24,
            view: Viewport::new(),
            cell,
            canvas_cells: Rect::default(),
            about_icon: None,
            hits: Vec::new(),
            popup: None,
            path,
            modified: false,
            quit: false,
            mouse_doc: None,
            hover: None,
            tile_px,
            perf: false,
            stats: FrameStats::default(),
            status: String::from("Welcome to termpaint — press ? for help"),
            status_at: Instant::now(),
            op: None,
            slider_drag: None,
            mouse_px: None,
            doc_dirty: Vec::new(),
            view_full: false,
            tiles: None,
            force_recreate: false,
            view_initialized: false,
            cursor_image: None,
            cursor_placed: None,
            icon_image: None,
            icon_placed: None,
        }
    }

    pub fn status(&self) -> &str {
        if self.status_at.elapsed().as_secs() < 6 { &self.status } else { "" }
    }

    pub fn set_status(&mut self, s: impl Into<String>) {
        self.status = s.into();
        self.status_at = Instant::now();
    }

    pub fn param_value(&self, p: Param) -> String {
        match p {
            Param::Size => format!("{}px", self.size),
            Param::Hardness => format!("{}%", self.hardness),
            Param::Opacity => format!("{}%", self.opacity),
            Param::Stabilizer => format!("{}", self.stabilizer),
            Param::Tolerance => format!("{}", self.tolerance),
        }
    }

    fn adjust_param(&mut self, p: Param, d: i32) {
        match p {
            Param::Size => {
                let step = if self.size >= 40 { 4 } else if self.size >= 12 { 2 } else { 1 };
                self.size = (self.size as i32 + d * step).clamp(1, 256) as u32;
            }
            Param::Hardness => self.hardness = (self.hardness as i32 + d * 5).clamp(0, 100) as u8,
            Param::Opacity => self.opacity = (self.opacity as i32 + d * 5).clamp(1, 100) as u8,
            Param::Stabilizer => self.stabilizer = (self.stabilizer as i32 + d).clamp(0, 10) as u8,
            Param::Tolerance => self.tolerance = (self.tolerance as i32 + d * 4).clamp(0, 255) as u8,
        }
    }

    pub fn set_primary(&mut self, c: Rgba) {
        self.primary = c;
        self.hsv = rgb_to_hsv(c);
    }

    pub fn slider_value(&self, s: Slider) -> f32 {
        let [r, g, b, a] = self.primary.0;
        match s {
            Slider::R => r as f32 / 255.0,
            Slider::G => g as f32 / 255.0,
            Slider::B => b as f32 / 255.0,
            Slider::A => a as f32 / 255.0,
            Slider::H => self.hsv.0 / 360.0,
            Slider::S => self.hsv.1,
            Slider::V => self.hsv.2,
            Slider::LayerOpacity => self.doc.layers[self.doc.active].opacity as f32 / 100.0,
        }
    }

    fn set_slider(&mut self, s: Slider, v: f32) {
        let v = v.clamp(0.0, 1.0);
        let b = (v * 255.0).round() as u8;
        match s {
            Slider::R | Slider::G | Slider::B | Slider::A => {
                let k = match s {
                    Slider::R => 0,
                    Slider::G => 1,
                    Slider::B => 2,
                    _ => 3,
                };
                let mut c = self.primary;
                c.0[k] = b;
                if k == 3 {
                    self.primary = c;
                } else {
                    self.set_primary(c);
                }
            }
            Slider::H | Slider::S | Slider::V => {
                match s {
                    Slider::H => self.hsv.0 = v * 360.0,
                    Slider::S => self.hsv.1 = v,
                    _ => self.hsv.2 = v,
                }
                let mut c = hsv_to_rgb(self.hsv);
                c.0[3] = self.primary.0[3];
                self.primary = c;
            }
            Slider::LayerOpacity => {
                let a = self.doc.active;
                self.doc.layers[a].opacity = (v * 100.0).round() as u8;
                self.invalidate_doc(self.doc.bounds());
                self.modified = true;
            }
        }
    }

    // ------------------------------------------------------------------
    // Invalidation

    fn invalidate_doc(&mut self, r: PxRect) {
        if r.is_empty() {
            return;
        }
        // Keep separate rects (a fast diagonal stroke stays a few small
        // boxes instead of one big bounding box), but bound the list.
        if self.doc_dirty.len() >= 32 {
            let u = self.doc_dirty.drain(..).fold(r, |a, b| a.union(&b));
            self.doc_dirty.push(u);
        } else {
            self.doc_dirty.push(r);
        }
    }

    fn invalidate_view(&mut self) {
        self.view_full = true;
    }

    fn apply_changed(&mut self, c: Changed) {
        match c {
            Changed::Region(r) => self.invalidate_doc(r),
            Changed::Everything => self.invalidate_doc(self.doc.bounds()),
        }
        self.modified = true;
    }

    // ------------------------------------------------------------------
    // Actions

    pub fn perform(&mut self, a: Action) {
        match a {
            Action::Tool(t) => {
                self.tool = t;
                self.set_status(format!("Tool: {}", t.label()));
            }
            Action::Param(p, d) => self.adjust_param(p, d),
            Action::Undo => {
                self.cancel_op();
                match self.history.undo(&mut self.doc) {
                    Some(c) => {
                        self.apply_changed(c);
                        self.set_status("Undo");
                    }
                    None => self.set_status("Nothing to undo"),
                }
            }
            Action::Redo => {
                self.cancel_op();
                match self.history.redo(&mut self.doc) {
                    Some(c) => {
                        self.apply_changed(c);
                        self.set_status("Redo");
                    }
                    None => self.set_status("Nothing to redo"),
                }
            }
            Action::Save => self.save(),
            Action::Help => self.popup = Some(Popup::Help),
            Action::About => self.popup = Some(Popup::About),
            Action::Quit => {
                if self.modified {
                    self.popup = Some(Popup::ConfirmQuit);
                } else {
                    self.quit = true;
                }
            }
            Action::SaveAndQuit => {
                self.save();
                if !self.modified {
                    self.quit = true;
                }
                self.popup = None;
            }
            Action::QuitNoSave => self.quit = true,
            Action::ClearLayer => {
                let l = self.doc.active;
                let r = self.doc.bounds();
                let before = self.doc.read_rect(l, r);
                self.doc.layers[l].pixels.fill(0);
                let after = self.doc.read_rect(l, r);
                self.history.push(Edit::Pixels { layer: l, rect: r, before, after });
                self.apply_changed(Changed::Everything);
                self.set_status("Layer cleared");
            }
            Action::ZoomIn | Action::ZoomOut => {
                let c = (self.view.w as f32 / 2.0, self.view.h as f32 / 2.0);
                self.view.zoom_step(if a == Action::ZoomIn { 1 } else { -1 }, c);
                self.invalidate_view();
            }
            Action::ZoomFit => {
                self.view.fit(&self.doc);
                self.invalidate_view();
            }
            Action::Swatch(i) => self.set_primary(palette_color(i)),
            Action::Swap => {
                let p = self.primary;
                self.set_primary(self.secondary);
                self.secondary = p;
            }
            Action::Slider(_) => {}
            Action::LayerSelect(i) => self.doc.active = i,
            Action::LayerToggle(i) => {
                self.doc.layers[i].visible = !self.doc.layers[i].visible;
                self.invalidate_doc(self.doc.bounds());
            }
            Action::LayerAdd => {
                let layer = self.doc.blank_layer();
                let index = self.doc.active + 1;
                self.doc.layers.insert(index, layer.clone());
                self.doc.active = index;
                self.history.push(Edit::AddLayer { index, layer });
                self.apply_changed(Changed::Everything);
            }
            Action::LayerDup => {
                let mut layer = self.doc.layers[self.doc.active].clone();
                layer.name = format!("{} copy", layer.name);
                let index = self.doc.active + 1;
                self.doc.layers.insert(index, layer.clone());
                self.doc.active = index;
                self.history.push(Edit::AddLayer { index, layer });
                self.apply_changed(Changed::Everything);
            }
            Action::LayerDelete => {
                if self.doc.layers.len() <= 1 {
                    self.set_status("Cannot delete the last layer");
                    return;
                }
                let index = self.doc.active;
                let layer = self.doc.layers.remove(index);
                self.doc.active = index.saturating_sub(1).min(self.doc.layers.len() - 1);
                self.history.push(Edit::DeleteLayer { index, layer });
                self.apply_changed(Changed::Everything);
            }
            Action::LayerUp | Action::LayerDown => {
                let from = self.doc.active;
                let to = if a == Action::LayerUp { from + 1 } else { from.wrapping_sub(1) };
                if to >= self.doc.layers.len() {
                    return;
                }
                let l = self.doc.layers.remove(from);
                self.doc.layers.insert(to, l);
                self.doc.active = to;
                self.history.push(Edit::MoveLayer { from, to });
                self.apply_changed(Changed::Everything);
            }
            Action::LayerMerge => {
                let index = self.doc.active;
                if index == 0 {
                    self.set_status("No layer below to merge into");
                    return;
                }
                let upper = self.doc.layers[index].clone();
                let lower_before = self.doc.layers[index - 1].pixels.clone();
                self.doc.merge_down(index);
                self.history.push(Edit::Merge { index, upper, lower_before });
                self.apply_changed(Changed::Everything);
            }
            Action::LayerRename => {
                let name = self.doc.layers[self.doc.active].name.clone();
                self.popup = Some(Popup::Rename(name));
            }
            Action::PopupClose => self.popup = None,
            Action::PopupOk => {
                if let Some(Popup::Rename(name)) = self.popup.take() {
                    let name = name.trim();
                    if !name.is_empty() {
                        let a = self.doc.active;
                        self.doc.layers[a].name = name.to_string();
                    }
                }
            }
        }
    }

    fn save(&mut self) {
        match crate::io::save_png(&self.path, self.doc.width, self.doc.height, &self.doc.flatten()) {
            Ok(()) => {
                self.modified = false;
                self.set_status(format!("Saved {}", self.path.display()));
            }
            Err(e) => self.set_status(format!("Save failed: {e}")),
        }
    }

    // ------------------------------------------------------------------
    // Keyboard

    pub fn on_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        if let Some(p) = &mut self.popup {
            match p {
                Popup::Rename(s) => match k.code {
                    KeyCode::Enter => self.perform(Action::PopupOk),
                    KeyCode::Esc => self.popup = None,
                    KeyCode::Backspace => {
                        s.pop();
                    }
                    KeyCode::Char(c) if !ctrl && s.chars().count() < 32 => s.push(c),
                    _ => {}
                },
                Popup::ConfirmQuit => match k.code {
                    KeyCode::Char('s') | KeyCode::Char('y') => self.perform(Action::SaveAndQuit),
                    KeyCode::Char('q') | KeyCode::Char('d') => self.quit = true,
                    KeyCode::Esc | KeyCode::Char('c') | KeyCode::Char('n') => self.popup = None,
                    _ => {}
                },
                Popup::Help | Popup::About => self.popup = None,
            }
            return;
        }
        if ctrl {
            match k.code {
                KeyCode::Char('z') => self.perform(Action::Undo),
                KeyCode::Char('y') => self.perform(Action::Redo),
                KeyCode::Char('s') => self.perform(Action::Save),
                KeyCode::Char('c') | KeyCode::Char('q') => self.perform(Action::Quit),
                _ => {}
            }
            return;
        }
        let pan = 64.0;
        match k.code {
            KeyCode::Char(c) => {
                if let Some(t) = Tool::ALL.iter().find(|t| t.key() == c) {
                    self.perform(Action::Tool(*t));
                    return;
                }
                match c {
                    '[' => self.adjust_param(Param::Size, -1),
                    ']' => self.adjust_param(Param::Size, 1),
                    '{' => self.adjust_param(Param::Hardness, -1),
                    '}' => self.adjust_param(Param::Hardness, 1),
                    '<' | ',' => self.adjust_param(Param::Opacity, -1),
                    '>' | '.' => self.adjust_param(Param::Opacity, 1),
                    's' => self.adjust_param(Param::Stabilizer, 1),
                    'S' => self.adjust_param(Param::Stabilizer, -1),
                    '+' | '=' => self.perform(Action::ZoomIn),
                    '-' | '_' => self.perform(Action::ZoomOut),
                    '0' => self.perform(Action::ZoomFit),
                    '1' => {
                        let c = (self.view.w as f32 / 2.0, self.view.h as f32 / 2.0);
                        self.view.zoom_to(1.0, c);
                        self.invalidate_view();
                    }
                    'x' => self.perform(Action::Swap),
                    'u' => self.perform(Action::Undo),
                    'U' => self.perform(Action::Redo),
                    'n' => self.perform(Action::LayerAdd),
                    'N' => self.perform(Action::LayerRename),
                    'D' => self.perform(Action::LayerDelete),
                    'M' => self.perform(Action::LayerMerge),
                    'v' => self.perform(Action::LayerToggle(self.doc.active)),
                    'K' => self.perform(Action::LayerUp),
                    'J' => self.perform(Action::LayerDown),
                    'k' if self.doc.active + 1 < self.doc.layers.len() => self.doc.active += 1,
                    'j' => self.doc.active = self.doc.active.saturating_sub(1),
                    '?' => self.perform(Action::Help),
                    'q' => self.perform(Action::Quit),
                    _ => {}
                }
            }
            KeyCode::F(1) => self.perform(Action::Help),
            KeyCode::F(2) => self.perform(Action::LayerRename),
            KeyCode::F(3) => self.perf = !self.perf,
            KeyCode::Delete => self.perform(Action::ClearLayer),
            KeyCode::Left => self.pan_by(pan, 0.0),
            KeyCode::Right => self.pan_by(-pan, 0.0),
            KeyCode::Up => self.pan_by(0.0, pan),
            KeyCode::Down => self.pan_by(0.0, -pan),
            KeyCode::Esc => self.cancel_op(),
            _ => {}
        }
    }

    fn pan_by(&mut self, dx: f32, dy: f32) {
        self.view.ox += dx;
        self.view.oy += dy;
        self.invalidate_view();
    }

    // ------------------------------------------------------------------
    // Mouse

    fn canvas_px_rect(&self) -> PxRect {
        let a = self.canvas_cells;
        let (cw, ch) = (self.cell.0 as i32, self.cell.1 as i32);
        PxRect::new(
            a.x as i32 * cw,
            a.y as i32 * ch,
            (a.x + a.width) as i32 * cw,
            (a.y + a.height) as i32 * ch,
        )
    }

    fn hit(&self, pos: Position) -> Option<(Rect, Action)> {
        self.hits.iter().rev().find(|(r, _)| r.contains(pos)).copied()
    }

    fn slider_from_col(&mut self, s: Slider, r: Rect, px: i32) {
        // Use pixel precision inside the slider for smooth values.
        let x0 = r.x as f32 * self.cell.0 as f32;
        let w = (r.width as f32 * self.cell.0 as f32 - 1.0).max(1.0);
        self.set_slider(s, (px as f32 - x0) / w);
    }

    /// `ev.column`/`ev.row` are *pixel* coordinates (SGR-Pixels mode 1016).
    pub fn on_mouse(&mut self, ev: MouseEvent) {
        let (mx, my) = (ev.column as i32, ev.row as i32);
        let cell = Position::new((mx / self.cell.0 as i32) as u16, (my / self.cell.1 as i32) as u16);
        self.mouse_px = Some((mx, my));
        self.hover = Some(cell);
        let cr = self.canvas_px_rect();
        let in_canvas = mx >= cr.x0 && mx < cr.x1 && my >= cr.y0 && my < cr.y1;
        let vp = ((mx - cr.x0) as f32 + 0.5, (my - cr.y0) as f32 + 0.5);
        let dp = self.view.view_to_doc(vp.0, vp.1);
        self.mouse_doc = if in_canvas {
            let (x, y) = (dp.0.floor() as i32, dp.1.floor() as i32);
            (x >= 0 && y >= 0 && x < self.doc.width as i32 && y < self.doc.height as i32).then_some((x, y))
        } else {
            None
        };

        match ev.kind {
            MouseEventKind::Down(btn) => {
                if self.popup.is_some() {
                    match self.hit(cell) {
                        Some((_, a)) => self.perform(a),
                        None => {
                            if matches!(self.popup, Some(Popup::Help | Popup::About)) {
                                self.popup = None;
                            }
                        }
                    }
                    return;
                }
                if self.op.is_some() {
                    return;
                }
                if let Some((r, a)) = self.hit(cell) {
                    match a {
                        Action::Slider(s) => {
                            self.slider_drag = Some((s, r));
                            self.slider_from_col(s, r, mx);
                        }
                        Action::Swatch(i) if btn == MouseButton::Right => self.secondary = palette_color(i),
                        Action::Param(p, d) if btn == MouseButton::Right => self.adjust_param(p, -d),
                        _ => self.perform(a),
                    }
                } else if in_canvas {
                    self.canvas_down(btn, vp, dp, (mx, my));
                }
            }
            MouseEventKind::Drag(_) => {
                if let Some((s, r)) = self.slider_drag {
                    self.slider_from_col(s, r, mx);
                } else {
                    self.canvas_drag(dp, (mx, my));
                }
            }
            MouseEventKind::Up(_) => {
                self.slider_drag = None;
                self.canvas_up();
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                if self.popup.is_some() {
                    return;
                }
                let d = if ev.kind == MouseEventKind::ScrollUp { 1 } else { -1 };
                if in_canvas {
                    if ev.modifiers.contains(KeyModifiers::CONTROL) {
                        self.adjust_param(Param::Size, d);
                    } else {
                        self.view.zoom_step(d, vp);
                        self.invalidate_view();
                    }
                } else if let Some((_, a)) = self.hit(cell) {
                    match a {
                        Action::Param(p, _) => self.adjust_param(p, d),
                        Action::Slider(s) => {
                            let v = self.slider_value(s) + d as f32 / 100.0;
                            self.set_slider(s, v);
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn canvas_down(&mut self, btn: MouseButton, _vp: (f32, f32), dp: (f32, f32), m: (i32, i32)) {
        if btn == MouseButton::Middle || self.tool == Tool::Hand {
            self.op = Some(ActiveOp::Pan { from: m, origin: (self.view.ox, self.view.oy) });
            return;
        }
        let color = if btn == MouseButton::Right { self.secondary } else { self.primary };
        if self.tool == Tool::Picker {
            if let Some((x, y)) = self.mouse_doc {
                let c = Rgba(self.doc.flatten_pixel(x, y));
                if btn == MouseButton::Right {
                    self.secondary = c;
                } else {
                    self.set_primary(c);
                }
                self.set_status(format!("Picked {}", c.hex()));
            }
            return;
        }
        let layer = &self.doc.layers[self.doc.active];
        if !layer.visible {
            self.set_status(format!("'{}' is hidden — show it to paint", layer.name));
            return;
        }
        let mode = if self.tool == Tool::Eraser { Mode::Erase } else { Mode::Paint };
        let brush = BrushSpec {
            size: self.size as f32,
            hardness: self.hardness as f32 / 100.0,
            antialias: self.tool != Tool::Pencil,
        };
        let mut op = Operation::begin(&self.doc, mode, color, self.opacity as f32 / 100.0, brush);
        match self.tool {
            Tool::Fill => {
                if let Some((x, y)) = self.mouse_doc {
                    let r = op.flood_fill(&mut self.doc, x, y, self.tolerance);
                    self.invalidate_doc(r);
                    self.finish(op);
                }
            }
            Tool::Pencil | Tool::Brush | Tool::Eraser => {
                let r = op.freehand_to(&mut self.doc, dp);
                self.invalidate_doc(r);
                self.op = Some(ActiveOp::Freehand { op, smooth: dp });
            }
            Tool::Line | Tool::Rect | Tool::Ellipse => {
                let r = op.set_shape(&mut self.doc, &[dp], false);
                self.invalidate_doc(r);
                self.op = Some(ActiveOp::Shape { op, start: dp, tool: self.tool });
            }
            Tool::Picker | Tool::Hand => unreachable!(),
        }
    }

    fn canvas_drag(&mut self, dp: (f32, f32), m: (i32, i32)) {
        let k = 1.0 / (1.0 + self.stabilizer as f32 * 0.8);
        let mut dirty = PxRect::EMPTY;
        match &mut self.op {
            Some(ActiveOp::Freehand { op, smooth }) => {
                smooth.0 += (dp.0 - smooth.0) * k;
                smooth.1 += (dp.1 - smooth.1) * k;
                dirty = op.freehand_to(&mut self.doc, *smooth);
            }
            Some(ActiveOp::Shape { op, start, tool }) => {
                let s = *start;
                let pts = match tool {
                    Tool::Line => vec![s, dp],
                    Tool::Rect => stroke::rect_points(s, dp),
                    _ => stroke::ellipse_points(s, dp),
                };
                dirty = op.set_shape(&mut self.doc, &pts, *tool != Tool::Line);
            }
            Some(ActiveOp::Pan { from, origin }) => {
                self.view.ox = origin.0 + (m.0 - from.0) as f32;
                self.view.oy = origin.1 + (m.1 - from.1) as f32;
                self.view_full = true;
            }
            None => {}
        }
        self.invalidate_doc(dirty);
    }

    fn canvas_up(&mut self) {
        let Some(active) = self.op.take() else { return };
        match active {
            ActiveOp::Freehand { mut op, smooth } => {
                // Catch the stabilised point up to the real pointer position.
                if let Some((mx, my)) = self.mouse_px {
                    let cr = self.canvas_px_rect();
                    let p = self.view.view_to_doc((mx - cr.x0) as f32 + 0.5, (my - cr.y0) as f32 + 0.5);
                    if self.stabilizer > 0 && ((p.0 - smooth.0).abs() > 0.5 || (p.1 - smooth.1).abs() > 0.5) {
                        let r = op.freehand_to(&mut self.doc, p);
                        self.invalidate_doc(r);
                    }
                }
                let r = op.freehand_end(&mut self.doc);
                self.invalidate_doc(r);
                self.finish(op);
            }
            ActiveOp::Shape { op, .. } => self.finish(op),
            ActiveOp::Pan { .. } => {}
        }
    }

    fn cancel_op(&mut self) {
        if let Some(ActiveOp::Freehand { op, .. } | ActiveOp::Shape { op, .. }) = self.op.take() {
            let r = op.touched.intersect(&self.doc.bounds());
            if !r.is_empty() {
                let before = op.snapshot_rect(r);
                self.doc.write_rect(op.layer, r, &before);
                self.invalidate_doc(r);
            }
        }
    }

    fn finish(&mut self, op: Operation) {
        let r = op.touched.intersect(&self.doc.bounds());
        if r.is_empty() {
            return;
        }
        let before = op.snapshot_rect(r);
        let after = self.doc.read_rect(op.layer, r);
        self.history.push(Edit::Pixels { layer: op.layer, rect: r, before, after });
        self.modified = true;
    }

    pub fn on_resize(&mut self, cell: (u32, u32)) {
        self.cell = cell;
        self.force_recreate = true;
    }

    // ------------------------------------------------------------------
    // Graphics synchronisation

    /// Push canvas changes to the terminal: re-render the dirty parts of
    /// the view bitmap and re-send only the tiles they touch.
    pub fn sync_graphics(&mut self, g: &mut Graphics) {
        let area = self.canvas_cells;
        let (cw, ch) = self.cell;
        let (w, h) = (area.width as u32 * cw, area.height as u32 * ch);
        let stale = self.force_recreate || self.tiles.as_ref().is_none_or(|t| t.area != area || t.cell != self.cell);
        if stale && let Some(old) = self.tiles.take() {
            old.delete_all(g);
            g.delete_image(CURSOR_IMAGE);
            self.cursor_image = None;
            self.cursor_placed = None;
        }
        if self.force_recreate && self.icon_image.take().is_some() {
            // The cell size may have changed; re-send at the new size.
            g.delete_image(ICON_IMAGE);
            self.icon_placed = None;
        }
        self.force_recreate = false;
        self.sync_icon(g);
        if w == 0 || h == 0 {
            return;
        }

        let dirty = std::mem::take(&mut self.doc_dirty);
        for r in &dirty {
            self.doc.recomposite(*r);
        }
        let tiles = match &mut self.tiles {
            Some(t) => t,
            None => {
                self.view.resize(w, h);
                if !self.view_initialized {
                    self.view.fit(&self.doc);
                    self.view_initialized = true;
                }
                self.view_full = true;
                self.tiles.insert(Tiles::new(area, self.cell, self.tile_px))
            }
        };
        if std::mem::take(&mut self.view_full) {
            self.view.render(&self.doc, self.view.view_rect());
            tiles.mark_all();
        } else {
            for r in dirty {
                let vr = self.view.doc_to_view(r);
                self.view.render(&self.doc, vr);
                tiles.mark(vr);
            }
        }
        self.stats.px = tiles.upload(&self.view, g, CANVAS_Z);
        self.sync_cursor(g);
    }

    /// Brush outline drawn as a second, tiny Kitty image stacked above the
    /// canvas (higher z) and moved with pixel offsets.
    fn sync_cursor(&mut self, g: &mut Graphics) {
        let cr = self.canvas_px_rect();
        let visible_at = match self.mouse_px {
            Some((mx, my))
                if self.popup.is_none()
                    && !matches!(self.op, Some(ActiveOp::Pan { .. }))
                    && mx >= cr.x0
                    && mx < cr.x1
                    && my >= cr.y0
                    && my < cr.y1 =>
            {
                Some((mx, my))
            }
            _ => None,
        };
        let Some((mx, my)) = visible_at else {
            if self.cursor_placed.take().is_some() {
                g.delete_placement(CURSOR_IMAGE, 1);
            }
            return;
        };

        let d = (self.size as f32 * self.view.zoom).round() as u32;
        let shape = if self.tool.draws_brush_cursor() && d >= 5 { CursorShape::Ring(d.min(1400)) } else { CursorShape::Cross };
        let side = match shape {
            CursorShape::Ring(d) => d + 4,
            CursorShape::Cross => 15,
        };
        if self.cursor_image.map(|c| c.0) != Some(shape) {
            g.transmit_rgba(CURSOR_IMAGE, side, side, &cursor_bitmap(shape, side));
            self.cursor_image = Some((shape, side));
            self.cursor_placed = None;
        }
        let half = side as i32 / 2;
        let full = PxRect::new(mx - half, my - half, mx - half + side as i32, my - half + side as i32);
        let vis = full.intersect(&cr);
        if vis.is_empty() {
            return;
        }
        let key = (vis, (vis.x0 - full.x0, vis.y0 - full.y0));
        if self.cursor_placed == Some(key) {
            return;
        }
        let (cw, ch) = (self.cell.0 as i32, self.cell.1 as i32);
        let crop = if vis == full {
            None
        } else {
            Some(((vis.x0 - full.x0) as u32, (vis.y0 - full.y0) as u32, vis.width() as u32, vis.height() as u32))
        };
        g.place(
            CURSOR_IMAGE,
            1,
            (vis.x0 / cw) as u16,
            (vis.y0 / ch) as u16,
            ((vis.x0 % cw) as u32, (vis.y0 % ch) as u32),
            crop,
            CURSOR_Z,
        );
        self.cursor_placed = Some(key);
    }

    /// Application icon in the About dialog: scaled to the blank cells the UI
    /// reserved for it, centred with pixel offsets, and removed (but kept
    /// uploaded for next time) when the dialog closes.
    fn sync_icon(&mut self, g: &mut Graphics) {
        let Some(area) = self.about_icon.filter(|r| !r.is_empty()) else {
            if self.icon_placed.take().is_some() {
                g.delete_placement(ICON_IMAGE, 1);
            }
            return;
        };
        let (cw, ch) = self.cell;
        let (aw, ah) = (area.width as u32 * cw, area.height as u32 * ch);
        let side = aw.min(ah).min(icon::SIZE);
        if self.icon_image != Some(side) {
            g.transmit_rgba(ICON_IMAGE, side, side, &icon::bitmap(side));
            self.icon_image = Some(side);
            self.icon_placed = None;
        }
        let (ox, oy) = ((aw - side) / 2, (ah - side) / 2);
        let key = (area.x + (ox / cw) as u16, area.y + (oy / ch) as u16, (ox % cw, oy % ch));
        if self.icon_placed != Some(key) {
            g.place(ICON_IMAGE, 1, key.0, key.1, key.2, None, ICON_Z);
            self.icon_placed = Some(key);
        }
    }

    pub fn shutdown(&self, g: &mut Graphics) {
        if let Some(t) = &self.tiles {
            t.delete_all(g);
        }
        g.delete_image(CURSOR_IMAGE);
        g.delete_image(ICON_IMAGE);
    }

    pub fn tile_grid(&self) -> (u16, u16) {
        self.tiles.as_ref().map_or((0, 0), |t| t.grid())
    }

    pub fn can_undo(&self) -> bool {
        self.history.can_undo()
    }
    pub fn can_redo(&self) -> bool {
        self.history.can_redo()
    }
}

fn cursor_bitmap(shape: CursorShape, side: u32) -> Vec<u8> {
    let mut buf = vec![0u8; (side * side * 4) as usize];
    let c = side as f32 / 2.0;
    let mut put = |x: u32, y: u32, v: [u8; 4]| {
        let i = ((y * side + x) * 4) as usize;
        if buf[i + 3] < v[3] {
            buf[i..i + 4].copy_from_slice(&v);
        }
    };
    const DARK: [u8; 4] = [20, 20, 20, 230];
    const LIGHT: [u8; 4] = [255, 255, 255, 200];
    match shape {
        CursorShape::Ring(d) => {
            let r = d as f32 / 2.0;
            for y in 0..side {
                for x in 0..side {
                    let dx = x as f32 + 0.5 - c;
                    let dy = y as f32 + 0.5 - c;
                    let dist = (dx * dx + dy * dy).sqrt();
                    if (dist - r).abs() <= 0.6 {
                        put(x, y, DARK);
                    } else if (dist - (r + 1.2)).abs() <= 0.6 || (dist - (r - 1.2)).abs() <= 0.6 {
                        put(x, y, LIGHT);
                    }
                }
            }
        }
        CursorShape::Cross => {
            let m = side / 2;
            for i in 0..side {
                if i.abs_diff(m) < 2 {
                    continue;
                }
                put(i, m, DARK);
                put(m, i, DARK);
                for o in [m - 1, m + 1] {
                    put(i, o, LIGHT);
                    put(o, i, LIGHT);
                }
            }
        }
    }
    buf
}

pub fn rgb_to_hsv(c: Rgba) -> (f32, f32, f32) {
    let [r, g, b, _] = c.0.map(|v| v as f32 / 255.0);
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let d = max - min;
    let h = if d == 0.0 {
        0.0
    } else if max == r {
        60.0 * (((g - b) / d).rem_euclid(6.0))
    } else if max == g {
        60.0 * ((b - r) / d + 2.0)
    } else {
        60.0 * ((r - g) / d + 4.0)
    };
    let s = if max == 0.0 { 0.0 } else { d / max };
    (h, s, max)
}

pub fn hsv_to_rgb((h, s, v): (f32, f32, f32)) -> Rgba {
    let c = v * s;
    let hp = (h.rem_euclid(360.0)) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r, g, b) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    let f = |u: f32| ((u + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    Rgba([f(r), f(g), f(b), 255])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hsv_roundtrip() {
        for c in PALETTE {
            let rgb = Rgba([(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]);
            assert_eq!(hsv_to_rgb(rgb_to_hsv(rgb)), rgb);
        }
    }

    /// Draw the UI, sync graphics and return the escape sequences sent.
    fn frame(app: &mut App, term: &mut ratatui::Terminal<ratatui::backend::TestBackend>) -> String {
        term.draw(|f| crate::ui::draw(f, app)).unwrap();
        let mut g = Graphics::new(false);
        app.sync_graphics(&mut g);
        let mut out = Vec::new();
        g.flush_to(&mut out).unwrap();
        String::from_utf8_lossy(&out).into_owned()
    }

    #[test]
    fn about_icon_is_uploaded_once_placed_and_removed() {
        let mut app = App::new(Document::new(64, 64), "t.png".into(), (19, 42), 64);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(104, 30)).unwrap();
        let upload = format!("a=t,f=32,t=d,i={ICON_IMAGE},s=252,v=252");
        let place = format!("a=p,i={ICON_IMAGE},p=1,z={ICON_Z},C=1");
        let remove = format!("a=d,d=i,i={ICON_IMAGE},p=1");
        assert!(!frame(&mut app, &mut term).contains(&format!("i={ICON_IMAGE}")));

        app.perform(Action::About);
        let s = frame(&mut app, &mut term);
        assert!(s.contains(&upload), "icon uploaded at 6 rows × 42px");
        // 14 cols × 19px = 266px wide: centred with a 7px offset.
        let r = app.about_icon.unwrap();
        assert!(s.contains(&format!("\x1b[{};{}H\x1b_G{place},X=7,Y=0", r.y + 1, r.x + 1)), "{s:?}");

        assert!(!frame(&mut app, &mut term).contains(&format!("i={ICON_IMAGE}")), "unchanged → nothing sent");

        app.perform(Action::PopupClose);
        let s = frame(&mut app, &mut term);
        assert!(s.contains(&remove));
        assert!(!s.contains("a=d,d=I"), "image data is kept for the next time");

        app.perform(Action::About);
        let s = frame(&mut app, &mut term);
        assert!(s.contains(&place) && !s.contains(&upload), "reopening only re-places");
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use ratatui::crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    use std::time::Instant;

    /// `cargo test --release bench_stroke -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn bench_stroke() {
        // Retina-ish kitty window: 16x32 px cells, 150x48 canvas cells.
        let cell = (16, 32);
        let doc = Document::new(2300, 1450);
        let tile = std::env::var("TILE").ok().and_then(|v| v.parse().ok()).unwrap_or(64);
        let mut app = App::new(doc, "bench.png".into(), cell, tile);
        app.canvas_cells = Rect::new(29, 3, 150, 48);
        app.size = 12;
        app.stabilizer = 0;
        let mut g = Graphics::new(true);
        app.sync_graphics(&mut g);
        let mut sink = Vec::new();
        g.flush_to(&mut sink).unwrap();

        let ev = |kind, x: i32, y: i32| MouseEvent { kind, column: x as u16, row: y as u16, modifiers: KeyModifiers::NONE };
        let (x0, y0) = (29 * 16 + 300, 3 * 32 + 300);
        let n = 600;
        let mut bytes = 0usize;
        let mut px = 0usize;
        let t = Instant::now();
        app.on_mouse(ev(MouseEventKind::Down(MouseButton::Left), x0, y0));
        for i in 1..=n {
            let a = i as f32 * 0.02;
            let x = x0 + (i as f32 * 2.5) as i32;
            let y = y0 + (200.0 * a.sin()) as i32;
            app.on_mouse(ev(MouseEventKind::Drag(MouseButton::Left), x, y));
            app.sync_graphics(&mut g);
            sink.clear();
            g.flush_to(&mut sink).unwrap();
            bytes += sink.len();
            px += app.stats.px;
        }
        app.on_mouse(ev(MouseEventKind::Up(MouseButton::Left), x0 + n * 5 / 2, y0));
        let el = t.elapsed();
        println!(
            "events={n} total={:.1}ms per_event={:.3}ms bytes/event={} terminal_px/event={} (whole view: {})",
            el.as_secs_f64() * 1e3,
            el.as_secs_f64() * 1e3 / n as f64,
            bytes / n as usize,
            px / n as usize,
            app.view.w * app.view.h
        );
    }
}
