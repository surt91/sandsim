//! Calibrated model parameters (see sandsim.py).

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Clear {
    Auto,
    In,
    Out,
    None,
}

/// Table geometry and sand model. Changing these requires a new simulation.
#[derive(Clone, PartialEq, Debug)]
pub struct SimParams {
    pub rho_mm: f64,      // rho = 1 corresponds to this radius (ball centre) [mm]
    pub sand_mm: f64,     // radius of the sand surface [mm]
    pub ball_r: f64,      // ball radius [mm]
    pub res: f64,         // simulation grid [mm/px]
    pub h0: f64,          // sand depth [mm], the ball rolls on the floor
    pub repose: f64,      // angle of repose after sliding [°]
    pub stat: f64,        // steepest stable slope (freshly cut walls) [°]
    pub smooth: f64,      // rounding of edges [mm]
    pub push_w: f64,      // width of the deposition ring around the ball [mm]
    pub side: f64,        // share deposited sideways (instead of only in front)
    pub clear_pitch: f64, // ring pitch of the clearing pattern [mm]
    pub step: f64,        // step size of the ball [mm]
    pub lag: f64,         // lag of the ball behind the magnet [mm] (rounds corners)
    pub clear: Clear,
    pub mirror: bool,
    pub seed: u64,
}

impl Default for SimParams {
    fn default() -> Self {
        Self {
            rho_mm: 290.0,
            sand_mm: 312.0,
            ball_r: 5.0,
            res: 0.5,
            h0: 4.5,
            repose: 32.0,
            stat: 40.0,
            smooth: 0.25,
            push_w: 2.0,
            side: 0.6,
            clear_pitch: 6.1,
            step: 0.5,
            lag: 6.0,
            clear: Clear::Auto,
            mirror: false,
            seed: 1,
        }
    }
}

/// LED strip lighting. Can be changed at any time, the image is re-lit.
#[derive(Clone, PartialEq, Debug)]
pub struct LightParams {
    pub n_led: usize,      // LED positions along the rim (strip = line source)
    pub n_led_live: usize, // LED positions while the pattern is being drawn (faster)
    pub r_led: f64,        // radius of the LED strip [mm]
    pub z_led: f64,        // height of the LEDs above the floor [mm]
    pub alpha0: f64,       // vertical beam angle [°]
    pub falloff: f64,      // falloff ~ 1/d^falloff
    pub shadow_mm: f64,    // maximum shadow length [mm]
    pub soft: f64,         // penumbra [°]
    pub ambient: f64,      // ambient light (relative to direct light)
    pub ao: f64,           // exponent of ambient occlusion in grooves
    pub ao_mm: f64,        // range of ambient occlusion [mm]
    pub grain: f64,        // grain (height noise) [mm]
    pub albedo_noise: f64, // grain colour variation
    pub exposure: f64,     // extra brightness factor (1 = calibrated)
    /// LED colour (sRGB); None: the orange LEDs the tone curve was calibrated with
    pub led_color: Option<[u8; 3]>,
}

impl Default for LightParams {
    fn default() -> Self {
        Self {
            n_led: 48,
            n_led_live: 12,
            r_led: 318.0,
            z_led: 25.0,
            alpha0: 30.0,
            falloff: 1.2,
            shadow_mm: 40.0,
            soft: 2.0,
            ambient: 0.3,
            ao: 3.0,
            ao_mm: 15.0,
            grain: 0.01,
            albedo_noise: 0.15,
            exposure: 1.0,
            led_color: None,
        }
    }
}

/// Tone curve: relative brightness (median = 1) -> colour, obtained by quantile matching
/// (0.5 .. 99.95 %) against photos of the table in orange LED light (top and oblique view).
pub const TONE_X: [f64; 13] =
    [0.283, 0.328, 0.384, 0.451, 0.591, 0.931, 1.873, 2.508, 2.902, 3.170, 3.275, 3.504, 9.0];
pub const TONE: [[f64; 3]; 13] = [
    [78., 33., 16.],
    [90., 40., 23.],
    [106., 48., 30.],
    [123., 57., 37.],
    [149., 69., 46.],
    [171., 79., 53.],
    [189., 89., 61.],
    [209., 100., 70.],
    [226., 109., 78.],
    [243., 120., 85.],
    [252., 130., 92.],
    [253., 134., 96.],
    [255., 150., 110.],
];
/// colour of the sand itself (relative, under white light)
pub const SAND_TINT: [f64; 3] = [1.0, 0.92, 0.77];
/// frame/surroundings
pub const BG: [f64; 3] = [0.035, 0.025, 0.025];

/// Oblique-view camera, fitted to a photo: position [mm], point on the floor it looks at [mm],
/// horizontal field of view [°], roll around the viewing direction [°].
#[derive(Clone, PartialEq, Debug)]
pub struct Camera {
    pub pos: [f64; 3],
    pub target: [f64; 2],
    pub hfov: f64,
    pub roll: f64,
    pub rot: f64, // rotation around the table centre [°], counter-clockwise
    pub width: usize,
    pub height: usize,
}

impl Default for Camera {
    fn default() -> Self {
        Self {
            pos: [-152.9, -380.0, 362.2],
            target: [-34.1, -82.2],
            hfov: 71.5,
            roll: -1.1,
            rot: 0.0,
            width: 1020,
            height: 768,
        }
    }
}
