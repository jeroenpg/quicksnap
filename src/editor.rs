//! Selection + annotation state. All coordinates are physical pixels of one screen's image.

use smithay_client_toolkit::seat::pointer::CursorIcon;

pub type P = (f32, f32);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Arrow,
    Ellipse,
    Rect,
    Pen,
}

pub const TOOLS: [Tool; 4] = [Tool::Arrow, Tool::Ellipse, Tool::Rect, Tool::Pen];

pub enum Shape {
    Arrow(P, P),
    Ellipse(P, P),
    Rect(P, P),
    Pen(Vec<P>),
}

#[derive(Clone, Copy)]
pub struct Sel {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Sel {
    fn from_points(a: P, b: P) -> Sel {
        Sel { x0: a.0.min(b.0), y0: a.1.min(b.1), x1: a.0.max(b.0), y1: a.1.max(b.1) }
    }

    fn contains(&self, p: P) -> bool {
        p.0 >= self.x0 && p.0 <= self.x1 && p.1 >= self.y0 && p.1 <= self.y1
    }

    /// Integer pixel bounds, clamped to the image.
    pub fn pixels(&self, w: u32, h: u32) -> (u32, u32, u32, u32) {
        let cx = |v: f32| (v.round().max(0.0) as u32).min(w);
        let cy = |v: f32| (v.round().max(0.0) as u32).min(h);
        (cx(self.x0), cy(self.y0), cx(self.x1), cy(self.y1))
    }
}

/// Size of the screen image being edited and its physical-per-logical scale.
pub struct View {
    pub w: f32,
    pub h: f32,
    pub scale: f32,
}

const LEFT: u8 = 1;
const RIGHT: u8 = 2;
const TOP: u8 = 4;
const BOTTOM: u8 = 8;

enum Drag {
    None,
    Selecting(P),
    Resizing(u8),
    Drawing,
}

pub struct Editor {
    /// Index of the screen holding the selection.
    pub screen: Option<usize>,
    pub sel: Option<Sel>,
    pub shapes: Vec<Shape>,
    pub tool: Tool,
    drag: Drag,
}

impl Default for Editor {
    fn default() -> Self {
        Editor { screen: None, sel: None, shapes: Vec::new(), tool: Tool::Arrow, drag: Drag::None }
    }
}

/// Toolbar placement: (x, y, button size, padding) in physical pixels.
pub fn toolbar(sel: &Sel, v: &View) -> (f32, f32, f32, f32) {
    let s = v.scale;
    let (btn, pad, gap, margin) = (34.0 * s, 4.0 * s, 8.0 * s, 4.0 * s);
    let tw = TOOLS.len() as f32 * (btn + pad) + pad;
    let th = btn + 2.0 * pad;
    let x = ((sel.x0 + sel.x1) / 2.0 - tw / 2.0).clamp(margin, (v.w - tw - margin).max(margin));
    let mut y = sel.y1 + gap;
    if y + th > v.h - margin {
        y = sel.y0 - gap - th;
        if y < margin {
            y = (sel.y1 - th - gap).max(margin);
        }
    }
    (x, y, btn, pad)
}

pub fn toolbar_button(sel: &Sel, v: &View, i: usize) -> (f32, f32, f32) {
    let (x, y, btn, pad) = toolbar(sel, v);
    (x + pad + i as f32 * (btn + pad), y + pad, btn)
}

impl Editor {
    pub fn selecting(&self) -> bool {
        matches!(self.drag, Drag::Selecting(_))
    }

    fn toolbar_hit(&self, sel: &Sel, v: &View, p: P) -> Option<usize> {
        (0..TOOLS.len()).find(|&i| {
            let (bx, by, b) = toolbar_button(sel, v, i);
            p.0 >= bx && p.0 <= bx + b && p.1 >= by && p.1 <= by + b
        })
    }

    fn edges_hit(sel: &Sel, v: &View, p: P) -> u8 {
        let t = 7.0 * v.scale;
        let inside_x = p.0 > sel.x0 - t && p.0 < sel.x1 + t;
        let inside_y = p.1 > sel.y0 - t && p.1 < sel.y1 + t;
        let mut e = 0;
        if inside_y && (p.0 - sel.x0).abs() <= t {
            e |= LEFT;
        } else if inside_y && (p.0 - sel.x1).abs() <= t {
            e |= RIGHT;
        }
        if inside_x && (p.1 - sel.y0).abs() <= t {
            e |= TOP;
        } else if inside_x && (p.1 - sel.y1).abs() <= t {
            e |= BOTTOM;
        }
        e
    }

    /// Left button down on screen `idx`. Returns a previously active screen that must be redrawn.
    pub fn press(&mut self, idx: usize, p: P, v: &View) -> Option<usize> {
        if self.screen == Some(idx) {
            if let Some(sel) = self.sel {
                if let Some(i) = self.toolbar_hit(&sel, v, p) {
                    self.tool = TOOLS[i];
                    return None;
                }
                let edges = Self::edges_hit(&sel, v, p);
                if edges != 0 {
                    self.drag = Drag::Resizing(edges);
                    return None;
                }
                if sel.contains(p) || !self.shapes.is_empty() {
                    self.shapes.push(match self.tool {
                        Tool::Arrow => Shape::Arrow(p, p),
                        Tool::Ellipse => Shape::Ellipse(p, p),
                        Tool::Rect => Shape::Rect(p, p),
                        Tool::Pen => Shape::Pen(vec![p]),
                    });
                    self.drag = Drag::Drawing;
                    return None;
                }
            }
        }
        let prev = self.screen.filter(|&s| s != idx);
        self.screen = Some(idx);
        self.sel = None;
        self.shapes.clear();
        self.drag = Drag::Selecting(p);
        prev
    }

    /// Replace the selection with `sel` on screen `idx` (right-click on a window).
    /// Returns a previously active screen that must be redrawn.
    pub fn select(&mut self, idx: usize, sel: Sel) -> Option<usize> {
        let prev = self.screen.filter(|&s| s != idx);
        self.screen = Some(idx);
        self.sel = Some(sel);
        self.shapes.clear();
        self.drag = Drag::None;
        prev
    }

    /// Pointer moved on screen `idx`. Returns whether anything changed.
    pub fn motion(&mut self, idx: usize, p: P, v: &View) -> bool {
        if self.screen != Some(idx) {
            return false;
        }
        let p = (p.0.clamp(0.0, v.w), p.1.clamp(0.0, v.h));
        match self.drag {
            Drag::None => false,
            Drag::Selecting(start) => {
                self.sel = Some(Sel::from_points(start, p));
                true
            }
            Drag::Resizing(edges) => {
                let Some(sel) = self.sel.as_mut() else { return false };
                if edges & LEFT != 0 {
                    sel.x0 = p.0;
                }
                if edges & RIGHT != 0 {
                    sel.x1 = p.0;
                }
                if edges & TOP != 0 {
                    sel.y0 = p.1;
                }
                if edges & BOTTOM != 0 {
                    sel.y1 = p.1;
                }
                // Dragging an edge past the opposite one flips which edge is held.
                let mut edges = edges;
                if sel.x0 > sel.x1 {
                    std::mem::swap(&mut sel.x0, &mut sel.x1);
                    edges ^= if edges & (LEFT | RIGHT) != 0 { LEFT | RIGHT } else { 0 };
                }
                if sel.y0 > sel.y1 {
                    std::mem::swap(&mut sel.y0, &mut sel.y1);
                    edges ^= if edges & (TOP | BOTTOM) != 0 { TOP | BOTTOM } else { 0 };
                }
                self.drag = Drag::Resizing(edges);
                true
            }
            Drag::Drawing => match self.shapes.last_mut() {
                Some(Shape::Arrow(_, b) | Shape::Ellipse(_, b) | Shape::Rect(_, b)) => {
                    *b = p;
                    true
                }
                Some(Shape::Pen(points)) => {
                    let last = *points.last().unwrap();
                    if (last.0 - p.0).hypot(last.1 - p.1) >= v.scale {
                        points.push(p);
                    }
                    true
                }
                None => false,
            },
        }
    }

    /// Left button up. Returns whether anything changed.
    pub fn release(&mut self, v: &View) -> bool {
        let min = 3.0 * v.scale;
        match std::mem::replace(&mut self.drag, Drag::None) {
            Drag::None => false,
            Drag::Selecting(_) => {
                if self.sel.is_some_and(|s| s.x1 - s.x0 < min || s.y1 - s.y0 < min) {
                    self.sel = None;
                }
                true
            }
            Drag::Resizing(_) => true,
            Drag::Drawing => {
                let tiny = match self.shapes.last() {
                    Some(Shape::Arrow(a, b) | Shape::Ellipse(a, b) | Shape::Rect(a, b)) => {
                        (a.0 - b.0).hypot(a.1 - b.1) < min
                    }
                    Some(Shape::Pen(points)) => points.len() < 2,
                    None => false,
                };
                if tiny {
                    self.shapes.pop();
                }
                true
            }
        }
    }

    pub fn undo(&mut self) -> bool {
        self.shapes.pop().is_some()
    }

    pub fn cursor(&self, idx: usize, p: P, v: &View) -> CursorIcon {
        let resize = |e: u8| match e {
            e if e == LEFT | TOP || e == RIGHT | BOTTOM => CursorIcon::NwseResize,
            e if e == RIGHT | TOP || e == LEFT | BOTTOM => CursorIcon::NeswResize,
            LEFT | RIGHT => CursorIcon::EwResize,
            _ => CursorIcon::NsResize,
        };
        match self.drag {
            Drag::Resizing(e) => return resize(e),
            Drag::Selecting(_) | Drag::Drawing => return CursorIcon::Crosshair,
            Drag::None => {}
        }
        if self.screen == Some(idx) {
            if let Some(sel) = self.sel {
                if self.toolbar_hit(&sel, v, p).is_some() {
                    return CursorIcon::Pointer;
                }
                let e = Self::edges_hit(&sel, v, p);
                if e != 0 {
                    return resize(e);
                }
            }
        }
        CursorIcon::Crosshair
    }
}
