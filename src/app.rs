//! Application state, input handling and canvas/graphics synchronisation.

use std::path::PathBuf;
use std::time::Instant;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::{Position, Rect};

use crate::document::{Document, PxRect, Rgba};
use crate::history::{Changed, Edit, History};
use crate::icon;
use crate::kitty::{Graphics, Z_BELOW_BG};
use crate::picker::{self, Part};
use crate::stroke::{self, BrushSpec, Mode, Operation};
use crate::tiles::Tiles;
use crate::viewport::Viewport;

pub const CURSOR_IMAGE: u32 = 0x7470_0002;
pub const ICON_IMAGE: u32 = 0x7470_0003;
pub const CANVAS_TILES: u32 = 0x7470_1000;
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
    Swap,
    /// The colour picker; which part was hit is resolved in pixels.
    Picker,
    /// Set the primary colour's opacity (percent).
    Alpha(u8),
    LayerOpacity,
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
    /// Opacity presets dropped down from the colour picker.
    AlphaMenu,
}

pub const ALPHA_PRESETS: [u8; 5] = [100, 75, 50, 25, 0];

/// Timing / bandwidth of the last rendered frame.
#[derive(Default, Clone, Copy)]
pub struct FrameStats {
    /// Time spent building the frame (UI + canvas + encoding), ms.
    pub ms: f32,
    /// Bytes written to the terminal.
    pub bytes: usize,
    /// Canvas and colour picker pixels re-sent.
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
    /// Colour picker geometry (set by the UI each frame; `None` when the
    /// panel is too small for it).
    pub picker: Option<picker::Layout>,
    /// Hex digits typed into the picker's colour field while it is edited.
    pub hex_edit: Option<String>,
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
    /// Layer opacity slider being dragged.
    slider_drag: Option<Rect>,
    /// Picker ring, triangle or opacity slider being dragged.
    picker_drag: Option<Part>,
    picker_gfx: Option<picker::Gfx>,
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
            picker: None,
            hex_edit: None,
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
            picker_drag: None,
            picker_gfx: None,
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

    /// Set the primary colour from HSV, keeping its opacity.
    fn set_hsv(&mut self, hsv: (f32, f32, f32)) {
        self.hsv = hsv;
        let mut c = hsv_to_rgb(hsv);
        c.0[3] = self.primary.0[3];
        self.primary = c;
    }

    /// Opacity of the primary colour in percent.
    pub fn alpha_percent(&self) -> u8 {
        (self.primary.0[3] as f32 / 2.55).round() as u8
    }

    fn set_alpha_percent(&mut self, p: i32) {
        self.primary.0[3] = (p.clamp(0, 100) as f32 * 2.55).round() as u8;
    }

    /// Hue in degrees and HSL saturation / lightness in percent of the
    /// primary colour, as shown in the picker.
    pub fn hsl_readout(&self) -> (u16, u16, u16) {
        let (_, s, l) = hsv_to_hsl(self.hsv);
        ((self.hsv.0.round() as u16) % 360, (s * 100.0).round() as u16, (l * 100.0).round() as u16)
    }

    pub fn layer_opacity(&self) -> f32 {
        self.doc.layers[self.doc.active].opacity as f32 / 100.0
    }

    fn set_layer_opacity(&mut self, v: f32) {
        let a = self.doc.active;
        self.doc.layers[a].opacity = (v.clamp(0.0, 1.0) * 100.0).round() as u8;
        self.invalidate_doc(self.doc.bounds());
        self.modified = true;
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
            Action::Swap => {
                let p = self.primary;
                self.set_primary(self.secondary);
                self.secondary = p;
            }
            Action::Picker | Action::LayerOpacity => {}
            Action::Alpha(p) => {
                self.set_alpha_percent(p as i32);
                self.popup = None;
            }
            Action::LayerSelect(i) => self.doc.active = i,
            Action::LayerToggle(i) => {
                self.doc.layers[i].visible = !self.doc.layers[i].visible;
                self.invalidate_doc(self.doc.bounds());
                self.modified = true;
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
                    let a = self.doc.active;
                    if !name.is_empty() && name != self.doc.layers[a].name {
                        self.doc.layers[a].name = name.to_string();
                        self.modified = true;
                    }
                }
            }
        }
    }

    fn save(&mut self) {
        match crate::io::save_document(&self.path, &self.doc) {
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
                Popup::Help | Popup::About | Popup::AlphaMenu => self.popup = None,
            }
            return;
        }
        // The hex field takes plain keys while it is edited (hex digits
        // double as tool shortcuts); Ctrl shortcuts abandon the edit.
        if let Some(s) = &mut self.hex_edit {
            if !ctrl {
                match k.code {
                    KeyCode::Enter => self.commit_hex(),
                    KeyCode::Esc => self.hex_edit = None,
                    KeyCode::Backspace => {
                        s.pop();
                    }
                    KeyCode::Char(c) if c.is_ascii_hexdigit() && s.len() < 8 => s.push(c.to_ascii_uppercase()),
                    _ => {}
                }
                return;
            }
            self.hex_edit = None;
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

    fn slider_from_col(&mut self, r: Rect, px: i32) {
        // Use pixel precision inside the slider for smooth values.
        let x0 = r.x as f32 * self.cell.0 as f32;
        let w = (r.width as f32 * self.cell.0 as f32 - 1.0).max(1.0);
        self.set_layer_opacity((px as f32 - x0) / w);
    }

    fn picker_part(&self, m: (i32, i32)) -> Option<Part> {
        self.picker.and_then(|l| l.part_at(m))
    }

    fn picker_down(&mut self, btn: MouseButton, m: (i32, i32)) {
        let Some(part) = self.picker_part(m) else { return };
        let right = btn == MouseButton::Right;
        match part {
            Part::Ring | Part::Triangle | Part::Alpha => {
                self.picker_drag = Some(part);
                self.picker_drag_to(part, m);
            }
            // Clicking the colour behind brings it to the front.
            Part::Secondary | Part::Swap => self.perform(Action::Swap),
            Part::Transparent => {
                if right {
                    self.secondary.0[3] = 0;
                } else {
                    self.primary.0[3] = 0;
                }
            }
            Part::Black | Part::White => {
                let c = if part == Part::Black { Rgba::BLACK } else { Rgba::WHITE };
                if right {
                    self.secondary = c;
                } else {
                    self.set_primary(c);
                }
            }
            Part::Hex => {
                if self.hex_edit.is_none() {
                    self.hex_edit = Some(String::new());
                }
            }
            Part::AlphaField | Part::AlphaMenu => self.popup = Some(Popup::AlphaMenu),
            Part::Primary | Part::Hue | Part::Saturation | Part::Lightness => {}
        }
    }

    fn picker_drag_to(&mut self, part: Part, m: (i32, i32)) {
        let Some(l) = self.picker else { return };
        let (h, s, v) = self.hsv;
        match part {
            Part::Ring => self.set_hsv((l.hue_at(m), s, v)),
            Part::Triangle => {
                let (ns, nv) = l.sv_at(m, h);
                self.set_hsv((h, ns.unwrap_or(s), nv));
            }
            Part::Alpha => self.primary.0[3] = l.alpha_at(m),
            _ => {}
        }
    }

    /// Mouse wheel over the picker fine-tunes the value under the pointer.
    fn picker_scroll(&mut self, d: i32, m: (i32, i32)) {
        let (h, s, v) = self.hsv;
        let step = d as f32 / 100.0;
        match self.picker_part(m) {
            Some(Part::Ring | Part::Hue) => self.set_hsv(((h + d as f32).rem_euclid(360.0), s, v)),
            Some(Part::Triangle) => self.set_hsv((h, s, (v + step).clamp(0.0, 1.0))),
            Some(part @ (Part::Saturation | Part::Lightness)) => {
                let (_, mut hs, mut hl) = hsv_to_hsl(self.hsv);
                if part == Part::Saturation {
                    hs = (hs + step).clamp(0.0, 1.0);
                } else {
                    hl = (hl + step).clamp(0.0, 1.0);
                }
                let (_, ns, nv) = hsl_to_hsv((h, hs, hl));
                // Saturation is undefined at black; keep the old one.
                self.set_hsv((h, if nv > 0.0 { ns } else { s }, nv));
            }
            Some(Part::Alpha | Part::AlphaField | Part::AlphaMenu) => self.set_alpha_percent(self.alpha_percent() as i32 + d),
            _ => {}
        }
    }

    /// Apply the digits typed into the hex field: 3 or 6 set the colour and
    /// keep its opacity, 8 set the opacity too. Empty leaves it unchanged.
    fn commit_hex(&mut self) {
        let Some(s) = self.hex_edit.take() else { return };
        if s.is_empty() {
            return;
        }
        match parse_hex(&s, self.primary.0[3]) {
            Some(c) if c == self.primary => {}
            Some(c) => self.set_primary(c),
            None => self.set_status(format!("Not a colour: #{s} (use 3, 6 or 8 hex digits)")),
        }
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
                            if matches!(self.popup, Some(Popup::Help | Popup::About | Popup::AlphaMenu)) {
                                self.popup = None;
                            }
                        }
                    }
                    return;
                }
                if self.op.is_some() {
                    return;
                }
                // Clicking anywhere but the hex field applies what was typed.
                if self.hex_edit.is_some() && self.picker_part((mx, my)) != Some(Part::Hex) {
                    self.commit_hex();
                }
                if let Some((r, a)) = self.hit(cell) {
                    match a {
                        Action::LayerOpacity => {
                            self.slider_drag = Some(r);
                            self.slider_from_col(r, mx);
                        }
                        Action::Picker => self.picker_down(btn, (mx, my)),
                        Action::Param(p, d) if btn == MouseButton::Right => self.adjust_param(p, -d),
                        _ => self.perform(a),
                    }
                } else if in_canvas {
                    self.canvas_down(btn, vp, dp, (mx, my));
                }
            }
            MouseEventKind::Drag(_) => {
                if let Some(part) = self.picker_drag {
                    self.picker_drag_to(part, (mx, my));
                } else if let Some(r) = self.slider_drag {
                    self.slider_from_col(r, mx);
                } else {
                    self.canvas_drag(dp, (mx, my));
                }
            }
            MouseEventKind::Up(_) => {
                self.slider_drag = None;
                self.picker_drag = None;
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
                        Action::LayerOpacity => self.set_layer_opacity(self.layer_opacity() + d as f32 / 100.0),
                        Action::Picker => self.picker_scroll(d, (mx, my)),
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
        let recreate = std::mem::replace(&mut self.force_recreate, false);
        self.sync_icon(g);
        let picker_px = self.sync_picker(g, recreate);
        if w == 0 || h == 0 {
            self.stats.px = picker_px;
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
                self.tiles.insert(Tiles::new(area, self.cell, self.tile_px, CANVAS_TILES))
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
        self.stats.px = tiles.upload(&self.view.buf, g, CANVAS_Z) + picker_px;
        self.sync_cursor(g);
    }

    fn picker_state(&self) -> picker::State {
        let idle = self.popup.is_none() && self.picker_drag.is_none();
        picker::State {
            hsv: self.hsv,
            primary: self.primary,
            secondary: self.secondary,
            hover: self.mouse_px.filter(|_| idle).and_then(|m| self.picker_part(m)).filter(|p| p.hoverable()),
            editing: self.hex_edit.is_some(),
            menu: matches!(self.popup, Some(Popup::AlphaMenu)),
        }
    }

    /// Colour picker tiles: rebuilt when the layout changes, otherwise
    /// re-rendered only when what they show changes. Returns pixels sent.
    fn sync_picker(&mut self, g: &mut Graphics, recreate: bool) -> usize {
        if (recreate || self.picker_gfx.as_ref().map(|p| p.layout) != self.picker)
            && let Some(old) = self.picker_gfx.take()
        {
            old.delete(g);
        }
        let Some(layout) = self.picker else { return 0 };
        let state = self.picker_state();
        let tile_px = self.tile_px;
        self.picker_gfx.get_or_insert_with(|| picker::Gfx::new(layout, tile_px)).sync(&state, g)
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
        if let Some(p) = &self.picker_gfx {
            p.delete(g);
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

/// HSV → HSL (hue unchanged; saturation and lightness in 0..=1).
pub fn hsv_to_hsl((h, s, v): (f32, f32, f32)) -> (f32, f32, f32) {
    let l = v * (1.0 - s / 2.0);
    let m = l.min(1.0 - l);
    (h, if m > 0.0 { (v - l) / m } else { 0.0 }, l)
}

pub fn hsl_to_hsv((h, s, l): (f32, f32, f32)) -> (f32, f32, f32) {
    let v = l + s * l.min(1.0 - l);
    (h, if v > 0.0 { 2.0 * (1.0 - l / v) } else { 0.0 }, v)
}

/// `RGB`/`RRGGBB` (opacity `alpha`) or `RRGGBBAA`.
fn parse_hex(s: &str, alpha: u8) -> Option<Rgba> {
    let s: String = if s.len() == 3 { s.chars().flat_map(|c| [c, c]).collect() } else { s.into() };
    let v = u32::from_str_radix(&s, 16).ok()?;
    match s.len() {
        6 => Some(Rgba([(v >> 16) as u8, (v >> 8) as u8, v as u8, alpha])),
        8 => Some(Rgba([(v >> 24) as u8, (v >> 16) as u8, (v >> 8) as u8, v as u8])),
        _ => None,
    }
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
        for c in [0x000000u32, 0x71717a, 0xffffff, 0x7f1d1d, 0xf97316, 0x22c55e, 0x0e7490, 0x6366f1, 0xf9a8d4, 0x78350f] {
            let rgb = Rgba([(c >> 16) as u8, (c >> 8) as u8, c as u8, 255]);
            assert_eq!(hsv_to_rgb(rgb_to_hsv(rgb)), rgb);
            let (h, s, v) = rgb_to_hsv(rgb);
            let (_, s2, v2) = hsl_to_hsv(hsv_to_hsl((h, s, v)));
            assert!((s - s2).abs() < 1e-4 && (v - v2).abs() < 1e-4, "HSL round trip of {c:06x}");
        }
        assert_eq!(hsv_to_hsl(rgb_to_hsv(Rgba([235, 235, 235, 255]))).2, 235.0 / 255.0);
    }

    #[test]
    fn hex_parsing() {
        assert_eq!(parse_hex("3366CC", 128), Some(Rgba([0x33, 0x66, 0xcc, 128])));
        assert_eq!(parse_hex("F80", 255), Some(Rgba([0xff, 0x88, 0x00, 255])));
        assert_eq!(parse_hex("11223344", 255), Some(Rgba([0x11, 0x22, 0x33, 0x44])));
        assert_eq!(parse_hex("12345", 255), None);
        assert_eq!(parse_hex("", 255), None);
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

    fn mouse(app: &mut App, kind: MouseEventKind, (x, y): (i32, i32)) {
        app.on_mouse(MouseEvent { kind, column: x as u16, row: y as u16, modifiers: KeyModifiers::NONE });
    }

    fn click(app: &mut App, btn: MouseButton, p: (i32, i32)) {
        mouse(app, MouseEventKind::Down(btn), p);
        mouse(app, MouseEventKind::Up(btn), p);
    }

    fn key(app: &mut App, code: KeyCode) {
        app.on_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    /// App after one frame, so the picker layout is known.
    fn with_picker() -> (App, ratatui::Terminal<ratatui::backend::TestBackend>, picker::Layout) {
        let mut app = App::new(Document::new(64, 64), "t.png".into(), (19, 42), 64);
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(140, 40)).unwrap();
        frame(&mut app, &mut term);
        let l = app.picker.expect("picker fits");
        (app, term, l)
    }

    /// Screen pixel of a point given relative to the ring centre.
    fn wheel_px(l: &picker::Layout, dx: f32, dy: f32) -> (i32, i32) {
        (
            (l.area.x as u32 * l.cell.0) as i32 + (l.center.0 + dx) as i32,
            (l.area.y as u32 * l.cell.1) as i32 + (l.center.1 + dy) as i32,
        )
    }

    fn ring_px(l: &picker::Layout, deg: f32) -> (i32, i32) {
        let r = (l.r_out + l.r_in) / 2.0;
        wheel_px(l, r * deg.to_radians().cos(), r * deg.to_radians().sin())
    }

    fn cell_px(r: Rect) -> (i32, i32) {
        (r.x as i32 * 19 + 5, r.y as i32 * 42 + 5)
    }

    #[test]
    fn wheel_picks_saturation_value_then_hue() {
        let (mut app, _, l) = with_picker();
        assert_eq!(app.primary, Rgba::BLACK);
        // Next to the pure-hue vertex of the triangle (red at hue 0).
        click(&mut app, MouseButton::Left, wheel_px(&l, l.r_tri - 4.0, 0.0));
        let [r, g, b, a] = app.primary.0;
        assert!(r > 240 && g < 20 && b < 20 && a == 255, "{:?}", app.primary);

        // Dragging round the ring turns it green, keeping saturation/value.
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), ring_px(&l, 0.0));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), ring_px(&l, 60.0));
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), wheel_px(&l, -0.5 * l.r_out, 0.866 * l.r_out));
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), ring_px(&l, 120.0));
        assert!((app.hsv.0 - 120.0).abs() < 1.0, "hue {}", app.hsv.0);
        let [r, g, b, _] = app.primary.0;
        assert!(r < 20 && g > 240 && b < 20, "{:?}", app.primary);
        assert!(!app.modified, "picking colours does not touch the document");
    }

    #[test]
    fn hex_field_captures_typing() {
        let (mut app, _, l) = with_picker();
        app.tool = Tool::Pencil;
        click(&mut app, MouseButton::Left, cell_px(l.hex));
        assert_eq!(app.hex_edit.as_deref(), Some(""));
        // b, e and f are tool shortcuts too.
        for c in "bEef0".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        key(&mut app, KeyCode::Backspace);
        for c in "00q".chars() {
            key(&mut app, KeyCode::Char(c));
        }
        assert_eq!(app.hex_edit.as_deref(), Some("BEEF00"));
        key(&mut app, KeyCode::Enter);
        assert_eq!((app.primary, app.tool, app.quit), (Rgba([0xbe, 0xef, 0x00, 255]), Tool::Pencil, false));

        // Clicking elsewhere applies the field; junk is rejected.
        click(&mut app, MouseButton::Left, cell_px(l.hex));
        key(&mut app, KeyCode::Char('1'));
        key(&mut app, KeyCode::Char('2'));
        click(&mut app, MouseButton::Left, cell_px(l.hsl[0]));
        assert_eq!((app.hex_edit.as_deref(), app.primary), (None, Rgba([0xbe, 0xef, 0x00, 255])));
        assert!(app.status().contains("#12"));
    }

    #[test]
    fn opacity_menu_and_slider() {
        let (mut app, mut term, l) = with_picker();
        click(&mut app, MouseButton::Left, cell_px(l.menu));
        assert!(matches!(app.popup, Some(Popup::AlphaMenu)));
        frame(&mut app, &mut term);
        let item = app.hits.iter().find(|(_, a)| *a == Action::Alpha(50)).map(|(r, _)| *r).expect("50% item");
        click(&mut app, MouseButton::Left, cell_px(item));
        assert_eq!((app.primary.0[3], app.alpha_percent()), (128, 50));
        assert!(app.popup.is_none());
        frame(&mut app, &mut term);

        let track = |x: f32| wheel_px(&l, x - l.center.0, (l.track.y0 + l.track.y1) / 2.0 - l.center.1);
        mouse(&mut app, MouseEventKind::Down(MouseButton::Left), track(l.track.x1 - 1.0));
        assert_eq!(app.primary.0[3], 255);
        mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), track(l.track.x0 - 50.0));
        assert_eq!(app.primary.0[3], 0, "dragging past the end clamps");
        mouse(&mut app, MouseEventKind::Up(MouseButton::Left), track(l.track.x0));
        mouse(&mut app, MouseEventKind::ScrollUp, cell_px(l.alpha));
        assert_eq!(app.alpha_percent(), 1);
    }

    #[test]
    fn swatches_swap_and_none() {
        let (mut app, _, l) = with_picker();
        let local = |x: f32, y: f32| wheel_px(&l, x - l.center.0, y - l.center.1);
        click(&mut app, MouseButton::Left, local(l.white.x0 + 3.0, l.white.y0 + 3.0));
        click(&mut app, MouseButton::Right, local(l.black.x0 + 3.0, l.black.y0 + 3.0));
        assert_eq!((app.primary, app.secondary), (Rgba::WHITE, Rgba::BLACK));
        click(&mut app, MouseButton::Left, local(l.secondary.x - 0.6 * l.secondary.r, l.secondary.y));
        assert_eq!((app.primary, app.secondary), (Rgba::BLACK, Rgba::WHITE), "clicking the back colour swaps");
        click(&mut app, MouseButton::Left, local(l.transparent.x, l.transparent.y));
        assert_eq!(app.primary, Rgba([0, 0, 0, 0]));
    }

    #[test]
    fn picker_hover_redraws_only_its_tiles() {
        let (mut app, mut term, l) = with_picker();
        assert!(frame(&mut app, &mut term).is_empty(), "idle frame sends nothing");
        let white = wheel_px(&l, l.white.x0 + 3.0 - l.center.0, l.white.y0 + 3.0 - l.center.1);
        mouse(&mut app, MouseEventKind::Moved, white);
        let s = frame(&mut app, &mut term);
        assert!(s.contains(&format!("i={}", picker::TILE_BASE)) || s.contains("a=T"), "hover redraws picker tiles");
        assert!(!s.contains(&format!("i={CANVAS_TILES},")), "canvas untouched");
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
