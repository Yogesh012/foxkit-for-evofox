//! Music styles on the keys, from what the Mac is playing.

use std::f64::consts::PI;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::{self, sleep, JoinHandle};
use std::time::{Duration, Instant};

use hidapi::HidDevice;
use objc2::rc::Retained;
use objc2::runtime::{NSObjectProtocol, ProtocolObject};
use objc2_foundation::{NSActivityOptions, NSProcessInfo, NSString};
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use serde::{Deserialize, Serialize};

use crate::audio::AudioSource;
use crate::ronin;

const FFT_SIZE: usize = 2048;
const FPS: u32 = 30;
const COLUMNS: usize = 14;
const LOWEST_HZ: f64 = 40.0;
const HIGHEST_HZ: f64 = 16000.0;
// How far a band drops each frame, as a share of the full height. Rises are immediate.
const FALL_PER_FRAME: f64 = 0.08;
// Levels bunch near the top of each column's range, so this spreads them down the rows.
const LEVEL_CURVE: i32 = 2;
// Levels are relative to recent sound, so playback volume doesn't matter. These four are
// on a natural-log scale, where 1 is about 8.7 dB.
const PEAK_FALL_PER_FRAME: f64 = 0.002;
const QUIET_RISE_PER_FRAME: f64 = 0.005;
const MIN_SPAN: f64 = 2.0;
const MAX_SPAN: f64 = 4.0;
const BAND_FLOOR: f64 = 0.01;
const SILENCE: f64 = 1e-4;
// LED slots by keyboard row, from bottom to top, left to right. The bottom row has fewer
// keys, spread across the same columns.
const ROWS: [&[usize]; 6] = [
    &[91, 92, 93, 94, 95, 97, 99, 100, 101],
    &[90, 89, 88, 87, 86, 85, 84, 83, 82, 81, 80, 79, 78, 41],
    &[58, 59, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 32],
    &[57, 56, 55, 54, 53, 52, 51, 50, 49, 48, 47, 46, 45, 44],
    &[16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29],
    &[15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 43],
];
// Music Rhythm mode takes about a second to start, so stay quiet for longer than that.
const MUSIC_ENTRY: Duration = Duration::from_millis(1500);
const STOP_POLL: Duration = Duration::from_millis(50);
const FAILED_PAGES_LIMIT: u32 = 30;
const FRAME_GAP: Duration = Duration::from_nanos(1_000_000_000 / FPS as u64);
// Small delays are made up to hold the rate. Past this, carry on from now instead.
const CATCH_UP_LIMIT: Duration = Duration::from_millis(100);
const LOG_EVERY: Duration = Duration::from_secs(60);
// Colour presets 1 to 7.
const SOLID_PRESETS: [[f64; 3]; 7] = [
    [255.0, 0.0, 0.0],
    [0.0, 255.0, 0.0],
    [0.0, 0.0, 255.0],
    [255.0, 255.0, 0.0],
    [255.0, 0.0, 255.0],
    [0.0, 255.0, 255.0],
    [255.0, 255.0, 255.0],
];
// Preset 8, spread across the columns.
const GRADIENT: [[f64; 3]; 5] = [
    [255.0, 0.0, 0.0],
    [0.0, 255.0, 0.0],
    [0.0, 0.0, 255.0],
    [255.0, 255.0, 0.0],
    [255.0, 0.0, 255.0],
];
// A change of look fades in over about 0.3 s.
const FADE_FRAMES: u32 = 9;
// The bass is these columns, up to about 220 Hz.
const BASS_COLUMNS: usize = 4;
// How far each frame's recent average moves towards the sound, which takes about 0.5 s to
// catch up. A rise above it is a hit, and steady sound isn't.
const AVERAGE_SHARE_PER_FRAME: f64 = 0.06;
// Pulse counts treble hits, not treble level, since treble is rarely quiet for long.
const PULSE_TREBLE_GAIN: f64 = 2.0;
// Glow's bass stays near the top of its range, so its columns show hits, over a dim level.
const GLOW_BASS_GAIN: f64 = 4.0;
const GLOW_BASS_LEVEL_SHARE: f64 = 0.3;
// Ripple shows the bass later the further a key is from the centre: about 0.35 s to the edge.
const RIPPLE_FRAMES_PER_KEY: f64 = 1.5;
const RIPPLE_HISTORY: usize = 12;
// Sparkle lights random keys from the treble, from about 1.2 kHz up, over a dim pulse.
const SPARK_FIRST_COLUMN: usize = 8;
const SPARKS_PER_FRAME: f64 = 2.0;
const SPARK_FALL_PER_FRAME: f64 = 0.06;
const SPARK_PULSE_SHARE: f64 = 0.35;
const LEDS: usize = ronin::FRAME_BYTES / 3;

type Levels = [f64; COLUMNS];

/// How the music is drawn on the keys.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Style {
    #[default]
    Bars,
    Pulse,
    Glow,
    Ripple,
    Sparkle,
}

/// The lighting settings the bars follow. Speed doesn't apply to them.
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct Look {
    pub colour: u8,
    pub brightness: u8,
    pub direction: u8,
    pub style: Style,
}

/// What each column is drawn with.
#[derive(Clone, Copy)]
struct Paint {
    colours: [[f64; 3]; COLUMNS],
    // The keyboard's own brightness doesn't dim frames, so they're dimmed here.
    brightness: f64,
}

impl Paint {
    fn blend(from: &Self, to: &Self, share: f64) -> Self {
        let mix = |from: f64, to: f64| from + (to - from) * share;
        Self {
            colours: std::array::from_fn(|column| {
                std::array::from_fn(|channel| {
                    mix(from.colours[column][channel], to.colours[column][channel])
                })
            }),
            brightness: mix(from.brightness, to.brightness),
        }
    }
}

/// Moves the bars gradually from one look to the next.
struct Fade {
    look: Look,
    from: Paint,
    to: Paint,
    shown: Paint,
    frames_left: u32,
}

impl Fade {
    fn new(look: Look) -> Self {
        let paint = look.paint();
        Self {
            look,
            from: paint,
            to: paint,
            shown: paint,
            frames_left: 0,
        }
    }

    fn next(&mut self, look: Look) -> &Paint {
        if look != self.look {
            self.look = look;
            self.from = self.shown;
            self.to = look.paint();
            self.frames_left = FADE_FRAMES;
        }
        if self.frames_left > 0 {
            self.frames_left -= 1;
            let share = 1.0 - f64::from(self.frames_left) / f64::from(FADE_FRAMES);
            self.shown = Paint::blend(&self.from, &self.to, share);
        }
        &self.shown
    }
}

impl Look {
    fn paint(&self) -> Paint {
        Paint {
            colours: std::array::from_fn(|column| self.column_colour(column)),
            brightness: f64::from(self.brightness) / f64::from(ronin::LEVEL_MAX),
        }
    }

    fn column_colour(&self, column: usize) -> [f64; 3] {
        if let Some(solid) = SOLID_PRESETS.get(usize::from(self.colour)) {
            return *solid;
        }
        let position = if self.direction == 0 {
            column
        } else {
            COLUMNS - 1 - column
        };
        let along = position as f64 * (GRADIENT.len() - 1) as f64 / (COLUMNS - 1) as f64;
        let stop = (along as usize).min(GRADIENT.len() - 2);
        let part = along - stop as f64;
        std::array::from_fn(|channel| {
            GRADIENT[stop][channel] + (GRADIENT[stop + 1][channel] - GRADIENT[stop][channel]) * part
        })
    }
}

/// Turns samples into column levels between 0 and 1.
struct Spectrum {
    fft: Arc<dyn Fft<f64>>,
    window: Vec<f64>,
    peak: Levels,
    quiet: Levels,
    levels: Levels,
}

impl Spectrum {
    fn new() -> Self {
        Self {
            fft: FftPlanner::new().plan_fft_forward(FFT_SIZE),
            window: (0..FFT_SIZE)
                .map(|n| 0.5 - 0.5 * (2.0 * PI * n as f64 / (FFT_SIZE - 1) as f64).cos())
                .collect(),
            peak: [f64::NEG_INFINITY; COLUMNS],
            quiet: [f64::INFINITY; COLUMNS],
            levels: [0.0; COLUMNS],
        }
    }

    fn update(&mut self, samples: &[f32], rate: u32) -> &Levels {
        let mut buffer: Vec<Complex<f64>> = samples
            .iter()
            .zip(&self.window)
            .map(|(&sample, window)| Complex::new(f64::from(sample) * window, 0.0))
            .collect();
        self.fft.process(&mut buffer);
        let bands = band_magnitudes(&buffer[..=FFT_SIZE / 2], rate);
        // Each column spans its own recent quietest to loudest, on a log scale, so higher
        // bands fill the board like bass and the bottom rows don't stay lit. The floor
        // keeps near-silent bands from filling the board with noise.
        let logs = bands.map(|band| band.max(SILENCE).ln());
        for column in 0..COLUMNS {
            self.peak[column] = (self.peak[column] - PEAK_FALL_PER_FRAME).max(logs[column]);
            self.quiet[column] = (self.quiet[column] + QUIET_RISE_PER_FRAME)
                .min(logs[column])
                .max(self.peak[column] - MAX_SPAN);
        }
        let loudest = self.peak.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let (lowest, highest) = samples.iter().fold(
            (f32::INFINITY, f32::NEG_INFINITY),
            |(low, high), &sample| (low.min(sample), high.max(sample)),
        );
        let silent = f64::from(highest) - f64::from(lowest) <= SILENCE;
        for column in 0..COLUMNS {
            let top = self.peak[column].max(loudest + BAND_FLOOR.ln());
            let span = (top - self.quiet[column]).max(MIN_SPAN);
            let target = if silent {
                0.0
            } else {
                ((logs[column] - self.quiet[column]) / span)
                    .clamp(0.0, 1.0)
                    .powi(LEVEL_CURVE)
            };
            self.levels[column] = target.max(self.levels[column] - FALL_PER_FRAME);
        }
        &self.levels
    }
}

fn band_magnitudes(bins: &[Complex<f64>], rate: u32) -> Levels {
    let bin = |column: usize| {
        let hz = LOWEST_HZ * (HIGHEST_HZ / LOWEST_HZ).powf(column as f64 / COLUMNS as f64);
        (hz * FFT_SIZE as f64 / f64::from(rate)) as usize
    };
    std::array::from_fn(|column| {
        let low = bin(column).min(bins.len() - 1);
        let high = bin(column + 1).max(low + 1).min(bins.len());
        bins[low..high]
            .iter()
            .map(|value| value.norm())
            .fold(0.0, f64::max)
    })
}

/// Share of columns that reach each row, from bottom to top.
fn rows_lit(levels: &Levels) -> [f64; ROWS.len()] {
    std::array::from_fn(|height| {
        let reached = levels
            .iter()
            .filter(|&&level| level * ROWS.len() as f64 > height as f64 + 0.5)
            .count();
        reached as f64 / COLUMNS as f64
    })
}

fn next_due(due: Instant, now: Instant) -> Instant {
    if now.saturating_duration_since(due) > CATCH_UP_LIMIT {
        now + FRAME_GAP
    } else {
        due + FRAME_GAP
    }
}

/// Stops macOS slowing this app's timers while its window is covered or the Mac is locked.
/// The Mac can still go to sleep.
struct PreciseTiming(Retained<ProtocolObject<dyn NSObjectProtocol>>);

impl PreciseTiming {
    fn begin() -> Self {
        let options = NSActivityOptions::UserInitiatedAllowingIdleSystemSleep
            | NSActivityOptions::LatencyCritical;
        Self(
            NSProcessInfo::processInfo()
                .beginActivityWithOptions_reason(options, &NSString::from_str("Audio bars")),
        )
    }
}

impl Drop for PreciseTiming {
    fn drop(&mut self) {
        // Safety: this token came from beginActivityWithOptions_reason and is ended once.
        unsafe { NSProcessInfo::processInfo().endActivity(&self.0) };
    }
}

fn key_column(position: usize, row: &[usize]) -> usize {
    (position as f64 * (COLUMNS - 1) as f64 / (row.len() - 1) as f64).round_ties_even() as usize
}

/// Draws every key, given how lit it should be from its row height, column and LED slot.
fn keys_frame(paint: &Paint, lit: impl Fn(usize, usize, usize) -> f64) -> ronin::Frame {
    let mut frame = [0; ronin::FRAME_BYTES];
    for (height, row) in ROWS.iter().enumerate() {
        for (position, &led) in row.iter().enumerate() {
            let column = key_column(position, row);
            let share = lit(height, column, led);
            let colour =
                paint.colours[column].map(|channel| (channel * share * paint.brightness) as u8);
            frame[3 * led..3 * led + 3].copy_from_slice(&colour);
        }
    }
    frame
}

fn bass_level(levels: &Levels) -> f64 {
    levels[..BASS_COLUMNS].iter().copied().fold(0.0, f64::max)
}

fn treble_level(levels: &Levels) -> f64 {
    levels[SPARK_FIRST_COLUMN..]
        .iter()
        .copied()
        .fold(0.0, f64::max)
}

fn bars_frame(levels: &Levels, paint: &Paint) -> ronin::Frame {
    // Share of each key that the bar covers, so bar tops can sit part-way up a key.
    keys_frame(paint, |height, column, _| {
        (levels[column] * ROWS.len() as f64 - height as f64).clamp(0.0, 1.0)
    })
}

/// Every key together, as bright as the bass or a treble hit, whichever is stronger.
fn pulse_frame(levels: &Levels, treble_rise: f64, paint: &Paint) -> ronin::Frame {
    let level = bass_level(levels).max((treble_rise * PULSE_TREBLE_GAIN).min(1.0));
    keys_frame(paint, |_, _, _| level)
}

/// Every key at once, each as bright as its own column's sound, from bass on the left to
/// treble on the right. The bass columns follow hits, so they move like the others do.
fn glow_frame(levels: &Levels, rises: &Levels, paint: &Paint) -> ronin::Frame {
    keys_frame(paint, |_, column, _| {
        if column < BASS_COLUMNS {
            (rises[column] * GLOW_BASS_GAIN)
                .min(1.0)
                .max(levels[column] * GLOW_BASS_LEVEL_SHARE)
        } else {
            levels[column]
        }
    })
}

fn ripple_delay(height: usize, column: usize) -> usize {
    let across = column as f64 - (COLUMNS - 1) as f64 / 2.0;
    let up = height as f64 - (ROWS.len() - 1) as f64 / 2.0;
    ((across.hypot(up) * RIPPLE_FRAMES_PER_KEY).round() as usize).min(RIPPLE_HISTORY - 1)
}

/// Draws the styles, and keeps what the ones that change over time need. It all keeps
/// updating whatever style is showing, so switching to one doesn't start it from nothing.
struct Drawer {
    /// Bass levels, newest first.
    bass: [f64; RIPPLE_HISTORY],
    /// Each column's recent average, and how far it's above that now.
    averages: Levels,
    rises: Levels,
    treble_average: f64,
    treble_rise: f64,
    /// How lit each LED slot's spark still is.
    sparks: [f64; LEDS],
    random: u64,
}

impl Drawer {
    fn new(seed: u64) -> Self {
        Self {
            bass: [0.0; RIPPLE_HISTORY],
            averages: [0.0; COLUMNS],
            rises: [0.0; COLUMNS],
            treble_average: 0.0,
            treble_rise: 0.0,
            sparks: [0.0; LEDS],
            random: seed | 1,
        }
    }

    // Xorshift, which is plenty for choosing keys to light.
    fn next_random(&mut self) -> u64 {
        self.random ^= self.random << 13;
        self.random ^= self.random >> 7;
        self.random ^= self.random << 17;
        self.random
    }

    fn update(&mut self, levels: &Levels) {
        self.bass.copy_within(..RIPPLE_HISTORY - 1, 1);
        self.bass[0] = bass_level(levels);
        for column in 0..COLUMNS {
            self.rises[column] = (levels[column] - self.averages[column]).max(0.0);
            self.averages[column] +=
                (levels[column] - self.averages[column]) * AVERAGE_SHARE_PER_FRAME;
        }
        let treble = treble_level(levels);
        self.treble_rise = (treble - self.treble_average).max(0.0);
        self.treble_average += (treble - self.treble_average) * AVERAGE_SHARE_PER_FRAME;
        for spark in &mut self.sparks {
            *spark = (*spark - SPARK_FALL_PER_FRAME).max(0.0);
        }
        let wanted = treble * SPARKS_PER_FRAME;
        let chance = self.next_random() as f64 / u64::MAX as f64;
        let count = wanted as usize + usize::from(chance < wanted.fract());
        let keys: usize = ROWS.iter().map(|row| row.len()).sum();
        for _ in 0..count {
            let pick = (self.next_random() % keys as u64) as usize;
            if let Some(&led) = ROWS.iter().flat_map(|row| row.iter()).nth(pick) {
                self.sparks[led] = 1.0;
            }
        }
    }

    fn frame(&mut self, style: Style, levels: &Levels, paint: &Paint) -> ronin::Frame {
        self.update(levels);
        match style {
            Style::Bars => bars_frame(levels, paint),
            Style::Pulse => pulse_frame(levels, self.treble_rise, paint),
            Style::Glow => glow_frame(levels, &self.rises, paint),
            Style::Ripple => keys_frame(paint, |height, column, _| {
                self.bass[ripple_delay(height, column)]
            }),
            Style::Sparkle => {
                let pulse = self.bass[0] * SPARK_PULSE_SHARE;
                keys_frame(paint, |_, _, led| self.sparks[led].max(pulse))
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    #[default]
    Starting,
    Running,
    Stopped,
}

/// How a run is going, for the window and for checking long runs.
#[derive(Clone, Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub phase: Phase,
    pub output: Option<String>,
    pub frames: u64,
    pub late: u64,
    pub failed_pages: u64,
    pub slowest_ms: f64,
    pub seconds: f64,
    /// Share of columns reaching each row on average, from bottom to top.
    pub rows_lit: [f64; ROWS.len()],
    /// What the keyboard last reported by itself while frames were going.
    pub keyboard_effect: Option<String>,
    pub look: Look,
    /// The effect shortcut ended the run, rather than Stop.
    pub ended_by_keyboard: bool,
    pub problem: Option<String>,
}

pub struct Running {
    stop: Arc<AtomicBool>,
    report: Arc<Mutex<Report>>,
    look: Arc<Mutex<Look>>,
    thread: JoinHandle<()>,
}

impl Running {
    /// Starts capture, then bars. Checks that the keyboard's lighting can be restored before
    /// anything is changed, and leaves the keyboard alone if capture doesn't start.
    pub fn start(device: HidDevice, product_id: u16, style: Style) -> Result<Self, String> {
        let status = ronin::read_status(&device, product_id)?;
        let restore = ronin::restore_change(&status)?;
        let look = Look {
            colour: status.colour,
            brightness: status.brightness,
            direction: status.direction,
            style,
        };
        let stop = Arc::new(AtomicBool::new(false));
        let report = Arc::new(Mutex::new(Report {
            look,
            ..Report::default()
        }));
        let look = Arc::new(Mutex::new(look));
        let thread = {
            let (stop, report, look) = (Arc::clone(&stop), Arc::clone(&report), Arc::clone(&look));
            thread::spawn(move || run(&device, product_id, restore, &look, &stop, &report))
        };
        Ok(Self {
            stop,
            report,
            look,
            thread,
        })
    }

    /// Changes the bars' look. It stays with the bars and never goes to the keyboard, because
    /// a lighting command would end Music Rhythm mode.
    pub fn set_look(&self, look: Look) -> Result<(), String> {
        if !(1..=ronin::LEVEL_MAX).contains(&look.brightness)
            || look.colour > ronin::COLOUR_MAX
            || look.direction > ronin::DIRECTION_MAX
        {
            return Err("That brightness, colour or direction isn't available.".to_string());
        }
        *self.look.lock().unwrap_or_else(PoisonError::into_inner) = look;
        Ok(())
    }

    pub fn is_finished(&self) -> bool {
        self.thread.is_finished()
    }

    /// Asks the bars to stop without waiting. The report shows when they have.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn report(&self) -> Report {
        self.report
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Stops the bars and waits for the keyboard's lighting to be restored.
    pub fn stop(self) -> Report {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.thread.join();
        self.report
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

fn run(
    device: &HidDevice,
    product_id: u16,
    restore: ronin::Change,
    look: &Mutex<Look>,
    stop: &AtomicBool,
    report: &Mutex<Report>,
) {
    let update = |change: &mut dyn FnMut(&mut Report)| {
        change(&mut report.lock().unwrap_or_else(PoisonError::into_inner))
    };
    let problem = match prepare_source(stop) {
        Err(problem) => Some(problem),
        Ok(None) => None,
        Ok(Some(source)) => {
            let streamed = stream(device, product_id, &source, look, stop, &update);
            // The keyboard has been in Music Rhythm mode from here on, so always put it back.
            // Looks chosen during the run were for the bars only.
            let left = matches!(streamed, Ok(true));
            // From Music Rhythm the effect shortcut always lands on the first effect, so move
            // on from the effect the bars started on instead.
            let change = if left {
                restore.with_next_effect()
            } else {
                restore
            };
            let restored = ronin::apply(device, product_id, change).map(|_| ());
            update(&mut |report| report.ended_by_keyboard = left);
            streamed.map(|_| ()).and(restored).err()
        }
    };
    update(&mut |report| {
        report.phase = Phase::Stopped;
        report.problem = problem.clone();
    });
}

fn prepare_source(stop: &AtomicBool) -> Result<Option<AudioSource>, String> {
    let source = AudioSource::start(FFT_SIZE)?;
    source.wait_until_ready(stop)?;
    Ok((!stop.load(Ordering::Relaxed)).then_some(source))
}

/// Sends frames until stopped. Returns whether the keyboard left Music Rhythm by itself,
/// after which frames no longer show.
fn stream(
    device: &HidDevice,
    product_id: u16,
    source: &AudioSource,
    look: &Mutex<Look>,
    stop: &AtomicBool,
    update: &dyn Fn(&mut dyn FnMut(&mut Report)),
) -> Result<bool, String> {
    ronin::enter_music_mode(device)?;
    let entered = Instant::now();
    while entered.elapsed() < MUSIC_ENTRY {
        if stop.load(Ordering::Relaxed) {
            return Ok(false);
        }
        sleep(STOP_POLL);
    }
    // Anything queued from before the mode changed would be out of date.
    let _ = ronin::latest_reported(device, product_id);

    let mut spectrum = Spectrum::new();
    let mut lit_total = [0.0; ROWS.len()];
    let (mut frames, mut late, mut failed_total, mut failed_in_a_row) =
        (0_u64, 0_u64, 0_u64, 0_u32);
    let mut slowest = Duration::ZERO;
    // Over the current log period, to show when frames ran late and why.
    let (mut period_late, mut period_slowest, mut period_lateness) =
        (0_u64, Duration::ZERO, Duration::ZERO);
    let mut last_seen = None;
    let current = |look: &Mutex<Look>| *look.lock().unwrap_or_else(PoisonError::into_inner);
    let first = current(look);
    eprintln!(
        "bars started: brightness {}, colour {}, direction {}",
        first.brightness, first.colour, first.direction
    );
    let mut fade = Fade::new(first);
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1, |time| time.as_nanos() as u64);
    let mut drawer = Drawer::new(seed);
    // The keyboard keeps reporting its own value, which a choice in the window doesn't change,
    // so only a change in what it reports means the shortcut was pressed.
    let mut reported_brightness = first.brightness;
    let started = Instant::now();
    let mut next_log = started + LOG_EVERY;
    let mut due = started;
    let _precise_timing = PreciseTiming::begin();
    update(&mut |report| report.phase = Phase::Running);
    while !stop.load(Ordering::Relaxed) {
        let now = Instant::now();
        if due > now {
            sleep(due - now);
        } else {
            late += 1;
            period_late += 1;
            period_lateness = period_lateness.max(now - due);
        }
        due = next_due(due, now);
        let levels = *spectrum.update(&source.latest(), source.rate());
        let sent_at = Instant::now();
        let chosen = current(look);
        let failed = ronin::send_frame(
            device,
            &drawer.frame(chosen.style, &levels, fade.next(chosen)),
        );
        slowest = slowest.max(sent_at.elapsed());
        period_slowest = period_slowest.max(sent_at.elapsed());
        failed_total += u64::from(failed);
        failed_in_a_row = if failed == 0 {
            0
        } else {
            failed_in_a_row + failed
        };
        frames += 1;
        for (total, lit) in lit_total.iter_mut().zip(rows_lit(&levels)) {
            *total += lit;
        }
        let seen = ronin::latest_reported(device, product_id).ok().flatten();
        if let Some(status) = &seen {
            if status.brightness != reported_brightness
                && (1..=ronin::LEVEL_MAX).contains(&status.brightness)
            {
                reported_brightness = status.brightness;
                look.lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .brightness = status.brightness;
            }
            let values = (
                status.effect.clone(),
                status.brightness,
                status.colour,
                status.direction,
            );
            if last_seen.as_ref() != Some(&values) {
                eprintln!(
                    "keyboard reported: effect {}, brightness {}, colour {}, direction {}",
                    values.0, values.1, values.2, values.3
                );
                last_seen = Some(values);
            }
            if status.effect_number.is_some() {
                return Ok(true);
            }
        }
        update(&mut |report| {
            report.output = source.device();
            report.frames = frames;
            report.late = late;
            report.failed_pages = failed_total;
            report.slowest_ms = slowest.as_secs_f64() * 1000.0;
            report.seconds = started.elapsed().as_secs_f64();
            report.rows_lit = lit_total.map(|total| total / frames as f64);
            report.look = current(look);
            if let Some(status) = &seen {
                report.keyboard_effect = Some(status.effect.clone());
            }
        });
        if Instant::now() >= next_log {
            next_log += LOG_EVERY;
            eprintln!(
                "bars {:.0} s: {frames} frames, {late} late, {failed_total} pages failed; this period {period_late} late, latest by {:.0} ms, slowest send {:.0} ms",
                started.elapsed().as_secs_f64(),
                period_lateness.as_secs_f64() * 1000.0,
                period_slowest.as_secs_f64() * 1000.0,
            );
            (period_late, period_slowest, period_lateness) = (0, Duration::ZERO, Duration::ZERO);
        }
        if failed_in_a_row >= FAILED_PAGES_LIMIT {
            return Err("The keyboard stopped taking frames. Check its connection.".to_string());
        }
    }
    Ok(false)
}

/// Prints audio levels without touching the keyboard, to compare with what the bars would do.
pub fn run_meter(seconds: u64) -> Result<(), String> {
    let never = AtomicBool::new(false);
    let source = AudioSource::start(FFT_SIZE)?;
    source.wait_until_ready(&never)?;
    println!(
        "capturing from {} at {} Hz",
        source.device().unwrap_or_default(),
        source.rate()
    );
    let mut spectrum = Spectrum::new();
    let mut lit_total = [0.0; ROWS.len()];
    let mut highest = 0.0_f32;
    let mut frames = 0_u32;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(seconds) {
        sleep(Duration::from_secs(1) / FPS);
        let samples = source.latest();
        let levels = *spectrum.update(&samples, source.rate());
        highest = samples
            .iter()
            .fold(highest, |high, sample| high.max(sample.abs()));
        for (total, lit) in lit_total.iter_mut().zip(rows_lit(&levels)) {
            *total += lit;
        }
        frames += 1;
        if frames % (FPS / 2) == 0 {
            let meter: String = levels
                .iter()
                .map(|level| b" .:-=+*#%@"[((level * 10.0) as usize).min(9)] as char)
                .collect();
            println!("{:5.1} s  {meter}", started.elapsed().as_secs_f64());
        }
    }
    let average = lit_total.map(|total| format!("{:.2}", total / f64::from(frames)));
    println!("highest peak: {highest:.4}");
    println!("row lit, bottom to top: {}", average.join(" "));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_samples(frame: u32) -> Vec<f32> {
        let frame = f64::from(frame);
        let amplitude = 0.2 + 0.15 * (frame * 0.7).sin();
        (0..FFT_SIZE)
            .map(|n| {
                let time = 2.0 * PI * n as f64 / 44100.0;
                (amplitude * (220.0 * time + frame * 0.3).sin()
                    + 0.1 * (frame % 3.0) * (3000.0 * time).sin()
                    + 0.02 * (11000.0 * time + frame).sin()) as f32
            })
            .collect()
    }

    fn assert_close(actual: &Levels, expected: &Levels) {
        for (column, (actual, expected)) in actual.iter().zip(expected).enumerate() {
            assert!(
                (actual - expected).abs() < 1e-5,
                "column {column}: {actual} against {expected}"
            );
        }
    }

    // Reference values, worked out separately from the same signal.
    #[test]
    fn levels_match_reference_values() {
        let mut spectrum = Spectrum::new();
        for frame in 0..=20 {
            let levels = *spectrum.update(&test_samples(frame), 44100);
            if frame == 0 {
                assert_close(&levels, &[0.0; COLUMNS]);
            }
            if frame == 5 {
                assert_close(
                    &levels,
                    &[
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                        0.0,
                        0.3519942857,
                        1.0,
                        4.8322e-06,
                        0.0,
                        0.0,
                    ],
                );
            }
            if frame == 20 {
                assert_close(
                    &levels,
                    &[
                        0.0995823023,
                        0.1427922769,
                        0.2374479282,
                        0.8660359981,
                        0.8661113017,
                        0.2885797535,
                        0.0814413275,
                        0.0514899624,
                        0.0234449939,
                        0.3510816223,
                        1.0,
                        7.88564e-05,
                        0.0,
                        0.0,
                    ],
                );
            }
        }
    }

    fn led(frame: &ronin::Frame, led: usize) -> [u8; 3] {
        frame[3 * led..3 * led + 3].try_into().unwrap()
    }

    #[test]
    fn bars_rise_from_the_bottom_row() {
        let levels: Levels = std::array::from_fn(|column| column as f64 / 13.0);
        let frame = bars_frame(
            &levels,
            &Look {
                colour: 6,
                brightness: 5,
                ..Look::default()
            }
            .paint(),
        );
        // The full column reaches the top row, and the empty one stays dark.
        assert_eq!(led(&frame, 43), [255; 3]);
        assert_eq!(led(&frame, 91), [0; 3]);
        // The bottom row has fewer keys, so its fourth key shares a column further along.
        assert_eq!(led(&frame, 94), [255; 3]);
        // Column 7 ends part-way up its fourth key.
        assert_eq!(led(&frame, 50), [58; 3]);
        assert_eq!(led(&frame, 23), [0; 3]);
    }

    #[test]
    fn presets_one_to_seven_are_solid_whatever_the_direction() {
        for (colour, solid) in SOLID_PRESETS.iter().enumerate() {
            for direction in [0, 1] {
                let look = Look {
                    colour: colour as u8,
                    brightness: 5,
                    direction,
                    ..Look::default()
                };
                assert!((0..COLUMNS).all(|column| look.column_colour(column) == *solid));
            }
        }
    }

    #[test]
    fn direction_flips_the_gradient() {
        let forward = Look {
            colour: 7,
            brightness: 5,
            ..Look::default()
        };
        let backward = Look {
            direction: 1,
            ..forward
        };
        assert_eq!(forward.column_colour(0), GRADIENT[0]);
        assert_eq!(
            forward.column_colour(COLUMNS - 1),
            GRADIENT[GRADIENT.len() - 1]
        );
        for column in 0..COLUMNS {
            assert_eq!(
                backward.column_colour(column),
                forward.column_colour(COLUMNS - 1 - column)
            );
        }
    }

    #[test]
    fn brightness_dims_the_frame() {
        let levels = [1.0; COLUMNS];
        for (brightness, expected) in [(1, 51), (3, 153), (5, 255)] {
            let frame = bars_frame(
                &levels,
                &Look {
                    colour: 0,
                    brightness,
                    ..Look::default()
                }
                .paint(),
            );
            assert_eq!(led(&frame, 43), [expected, 0, 0]);
        }
    }

    #[test]
    fn pulse_lights_every_key_with_the_stronger_of_bass_and_treble() {
        let paint = Look {
            colour: 6,
            brightness: 5,
            ..Look::default()
        }
        .paint();
        let mut levels = [0.0; COLUMNS];
        // Mids alone don't move it.
        levels[BASS_COLUMNS] = 1.0;
        levels[SPARK_FIRST_COLUMN - 1] = 1.0;
        assert_eq!(pulse_frame(&levels, 0.0, &paint), [0; ronin::FRAME_BYTES]);
        levels[1] = 0.5;
        let frame = pulse_frame(&levels, 0.0, &paint);
        for row in ROWS {
            for &slot in row {
                assert_eq!(led(&frame, slot), [127; 3]);
            }
        }
        // A treble hit lifts it past the bass.
        assert_eq!(led(&pulse_frame(&levels, 1.0, &paint), 50), [255; 3]);
    }

    #[test]
    fn pulse_shows_treble_hits_but_not_steady_treble() {
        let mut drawer = Drawer::new(1);
        let mut levels = [0.0; COLUMNS];
        levels[COLUMNS - 1] = 1.0;
        let hit = drawer.frame(Style::Pulse, &levels, &white());
        assert_eq!(led(&hit, 50), [255; 3]);
        let mut steady = hit;
        for _ in 0..200 {
            steady = drawer.frame(Style::Pulse, &levels, &white());
        }
        assert_eq!(steady, [0; ronin::FRAME_BYTES]);
    }

    #[test]
    fn glow_bass_jumps_on_hits_and_settles_when_steady() {
        let mut drawer = Drawer::new(1);
        let steady = bass_hit();
        let hit = drawer.frame(Style::Glow, &steady, &white());
        assert_eq!(led(&hit, 15), [255; 3]);
        let mut settled = hit;
        for _ in 0..200 {
            settled = drawer.frame(Style::Glow, &steady, &white());
        }
        assert_eq!(led(&settled, 15), [76; 3]);
    }

    fn white() -> Paint {
        Look {
            colour: 6,
            brightness: 5,
            ..Look::default()
        }
        .paint()
    }

    fn bass_hit() -> Levels {
        let mut levels = [0.0; COLUMNS];
        levels[0] = 1.0;
        levels
    }

    #[test]
    fn ripple_reaches_the_centre_before_the_edge() {
        let mut drawer = Drawer::new(1);
        let (centre, edge) = (50, 15);
        let mut centre_lit = None;
        let mut edge_lit = None;
        for frame_number in 0..RIPPLE_HISTORY {
            let levels = if frame_number == 0 {
                bass_hit()
            } else {
                [0.0; COLUMNS]
            };
            let frame = drawer.frame(Style::Ripple, &levels, &white());
            if led(&frame, centre) == [255; 3] {
                centre_lit = Some(frame_number);
            }
            if led(&frame, edge) == [255; 3] {
                edge_lit = Some(frame_number);
            }
        }
        assert_eq!(centre_lit, Some(1));
        assert_eq!(edge_lit, Some(10));
    }

    #[test]
    fn ripple_ignores_treble() {
        let mut drawer = Drawer::new(1);
        let mut levels = [0.0; COLUMNS];
        levels[COLUMNS - 1] = 1.0;
        drawer.frame(Style::Ripple, &levels, &white());
        let frame = drawer.frame(Style::Ripple, &[0.0; COLUMNS], &white());
        assert_eq!(frame, [0; ronin::FRAME_BYTES]);
    }

    #[test]
    fn ripple_delays_all_fit_the_history() {
        for (height, row) in ROWS.iter().enumerate() {
            for position in 0..row.len() {
                assert!(
                    (ripple_delay(height, key_column(position, row)) as f64)
                        < RIPPLE_HISTORY as f64 - 1.0
                );
            }
        }
    }

    #[test]
    fn sparkle_lights_some_keys_from_treble_and_they_fade() {
        let mut drawer = Drawer::new(7);
        let mut levels = [0.0; COLUMNS];
        levels[COLUMNS - 1] = 1.0;
        let frame = drawer.frame(Style::Sparkle, &levels, &white());
        let lit = ROWS
            .iter()
            .flat_map(|row| row.iter())
            .filter(|&&slot| led(&frame, slot) == [255; 3])
            .count();
        assert!((1..=2).contains(&lit), "{lit}");
        let quiet = [0.0; COLUMNS];
        let mut frame = frame;
        for _ in 0..20 {
            frame = drawer.frame(Style::Sparkle, &quiet, &white());
        }
        assert_eq!(frame, [0; ronin::FRAME_BYTES]);
    }

    #[test]
    fn sparkle_has_a_dim_pulse_underneath() {
        let mut drawer = Drawer::new(7);
        let frame = drawer.frame(Style::Sparkle, &bass_hit(), &white());
        assert_eq!(led(&frame, 50), [89; 3]);
    }

    #[test]
    fn glow_gives_each_column_its_own_level() {
        let paint = Look {
            colour: 6,
            brightness: 5,
            ..Look::default()
        }
        .paint();
        let mut levels = [0.0; COLUMNS];
        levels[COLUMNS - 1] = 1.0;
        levels[0] = 0.5;
        let frame = glow_frame(&levels, &[0.0; COLUMNS], &paint);
        // The top row's slots run from column 0 to the last column. Column 0 is bass.
        assert_eq!(led(&frame, 15), [38; 3]);
        assert_eq!(led(&frame, 43), [255; 3]);
        assert_eq!(led(&frame, 10), [0; 3]);
    }

    #[test]
    fn pulse_follows_the_preset_and_brightness() {
        let levels = [1.0; COLUMNS];
        let red = pulse_frame(
            &levels,
            0.0,
            &Look {
                colour: 0,
                brightness: 1,
                ..Look::default()
            }
            .paint(),
        );
        assert_eq!(led(&red, 43), [51, 0, 0]);
        let gradient = Look {
            colour: 7,
            brightness: 5,
            ..Look::default()
        };
        let flipped = Look {
            direction: 1,
            ..gradient
        };
        assert_eq!(
            led(&pulse_frame(&levels, 0.0, &gradient.paint()), 15),
            [255, 0, 0]
        );
        assert_eq!(
            led(&pulse_frame(&levels, 0.0, &flipped.paint()), 15),
            [255, 0, 255]
        );
    }

    #[test]
    fn a_new_look_fades_in() {
        let red = Look {
            colour: 0,
            brightness: 5,
            ..Look::default()
        };
        let blue = Look { colour: 2, ..red };
        let mut fade = Fade::new(red);
        assert_eq!(fade.next(red).colours[0], [255.0, 0.0, 0.0]);
        let first = fade.next(blue).colours[0];
        assert!(
            first[0] > 200.0 && first[2] > 0.0 && first[2] < 50.0,
            "{first:?}"
        );
        for _ in 1..FADE_FRAMES {
            fade.next(blue);
        }
        assert_eq!(fade.next(blue).colours[0], [0.0, 0.0, 255.0]);
    }

    #[test]
    fn a_change_mid_fade_starts_from_what_is_showing() {
        let red = Look {
            colour: 0,
            brightness: 5,
            ..Look::default()
        };
        let blue = Look { colour: 2, ..red };
        let green = Look { colour: 1, ..red };
        let mut fade = Fade::new(red);
        for _ in 0..4 {
            fade.next(blue);
        }
        let first = fade.next(green).colours[0];
        assert!(
            first[0] > 0.0 && first[1] > 0.0 && first[2] > 0.0,
            "{first:?}"
        );
    }

    #[test]
    fn silence_lets_the_bars_fall() {
        let mut spectrum = Spectrum::new();
        spectrum.update(&test_samples(5), 44100);
        let quiet = vec![0.0; FFT_SIZE];
        for _ in 0..20 {
            spectrum.update(&quiet, 44100);
        }
        assert_eq!(spectrum.levels, [0.0; COLUMNS]);
    }

    #[test]
    fn low_sample_rates_stay_inside_the_spectrum() {
        let mut spectrum = Spectrum::new();
        let levels = spectrum.update(&test_samples(1), 8000);
        assert!(levels.iter().all(|level| level.is_finite()));
    }

    #[test]
    fn small_delays_are_made_up_but_long_ones_are_not() {
        let due = Instant::now();
        assert_eq!(next_due(due, due), due + FRAME_GAP);
        let slightly_late = due + Duration::from_millis(20);
        assert_eq!(next_due(due, slightly_late), due + FRAME_GAP);
        let very_late = due + Duration::from_secs(60);
        assert_eq!(next_due(due, very_late), very_late + FRAME_GAP);
    }

    #[test]
    fn rows_lit_counts_columns_reaching_each_row() {
        let mut levels = [0.0; COLUMNS];
        levels[0] = 1.0;
        levels[1] = 0.2;
        let lit = rows_lit(&levels);
        assert!((lit[0] - 2.0 / 14.0).abs() < 1e-9);
        assert!((lit[1] - 1.0 / 14.0).abs() < 1e-9);
        assert!((lit[5] - 1.0 / 14.0).abs() < 1e-9);
    }
}
