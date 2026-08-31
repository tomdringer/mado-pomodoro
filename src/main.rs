// mado-pomodoro — Pomodoro timer pixel plugin for Mado

use std::f32::consts::PI;
use std::io::{BufRead, BufReader, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
#[serde(default)]
struct Config {
    work_mins:                  u32,
    short_break_mins:           u32,
    long_break_mins:            u32,
    sessions_before_long_break: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            work_mins:                  25,
            short_break_mins:            5,
            long_break_mins:            15,
            sessions_before_long_break:  4,
        }
    }
}

impl Config {
    fn load() -> Self {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        let path = std::path::Path::new(&home)
            .join(".config/mado/plugins/pomodoro.toml");
        let content = std::fs::read_to_string(path).unwrap_or_default();
        toml::from_str(&content).unwrap_or_default()
    }
}

// ── Palette ───────────────────────────────────────────────────────────────────

const BG:           [u8; 4] = [15,  23,  42,  255]; // slate-900
const TEXT:         [u8; 4] = [248, 250, 252, 255]; // slate-50
const DIM:          [u8; 4] = [100, 116, 139, 255]; // slate-500
const WORK_ARC:     [u8; 4] = [99,  102, 241, 255]; // indigo-500
const BREAK_ARC:    [u8; 4] = [52,  211, 153, 255]; // emerald-400
const TRACK:        [u8; 4] = [30,  41,  59,  255]; // slate-800
const PAUSE_DIM:    [u8; 4] = [71,  85,  105, 255]; // slate-600

// ── Font ──────────────────────────────────────────────────────────────────────

fn load_system_font() -> Option<fontdue::Font> {
    let candidates = [
        "/System/Library/Fonts/Helvetica.ttc",
        "/Library/Fonts/Arial.ttf",
        "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
        "/usr/share/fonts/TTF/DejaVuSans.ttf",
        "C:\\Windows\\Fonts\\arial.ttf",
    ];
    for path in &candidates {
        if let Ok(data) = std::fs::read(path) {
            if let Ok(font) = fontdue::Font::from_bytes(
                data.as_slice(), fontdue::FontSettings::default()) {
                return Some(font);
            }
        }
    }
    None
}

// ── Canvas ────────────────────────────────────────────────────────────────────

struct Canvas { pixels: Vec<u8>, w: usize, h: usize }

impl Canvas {
    fn new(w: usize, h: usize) -> Self {
        let mut pixels = vec![0u8; w * h * 4];
        for px in pixels.chunks_exact_mut(4) { px.copy_from_slice(&BG); }
        Canvas { pixels, w, h }
    }

    fn blend(&mut self, x: isize, y: isize, color: [u8; 4], alpha: f32) {
        if x < 0 || y < 0 || x >= self.w as isize || y >= self.h as isize { return; }
        let i = (y as usize * self.w + x as usize) * 4;
        let ia = 1.0 - alpha;
        for c in 0..3 {
            self.pixels[i + c] =
                (self.pixels[i + c] as f32 * ia + color[c] as f32 * alpha).round() as u8;
        }
        self.pixels[i + 3] = 255;
    }

    fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: [u8; 4]) {
        for row in y..(y + h).min(self.h) {
            for col in x..(x + w).min(self.w) {
                let i = (row * self.w + col) * 4;
                self.pixels[i..i + 4].copy_from_slice(&color);
            }
        }
    }

    /// Draw a thick arc. Angles in radians, 0 = top, clockwise.
    fn arc(&mut self, cx: f32, cy: f32, radius: f32, thickness: f32,
           start_angle: f32, end_angle: f32, color: [u8; 4]) {
        let steps = (radius * 2.0 * PI) as usize * 4;
        let steps = steps.max(360);
        // Normalise so we always go clockwise from start to end
        let span = if end_angle >= start_angle {
            end_angle - start_angle
        } else {
            2.0 * PI - (start_angle - end_angle)
        };
        for i in 0..=steps {
            let t = i as f32 / steps as f32;
            let angle = start_angle + t * span;
            for r in 0..=(thickness as usize * 2) {
                let r_off = r as f32 / 2.0 - thickness / 2.0;
                let r_act = radius + r_off;
                let x = cx + angle.sin() * r_act;
                let y = cy - angle.cos() * r_act;
                // Simple anti-alias based on distance from ideal radius
                let dist = r_off.abs();
                let alpha = (1.0 - (dist / (thickness / 2.0 + 0.5)).powi(2)).max(0.0);
                self.blend(x.round() as isize, y.round() as isize, color, alpha);
            }
        }
    }

    fn text(&mut self, font: &fontdue::Font, text: &str,
            size: f32, x: usize, y: usize, color: [u8; 4]) -> usize {
        let mut cx = x;
        for ch in text.chars() {
            let (m, bmp) = font.rasterize(ch, size);
            let gx = cx as isize + m.xmin as isize;
            let gy = y as isize - m.height as isize - m.ymin as isize;
            for (k, &cov) in bmp.iter().enumerate() {
                if cov == 0 { continue; }
                let px = gx + (k % m.width) as isize;
                let py = gy + (k / m.width) as isize;
                self.blend(px, py, color, cov as f32 / 255.0);
            }
            cx += m.advance_width.round() as usize;
        }
        cx
    }

    fn measure(font: &fontdue::Font, text: &str, size: f32) -> usize {
        text.chars().map(|ch| {
            let (m, _) = font.rasterize(ch, size);
            m.advance_width.round() as usize
        }).sum()
    }

    fn text_centered(&mut self, font: &fontdue::Font, text: &str,
                     size: f32, y: usize, color: [u8; 4]) {
        let tw = Self::measure(font, text, size);
        let x = self.w.saturating_sub(tw) / 2;
        self.text(font, text, size, x, y, color);
    }

    fn write_frame(&self, out: &mut impl Write) {
        out.write_all(b"MADO").unwrap();
        out.write_all(&(self.w as u32).to_le_bytes()).unwrap();
        out.write_all(&(self.h as u32).to_le_bytes()).unwrap();
        out.write_all(&self.pixels).unwrap();
        out.flush().unwrap();
    }
}

// ── Timer state ───────────────────────────────────────────────────────────────

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase { Work, ShortBreak, LongBreak }

impl Phase {
    fn label(&self) -> &'static str {
        match self {
            Phase::Work       => "FOCUS",
            Phase::ShortBreak => "SHORT BREAK",
            Phase::LongBreak  => "LONG BREAK",
        }
    }
    fn arc_color(&self) -> [u8; 4] {
        match self {
            Phase::Work => WORK_ARC,
            _           => BREAK_ARC,
        }
    }
}

struct Timer {
    phase:           Phase,
    total_secs:      u32,
    remaining_secs:  u32,
    running:         bool,
    last_tick:       Option<Instant>,
    sessions_done:   u32,          // completed work sessions
    cfg:             Config,
}

impl Timer {
    fn new(cfg: Config) -> Self {
        let total = cfg.work_mins * 60;
        Timer {
            phase: Phase::Work,
            total_secs: total,
            remaining_secs: total,
            running: false,
            last_tick: None,
            sessions_done: 0,
            cfg,
        }
    }

    fn tick(&mut self) {
        if !self.running { return; }
        let now = Instant::now();
        if let Some(last) = self.last_tick {
            let elapsed = now.duration_since(last).as_secs() as u32;
            if elapsed > 0 {
                self.last_tick = Some(now);
                if elapsed >= self.remaining_secs {
                    self.remaining_secs = 0;
                    self.running = false;
                    self.on_complete();
                } else {
                    self.remaining_secs -= elapsed;
                }
            }
        } else {
            self.last_tick = Some(now);
        }
    }

    fn on_complete(&mut self) {
        notify(self.phase.label());
        match self.phase {
            Phase::Work => {
                self.sessions_done += 1;
                if self.sessions_done % self.cfg.sessions_before_long_break == 0 {
                    self.start_phase(Phase::LongBreak);
                } else {
                    self.start_phase(Phase::ShortBreak);
                }
            }
            _ => self.start_phase(Phase::Work),
        }
    }

    fn start_phase(&mut self, phase: Phase) {
        self.phase = phase;
        self.total_secs = match phase {
            Phase::Work       => self.cfg.work_mins * 60,
            Phase::ShortBreak => self.cfg.short_break_mins * 60,
            Phase::LongBreak  => self.cfg.long_break_mins * 60,
        };
        self.remaining_secs = self.total_secs;
        self.running = false;
        self.last_tick = None;
    }

    fn toggle(&mut self) {
        self.running = !self.running;
        if self.running {
            self.last_tick = Some(Instant::now());
        }
    }

    fn reset(&mut self) {
        self.remaining_secs = self.total_secs;
        self.running = false;
        self.last_tick = None;
    }

    fn progress(&self) -> f32 {
        if self.total_secs == 0 { return 0.0; }
        1.0 - self.remaining_secs as f32 / self.total_secs as f32
    }
}

fn notify(phase_label: &str) {
    let msg = match phase_label {
        "FOCUS"       => "Time's up! Take a break.",
        "SHORT BREAK" => "Break over. Back to work!",
        "LONG BREAK"  => "Long break done. Let's go!",
        _             => "Timer complete.",
    };
    let script = format!(
        "display notification \"{}\" with title \"Mado Pomodoro\"", msg);
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .spawn();
}

// ── Protocol events ───────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind:   String,
    width:  Option<u32>,
    height: Option<u32>,
    x:      Option<f32>,
    y:      Option<f32>,
    text:   Option<String>,
}

// ── Rendering ─────────────────────────────────────────────────────────────────

fn render(timer: &Timer, font: &fontdue::Font, w: usize, h: usize, out: &mut impl Write) {
    let mut canvas = Canvas::new(w, h);

    let cx = w as f32 / 2.0;
    let radius = (w as f32 * 0.32).clamp(50.0, 110.0);
    let thickness = (w as f32 * 0.045).clamp(6.0, 14.0);
    let circle_top = h as f32 * 0.14;
    let cy = circle_top + radius;

    // Track ring
    canvas.arc(cx, cy, radius, thickness, 0.0, 2.0 * PI, TRACK);

    // Progress arc
    let progress = timer.progress();
    if progress > 0.001 {
        let arc_color = if timer.running { timer.phase.arc_color() }
                        else { PAUSE_DIM };
        canvas.arc(cx, cy, radius, thickness, 0.0, progress * 2.0 * PI, arc_color);
    }

    // Time text inside circle
    let mins = timer.remaining_secs / 60;
    let secs = timer.remaining_secs % 60;
    let time_str = format!("{:02}:{:02}", mins, secs);
    let time_size = (w as f32 * 0.16).clamp(20.0, 44.0);
    let time_color = if timer.running { TEXT } else { DIM };
    let time_y = (cy + time_size * 0.35) as usize;
    canvas.text_centered(font, &time_str, time_size, time_y, time_color);

    // Phase label
    let label_size = (w as f32 * 0.065).clamp(9.0, 14.0);
    let label_y = (cy + radius + label_size * 1.8) as usize;
    canvas.text_centered(font, timer.phase.label(), label_size, label_y, DIM);

    // Hint text at bottom (anchored first so dots sit above it)
    let hint = if timer.running { "space / click to pause" } else { "space / click to start" };
    let hint_size = (w as f32 * 0.055).clamp(8.0, 11.0);
    let hint_y = h - (h as f32 * 0.06) as usize;
    canvas.text_centered(font, hint, hint_size, hint_y, PAUSE_DIM);

    // Reset label bottom-right (same row as hint)
    let reset_str = "reset";
    let reset_x = w - Canvas::measure(font, reset_str, hint_size) - 10;
    canvas.text(font, reset_str, hint_size, reset_x, hint_y, PAUSE_DIM);

    // Session counter dots — pinned just above the hint text
    let sessions_before = timer.cfg.sessions_before_long_break;
    let session_in_cycle = timer.sessions_done % sessions_before;
    let dot_r = (w as f32 * 0.025).clamp(3.0, 6.0);
    let dot_gap = dot_r * 2.8;
    let total_dot_w = sessions_before as f32 * dot_gap - (dot_gap - dot_r * 2.0);
    let dot_start_x = (w as f32 - total_dot_w) / 2.0 + dot_r;
    let dot_y_f = hint_y as f32 - dot_r * 2.0 - hint_size * 1.2;
    for i in 0..sessions_before {
        let dx = dot_start_x + i as f32 * dot_gap;
        let filled = i < session_in_cycle;
        // Draw filled or outlined dot
        for py in (dot_y_f - dot_r) as isize..=(dot_y_f + dot_r) as isize {
            for px in (dx - dot_r) as isize..=(dx + dot_r) as isize {
                let dist = (((px as f32 - dx).powi(2) + (py as f32 - dot_y_f).powi(2)) as f32).sqrt();
                if filled {
                    let alpha = (1.0 - ((dist - dot_r + 0.5) / 0.5).max(0.0)).clamp(0.0, 1.0);
                    if alpha > 0.0 { canvas.blend(px, py, DIM, alpha); }
                } else {
                    let ring = (dist - (dot_r - 1.0)).abs();
                    let alpha = (1.0 - ring).clamp(0.0, 1.0);
                    if alpha > 0.0 { canvas.blend(px, py, DIM, alpha); }
                }
            }
        }
    }


    canvas.write_frame(out);
}

// ── Main ──────────────────────────────────────────────────────────────────────

fn main() {
    let cfg  = Config::load();
    let font = load_system_font().unwrap_or_else(|| {
        eprintln!("mado-pomodoro: no system font found");
        std::process::exit(1);
    });

    let dims:  Arc<Mutex<(u32, u32)>> = Arc::new(Mutex::new((300, 400)));
    let timer: Arc<Mutex<Timer>>      = Arc::new(Mutex::new(Timer::new(cfg)));

    // Stdin event listener
    {
        let dims  = Arc::clone(&dims);
        let timer = Arc::clone(&timer);
        std::thread::spawn(move || {
            let stdin = std::io::stdin();
            for line in BufReader::new(stdin.lock()).lines().flatten() {
                if let Ok(ev) = serde_json::from_str::<Event>(&line) {
                    match ev.kind.as_str() {
                        "resize" => {
                            if let (Some(w), Some(h)) = (ev.width, ev.height) {
                                if w > 0 && h > 0 { *dims.lock().unwrap() = (w, h); }
                            }
                        }
                        "click" => {
                            if let (Some(x), Some(y)) = (ev.x, ev.y) {
                                let (w, h) = *dims.lock().unwrap();
                                let mut t = timer.lock().unwrap();
                                // Reset zone: bottom-right corner
                                let reset_zone_x = w as f32 * 0.55;
                                let reset_zone_y = h as f32 * 0.88;
                                if x > reset_zone_x && y > reset_zone_y {
                                    t.reset();
                                } else {
                                    t.toggle();
                                }
                            }
                        }
                        "key" => {
                            if let Some(text) = &ev.text {
                                let mut t = timer.lock().unwrap();
                                match text.as_str() {
                                    "\r" | "\n" | " " => t.toggle(),
                                    "r" | "R"         => t.reset(),
                                    _ => {}
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        });
    }

    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout.lock());

    loop {
        {
            let mut t = timer.lock().unwrap();
            t.tick();
            let (w, h) = *dims.lock().unwrap();
            render(&t, &font, w as usize, h as usize, &mut out);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_cfg() -> Config {
        Config::default()
    }

    fn make_timer() -> Timer {
        Timer::new(default_cfg())
    }

    // ── new() ─────────────────────────────────────────────────────────────────

    #[test]
    fn new_starts_paused_in_work_phase() {
        let t = make_timer();
        assert!(!t.running);
        assert_eq!(t.phase, Phase::Work);
        assert_eq!(t.remaining_secs, 25 * 60);
        assert_eq!(t.total_secs, 25 * 60);
        assert_eq!(t.sessions_done, 0);
        assert!(t.last_tick.is_none());
    }

    // ── toggle() ──────────────────────────────────────────────────────────────

    #[test]
    fn toggle_starts_timer() {
        let mut t = make_timer();
        t.toggle();
        assert!(t.running);
        assert!(t.last_tick.is_some());
    }

    #[test]
    fn toggle_twice_stops_timer() {
        let mut t = make_timer();
        t.toggle();
        t.toggle();
        assert!(!t.running);
    }

    // ── reset() ───────────────────────────────────────────────────────────────

    #[test]
    fn reset_restores_full_duration() {
        let mut t = make_timer();
        t.remaining_secs = 100;
        t.running = true;
        t.reset();
        assert_eq!(t.remaining_secs, t.total_secs);
        assert!(!t.running);
        assert!(t.last_tick.is_none());
    }

    // ── progress() ────────────────────────────────────────────────────────────

    #[test]
    fn progress_zero_at_start() {
        let t = make_timer();
        assert!((t.progress() - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn progress_one_when_complete() {
        let mut t = make_timer();
        t.remaining_secs = 0;
        assert!((t.progress() - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn progress_half_at_midpoint() {
        let mut t = make_timer();
        t.remaining_secs = t.total_secs / 2;
        assert!((t.progress() - 0.5).abs() < 0.01);
    }

    // ── phase transitions ─────────────────────────────────────────────────────

    #[test]
    fn first_work_completion_goes_to_short_break() {
        let mut t = make_timer();
        t.remaining_secs = 0;
        t.running = false;
        t.on_complete();
        assert_eq!(t.phase, Phase::ShortBreak);
        assert_eq!(t.sessions_done, 1);
        assert_eq!(t.remaining_secs, 5 * 60);
    }

    #[test]
    fn fourth_work_completion_goes_to_long_break() {
        let mut t = make_timer();
        t.sessions_done = 3; // about to complete 4th
        t.on_complete();
        assert_eq!(t.phase, Phase::LongBreak);
        assert_eq!(t.sessions_done, 4);
        assert_eq!(t.remaining_secs, 15 * 60);
    }

    #[test]
    fn short_break_completion_returns_to_work() {
        let mut t = make_timer();
        t.phase = Phase::ShortBreak;
        t.total_secs = 5 * 60;
        t.remaining_secs = 0;
        t.on_complete();
        assert_eq!(t.phase, Phase::Work);
        assert_eq!(t.remaining_secs, 25 * 60);
    }

    #[test]
    fn long_break_completion_returns_to_work() {
        let mut t = make_timer();
        t.phase = Phase::LongBreak;
        t.total_secs = 15 * 60;
        t.on_complete();
        assert_eq!(t.phase, Phase::Work);
    }

    // ── Phase enum ────────────────────────────────────────────────────────────

    #[test]
    fn phase_labels() {
        assert_eq!(Phase::Work.label(),       "FOCUS");
        assert_eq!(Phase::ShortBreak.label(), "SHORT BREAK");
        assert_eq!(Phase::LongBreak.label(),  "LONG BREAK");
    }

    #[test]
    fn work_arc_color_differs_from_break() {
        assert_ne!(Phase::Work.arc_color(), Phase::ShortBreak.arc_color());
        assert_eq!(Phase::ShortBreak.arc_color(), Phase::LongBreak.arc_color());
    }

    // ── Config defaults ───────────────────────────────────────────────────────

    #[test]
    fn default_config_standard_pomodoro_durations() {
        let cfg = Config::default();
        assert_eq!(cfg.work_mins, 25);
        assert_eq!(cfg.short_break_mins, 5);
        assert_eq!(cfg.long_break_mins, 15);
        assert_eq!(cfg.sessions_before_long_break, 4);
    }
}
