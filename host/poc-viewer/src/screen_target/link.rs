//! The messages between the viewer and the screen target page: JSON text over the page's
//! WebSocket (`/ws`), one message per WebSocket message, tagged by `type`. Times on the page
//! are its `performance.now()` in ms; times of the viewer are µs since the target's epoch.
//! Screen positions and sizes are in the page's CSS pixels, origin at the top left of its
//! viewport.

use serde::{Deserialize, Serialize};

use super::model::Model;

/// Viewer to page.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ToPage {
    /// Clock probe: the page answers [`FromPage::Pong`] at once.
    Ping { t: f64 },
    /// Show `content` from the page's next frame on; the page answers [`FromPage::Shown`]
    /// with the time of the first frame that shows it.
    Show { id: u32, content: Content },
}

/// What the page shows. Everything is white on black.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Content {
    Black,
    White,
    /// One bit of the reflected binary (Gray) code of the screen pixel's column (`X`) or row
    /// (`Y`): the pixel at `(x, y)` is white where bit `bit` (0 the least significant) of
    /// `g = c ^ (c >> 1)` is 1, `c = floor(x)` (or `floor(y)`); with `inverse`, where it is 0.
    Gray {
        axis: Axis,
        bit: u8,
        inverse: bool,
    },
    /// Gaussian dots at fixed places.
    Dots {
        dots: Vec<Dot>,
    },
    /// An animated scene, placed through `model` on a sensor window of `sensor` pixels; the
    /// page reports every frame it draws ([`FromPage::Drawn`]).
    Scene {
        scene: Scene,
        model: Model,
        sensor: [u16; 2],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Axis {
    X,
    Y,
}

/// A Gaussian spot on the screen: white at its centre, `exp(-r²/2σ²)` in linear light.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Dot {
    pub x: f64,
    pub y: f64,
    /// Standard deviation, screen pixels.
    pub sigma: f64,
}

/// An animated scene, in sensor pixels: the page converts through the model.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Scene {
    pub kind: SceneKind,
    /// Markers (static, Lissajous).
    pub count: u8,
    /// Marker size: the full width at half maximum on the sensor, pixels (all but the random
    /// walk, which varies it).
    pub size: f64,
    /// Speed, sensor pixels per second (Lissajous: the fastest; the sweep and the random walk
    /// set their own).
    pub speed: f64,
    /// Seed of the random walk.
    pub seed: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneKind {
    /// `count` markers held still on a grid over the visible area.
    Static,
    /// `count` markers on Lissajous paths, each in its own column of the visible area.
    Lissajous,
    /// One marker going back and forth across the visible area, its speed ramped from
    /// [`SWEEP_SPEEDS`]`.0` to `.1` (log scale) over [`SWEEP_S`] and again.
    Sweep,
    /// 2 to 10 markers (the count drifting), on smoothed random walks with smoothly varying
    /// velocities and sizes (2 to 20 pixels): one that leaves the visible area is replaced by
    /// a new one entering at another place of its edge.
    RandomWalk,
}

/// The speed sweep's range, sensor pixels per second.
pub const SWEEP_SPEEDS: (f64, f64) = (50.0, 6000.0);
/// The speed sweep's period, seconds.
pub const SWEEP_S: f64 = 30.0;
/// The random walk's marker count and sizes.
pub const WALK_COUNT: (u8, u8) = (2, 10);
pub const WALK_SIZE: (f64, f64) = (2.0, 20.0);

impl SceneKind {
    pub const ALL: [Self; 4] = [Self::Static, Self::Lissajous, Self::Sweep, Self::RandomWalk];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Static => "Static",
            Self::Lissajous => "Lissajous",
            Self::Sweep => "Speed sweep",
            Self::RandomWalk => "Random walk",
        }
    }
}

/// Page to viewer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FromPage {
    /// At connection and whenever the viewport changes (entering full screen does).
    Hello {
        /// The viewport, CSS pixels.
        width: f64,
        height: f64,
        /// Device pixels per CSS pixel.
        dpr: f64,
        /// The browser's user agent.
        agent: String,
    },
    /// The answer to [`ToPage::Ping`]: its `t`, and the page's clock when it answered.
    Pong { t: f64, now: f64 },
    /// The first frame showing [`ToPage::Show`] `id` was drawn at `now`.
    Shown { id: u32, now: f64 },
    /// A frame of a scene, drawn at `now` (the animation frame's time): its markers.
    Drawn { now: f64, markers: Vec<Drawn> },
    /// The page's frame rate, measured over its last second, Hz.
    Refresh { hz: f64 },
}

/// One marker of a drawn frame.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Drawn {
    /// The marker's identity while it lives (a replaced marker gets a new one).
    pub id: u32,
    pub x: f64,
    pub y: f64,
    /// Standard deviation, screen pixels.
    pub sigma: f64,
}

/// FWHM of a Gaussian over its standard deviation.
pub const FWHM_PER_SIGMA: f64 = 2.354_820_045_030_949;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_are_tagged_json() {
        let m = ToPage::Show {
            id: 3,
            content: Content::Gray {
                axis: Axis::X,
                bit: 4,
                inverse: true,
            },
        };
        assert_eq!(
            serde_json::to_string(&m).unwrap(),
            r#"{"type":"show","id":3,"content":{"kind":"gray","axis":"X","bit":4,"inverse":true}}"#
        );
        let p: FromPage = serde_json::from_str(
            r#"{"type":"drawn","now":12.5,"markers":[{"id":1,"x":2,"y":3,"sigma":0.5}]}"#,
        )
        .unwrap();
        assert_eq!(
            p,
            FromPage::Drawn {
                now: 12.5,
                markers: vec![Drawn {
                    id: 1,
                    x: 2.0,
                    y: 3.0,
                    sigma: 0.5
                }]
            }
        );
    }
}
