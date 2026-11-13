//! 5-point similarity alignment to the canonical ArcFace 112×112 template,
//! shared by SFace, ArcFace, AdaFace and EdgeFace.

use crate::detect::Face;
use crate::image::Gray;

pub const ARCFACE_112: [[f32; 2]; 5] = [
    [38.2946, 51.6963],
    [73.5318, 51.5014],
    [56.0252, 71.7366],
    [41.5493, 92.3655],
    [70.7299, 92.2041],
];

/// Similarity transform `dst = [a -b; b a]·src + t`.
#[derive(Clone, Copy, Debug)]
pub struct Similarity {
    pub a: f32,
    pub b: f32,
    pub tx: f32,
    pub ty: f32,
}

impl Similarity {
    /// Least-squares fit (no reflection) mapping `src` onto `dst`.
    pub fn estimate(src: &[[f32; 2]], dst: &[[f32; 2]]) -> Self {
        let n = src.len() as f32;
        let (mut msx, mut msy, mut mdx, mut mdy) = (0.0, 0.0, 0.0, 0.0);
        for (s, d) in src.iter().zip(dst) {
            msx += s[0];
            msy += s[1];
            mdx += d[0];
            mdy += d[1];
        }
        msx /= n;
        msy /= n;
        mdx /= n;
        mdy /= n;
        let (mut num_a, mut num_b, mut den) = (0.0, 0.0, 0.0);
        for (s, d) in src.iter().zip(dst) {
            let (sx, sy) = (s[0] - msx, s[1] - msy);
            let (dx, dy) = (d[0] - mdx, d[1] - mdy);
            num_a += sx * dx + sy * dy;
            num_b += sx * dy - sy * dx;
            den += sx * sx + sy * sy;
        }
        let a = num_a / den;
        let b = num_b / den;
        Self { a, b, tx: mdx - (a * msx - b * msy), ty: mdy - (b * msx + a * msy) }
    }

    /// Maps a destination point back to source coordinates.
    #[inline]
    pub fn inverse_apply(&self, x: f32, y: f32) -> (f32, f32) {
        let s2 = self.a * self.a + self.b * self.b;
        let (dx, dy) = (x - self.tx, y - self.ty);
        ((self.a * dx + self.b * dy) / s2, (-self.b * dx + self.a * dy) / s2)
    }
}

/// Crops and aligns `face` from `img` into a `size`×`size` patch.
pub fn align(img: &Gray, face: &Face, size: usize) -> Gray {
    let k = size as f32 / 112.0;
    let dst: Vec<[f32; 2]> = ARCFACE_112.iter().map(|p| [p[0] * k, p[1] * k]).collect();
    let m = Similarity::estimate(&face.kps, &dst);
    let mut out = Gray::new(size, size);
    for y in 0..size {
        for x in 0..size {
            let (sx, sy) = m.inverse_apply(x as f32, y as f32);
            out.px[y * size + x] = img.sample(sx, sy).round() as u8;
        }
    }
    out
}
