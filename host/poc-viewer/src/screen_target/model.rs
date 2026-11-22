//! Where a point of the screen lands on the sensor: a homography (the screen is a plane), then
//! the lens's radial distortion about the sensor's centre. The page has the same model in
//! JavaScript (`assets/screen-target/index.html`, `Model`) to place markers given in sensor pixels;
//! what it reports is in screen pixels, so only this side decides where a marker landed.

use serde::{Deserialize, Serialize};

/// Screen (CSS pixels of the page) to sensor (pixels, centres at integer coordinates).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Model {
    /// Row-major 3x3 homography, screen to undistorted sensor pixels, `h[8]` = 1.
    pub h: [f64; 9],
    /// Radial distortion: a point at radius `r` from the centre goes to `r (1 + k1 q² + k2 q⁴)`,
    /// `q = r / norm`.
    pub k1: f64,
    pub k2: f64,
    /// Centre of the distortion, sensor pixels.
    pub cx: f64,
    pub cy: f64,
    /// The unit of `q`, sensor pixels: half the sensor window's diagonal.
    pub norm: f64,
}

impl Model {
    /// A model without distortion, for a sensor window `width` x `height`.
    pub fn homography(h: [f64; 9], width: u16, height: u16) -> Self {
        let (w, hh) = (f64::from(width), f64::from(height));
        Self {
            h: normalised(h),
            k1: 0.0,
            k2: 0.0,
            cx: (w - 1.0) / 2.0,
            cy: (hh - 1.0) / 2.0,
            norm: (w * w + hh * hh).sqrt() / 2.0,
        }
    }

    /// The undistorted sensor point of screen point `(x, y)`.
    pub fn project(&self, x: f64, y: f64) -> (f64, f64) {
        apply(&self.h, x, y)
    }

    /// The sensor point of screen point `(x, y)`.
    pub fn to_sensor(&self, x: f64, y: f64) -> (f64, f64) {
        let (u, v) = self.project(x, y);
        self.distort(u, v)
    }

    /// The screen point that lands on sensor point `(u, v)`.
    pub fn to_screen(&self, u: f64, v: f64) -> (f64, f64) {
        let (u, v) = self.undistort(u, v);
        apply(&invert(&self.h), u, v)
    }

    fn factor(&self, du: f64, dv: f64) -> f64 {
        let q2 = (du * du + dv * dv) / (self.norm * self.norm);
        1.0 + self.k1 * q2 + self.k2 * q2 * q2
    }

    /// Undistorted to distorted sensor pixels.
    pub fn distort(&self, u: f64, v: f64) -> (f64, f64) {
        let (du, dv) = (u - self.cx, v - self.cy);
        let f = self.factor(du, dv);
        (self.cx + du * f, self.cy + dv * f)
    }

    /// Distorted to undistorted sensor pixels (fixed-point iteration; the distortion of a
    /// camera lens is mild enough for it to converge).
    pub fn undistort(&self, u: f64, v: f64) -> (f64, f64) {
        let (du, dv) = (u - self.cx, v - self.cy);
        let (mut x, mut y) = (du, dv);
        for _ in 0..20 {
            let f = self.factor(x, y);
            (x, y) = (du / f, dv / f);
        }
        (self.cx + x, self.cy + y)
    }

    /// Sensor pixels per screen pixel around screen point `(x, y)` (the square root of the
    /// area ratio).
    pub fn scale_at(&self, x: f64, y: f64) -> f64 {
        let d = 0.5;
        let (ax, ay) = self.to_sensor(x - d, y);
        let (bx, by) = self.to_sensor(x + d, y);
        let (cx, cy) = self.to_sensor(x, y - d);
        let (ex, ey) = self.to_sensor(x, y + d);
        let det = (bx - ax) * (ey - cy) - (by - ay) * (ex - cx);
        (det.abs() / (4.0 * d * d)).sqrt()
    }

    /// The screen's rotation on the sensor around screen point `(x, y)`, degrees: the angle of
    /// the screen's x axis on the sensor.
    pub fn rotation_at(&self, x: f64, y: f64) -> f64 {
        let (ax, ay) = self.to_sensor(x - 0.5, y);
        let (bx, by) = self.to_sensor(x + 0.5, y);
        (by - ay).atan2(bx - ax).to_degrees()
    }
}

/// `h` scaled so that `h[8]` = 1.
pub fn normalised(h: [f64; 9]) -> [f64; 9] {
    h.map(|v| v / h[8])
}

/// The homography `h` applied to `(x, y)`.
pub fn apply(h: &[f64; 9], x: f64, y: f64) -> (f64, f64) {
    let w = h[6] * x + h[7] * y + h[8];
    (
        (h[0] * x + h[1] * y + h[2]) / w,
        (h[3] * x + h[4] * y + h[5]) / w,
    )
}

/// The inverse of a 3x3 matrix (adjugate over determinant), normalised.
pub fn invert(h: &[f64; 9]) -> [f64; 9] {
    let [a, b, c, d, e, f, g, i, j] = *h;
    let adj = [
        e * j - f * i,
        c * i - b * j,
        b * f - c * e,
        f * g - d * j,
        a * j - c * g,
        c * d - a * f,
        d * i - e * g,
        b * g - a * i,
        a * e - b * d,
    ];
    normalised(adj)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// The simulated screen's model.
    pub fn sample() -> Model {
        crate::screen_target::sim::model()
    }

    #[test]
    fn to_screen_inverts_to_sensor() {
        let m = sample();
        for (x, y) in [(300.0, 300.0), (420.0, 333.3), (500.0, 450.0)] {
            let (u, v) = m.to_sensor(x, y);
            let (bx, by) = m.to_screen(u, v);
            assert!(
                (bx - x).abs() < 1e-6 && (by - y).abs() < 1e-6,
                "{x},{y} -> {bx},{by}"
            );
        }
    }

    #[test]
    fn scale_and_rotation_are_those_of_an_affine_map() {
        let (s, a) = (4.5, 181.5f64.to_radians());
        let (c, n) = (s * a.cos(), s * a.sin());
        let m = Model::homography([c, -n, 2600.0, n, c, 2100.0, 0.0, 0.0, 1.0], 1280, 800);
        let r = m.rotation_at(300.0, 300.0);
        assert!((m.scale_at(300.0, 300.0) - 4.5).abs() < 1e-9);
        assert!((r + 178.5).abs() < 1e-9, "{r}");
    }
}
