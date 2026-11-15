//! Minimal 8-bit grayscale image type. The IR camera only produces GREY frames,
//! so we avoid pulling a full image-processing stack into the root daemon.

#[derive(Clone, Debug)]
pub struct Gray {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

/// Axis-aligned rectangle in pixel coordinates (x, y, width, height).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    pub fn area(&self) -> f32 {
        self.w.max(0.0) * self.h.max(0.0)
    }

    pub fn iou(&self, o: &Rect) -> f32 {
        let x0 = self.x.max(o.x);
        let y0 = self.y.max(o.y);
        let x1 = (self.x + self.w).min(o.x + o.w);
        let y1 = (self.y + self.h).min(o.y + o.h);
        let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
        let union = self.area() + o.area() - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }

    /// Integer pixel bounds clipped to an image of size `w`×`h`.
    pub fn clip(&self, w: usize, h: usize) -> (usize, usize, usize, usize) {
        let x0 = self.x.floor().clamp(0.0, w as f32) as usize;
        let y0 = self.y.floor().clamp(0.0, h as f32) as usize;
        let x1 = (self.x + self.w).ceil().clamp(0.0, w as f32) as usize;
        let y1 = (self.y + self.h).ceil().clamp(0.0, h as f32) as usize;
        (x0, y0, x1, y1)
    }
}

impl Gray {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![0; w * h] }
    }

    pub fn from_raw(w: usize, h: usize, px: Vec<u8>) -> Self {
        assert_eq!(px.len(), w * h, "buffer size mismatch");
        Self { w, h, px }
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> u8 {
        self.px[y * self.w + x]
    }

    pub fn mean(&self) -> f32 {
        if self.px.is_empty() {
            return 0.0;
        }
        // Sub-sample: brightness classification runs on every frame.
        let step = 7;
        let (mut s, mut n) = (0u64, 0u64);
        for v in self.px.iter().step_by(step) {
            s += *v as u64;
            n += 1;
        }
        s as f32 / n as f32
    }

    pub fn mean_in(&self, r: &Rect) -> f32 {
        let (x0, y0, x1, y1) = r.clip(self.w, self.h);
        let (mut s, mut n) = (0u64, 0u64);
        for y in y0..y1 {
            for x in x0..x1 {
                s += self.at(x, y) as u64;
                n += 1;
            }
        }
        if n == 0 { 0.0 } else { s as f32 / n as f32 }
    }

    /// Bilinear sample with edge clamping.
    #[inline]
    pub fn sample(&self, x: f32, y: f32) -> f32 {
        let xf = x.clamp(0.0, (self.w - 1) as f32);
        let yf = y.clamp(0.0, (self.h - 1) as f32);
        let x0 = xf.floor() as usize;
        let y0 = yf.floor() as usize;
        let x1 = (x0 + 1).min(self.w - 1);
        let y1 = (y0 + 1).min(self.h - 1);
        let fx = xf - x0 as f32;
        let fy = yf - y0 as f32;
        let a = self.at(x0, y0) as f32;
        let b = self.at(x1, y0) as f32;
        let c = self.at(x0, y1) as f32;
        let d = self.at(x1, y1) as f32;
        let top = a + (b - a) * fx;
        let bot = c + (d - c) * fx;
        top + (bot - top) * fy
    }

    pub fn resize(&self, w: usize, h: usize) -> Gray {
        let mut out = Gray::new(w, h);
        let sx = self.w as f32 / w as f32;
        let sy = self.h as f32 / h as f32;
        for y in 0..h {
            let fy = (y as f32 + 0.5) * sy - 0.5;
            for x in 0..w {
                let fx = (x as f32 + 0.5) * sx - 0.5;
                out.px[y * w + x] = self.sample(fx, fy).round() as u8;
            }
        }
        out
    }

    /// Saturating per-pixel difference `self - other` (lit minus unlit frame).
    pub fn sub_sat(&self, other: &Gray) -> Gray {
        assert_eq!((self.w, self.h), (other.w, other.h));
        let px = self.px.iter().zip(&other.px).map(|(a, b)| a.saturating_sub(*b)).collect();
        Gray { w: self.w, h: self.h, px }
    }
}

/// 8-bit colour image, BGR byte order (what MiniFASNet was trained on).
/// Only colour webcams produce one.
#[derive(Clone, Debug)]
pub struct Bgr {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Bgr {
    pub fn from_raw(w: usize, h: usize, px: Vec<u8>) -> Self {
        assert_eq!(px.len(), w * h * 3, "buffer size mismatch");
        Self { w, h, px }
    }

    pub fn resize(&self, w: usize, h: usize) -> Bgr {
        self.crop_resize((0, 0, self.w - 1, self.h - 1), w, h)
    }

    /// Bilinear resize of the pixel box `x0..=x1, y0..=y1` to `w`×`h`
    /// (pixel-centre sampling, like OpenCV's INTER_LINEAR).
    pub fn crop_resize(&self, (x0, y0, x1, y1): (usize, usize, usize, usize), w: usize, h: usize) -> Bgr {
        let x1 = x1.min(self.w - 1).max(x0);
        let y1 = y1.min(self.h - 1).max(y0);
        let sx = (x1 - x0 + 1) as f32 / w as f32;
        let sy = (y1 - y0 + 1) as f32 / h as f32;
        let mut px = Vec::with_capacity(w * h * 3);
        for y in 0..h {
            let fy = ((y as f32 + 0.5) * sy - 0.5).clamp(0.0, (y1 - y0) as f32) + y0 as f32;
            let (ya, fy) = (fy.floor() as usize, fy.fract());
            let yb = (ya + 1).min(y1);
            for x in 0..w {
                let fx = ((x as f32 + 0.5) * sx - 0.5).clamp(0.0, (x1 - x0) as f32) + x0 as f32;
                let (xa, fx) = (fx.floor() as usize, fx.fract());
                let xb = (xa + 1).min(x1);
                for c in 0..3 {
                    let at = |x: usize, y: usize| self.px[(y * self.w + x) * 3 + c] as f32;
                    let top = at(xa, ya) + (at(xb, ya) - at(xa, ya)) * fx;
                    let bot = at(xa, yb) + (at(xb, yb) - at(xa, yb)) * fx;
                    px.push((top + (bot - top) * fy).round() as u8);
                }
            }
        }
        Bgr { w, h, px }
    }
}

/// One YUYV (YUY2) row to BGR, BT.601 limited range as webcams send it.
pub fn yuyv_to_bgr(row: &[u8], out: &mut Vec<u8>) {
    for q in row.as_chunks::<4>().0 {
        let (u, v) = (q[1] as i32 - 128, q[3] as i32 - 128);
        for y in [q[0], q[2]] {
            let c = 298 * (y as i32 - 16);
            let px = |v: i32| ((v + 128) >> 8).clamp(0, 255) as u8;
            out.extend([px(c + 516 * u), px(c - 100 * u - 208 * v), px(c + 409 * v)]);
        }
    }
}
