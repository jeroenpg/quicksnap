//! Drawing. Pixel buffers are BGRA (wl_shm XRGB8888), so colours are built with red and
//! blue swapped; tiny-skia blends every channel the same way, so the result is correct.

use tiny_skia::{
    Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, PixmapMut, Rect, Stroke, Transform,
};

use crate::capture::Image;
use crate::editor::{toolbar, toolbar_button, Editor, Sel, Shape, View, P, TOOLS};

fn color(r: u8, g: u8, b: u8, a: u8) -> Color {
    Color::from_rgba8(b, g, r, a)
}

fn paint(c: Color) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color(c);
    p.anti_alias = true;
    p
}

fn stroke(width: f32) -> Stroke {
    Stroke { width, line_cap: LineCap::Round, line_join: LineJoin::Round, ..Default::default() }
}

const RED: (u8, u8, u8) = (235, 32, 32);

fn red() -> Color {
    color(RED.0, RED.1, RED.2, 255)
}

/// Copy rows [x0, x1) x [y0, y1) from `src` into `dst` (both full-width images of width `w`).
fn copy_rect(dst: &mut [u8], src: &[u8], w: u32, (x0, y0, x1, y1): (u32, u32, u32, u32)) {
    let (w, x0, x1) = (w as usize, x0 as usize, x1 as usize);
    for y in y0 as usize..y1 as usize {
        let o = (y * w + x0) * 4..(y * w + x1) * 4;
        dst[o.clone()].copy_from_slice(&src[o]);
    }
}

/// Paint one overlay frame into `canvas` (same size as `img`).
pub fn compose(canvas: &mut [u8], img: &Image, ed: &Editor, idx: usize, scale: f32) {
    canvas.copy_from_slice(&img.dimmed);
    if ed.screen != Some(idx) {
        return;
    }
    let Some(sel) = ed.sel else { return };
    copy_rect(canvas, &img.data, img.width, sel.pixels(img.width, img.height));

    let mut pm = PixmapMut::from_bytes(canvas, img.width, img.height).unwrap();
    draw_shapes(&mut pm, &ed.shapes, scale, Transform::identity());
    draw_selection(&mut pm, &sel, scale);
    if !ed.selecting() {
        let view = View { w: img.width as f32, h: img.height as f32, scale };
        draw_toolbar(&mut pm, ed, &sel, &view);
    }
}

/// Cut out `rect` with annotations. Returns (width, height, RGB bytes).
pub fn export(img: &Image, ed: &Editor, rect: (u32, u32, u32, u32), scale: f32) -> (u32, u32, Vec<u8>) {
    let (x0, y0, x1, y1) = rect;
    let (w, h) = (x1 - x0, y1 - y0);
    let mut buf = vec![0u8; (w * h * 4) as usize];
    for y in 0..h as usize {
        let src = ((y + y0 as usize) * img.width as usize + x0 as usize) * 4;
        buf[y * w as usize * 4..(y + 1) * w as usize * 4]
            .copy_from_slice(&img.data[src..src + w as usize * 4]);
    }
    if let Some(mut pm) = PixmapMut::from_bytes(&mut buf, w, h) {
        draw_shapes(&mut pm, &ed.shapes, scale, Transform::from_translate(-(x0 as f32), -(y0 as f32)));
    }
    let rgb = buf.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
    (w, h, rgb)
}

fn draw_shapes(pm: &mut PixmapMut, shapes: &[Shape], scale: f32, t: Transform) {
    let width = 4.0 * scale;
    let paint = paint(red());
    for shape in shapes {
        match shape {
            Shape::Arrow(a, b) => draw_arrow(pm, *a, *b, width, &paint, t),
            Shape::Ellipse(a, b) => {
                if let Some(path) = rect_of(*a, *b).and_then(PathBuilder::from_oval) {
                    pm.stroke_path(&path, &paint, &stroke(width), t, None);
                }
            }
            Shape::Rect(a, b) => {
                if let Some(r) = rect_of(*a, *b) {
                    pm.stroke_path(&PathBuilder::from_rect(r), &paint, &stroke(width), t, None);
                }
            }
            Shape::Pen(points) => {
                let mut pb = PathBuilder::new();
                pb.move_to(points[0].0, points[0].1);
                for p in &points[1..] {
                    pb.line_to(p.0, p.1);
                }
                if let Some(path) = pb.finish() {
                    pm.stroke_path(&path, &paint, &stroke(width), t, None);
                }
            }
            Shape::Marker(c, n) => draw_marker(pm, *c, *n, scale, t),
        }
    }
}

/// Radius of a numbered marker in logical pixels.
pub const MARKER_RADIUS: f32 = 16.0;

/// A red disc with a white ring and the number `n` in white, centred on `c`.
fn draw_marker(pm: &mut PixmapMut, c: P, n: u8, scale: f32, t: Transform) {
    let r = MARKER_RADIUS * scale;
    // A soft shadow keeps it readable on red or white backgrounds.
    if let Some(path) = PathBuilder::from_circle(c.0, c.1 + 1.5 * scale, r + 2.5 * scale) {
        pm.fill_path(&path, &paint(color(0, 0, 0, 90)), FillRule::Winding, t, None);
    }
    if let Some(path) = PathBuilder::from_circle(c.0, c.1, r) {
        pm.fill_path(&path, &paint(red()), FillRule::Winding, t, None);
        pm.stroke_path(&path, &paint(color(255, 255, 255, 255)), &stroke(2.5 * scale), t, None);
    }
    let digits: Vec<u8> = n.to_string().bytes().map(|b| b - b'0').collect();
    // Glyphs are 1 unit tall; two digits get a little smaller to fit the disc.
    let h = if digits.len() > 1 { 0.8 * r } else { 0.95 * r };
    let gap = 0.2;
    let ink = |d: u8| if d == 1 { (0.06, 0.30) } else if d == 4 { (0.0, 0.58) } else { (0.0, 0.56) };
    let total: f32 = digits.iter().map(|&d| ink(d).1 - ink(d).0).sum::<f32>() + gap * (digits.len() - 1) as f32;
    let mut x = c.0 - total * h / 2.0;
    let y = c.1 - h / 2.0;
    let mut pb = PathBuilder::new();
    for &d in &digits {
        let (x0, x1) = ink(d);
        push_digit(&mut pb, d, x - x0 * h, y, h);
        x += (x1 - x0 + gap) * h;
    }
    if let Some(path) = pb.finish() {
        pm.stroke_path(&path, &paint(color(255, 255, 255, 255)), &stroke(0.17 * h), t, None);
    }
}

/// Add the stroke outline of digit `d` to `pb`, in a box `size` tall with its top-left at (x, y).
/// Glyph coordinates are in units of the height: about 0.56 wide, 1 tall, y pointing down.
fn push_digit(pb: &mut PathBuilder, d: u8, x: f32, y: f32, size: f32) {
    let p = |gx: f32, gy: f32| (x + gx * size, y + gy * size);
    let mv = |pb: &mut PathBuilder, a: f32, b: f32| {
        let (px, py) = p(a, b);
        pb.move_to(px, py);
    };
    let ln = |pb: &mut PathBuilder, a: f32, b: f32| {
        let (px, py) = p(a, b);
        pb.line_to(px, py);
    };
    let quad = |pb: &mut PathBuilder, c: [f32; 4]| {
        let ((ax, ay), (bx, by)) = (p(c[0], c[1]), p(c[2], c[3]));
        pb.quad_to(ax, ay, bx, by);
    };
    let cubic = |pb: &mut PathBuilder, c: [f32; 6]| {
        let ((ax, ay), (bx, by), (cx, cy)) = (p(c[0], c[1]), p(c[2], c[3]), p(c[4], c[5]));
        pb.cubic_to(ax, ay, bx, by, cx, cy);
    };
    let oval = |pb: &mut PathBuilder, l: f32, t: f32, r: f32, b: f32| {
        let ((l, t), (r, b)) = (p(l, t), p(r, b));
        if let Some(rect) = Rect::from_ltrb(l, t, r, b) {
            pb.push_oval(rect);
        }
    };
    match d {
        0 => oval(pb, 0.0, 0.0, 0.56, 1.0),
        1 => {
            mv(pb, 0.06, 0.2);
            ln(pb, 0.30, 0.0);
            ln(pb, 0.30, 1.0);
        }
        2 => {
            mv(pb, 0.02, 0.24);
            cubic(pb, [0.04, -0.06, 0.56, -0.06, 0.56, 0.28]);
            cubic(pb, [0.56, 0.5, 0.12, 0.72, 0.0, 1.0]);
            ln(pb, 0.56, 1.0);
        }
        3 => {
            mv(pb, 0.03, 0.14);
            cubic(pb, [0.14, -0.05, 0.54, -0.04, 0.53, 0.24]);
            cubic(pb, [0.52, 0.42, 0.36, 0.47, 0.22, 0.47]);
            cubic(pb, [0.44, 0.47, 0.58, 0.58, 0.56, 0.74]);
            cubic(pb, [0.54, 1.03, 0.12, 1.04, 0.0, 0.86]);
        }
        4 => {
            mv(pb, 0.42, 1.0);
            ln(pb, 0.42, 0.0);
            ln(pb, 0.0, 0.68);
            ln(pb, 0.58, 0.68);
        }
        5 => {
            mv(pb, 0.52, 0.0);
            ln(pb, 0.08, 0.0);
            ln(pb, 0.04, 0.45);
            cubic(pb, [0.3, 0.32, 0.58, 0.42, 0.56, 0.68]);
            cubic(pb, [0.55, 1.03, 0.12, 1.04, 0.0, 0.86]);
        }
        6 => {
            mv(pb, 0.48, 0.02);
            quad(pb, [0.02, 0.1, 0.0, 0.68]);
            oval(pb, 0.0, 0.38, 0.56, 1.0);
        }
        7 => {
            mv(pb, 0.0, 0.0);
            ln(pb, 0.56, 0.0);
            ln(pb, 0.18, 1.0);
        }
        8 => {
            oval(pb, 0.05, 0.0, 0.51, 0.46);
            oval(pb, 0.0, 0.46, 0.56, 1.0);
        }
        _ => {
            oval(pb, 0.0, 0.0, 0.56, 0.62);
            mv(pb, 0.56, 0.32);
            quad(pb, [0.54, 0.9, 0.08, 0.98]);
        }
    }
}

fn rect_of(a: P, b: P) -> Option<Rect> {
    Rect::from_ltrb(a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1))
}

fn draw_arrow(pm: &mut PixmapMut, a: P, b: P, width: f32, paint: &Paint, t: Transform) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len = dx.hypot(dy);
    if len < 1.0 {
        return;
    }
    let (ux, uy) = (dx / len, dy / len);
    let head = (width * 4.5).min(len);
    let half = head * 0.55;
    let base = (b.0 - ux * head, b.1 - uy * head);

    // Shaft stops inside the head so the round cap never pokes past the tip.
    let shaft_end = (b.0 - ux * head * 0.6, b.1 - uy * head * 0.6);
    if len > head {
        let mut pb = PathBuilder::new();
        pb.move_to(a.0, a.1);
        pb.line_to(shaft_end.0, shaft_end.1);
        if let Some(path) = pb.finish() {
            pm.stroke_path(&path, paint, &stroke(width), t, None);
        }
    }
    let mut pb = PathBuilder::new();
    pb.move_to(b.0, b.1);
    pb.line_to(base.0 - uy * half, base.1 + ux * half);
    pb.line_to(base.0 + uy * half, base.1 - ux * half);
    pb.close();
    if let Some(path) = pb.finish() {
        pm.fill_path(&path, paint, FillRule::Winding, t, None);
        // A thin stroke rounds the head's corners a little.
        pm.stroke_path(&path, paint, &stroke(width * 0.5), t, None);
    }
}

fn draw_selection(pm: &mut PixmapMut, sel: &Sel, s: f32) {
    let white = paint(color(255, 255, 255, 255));
    if let Some(r) = Rect::from_ltrb(sel.x0, sel.y0, sel.x1, sel.y1) {
        let st = Stroke { width: 1.5 * s, ..Default::default() };
        pm.stroke_path(&PathBuilder::from_rect(r), &white, &st, Transform::identity(), None);
    }
    let k = 3.5 * s;
    let (mx, my) = ((sel.x0 + sel.x1) / 2.0, (sel.y0 + sel.y1) / 2.0);
    for (x, y) in [
        (sel.x0, sel.y0), (mx, sel.y0), (sel.x1, sel.y0), (sel.x1, my),
        (sel.x1, sel.y1), (mx, sel.y1), (sel.x0, sel.y1), (sel.x0, my),
    ] {
        if let Some(r) = Rect::from_xywh(x - k, y - k, 2.0 * k, 2.0 * k) {
            pm.fill_rect(r, &white, Transform::identity(), None);
        }
    }
}

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let k = 0.4477 * r; // cubic approximation of a quarter circle
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - k, y, x + w, y + k, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - k, x + w - k, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + k, y + h, x, y + h - k, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + k, x + k, y, x + r, y);
    pb.close();
    pb.finish()
}

fn draw_toolbar(pm: &mut PixmapMut, ed: &Editor, sel: &Sel, v: &View) {
    let s = v.scale;
    let id = Transform::identity();
    let (x, y, btn, pad) = toolbar(sel, v);
    let tw = TOOLS.len() as f32 * (btn + pad) + pad;
    if let Some(bg) = rounded(x, y, tw, btn + 2.0 * pad, 7.0 * s) {
        pm.fill_path(&bg, &paint(color(32, 33, 36, 235)), FillRule::Winding, id, None);
    }
    let white = paint(color(255, 255, 255, 255));
    let line = stroke(2.2 * s);
    for (i, tool) in TOOLS.iter().enumerate() {
        let (bx, by, b) = toolbar_button(sel, v, i);
        if *tool == ed.tool {
            if let Some(hl) = rounded(bx, by, b, b, 5.0 * s) {
                pm.fill_path(&hl, &paint(red()), FillRule::Winding, id, None);
            }
        }
        let m = 9.0 * s;
        let (l, t, r, btm) = (bx + m, by + m, bx + b - m, by + b - m);
        match tool {
            crate::editor::Tool::Arrow => draw_arrow(pm, (l, btm), (r, t), 2.2 * s, &white, id),
            crate::editor::Tool::Ellipse => {
                if let Some(p) = Rect::from_ltrb(l, t, r, btm).and_then(PathBuilder::from_oval) {
                    pm.stroke_path(&p, &white, &line, id, None);
                }
            }
            crate::editor::Tool::Rect => {
                if let Some(rc) = Rect::from_ltrb(l, t + 2.0 * s, r, btm - 2.0 * s) {
                    pm.stroke_path(&PathBuilder::from_rect(rc), &white, &line, id, None);
                }
            }
            crate::editor::Tool::Pen => {
                let cy = (t + btm) / 2.0;
                let w = r - l;
                let mut pb = PathBuilder::new();
                pb.move_to(l, cy + 4.0 * s);
                pb.cubic_to(l + w * 0.3, cy - 10.0 * s, l + w * 0.6, cy + 10.0 * s, r, cy - 4.0 * s);
                if let Some(p) = pb.finish() {
                    pm.stroke_path(&p, &white, &line, id, None);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Tool, TOOLS};

    fn test_image(w: u32, h: u32) -> Image {
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                // BGRA: a soft blue/green gradient with a grid so the dimming is visible.
                let grid = if x % 50 == 0 || y % 50 == 0 { 60 } else { 0 };
                data.extend_from_slice(&[200 - grid, (x * 150 / w) as u8 + 60, (y * 120 / h) as u8 + 40, 255]);
            }
        }
        let dimmed = data.iter().enumerate().map(|(i, &c)| if i % 4 == 3 { 255 } else { c / 2 }).collect();
        Image { width: w, height: h, data, dimmed }
    }

    fn write_png(path: &str, w: u32, h: u32, bgra: &[u8]) {
        let rgb: Vec<u8> = bgra.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
        let file = std::fs::File::create(path).unwrap();
        let mut enc = png::Encoder::new(file, w, h);
        enc.set_color(png::ColorType::Rgb);
        enc.write_header().unwrap().write_image_data(&rgb).unwrap();
    }

    fn view(img: &Image, scale: f32) -> View {
        View { w: img.width as f32, h: img.height as f32, scale }
    }

    #[test]
    fn select_then_draw_flow() {
        let img = test_image(800, 500);
        let v = view(&img, 1.0);
        let mut ed = Editor::default();
        assert!(ed.tool == Tool::Arrow, "arrow is the default tool");

        // Drag out a selection.
        ed.press(0, (100.0, 100.0), &v);
        ed.motion(0, (500.0, 300.0), &v);
        ed.release(&v);
        let sel = ed.sel.expect("selection");
        assert_eq!(sel.pixels(800, 500), (100, 100, 500, 300));

        // Dragging inside the selection draws an arrow.
        ed.press(0, (150.0, 250.0), &v);
        ed.motion(0, (350.0, 150.0), &v);
        ed.release(&v);
        assert!(matches!(ed.shapes.as_slice(), [Shape::Arrow(..)]));

        // A tiny click does not leave a shape behind.
        ed.press(0, (200.0, 200.0), &v);
        ed.release(&v);
        assert_eq!(ed.shapes.len(), 1);

        // Clicking a toolbar button switches tool without drawing.
        let (bx, by, b) = toolbar_button(&sel, &v, 1);
        ed.press(0, (bx + b / 2.0, by + b / 2.0), &v);
        ed.release(&v);
        assert!(ed.tool == TOOLS[1] && ed.shapes.len() == 1);

        // Dragging the right edge resizes the selection.
        ed.press(0, (500.0, 200.0), &v);
        ed.motion(0, (600.0, 200.0), &v);
        ed.release(&v);
        assert_eq!(ed.sel.unwrap().pixels(800, 500), (100, 100, 600, 300));

        assert!(ed.undo());
        assert!(ed.shapes.is_empty());

        // A drag on another screen starts over there.
        assert_eq!(ed.press(1, (10.0, 10.0), &v), Some(0));
        assert_eq!(ed.screen, Some(1));
    }

    #[test]
    fn preview_and_export() {
        let img = test_image(800, 500);
        let mut ed = Editor::default();
        ed.screen = Some(0);
        ed.sel = Some(Sel { x0: 120.0, y0: 80.0, x1: 620.0, y1: 380.0 });
        ed.shapes = vec![
            Shape::Arrow((160.0, 340.0), (380.0, 150.0)),
            Shape::Ellipse((420.0, 110.0), (590.0, 220.0)),
            Shape::Rect((420.0, 250.0), (590.0, 350.0)),
            Shape::Pen((0..40).map(|i| (150.0 + i as f32 * 6.0, 120.0 + (i as f32 * 0.5).sin() * 15.0)).collect()),
        ];
        // Markers 1..10 in a row along the bottom of the selection.
        ed.shapes.extend((1..=10).map(|n| Shape::Marker((150.0 + (n - 1) as f32 * 48.0, 300.0), n)));
        let mut canvas = vec![0u8; img.data.len()];
        compose(&mut canvas, &img, &ed, 0, 1.0);
        write_png("/tmp/quicksnap-preview.png", img.width, img.height, &canvas);

        let (w, h, rgb) = export(&img, &ed, ed.sel.unwrap().pixels(800, 500), 1.0);
        assert_eq!((w, h, rgb.len()), (500, 300, 500 * 300 * 3));
        // The arrow tip area is red in the export.
        let tip = ((150 - 80 + 6) * 500 + (380 - 120 - 6)) as usize * 3;
        assert!(rgb[tip] > 200 && rgb[tip + 1] < 80 && rgb[tip + 2] < 80, "arrow is red: {:?}", &rgb[tip..tip + 3]);
        // Marker 1 sits at (150, 300): white digit stroke in the middle, red disc beside it.
        let at = |x: u32, y: u32| ((y - 80) * 500 + (x - 120)) as usize * 3;
        let mid = at(151, 300);
        assert!(rgb[mid..mid + 3].iter().all(|&c| c > 200), "digit is white: {:?}", &rgb[mid..mid + 3]);
        let disc = at(140, 306);
        assert!(rgb[disc] > 200 && rgb[disc + 1] < 80, "disc is red: {:?}", &rgb[disc..disc + 3]);
    }

    #[test]
    fn number_keys_place_markers() {
        let img = test_image(800, 500);
        let v = view(&img, 1.0);
        let mut ed = Editor::default();
        // No selection yet: nothing to annotate.
        assert!(!ed.place_marker(0, (50.0, 50.0), 1));
        ed.press(0, (100.0, 100.0), &v);
        ed.motion(0, (500.0, 300.0), &v);
        ed.release(&v);
        // Not on another screen, but anywhere on this one.
        assert!(!ed.place_marker(1, (50.0, 50.0), 1));
        assert!(ed.place_marker(0, (200.0, 200.0), 1));
        assert!(ed.place_marker(0, (300.0, 200.0), 10));
        assert!(matches!(ed.shapes.as_slice(), [Shape::Marker(_, 1), Shape::Marker(_, 10)]));
        assert!(ed.undo());
        assert_eq!(ed.shapes.len(), 1);
    }
}
