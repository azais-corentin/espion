//! The screen target end to end without a board or a browser: the fake camera films a
//! simulated screen (`screen_target::sim`) that shows what the tests' page draws, and the viewer's
//! [`Target`] calibrates it and checks the blobs. The simulated screen's model is the known
//! answer.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use espion_proto::cam::{Encoding, StreamSettings};
use poc_viewer::{
    fake::{self, FakeCamera, FakeConfig},
    net::{BlobFrame, Client},
    screen_target::{
        Target,
        link::{Scene, SceneKind},
        sim::{self, SimScreen, TestPage},
    },
};

struct Rig {
    _fake: FakeCamera,
    client: Client,
    target: Target,
    _page: TestPage,
    sim: Arc<SimScreen>,
    seq: u64,
    blobs: Vec<BlobFrame>,
}

impl Rig {
    fn start() -> Self {
        let sim = SimScreen::new(sim::model());
        let target = Target::new(|| {});
        target.hub().set_tap(sim.clone());
        let fake = FakeCamera::spawn(FakeConfig {
            // RAW8 frames (no decoding needed), 60 a second; detection on every frame.
            stream: StreamSettings {
                encoding: Encoding::Raw8,
                max_fps: 60,
                ..fake::default_stream()
            },
            screen: Some(sim.clone()),
            ..FakeConfig::default()
        })
        .expect("fake camera starts");
        let client = Client::connect(fake.addr(), || {}).expect("client connects");
        let page = TestPage::start(target.hub());
        Self {
            _fake: fake,
            client,
            target,
            _page: page,
            sim,
            seq: 0,
            blobs: Vec::new(),
        }
    }

    /// Runs the target for `secs` or until `done`.
    fn run(&mut self, secs: f64, done: impl Fn(&Target) -> bool) {
        let end = Instant::now() + Duration::from_secs_f64(secs);
        while Instant::now() < end && !done(&self.target) {
            let stats = self.client.stats();
            for c in self.target.update(Instant::now(), stats.state.as_ref()) {
                self.client.send(c);
            }
            if let Some(f) = self.client.wait_frame(Duration::from_millis(2)) {
                self.target.frame(&f);
                self.client.recycle(f.data);
            }
            self.seq = self.client.blob_frames_since(self.seq, &mut self.blobs);
            for b in self.blobs.drain(..) {
                self.target.blobs(&b);
            }
        }
    }
}

#[test]
fn calibration_finds_the_screen_and_the_check_its_markers() {
    let mut rig = Rig::start();
    rig.run(10.0, |t| {
        t.page().is_some_and(|p| p.viewport.is_some()) && t.page().unwrap().rtt_us.is_some()
    });
    rig.target.calibrate().expect("calibration starts");
    rig.run(60.0, |t| t.calibrating().is_none());
    assert_eq!(rig.target.calib_error, None);
    let fit = rig.target.fit().expect("a fit").clone();
    println!("{fit:?}");
    // Automatic exposure is back on after the calibration.
    rig.run(1.0, |_| false);
    assert!(rig.client.stats().state.unwrap().camera.auto_exposure);

    let truth = rig.sim.model();
    assert!(fit.dots > 40, "{} dots", fit.dots);
    assert!(fit.rms < 0.1, "dots RMS {}", fit.rms);
    assert!(fit.visible > 0.99, "visible {}", fit.visible);
    // The finest stripes blur to grey: the coarser bits carry the Gray code.
    assert!(fit.bits[0] >= 1 && fit.bits[1] >= 1, "bits {:?}", fit.bits);
    assert!(fit.decoded > 0.5, "decoded {}", fit.decoded);
    let mut worst = 0f64;
    for v in (0..800).step_by(50) {
        for u in (0..1280).step_by(50) {
            let (x, y) = truth.to_screen(f64::from(u), f64::from(v));
            let (fu, fv) = fit.model.to_sensor(x, y);
            worst = worst.max((fu - f64::from(u)).hypot(fv - f64::from(v)));
        }
    }
    assert!(worst < 0.25, "the fit is off by up to {worst} px");

    // The vignette follows the simulated lens's falloff (up to a constant: the exposure).
    let vg = fit.vignette.expect("a vignette");
    let at_centre = vg.log_flux(truth.cx, truth.cy);
    let mut worst = 0f64;
    for v in (100..700).step_by(50) {
        for u in (100..1180).step_by(50) {
            let (u, v) = (f64::from(u), f64::from(v));
            let fitted = vg.log_flux(u, v) - at_centre;
            worst = worst.max((fitted - sim::falloff(truth, u, v).ln()).abs());
        }
    }
    assert!(
        worst < 0.1,
        "the vignette is off by up to {worst} (ln flux)"
    );

    // A static row of markers: the vignetting spreads their brightness, the compensation
    // evens it out.
    let mut spread = |compensate| {
        let scene = Scene {
            kind: SceneKind::Static,
            count: 5,
            size: 6.0,
            speed: 0.0,
            seed: 1,
        };
        rig.target
            .start_scene(scene, compensate)
            .expect("the scene starts");
        rig.run(3.0, |_| false);
        let r = rig.target.check().expect("a check").report().clone();
        r.flux_cv.expect("a spread")
    };
    let (raw, even) = (spread(false), spread(true));
    assert!(raw > 0.1, "uncompensated spread {raw}");
    assert!(
        even < 0.05 && even < raw / 4.0,
        "compensated spread {even} (uncompensated {raw})"
    );

    rig.target
        .start_scene(
            Scene {
                kind: SceneKind::Lissajous,
                count: 3,
                size: 6.0,
                speed: 300.0,
                seed: 1,
            },
            true,
        )
        .expect("the scene starts");
    rig.run(8.0, |_| false);
    let r = rig.target.check().expect("a check").report().clone();
    println!("{r:#?}");
    let w = r.window;
    assert!(w.frames > 300, "{w:?}");
    assert!(w.exact as f64 >= 0.98 * w.frames as f64, "{w:?}");
    let e = r.errors.expect("errors");
    assert!(e.rms < 0.3, "{e:?}");
    // The simulated screen lights 40 ms after the page's report, less the exposure and the
    // readout the viewer counts back from the capture, within one page frame (60 Hz).
    let d = r.delay_us.expect("a delay") / 1e3;
    assert!((10.0..=45.0).contains(&d), "delay {d} ms");
}
