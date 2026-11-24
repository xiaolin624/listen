//! The status capsule's state model and visual specification (状态胶囊),
//! independent of Windows.
//!
//! The capsule never shows text, numbers or live transcription: colour, shape and
//! motion are the only status a user gets. The whole visual specification is
//! [`CapsuleState::style`] — one exhaustive match from a state to its colour,
//! opacity falloff and motion. [`CapsuleState::animates`] and [`paint`] only read
//! that value, so a state cannot animate in one place and stand still in another.
//! The window module keeps only the DIB section, the timer cadence, the message
//! loop, DPI, dragging and `WDA_EXCLUDEFROMCAPTURE`; the desktop acceptance script
//! verifies the window itself.

use listen::{Phase, Preparation, Snapshot};

/// What the resident capsule is showing right now. This is deliberately a
/// different vocabulary from `Phase`: `Result` and `NoSpeech` both mean "nothing is
/// happening, the tool is usable", while a pending or failed model preparation must
/// never look like that.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapsuleState {
    /// The local model is still loading (or the application is in its own
    /// first-use preparation). Capture may still be possible, but recognition is
    /// not available yet, so the capsule must not look ready.
    Preparing,
    /// Idle and usable: a capture can start.
    Ready,
    /// Playback audio is being captured.
    Capturing,
    /// Captured audio is being recognized locally.
    Processing,
    /// The last operation failed, or model preparation failed.
    Failure,
}

impl CapsuleState {
    /// The stable `status.txt` spelling.
    pub fn name(self) -> &'static str {
        match self {
            CapsuleState::Preparing => "Preparing",
            CapsuleState::Ready => "Ready",
            CapsuleState::Capturing => "Capturing",
            CapsuleState::Processing => "Processing",
            CapsuleState::Failure => "Failure",
        }
    }

    /// Whether the capsule's picture changes with time. Read from the single
    /// visual specification, never from a second list of states.
    pub fn animates(self) -> bool {
        self.style().motion.animates()
    }

    /// The capsule's whole look: colour, opacity falloff and the one motion it has.
    ///
    /// This is the only place a state turns into a picture. The match is
    /// exhaustive, so adding a `CapsuleState` variant fails to compile here
    /// instead of silently rendering as a new static colour.
    pub fn style(self) -> Style {
        match self {
            // Calm and dim: the tool is idle, nothing needs attention.
            CapsuleState::Ready => Style {
                body: Body::new(0.36, 0.62, 0.55, 0.32, 0.14),
                motion: Motion::Still,
            },
            // Dim and dashed with a slow breath: not ready yet, do not expect it.
            CapsuleState::Preparing => Style {
                body: Body::new(0.58, 0.64, 0.74, 0.22, 0.18),
                motion: Motion::Breathe {
                    period: 1.8,
                    floor: 0.55,
                    depth: 0.45,
                    dash: Some(14.0),
                },
            },
            // Live capture: saturated green that breathes steadily.
            CapsuleState::Capturing => Style {
                body: Body::new(0.10, 0.86, 0.58, 0.50, 0.32),
                motion: Motion::Breathe {
                    period: 1.1,
                    floor: 0.80,
                    depth: 0.20,
                    dash: None,
                },
            },
            // Working: a bright band sweeps across a dim blue body.
            CapsuleState::Processing => Style {
                body: Body::new(0.38, 0.52, 1.0, 0.18, 0.24),
                motion: Motion::Sweep {
                    period: 1.15,
                    boost: (0.45, 0.32, 0.0),
                    glow: 0.55,
                    ceiling: 0.94,
                },
            },
            // Failure: red and static, the only warm colour the capsule uses.
            CapsuleState::Failure => Style {
                body: Body::new(0.93, 0.24, 0.27, 0.52, 0.32),
                motion: Motion::Still,
            },
        }
    }
}

/// One state's complete visual specification.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Style {
    body: Body,
    motion: Motion,
}

/// Base colour and how opacity falls off from the bar's centre line to its edges.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Body {
    red: f32,
    green: f32,
    blue: f32,
    /// Opacity on the horizontal centre line, before any motion.
    alpha: f32,
    /// Extra opacity from the centre line to the outer edge.
    alpha_vertical: f32,
}

impl Body {
    const fn new(red: f32, green: f32, blue: f32, alpha: f32, alpha_vertical: f32) -> Self {
        Self {
            red,
            green,
            blue,
            alpha,
            alpha_vertical,
        }
    }
}

/// The one motion a state has, if any.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Motion {
    /// Nothing moves: the frame depends on the state alone.
    Still,
    /// Opacity is multiplied by `floor + depth * pulse`, where `pulse` is a sine
    /// in `[0, 1]` with `period` seconds. `dash` adds a horizontal on/off period.
    Breathe {
        period: f32,
        floor: f32,
        depth: f32,
        dash: Option<f32>,
    },
    /// A bright band sweeps across the body once per `period`, brightening the
    /// colour by `boost` and the opacity by `glow`, up to `ceiling`.
    Sweep {
        period: f32,
        boost: (f32, f32, f32),
        glow: f32,
        ceiling: f32,
    },
}

impl Motion {
    fn animates(self) -> bool {
        match self {
            Motion::Still => false,
            Motion::Breathe { .. } | Motion::Sweep { .. } => true,
        }
    }
}

/// Colour and opacity at one logical point, before the rasteriser premultiplies
/// and quantises it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layer {
    pub red: f32,
    pub green: f32,
    pub blue: f32,
    pub alpha: f32,
}

impl Style {
    /// Colour and opacity for one logical point of the bar. `time` is seconds
    /// since the capsule appeared; only a moving state reads it.
    fn layer(self, x: f32, y: f32, width: f32, height: f32, time: f32) -> Layer {
        let half_height = (height * 0.5).max(1.0);
        let vertical = (1.0 - ((y - half_height) / half_height).abs()).clamp(0.0, 1.0);
        let base = self.body.alpha + self.body.alpha_vertical * vertical;
        match self.motion {
            Motion::Still => Layer {
                red: self.body.red,
                green: self.body.green,
                blue: self.body.blue,
                alpha: base,
            },
            Motion::Breathe {
                period,
                floor,
                depth,
                dash,
            } => {
                let pulse = 0.5 + 0.5 * (std::f32::consts::TAU * time / period).sin();
                let dash = match dash {
                    Some(period) => {
                        if ((x / period) as i32) % 2 == 0 {
                            1.0
                        } else {
                            0.16
                        }
                    }
                    None => 1.0,
                };
                Layer {
                    red: self.body.red,
                    green: self.body.green,
                    blue: self.body.blue,
                    alpha: (base * dash) * (floor + depth * pulse),
                }
            }
            Motion::Sweep {
                period,
                boost,
                glow,
                ceiling,
            } => {
                let sweep = ((time / period) % 1.0) * (width + 56.0) - 28.0;
                let offset = (x - sweep) / 13.0;
                let band = (-(offset * offset)).exp();
                Layer {
                    red: self.body.red + boost.0 * band,
                    green: self.body.green + boost.1 * band,
                    blue: self.body.blue + boost.2 * band,
                    alpha: (base + glow * band).min(ceiling),
                }
            }
        }
    }
}

/// The stable `status.txt` spelling of the preparation state.
pub fn preparation_name(preparation: Preparation) -> &'static str {
    match preparation {
        Preparation::Pending => "Pending",
        Preparation::Ready => "Ready",
        Preparation::Failed => "Failed",
    }
}

/// Map the application snapshot to the capsule's visual state.
///
/// Precedence, highest first:
///
/// 1. `Phase::Failure` wins over everything: a failed capture or a failed model
///    preparation is always visible.
/// 2. `Phase::Capturing` wins over preparation: the user pressed the hotkey, so
///    "capture is live" must be visible even while `Preparation::Pending`.
/// 3. `Phase::Processing`.
/// 4. A failed preparation in any other phase is `Failure`, never `Ready`: the
///    capsule must not invite the user to start a capture that cannot be
///    recognized.
/// 5. `Phase::Preparing`, or `Preparation::Pending` in an idle phase, is
///    `Preparing`.
/// 6. `Ready`, `Result` and `NoSpeech` with a ready preparation are `Ready`; the
///    last result is not something the capsule displays, so it stays calm.
pub fn capsule_state(snapshot: &Snapshot) -> CapsuleState {
    match snapshot.phase {
        Phase::Failure => CapsuleState::Failure,
        Phase::Capturing => CapsuleState::Capturing,
        Phase::Processing => CapsuleState::Processing,
        // Every remaining phase is idle with respect to capture, so preparation
        // decides between "not ready yet", "failed" and "ready".
        Phase::Preparing | Phase::Ready | Phase::Result | Phase::NoSpeech => {
            match snapshot.preparation {
                Preparation::Failed => CapsuleState::Failure,
                Preparation::Pending => CapsuleState::Preparing,
                // The application reports a finished preparation as `Ready`, so
                // `Phase::Preparing` here would only mean the phase update is one
                // poll behind; it must not look usable yet.
                Preparation::Ready if snapshot.phase == Phase::Preparing => CapsuleState::Preparing,
                Preparation::Ready => CapsuleState::Ready,
            }
        }
    }
}

/// Rasterise one frame of the capsule into premultiplied ARGB pixels, `width *
/// height` of them. Pure except for writing `pixels`; no GDI text and no external
/// dependencies. The rounded bar is a stadium shape sampled analytically with a
/// one-device-pixel anti-aliasing ramp, and the state only changes colour,
/// brightness and horizontal motion.
pub fn paint(
    pixels: &mut [u32],
    width: i32,
    height: i32,
    scale: f32,
    state: CapsuleState,
    time: f32,
) {
    let logical_width = width as f32 / scale;
    let logical_height = height as f32 / scale;
    let half_width = logical_width * 0.5;
    let half_height = logical_height * 0.5;
    let radius = half_height;
    let style = state.style();
    for y in 0..height {
        for x in 0..width {
            let px = (x as f32 + 0.5) / scale;
            let py = (y as f32 + 0.5) / scale;
            let coverage = capsule_coverage(px, py, half_width, half_height, radius, scale);
            let index = (y * width + x) as usize;
            if coverage <= 0.0 {
                pixels[index] = 0;
                continue;
            }
            let layer = style.layer(px, py, logical_width, logical_height, time);
            let alpha = (layer.alpha * coverage).clamp(0.0, 1.0);
            let a = (alpha * 255.0).round() as u32;
            let r = (layer.red.clamp(0.0, 1.0) * a as f32).round() as u32;
            let g = (layer.green.clamp(0.0, 1.0) * a as f32).round() as u32;
            let b = (layer.blue.clamp(0.0, 1.0) * a as f32).round() as u32;
            pixels[index] = (a << 24) | (r << 16) | (g << 8) | b;
        }
    }
}

fn capsule_coverage(
    x: f32,
    y: f32,
    half_width: f32,
    half_height: f32,
    radius: f32,
    scale: f32,
) -> f32 {
    let qx = (x - half_width).abs() - (half_width - radius);
    let qy = (y - half_height).abs() - (half_height - radius);
    let outside = (qx.max(0.0) * qx.max(0.0) + qy.max(0.0) * qy.max(0.0)).sqrt();
    let inside = qx.max(qy).min(0.0);
    let distance = outside + inside - radius;
    (0.5 - distance * scale).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use listen::{
        Phase, Preparation, Snapshot, Timing, recognition::RecognitionMode,
        streaming::CaptureProgress,
    };

    fn snapshot(phase: Phase, preparation: Preparation) -> Snapshot {
        Snapshot {
            phase,
            preparation,
            recognition: None,
            detail: String::new(),
            text: String::new(),
            retained_audio: Vec::new(),
            progress: CaptureProgress::default(),
            delivery: None,
            retry_available: false,
            endpoint: None,
            backend: None,
            mode: RecognitionMode::Local,
            timing: Timing::default(),
            refine: listen::Refine::Unavailable("not connected in this test".into()),
            refined_text: None,
            notice: None,
            usage: None,
            segments: Vec::new(),
            current_sequence: None,
            storage: None,
        }
    }

    const PHASES: [Phase; 7] = [
        Phase::Preparing,
        Phase::Ready,
        Phase::Capturing,
        Phase::Processing,
        Phase::Result,
        Phase::NoSpeech,
        Phase::Failure,
    ];
    const PREPARATIONS: [Preparation; 3] = [
        Preparation::Pending,
        Preparation::Ready,
        Preparation::Failed,
    ];

    #[test]
    fn every_phase_and_preparation_combination_maps_to_the_specified_capsule_state() {
        use CapsuleState as C;
        use Phase as P;
        use Preparation as R;
        // The specification table, written out as literals so the test does not
        // recompute the precedence it is checking.
        let table = [
            (P::Preparing, R::Pending, C::Preparing),
            (P::Preparing, R::Ready, C::Preparing),
            (P::Preparing, R::Failed, C::Failure),
            (P::Ready, R::Pending, C::Preparing),
            (P::Ready, R::Ready, C::Ready),
            (P::Ready, R::Failed, C::Failure),
            (P::Capturing, R::Pending, C::Capturing),
            (P::Capturing, R::Ready, C::Capturing),
            (P::Capturing, R::Failed, C::Capturing),
            (P::Processing, R::Pending, C::Processing),
            (P::Processing, R::Ready, C::Processing),
            (P::Processing, R::Failed, C::Processing),
            (P::Result, R::Pending, C::Preparing),
            (P::Result, R::Ready, C::Ready),
            (P::Result, R::Failed, C::Failure),
            (P::NoSpeech, R::Pending, C::Preparing),
            (P::NoSpeech, R::Ready, C::Ready),
            (P::NoSpeech, R::Failed, C::Failure),
            (P::Failure, R::Pending, C::Failure),
            (P::Failure, R::Ready, C::Failure),
            (P::Failure, R::Failed, C::Failure),
        ];
        assert_eq!(table.len(), PHASES.len() * PREPARATIONS.len());
        for (phase, preparation, expected) in table {
            let actual = capsule_state(&snapshot(phase.clone(), preparation));
            assert_eq!(
                actual, expected,
                "phase={phase:?} preparation={preparation:?}"
            );
        }
    }

    #[test]
    fn a_pending_or_failed_preparation_never_renders_the_capsule_as_ready() {
        for phase in &PHASES {
            for preparation in [Preparation::Pending, Preparation::Failed] {
                assert_ne!(
                    capsule_state(&snapshot(phase.clone(), preparation)),
                    CapsuleState::Ready,
                    "phase={phase:?} preparation={preparation:?} must not look ready"
                );
            }
        }
    }

    #[test]
    fn capture_started_while_the_model_is_still_preparing_shows_live_capture() {
        let snapshot = snapshot(Phase::Capturing, Preparation::Pending);
        assert_eq!(capsule_state(&snapshot), CapsuleState::Capturing);
    }

    #[test]
    fn status_names_are_stable() {
        assert_eq!(CapsuleState::Preparing.name(), "Preparing");
        assert_eq!(CapsuleState::Ready.name(), "Ready");
        assert_eq!(CapsuleState::Capturing.name(), "Capturing");
        assert_eq!(CapsuleState::Processing.name(), "Processing");
        assert_eq!(CapsuleState::Failure.name(), "Failure");
        assert_eq!(preparation_name(Preparation::Pending), "Pending");
        assert_eq!(preparation_name(Preparation::Ready), "Ready");
        assert_eq!(preparation_name(Preparation::Failed), "Failed");
    }

    /// The colour at one point in the middle of a standard 220 x 26 bar: these
    /// tests care about what a state looks like, not about the rasteriser.
    fn at_rest(state: CapsuleState, time: f32) -> Layer {
        state.style().layer(40.0, 13.0, 220.0, 26.0, time)
    }

    #[test]
    fn ready_is_still() {
        assert!(!CapsuleState::Ready.animates());
        let start = at_rest(CapsuleState::Ready, 0.0);
        for time in [0.275, 1.15, 60.0] {
            assert_eq!(
                start,
                at_rest(CapsuleState::Ready, time),
                "Ready must not move: t={time}"
            );
        }
    }

    #[test]
    fn capturing_breathes() {
        assert!(CapsuleState::Capturing.animates());
        let dim = at_rest(CapsuleState::Capturing, 0.0);
        // A quarter of its 1.1 s period is the top of the breath.
        let bright = at_rest(CapsuleState::Capturing, 0.275);
        assert!(
            bright.alpha > dim.alpha,
            "capturing must breathe brighter, not darker: {} vs {}",
            bright.alpha,
            dim.alpha
        );
        // A breath is opacity only; the colour never changes.
        assert_eq!(
            (dim.red, dim.green, dim.blue),
            (bright.red, bright.green, bright.blue)
        );
        let next_breath = at_rest(CapsuleState::Capturing, 1.1);
        assert!(
            (next_breath.alpha - dim.alpha).abs() < 1e-5,
            "one period must return to the same picture: {} vs {}",
            next_breath.alpha,
            dim.alpha
        );
    }

    #[test]
    fn failure_is_warm_and_still() {
        assert!(!CapsuleState::Failure.animates());
        let layer = at_rest(CapsuleState::Failure, 0.0);
        assert!(
            layer.red > 0.5 && layer.red > layer.green && layer.red > layer.blue,
            "failure keeps the only warm colour: {layer:?}"
        );
        for time in [0.275, 1.15, 60.0] {
            assert_eq!(
                layer,
                at_rest(CapsuleState::Failure, time),
                "failure must not move: t={time}"
            );
        }
    }

    #[test]
    fn only_the_working_states_declare_motion() {
        let moving: Vec<&str> = [
            CapsuleState::Preparing,
            CapsuleState::Ready,
            CapsuleState::Capturing,
            CapsuleState::Processing,
            CapsuleState::Failure,
        ]
        .into_iter()
        .filter(|state| state.animates())
        .map(|state| state.name())
        .collect();
        assert_eq!(moving, ["Preparing", "Capturing", "Processing"]);
    }

    #[test]
    fn several_segments_change_nothing_about_the_capsules_colour_only_vocabulary() {
        // The capsule never shows waiting counts, segment numbers or text: every
        // multi-segment fact belongs to the panel. Its whole vocabulary is these five
        // colour/shape states, and each name is a word without any number.
        let states = [
            CapsuleState::Preparing,
            CapsuleState::Ready,
            CapsuleState::Capturing,
            CapsuleState::Processing,
            CapsuleState::Failure,
        ];
        for state in states {
            assert!(
                !state
                    .name()
                    .chars()
                    .any(|character| character.is_ascii_digit()),
                "the capsule state must stay number-free: {}",
                state.name()
            );
        }

        // A snapshot with an older waiting segment and a waiting result maps exactly
        // like the same phase without them.
        let mut idle = snapshot(Phase::Ready, Preparation::Ready);
        idle.segments = vec![
            listen::SegmentSummary {
                sequence: 1,
                state: listen::SegmentState::Result,
                detail: "pending".into(),
                text: "一段完整结果".into(),
                delivery: Some(listen::Delivery::Fallback),
                usage: None,
            },
            listen::SegmentSummary {
                sequence: 2,
                state: listen::SegmentState::WaitingForNetwork,
                detail: "waiting".into(),
                text: String::new(),
                delivery: None,
                usage: None,
            },
        ];
        assert_eq!(capsule_state(&idle), CapsuleState::Ready);
        idle.phase = Phase::Capturing;
        assert_eq!(capsule_state(&idle), CapsuleState::Capturing);
        idle.phase = Phase::Processing;
        assert_eq!(capsule_state(&idle), CapsuleState::Processing);
        idle.phase = Phase::Failure;
        assert_eq!(capsule_state(&idle), CapsuleState::Failure);
    }

    #[test]
    fn the_capsule_is_transparent_outside_its_rounded_bar() {
        let (width, height) = (220, 26);
        let mut pixels = vec![0u32; (width * height) as usize];
        paint(&mut pixels, width, height, 1.0, CapsuleState::Ready, 0.0);
        // The corners are outside the stadium shape; the middle is fully covered.
        assert_eq!(pixels[0], 0);
        assert_eq!(pixels[(width - 1) as usize], 0);
        assert_ne!(pixels[((height / 2) * width + width / 2) as usize], 0);
    }

    /// FNV-1a over the premultiplied pixels: one changed byte anywhere in a frame
    /// changes the value. The expected hashes were captured from the pre-refactor
    /// `capsule_window::paint_pixels` before issue #50 moved the pixel mapping into
    /// this module; they are the evidence that the move changed no pixel.
    fn frame_hash(pixels: &[u32]) -> u64 {
        let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
        for pixel in pixels {
            for byte in pixel.to_le_bytes() {
                hash ^= byte as u64;
                hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            }
        }
        hash
    }

    #[test]
    fn every_state_renders_exactly_the_frame_the_window_module_drew_before_the_move() {
        use CapsuleState as C;
        // (state, time in seconds, DPI scale, hash of the whole frame at 220 x 26
        // logical pixels). Five states at five times, plus one scaled frame per
        // state so the DPI rounding is pinned too.
        let expected: &[(C, f32, f32, u64)] = &[
            (C::Ready, 0.0, 1.0, 0x8696_bf43_74dc_ec9d),
            (C::Ready, 0.275, 1.0, 0x8696_bf43_74dc_ec9d),
            (C::Ready, 0.55, 1.0, 0x8696_bf43_74dc_ec9d),
            (C::Ready, 1.15, 1.0, 0x8696_bf43_74dc_ec9d),
            (C::Ready, 2.5, 1.0, 0x8696_bf43_74dc_ec9d),
            (C::Preparing, 0.0, 1.0, 0xf111_0a3d_cb11_8dd1),
            (C::Preparing, 0.275, 1.0, 0x5b95_56b6_2cd2_6d7d),
            (C::Preparing, 0.55, 1.0, 0x5064_9b00_b649_f429),
            (C::Preparing, 1.15, 1.0, 0x3c81_f094_770f_996d),
            (C::Preparing, 2.5, 1.0, 0xd4fe_21df_61f6_92dd),
            (C::Capturing, 0.0, 1.0, 0x76ff_85f1_ff4e_dc1d),
            (C::Capturing, 0.275, 1.0, 0xdccf_0c40_edac_e025),
            (C::Capturing, 0.55, 1.0, 0x76ff_85f1_ff4e_dc1d),
            (C::Capturing, 1.15, 1.0, 0xf795_21b3_5bf8_94ad),
            (C::Capturing, 2.5, 1.0, 0x5f1b_ad43_0b60_3ce5),
            (C::Processing, 0.0, 1.0, 0x54df_1e2f_661e_db79),
            (C::Processing, 0.275, 1.0, 0xb18a_674f_e199_ca0d),
            (C::Processing, 0.55, 1.0, 0xf96a_9d5e_2cd1_ed3d),
            (C::Processing, 1.15, 1.0, 0x54df_1e2f_661e_db79),
            (C::Processing, 2.5, 1.0, 0xf336_6539_afb3_6a21),
            (C::Failure, 0.0, 1.0, 0xc64a_ac0e_3c1d_0aa5),
            (C::Failure, 0.275, 1.0, 0xc64a_ac0e_3c1d_0aa5),
            (C::Failure, 0.55, 1.0, 0xc64a_ac0e_3c1d_0aa5),
            (C::Failure, 1.15, 1.0, 0xc64a_ac0e_3c1d_0aa5),
            (C::Failure, 2.5, 1.0, 0xc64a_ac0e_3c1d_0aa5),
            (C::Ready, 0.0, 1.25, 0x6ce1_6ad6_f504_697a),
            (C::Preparing, 0.7, 1.5, 0x3103_dac2_d981_97d5),
            (C::Capturing, 0.4, 2.0, 0x3e92_af95_65a5_b60d),
            (C::Processing, 0.9, 1.25, 0xa0c1_83ca_2892_760a),
            (C::Failure, 1.0, 1.5, 0x673b_c327_b622_4af1),
        ];
        for &(state, time, scale, expected) in expected {
            let width = (220.0 * scale).round() as i32;
            let height = (26.0 * scale).round() as i32;
            let mut pixels = vec![0u32; (width * height) as usize];
            paint(&mut pixels, width, height, scale, state, time);
            assert_eq!(
                frame_hash(&pixels),
                expected,
                "state={} time={time} scale={scale} {width}x{height}",
                state.name()
            );
        }
    }
}
