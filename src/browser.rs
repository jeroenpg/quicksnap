//! Finds the page area of a browser window in a screenshot, so right-click can skip the tab
//! strip, toolbar and bookmarks bar. KWin only knows the outer frame, so this reads pixels:
//! browser chrome is drawn in a few flat toolbar colours, and the page starts at the first
//! rows where almost nothing has those colours.

const BROWSERS: &[&str] =
    &["brave", "chrom", "firefox", "vivaldi", "msedge", "opera", "librewolf", "floorp", "zen"];

pub fn is_browser(class: &str) -> bool {
    let class = class.to_ascii_lowercase();
    BROWSERS.iter().any(|b| class.contains(b))
}

type Px = [u8; 3];

/// Pixel rectangle (x0, y0, x1, y1), end-exclusive.
pub type Rect = (u32, u32, u32, u32);

/// `img` is BGRA with row length `width`; `win` is the window in image pixels and `s` the
/// image's pixels per logical pixel. Returns the page area, or None when no plain toolbar
/// is recognised (fullscreen video, unusual themes, window top off-screen...).
pub fn content_rect(img: &[u8], width: u32, win: Rect, s: f32) -> Option<Rect> {
    let (x0, y0, x1, y1) = win;
    let lp = |v: f32| ((v * s).round() as u32).max(1);
    if x1.saturating_sub(x0) < lp(300.0) || y1.saturating_sub(y0) < lp(200.0) {
        return None;
    }
    let at = |x: u32, y: u32| -> Px {
        let i = ((y * width + x) * 4) as usize;
        [img[i], img[i + 1], img[i + 2]]
    };
    let diff = |a: Px, b: Px| a.iter().zip(b).map(|(a, b)| a.abs_diff(b) as u32).sum::<u32>();
    // Toolbars are perfectly flat, so chrome colour must match almost exactly: dark pages are
    // often only a few shades away from a dark toolbar.
    let close = |a: Px, b: Px| diff(a, b) <= 8;

    // Chrome colours: the window's left edge across the top (tab strip, toolbar, title bar).
    let mut chrome: Vec<Px> = Vec::new();
    for y in (y0 + lp(2.0)..y0 + lp(60.0)).step_by(lp(2.0) as usize) {
        let c = at(x0 + lp(2.0), y);
        if !chrome.iter().any(|&k| close(k, c)) {
            chrome.push(c);
        }
    }
    if chrome.len() > 8 {
        return None; // too varied for a toolbar
    }
    let is_chrome = |c: Px| chrome.iter().any(|&k| close(k, c));

    // Per-row measures over the width, skipping side margins.
    let (sx0, sx1) = (x0 + lp(12.0), x1 - lp(12.0));
    let step = ((sx1 - sx0) / 200).max(1) as usize;
    let share = |f: &dyn Fn(u32) -> bool| {
        let xs = (sx0..sx1).step_by(step);
        let total = xs.len() as f32;
        (sx0..sx1).step_by(step).filter(|&x| f(x)).count() as f32 / total
    };
    let chrome_share = |y: u32| share(&|x| is_chrome(at(x, y)));
    // A horizontal edge across (nearly) the whole width between rows y-1 and y.
    let edge = |y: u32| share(&|x| diff(at(x, y), at(x, y - 1)) > 12) > 0.6;

    // The page starts at a full-width edge with little chrome colour below it. The edge under
    // the tab strip fails that test (flat toolbar rows follow), as does the address field,
    // which only covers part of its rows.
    let run = lp(6.0);
    let limit = (y0 + lp(220.0)).min(y1 - run);
    let below = |y: u32| (y..y + run).map(chrome_share).sum::<f32>() / run as f32;
    let top = (y0 + lp(20.0)..limit).find(|&y| edge(y) && below(y) < 0.3)?;
    // A 1px separator line gives two edges in a row; the page starts after it.
    let top = if edge(top + 1) { top + 1 } else { top };

    // Some browsers (Brave) frame the page with a thin margin in toolbar colour, plus a faint
    // border and shading; trim it. The margin starts in chrome colour at the window edge and
    // stays loosely that one colour until the clear step into the page.
    let cap = lp(24.0);
    let probe_y = (top + lp(16.0)).min(y1 - 1);
    let probe_x = (x0 + x1) / 2;
    let inset = |pts: &mut dyn Iterator<Item = (u32, u32)>| {
        let mut pts = pts.take(cap as usize).map(|(x, y)| at(x, y)).peekable();
        let near_chrome = |c: Px| chrome.iter().any(|&k| diff(k, c) <= 24);
        let Some(&margin) = pts.peek().filter(|&&c| near_chrome(c)) else { return 0 };
        let n = pts.take_while(|&c| diff(c, margin) <= 36).count() as u32;
        if n == cap { 0 } else { n } // ran into the cap: that is page, not margin
    };
    let left = inset(&mut (x0..x1).map(|x| (x, probe_y)));
    let right = inset(&mut (x0..x1).rev().map(|x| (x, probe_y)));
    let bottom = inset(&mut (top..y1).rev().map(|y| (probe_x, y)));
    Some((x0 + left, top, x1 - right, y1 - bottom))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A Brave-like window: tab strip, toolbar with address field, bookmarks bar, a page with
    /// a dark header, and a 5px toolbar-coloured frame around the page.
    fn fake_browser(w: u32, h: u32) -> Vec<u8> {
        let (strip, bar, field, page, header) =
            ([40, 36, 36], [61, 60, 60], [30, 30, 30], [245, 245, 245], [120, 20, 20]);
        let mut img = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                let c = if y < 40 {
                    if (200..400).contains(&x) { bar } else { strip }
                } else if y < 115 {
                    if (44..76).contains(&y) && (420..1500).contains(&x) { field } else { bar }
                } else if x < 5 || x >= w - 5 || y >= h - 5 {
                    bar
                } else if y < 175 {
                    header
                } else {
                    page
                };
                img.extend_from_slice(&[c[0], c[1], c[2], 255]);
            }
        }
        img
    }

    #[test]
    fn finds_page_below_toolbars() {
        let (w, h) = (1920, 1036);
        let img = fake_browser(w, h);
        assert_eq!(content_rect(&img, w, (0, 0, w, h), 1.0), Some((5, 115, w - 5, h - 5)));
    }

    #[test]
    fn plain_window_is_left_alone() {
        // One flat colour everywhere: no page edge to find.
        let (w, h) = (800, 600);
        let img = vec![200u8; (w * h * 4) as usize];
        assert_eq!(content_rect(&img, w, (0, 0, w, h), 1.0), None);
    }

    #[test]
    fn browser_classes() {
        assert!(is_browser("brave-browser") && is_browser("firefox") && is_browser("Chromium"));
        assert!(!is_browser("dev.zed.Zed") && !is_browser("tilix"));
    }

    /// `QUICKSNAP_SAMPLE=window.rgba:W:H cargo test real_sample -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn real_sample() {
        let spec = std::env::var("QUICKSNAP_SAMPLE").unwrap();
        let parts: Vec<&str> = spec.split(':').collect();
        let (w, h): (u32, u32) = (parts[1].parse().unwrap(), parts[2].parse().unwrap());
        let rgba = std::fs::read(parts[0]).unwrap();
        let bgra: Vec<u8> = rgba.chunks_exact(4).flat_map(|p| [p[2], p[1], p[0], 255]).collect();
        println!("content: {:?}", content_rect(&bgra, w, (0, 0, w, h), 1.0));
    }
}
