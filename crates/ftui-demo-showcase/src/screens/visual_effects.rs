#![forbid(unsafe_code)]

//! Mind-blowing visual effects screen.
//!
//! Showcases advanced rendering techniques using Braille characters:
//! - Metaballs with organic glow and color mixing
//! - 3D rotating wireframes with depth and starfield
//! - Psychedelic plasma with multiple palettes
//! - Particle fireworks with explosions and trails
//! - Matrix digital rain
//! - Tunnel zoom effect
//! - Fire simulation

use std::cell::{Cell, RefCell};
use std::collections::{HashSet, VecDeque};
use std::env;
use std::f64::consts::TAU;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use ftui_core::event::{Event, KeyCode, KeyEvent, KeyEventKind, MouseButton, MouseEventKind};
use ftui_core::geometry::Rect;
use ftui_extras::canvas::{CanvasRef, Mode, Painter};
use ftui_extras::markdown::render_markdown;
use ftui_extras::text_effects::{
    AsciiArtStyle, AsciiArtText, ColorGradient, Direction, Easing, Reflection, StyledMultiLine,
    StyledText, TextEffect, TransitionState,
};
use ftui_extras::visual_fx::{
    FxQuality, MetaballsCanvasAdapter, PlasmaCanvasAdapter, PlasmaPalette, ThemeInputs,
};
use ftui_layout::{Constraint, Flex};
use ftui_render::cell::PackedRgba;
use ftui_render::frame::Frame;
use ftui_runtime::Cmd;
use ftui_style::Style;
use ftui_text::text::Text;
use ftui_text::{WrapMode, display_width, truncate_to_width};
use ftui_widgets::Widget;
use ftui_widgets::block::{Alignment, Block};
use ftui_widgets::borders::{BorderType, Borders};
use ftui_widgets::paragraph::Paragraph;

use super::{HelpEntry, Screen};
use crate::theme;

mod three_d_data {
    include!("3d_data.rs");
}

use three_d_data::{
    FREEDOOM_E1M1_LINES, FREEDOOM_E1M1_PLAYER_START, QUAKE_E1M1_TRIS, QUAKE_E1M1_VERTS,
};

const MARKDOWN_OVERLAY: &str = r#"# FrankenTUI Visual FX

This panel is **real markdown** rendered on top of animated backdrops.

- deterministic output
- alpha-correct compositing
- crisp text over motion

`←/→` switch effects · `p` palette · `t` text mode
"#;

/// Visual effects demo screen.
pub struct VisualEffectsScreen {
    /// Current effect being displayed.
    effect: EffectType,
    /// Animation frame counter.
    frame: u64,
    /// Global time for animations.
    time: f64,
    /// Metaballs canvas adapter (high-res via Braille).
    metaballs_adapter: RefCell<MetaballsCanvasAdapter>,
    /// 3D shape state.
    shape3d: Shape3DState,
    /// Plasma canvas adapter (high-res via Braille).
    plasma_adapter: RefCell<PlasmaCanvasAdapter>,
    /// Current plasma palette.
    plasma_palette: PlasmaPalette,
    /// Particle system state.
    particles: ParticleState,
    /// Matrix rain state.
    matrix: MatrixState,
    /// Tunnel state.
    tunnel: TunnelState,
    /// Fire state.
    fire: FireState,
    /// Reaction-diffusion (Gray-Scott) state.
    reaction_diffusion: ReactionDiffusionState,
    /// Strange attractor state.
    attractor: AttractorState,
    /// Mandelbrot zoom state.
    mandelbrot: MandelbrotState,
    /// Lissajous/harmonograph state.
    lissajous: LissajousState,
    /// Flow field particle state.
    flow_field: FlowFieldState,
    /// Julia set state.
    julia: JuliaState,
    /// Wave interference state.
    wave_interference: WaveInterferenceState,
    /// Spiral galaxy/vortex state.
    spiral: SpiralState,
    /// Spin lattice state.
    spin_lattice: SpinLatticeState,
    /// Doom E1M1 braille automap state (lazy init).
    doom_e1m1: RefCell<Option<DoomE1M1State>>,
    /// Quake E1M1 braille rasterizer state (lazy init).
    quake_e1m1: RefCell<Option<QuakeE1M1State>>,
    // FPS tracking
    /// Frame times for FPS calculation (microseconds).
    frame_times: VecDeque<u64>,
    /// Last frame instant.
    last_frame: Option<Instant>,
    /// Current FPS.
    fps: f64,
    /// Average frame time in microseconds.
    avg_frame_time_us: f64,
    /// Min frame time (best case).
    min_frame_time_us: f64,
    /// Max frame time (worst case).
    max_frame_time_us: f64,
    /// Transition overlay state for effect changes.
    transition: TransitionState,
    /// Cached painter buffer (grow-only) for canvas rendering.
    painter: RefCell<Painter>,
    /// Last render quality (used to throttle updates).
    last_quality: Cell<FxQuality>,
    /// Last effect that panicked during render (handled on next tick).
    effect_panic: Cell<Option<EffectType>>,
    // Text effects demo (bd-2b82)
    /// Current demo mode: Canvas or TextEffects
    demo_mode: DemoMode,
    /// Text effects demo state
    text_effects: TextEffectsDemo,
    /// Markdown panel rendered over backdrop effects.
    markdown_panel: Text,
    /// Active FPS movement input state (WASD).
    fps_input: FpsInputState,
    /// Last mouse position for FPS-style mouse look.
    fps_last_mouse: Option<(u16, u16)>,
    /// Mouse sensitivity for FPS-style mouse look.
    fps_mouse_sensitivity: f32,
    /// Deterministic mode for harness runs (fixed FPS stats).
    deterministic_mode: bool,
    /// Tick cadence used for deterministic FPS stats.
    deterministic_tick_ms: u64,
}

#[derive(Debug, Default, Clone, Copy)]
struct FpsInputState {
    forward: bool,
    back: bool,
    strafe_left: bool,
    strafe_right: bool,
    turn_left: bool,
    turn_right: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EffectType {
    Metaballs,
    Shape3D,
    Plasma,
    Particles,
    Matrix,
    Tunnel,
    Fire,
    // Mathematical effects
    ReactionDiffusion,
    StrangeAttractor,
    Mandelbrot,
    Lissajous,
    FlowField,
    Julia,
    WaveInterference,
    Spiral,
    SpinLattice,
    DoomE1M1,
    QuakeE1M1,
}

impl EffectType {
    const ALL: &[Self] = &[
        Self::Metaballs,
        Self::Shape3D,
        Self::Plasma,
        Self::Particles,
        Self::Matrix,
        Self::Tunnel,
        Self::Fire,
        Self::ReactionDiffusion,
        Self::StrangeAttractor,
        Self::Mandelbrot,
        Self::Lissajous,
        Self::FlowField,
        Self::Julia,
        Self::WaveInterference,
        Self::Spiral,
        Self::SpinLattice,
        Self::DoomE1M1,
        Self::QuakeE1M1,
    ];

    fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "metaballs" => Some(Self::Metaballs),
            "shape3d" | "shape-3d" | "shape" => Some(Self::Shape3D),
            "plasma" => Some(Self::Plasma),
            "particles" | "fireworks" => Some(Self::Particles),
            "matrix" => Some(Self::Matrix),
            "tunnel" => Some(Self::Tunnel),
            "fire" => Some(Self::Fire),
            "reaction-diffusion" | "reaction_diffusion" | "rd" => Some(Self::ReactionDiffusion),
            "strange-attractor" | "strange_attractor" | "attractor" => Some(Self::StrangeAttractor),
            "mandelbrot" | "mandel" => Some(Self::Mandelbrot),
            "lissajous" | "harmonograph" => Some(Self::Lissajous),
            "flow-field" | "flow_field" => Some(Self::FlowField),
            "julia" => Some(Self::Julia),
            "wave" | "wave-interference" | "wave_interference" => Some(Self::WaveInterference),
            "spiral" | "galaxy" => Some(Self::Spiral),
            "spin-lattice" | "spin_lattice" => Some(Self::SpinLattice),
            "doom" | "doom-e1m1" => Some(Self::DoomE1M1),
            "quake" | "quake-e1m1" | "e1m1" => Some(Self::QuakeE1M1),
            _ => None,
        }
    }

    fn key(self) -> &'static str {
        match self {
            Self::Metaballs => "metaballs",
            Self::Shape3D => "shape3d",
            Self::Plasma => "plasma",
            Self::Particles => "particles",
            Self::Matrix => "matrix",
            Self::Tunnel => "tunnel",
            Self::Fire => "fire",
            Self::ReactionDiffusion => "reaction-diffusion",
            Self::StrangeAttractor => "strange-attractor",
            Self::Mandelbrot => "mandelbrot",
            Self::Lissajous => "lissajous",
            Self::FlowField => "flow-field",
            Self::Julia => "julia",
            Self::WaveInterference => "wave-interference",
            Self::Spiral => "spiral",
            Self::SpinLattice => "spin-lattice",
            Self::DoomE1M1 => "doom-e1m1",
            Self::QuakeE1M1 => "quake-e1m1",
        }
    }

    fn next(self) -> Self {
        let idx = Self::ALL.iter().position(|&e| e == self).unwrap_or(0);
        Self::ALL[(idx + 1) % Self::ALL.len()]
    }

    fn prev(self) -> Self {
        let idx = Self::ALL.iter().position(|&e| e == self).unwrap_or(0);
        Self::ALL[(idx + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    fn name(self) -> &'static str {
        match self {
            Self::Metaballs => "⬤ Metaballs",
            Self::Shape3D => "◇ 3D Shapes",
            Self::Plasma => "≋ Plasma",
            Self::Particles => "✦ Fireworks",
            Self::Matrix => "▓ Matrix Rain",
            Self::Tunnel => "◎ Tunnel",
            Self::Fire => "🔥 Fire",
            Self::ReactionDiffusion => "◉ Gray-Scott",
            Self::StrangeAttractor => "∞ Attractor",
            Self::Mandelbrot => "❋ Mandelbrot",
            Self::Lissajous => "∿ Lissajous",
            Self::FlowField => "〰 Flow Field",
            Self::Julia => "❂ Julia Set",
            Self::WaveInterference => "≈ Wave Interference",
            Self::Spiral => "✦ Spiral Galaxy",
            Self::SpinLattice => "◈ Spin Lattice",
            Self::DoomE1M1 => "⛦ Doom E1M1",
            Self::QuakeE1M1 => "⛧ Quake E1M1",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Metaballs => "Organic blobs with implicit surface rendering and color mixing",
            Self::Shape3D => "Rotating wireframe geometry with perspective projection",
            Self::Plasma => "Psychedelic sine wave interference patterns",
            Self::Particles => "Explosive particle systems with gravity and trails",
            Self::Matrix => "Classic digital rain cascading down the screen",
            Self::Tunnel => "Flying through an infinite vortex tunnel",
            Self::Fire => "Cellular automaton fire simulation rising upward",
            Self::ReactionDiffusion => "Gray-Scott Turing patterns: morphogenesis simulation",
            Self::StrangeAttractor => "Clifford attractor: deterministic chaos with beauty",
            Self::Mandelbrot => "Deep zoom into the infinite fractal boundary",
            Self::Lissajous => "Harmonograph curves from overlapping oscillations",
            Self::FlowField => "Particles dancing through Perlin noise vector fields",
            Self::Julia => "The Mandelbrot's companion with morphing c parameter",
            Self::WaveInterference => "Multiple wave sources creating interference patterns",
            Self::Spiral => "Logarithmic spiral galaxy with rotating star field",
            Self::SpinLattice => "Landau-Lifshitz spin dynamics on a magnetic lattice",
            Self::DoomE1M1 => "First-person raycasted braille renderer (Freedoom E1M1)",
            Self::QuakeE1M1 => "First-person braille rasterizer of Quake's Slipgate Complex",
        }
    }
}

// =============================================================================
// Text Effects Demo (bd-2b82)
// =============================================================================

/// Demo mode: Canvas-based effects vs Text effects
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum DemoMode {
    /// Canvas-based visual effects (metaballs, plasma, etc.)
    #[default]
    Canvas,
    /// Text effects demo (gradients, animations, typography)
    TextEffects,
}

/// Text effects tab categories (1-6 keys to switch)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum TextEffectsTab {
    /// Horizontal, Vertical, Diagonal, Radial, Animated gradients
    #[default]
    Gradients,
    /// Wave, Bounce, Shake, Cascade, Pulse, Breathing
    Animations,
    /// Shadow, Glow, Outline, Mirror, ASCII Art
    Typography,
    /// Glitch, Chromatic, Scanline, Matrix
    SpecialFx,
    /// Preset effect combinations (placeholder for bd-4r5h)
    Presets,
    /// User-toggleable effect combinations
    Combinations,
}

impl TextEffectsTab {
    const ALL: &[Self] = &[
        Self::Gradients,
        Self::Animations,
        Self::Typography,
        Self::SpecialFx,
        Self::Presets,
        Self::Combinations,
    ];

    fn from_key(n: u8) -> Option<Self> {
        match n {
            1 => Some(Self::Gradients),
            2 => Some(Self::Animations),
            3 => Some(Self::Typography),
            4 => Some(Self::SpecialFx),
            5 => Some(Self::Presets),
            6 => Some(Self::Combinations),
            _ => None,
        }
    }

    fn index(self) -> usize {
        Self::ALL.iter().position(|&t| t == self).unwrap_or(0)
    }

    fn name(self) -> &'static str {
        match self {
            Self::Gradients => "Gradients",
            Self::Animations => "Animations",
            Self::Typography => "Typography",
            Self::SpecialFx => "Special FX",
            Self::Presets => "Presets",
            Self::Combinations => "Combos",
        }
    }

    fn effect_count(self) -> usize {
        match self {
            Self::Gradients => 5,    // Horizontal, Vertical, Diagonal, Radial, Animated
            Self::Animations => 6,   // Wave, Bounce, Shake, Cascade, Pulse, OrganicPulse
            Self::Typography => 5,   // Shadow, Glow, Outline, Mirror, ASCII Art
            Self::SpecialFx => 4,    // Glitch, Chromatic, Scanline, Matrix
            Self::Presets => 3,      // Placeholder: neon, retro, elegant
            Self::Combinations => 4, // Custom combo builder slots
        }
    }
}

/// State for the text effects demo
#[derive(Debug)]
struct TextEffectsDemo {
    /// Current tab
    tab: TextEffectsTab,
    /// Current effect index within tab (0..tab.effect_count())
    effect_idx: usize,
    /// Animation time (0.0..1.0 cycles)
    time: f64,
    /// Easing comparison mode
    easing_mode: bool,
    /// Current easing function
    easing: Easing,
    /// Combination mode: which effects are enabled [gradient, animation, typography, specialfx]
    combo_enabled: [bool; 4],
    /// Demo text to display
    demo_text: &'static str,
    /// ASCII art cache
    ascii_cache: Option<AsciiArtText>,
}

impl Default for TextEffectsDemo {
    fn default() -> Self {
        Self {
            tab: TextEffectsTab::Gradients,
            effect_idx: 0,
            time: 0.0,
            easing_mode: false,
            easing: Easing::Linear,
            combo_enabled: [true, false, false, false],
            demo_text: "FrankenTUI",
            ascii_cache: None,
        }
    }
}

impl TextEffectsDemo {
    /// Get the current effect name
    fn current_effect_name(&self) -> &'static str {
        match self.tab {
            TextEffectsTab::Gradients => match self.effect_idx {
                0 => "Horizontal Gradient",
                1 => "Vertical Gradient",
                2 => "Diagonal Gradient",
                3 => "Radial Gradient",
                _ => "Animated Gradient",
            },
            TextEffectsTab::Animations => match self.effect_idx {
                0 => "Wave",
                1 => "Bounce",
                2 => "Shake",
                3 => "Cascade",
                4 => "Pulse",
                _ => "Organic Breathing",
            },
            TextEffectsTab::Typography => match self.effect_idx {
                0 => "Shadow",
                1 => "Glow",
                2 => "Outline",
                3 => "Mirror Reflection",
                _ => "ASCII Art",
            },
            TextEffectsTab::SpecialFx => match self.effect_idx {
                0 => "Glitch",
                1 => "Chromatic Aberration",
                2 => "Scanline",
                _ => "Matrix Style",
            },
            TextEffectsTab::Presets => match self.effect_idx {
                0 => "Neon Sign",
                1 => "Retro Terminal",
                _ => "Elegant Fade",
            },
            TextEffectsTab::Combinations => "Custom Combo",
        }
    }

    /// Get description for current effect
    fn current_effect_description(&self) -> &'static str {
        match self.tab {
            TextEffectsTab::Gradients => match self.effect_idx {
                0 => "Rainbow colors flowing left to right",
                1 => "Gradient transitioning from top to bottom",
                2 => "45° diagonal color sweep",
                3 => "Colors radiating from center outward",
                _ => "Moving rainbow animation",
            },
            TextEffectsTab::Animations => match self.effect_idx {
                0 => "Characters oscillate in a sine wave pattern",
                1 => "Characters bounce with physics simulation",
                2 => "Random jitter for emphasis or alerts",
                3 => "Sequential reveal from direction",
                4 => "Brightness pulsing at steady rate",
                _ => "Natural breathing with asymmetric timing",
            },
            TextEffectsTab::Typography => match self.effect_idx {
                0 => "Drop shadow for depth perception",
                1 => "Neon-style glow around characters",
                2 => "Bold outline for high contrast",
                3 => "Reflected text below baseline",
                _ => "Large block-style ASCII characters",
            },
            TextEffectsTab::SpecialFx => match self.effect_idx {
                0 => "Random character corruption and flicker",
                1 => "RGB channel separation for 3D effect",
                2 => "CRT-style horizontal lines",
                _ => "Digital rain character styling",
            },
            TextEffectsTab::Presets => match self.effect_idx {
                0 => "Glowing neon with pulsing animation",
                1 => "Green phosphor terminal aesthetic",
                _ => "Subtle fade with smooth transitions",
            },
            TextEffectsTab::Combinations => "Mix and match effects with number keys",
        }
    }

    /// Cycle to next effect within current tab
    fn next_effect(&mut self) {
        let count = self.tab.effect_count();
        self.effect_idx = (self.effect_idx + 1) % count;
    }

    /// Cycle to previous effect within current tab
    fn prev_effect(&mut self) {
        let count = self.tab.effect_count();
        if count == 0 {
            return;
        }
        self.effect_idx = if self.effect_idx == 0 {
            count.saturating_sub(1)
        } else {
            self.effect_idx - 1
        };
    }

    /// Build the current text effect
    fn build_effect(&self) -> TextEffect {
        match self.tab {
            TextEffectsTab::Gradients => self.build_gradient_effect(),
            TextEffectsTab::Animations => self.build_animation_effect(),
            TextEffectsTab::Typography => self.build_typography_effect(),
            TextEffectsTab::SpecialFx => self.build_special_fx_effect(),
            TextEffectsTab::Presets => self.build_preset_effect(),
            TextEffectsTab::Combinations => self.build_combo_effect(),
        }
    }

    fn variant_with_effect(&self, effect_idx: usize) -> Self {
        Self {
            tab: self.tab,
            effect_idx: effect_idx % self.tab.effect_count(),
            time: self.time,
            easing_mode: self.easing_mode,
            easing: self.easing,
            combo_enabled: self.combo_enabled,
            demo_text: self.demo_text,
            ascii_cache: self.ascii_cache.clone(),
        }
    }

    fn build_gradient_effect(&self) -> TextEffect {
        match self.effect_idx {
            0 => TextEffect::HorizontalGradient {
                gradient: ColorGradient::rainbow(),
            },
            1 => TextEffect::VerticalGradient {
                gradient: ColorGradient::sunset(),
            },
            2 => TextEffect::DiagonalGradient {
                gradient: ColorGradient::ocean(),
                angle: 45.0,
            },
            3 => TextEffect::RadialGradient {
                gradient: ColorGradient::fire(),
                center: (0.5, 0.5),
                aspect: 1.5,
            },
            _ => TextEffect::AnimatedGradient {
                gradient: ColorGradient::rainbow(),
                speed: 0.5,
            },
        }
    }

    fn build_animation_effect(&self) -> TextEffect {
        match self.effect_idx {
            0 => TextEffect::Wave {
                amplitude: 1.5,
                wavelength: 8.0,
                speed: 2.0,
                direction: Direction::Down,
            },
            1 => TextEffect::Bounce {
                height: 2.0,
                speed: 1.5,
                stagger: 0.1,
                damping: 0.85,
            },
            2 => TextEffect::Shake {
                intensity: 1.0,
                speed: 15.0,
                seed: 42,
            },
            3 => TextEffect::Cascade {
                speed: 8.0,
                direction: Direction::Right,
                stagger: 0.1,
            },
            4 => TextEffect::Pulse {
                speed: 1.5,
                min_alpha: 0.3,
            },
            _ => TextEffect::OrganicPulse {
                speed: 0.5,
                min_brightness: 0.4,
                asymmetry: 0.6,
                phase_variation: 0.2,
                seed: 42,
            },
        }
    }

    fn build_typography_effect(&self) -> TextEffect {
        match self.effect_idx {
            0..=3 => {
                // Shadow, Glow, Outline, Mirror are handled specially in render
                TextEffect::None
            }
            _ => {
                // ASCII Art is handled specially
                TextEffect::None
            }
        }
    }

    fn build_special_fx_effect(&self) -> TextEffect {
        match self.effect_idx {
            0 => TextEffect::Glitch {
                intensity: 0.3 + 0.2 * (self.time * TAU).sin(),
            },
            1 => TextEffect::ChromaticAberration {
                offset: 2,
                direction: Direction::Right,
                animated: true,
                speed: 0.5,
            },
            2 | 3 => {
                // Scanline and Matrix handled specially
                TextEffect::None
            }
            _ => TextEffect::None,
        }
    }

    fn build_preset_effect(&self) -> TextEffect {
        // Presets will be implemented in bd-4r5h
        // For now, provide simple demonstrations
        match self.effect_idx {
            0 => TextEffect::PulsingGlow {
                color: PackedRgba::rgb(0, 255, 200),
                speed: 2.0,
            },
            1 => TextEffect::HorizontalGradient {
                gradient: ColorGradient::new(vec![
                    (0.0, PackedRgba::rgb(0, 180, 0)),
                    (0.5, PackedRgba::rgb(0, 255, 0)),
                    (1.0, PackedRgba::rgb(100, 255, 100)),
                ]),
            },
            _ => TextEffect::Pulse {
                speed: 0.8,
                min_alpha: 0.5,
            },
        }
    }

    fn build_combo_effect(&self) -> TextEffect {
        // Combinations return the first enabled effect
        // Multiple effects are composed in render
        if self.combo_enabled[0] {
            TextEffect::RainbowGradient { speed: 0.3 }
        } else if self.combo_enabled[1] {
            TextEffect::Wave {
                amplitude: 1.0,
                wavelength: 10.0,
                speed: 1.5,
                direction: Direction::Down,
            }
        } else if self.combo_enabled[2] {
            TextEffect::Glow {
                color: PackedRgba::rgb(100, 200, 255),
                intensity: 0.8,
            }
        } else if self.combo_enabled[3] {
            TextEffect::Glitch { intensity: 0.2 }
        } else {
            TextEffect::None
        }
    }

    /// Update animation time
    fn tick(&mut self) {
        self.time += 0.02;
        if self.time > 1.0 {
            self.time -= 1.0;
        }
    }

    /// Cycle through easing functions
    fn next_easing(&mut self) {
        self.easing = match self.easing {
            Easing::Linear => Easing::EaseIn,
            Easing::EaseIn => Easing::EaseOut,
            Easing::EaseOut => Easing::EaseInOut,
            Easing::EaseInOut => Easing::EaseInQuad,
            Easing::EaseInQuad => Easing::EaseOutQuad,
            Easing::EaseOutQuad => Easing::EaseInOutQuad,
            Easing::EaseInOutQuad => Easing::Bounce,
            Easing::Bounce => Easing::Elastic,
            Easing::Elastic => Easing::Back,
            Easing::Back => Easing::Step(4),
            Easing::Step(_) => Easing::Linear,
        };
    }

    /// Toggle a combo effect by index (1-4)
    fn toggle_combo(&mut self, idx: usize) {
        if idx < 4 {
            self.combo_enabled[idx] = !self.combo_enabled[idx];
        }
    }
}

// =============================================================================
// Color Palettes - Beautiful gradients for effects
// =============================================================================

fn palette_sunset(t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    // Deep purple -> hot pink -> orange -> yellow
    let (r, g, b) = if t < 0.33 {
        let s = t / 0.33;
        lerp_rgb((80, 20, 120), (255, 50, 120), s)
    } else if t < 0.66 {
        let s = (t - 0.33) / 0.33;
        lerp_rgb((255, 50, 120), (255, 150, 50), s)
    } else {
        let s = (t - 0.66) / 0.34;
        lerp_rgb((255, 150, 50), (255, 255, 150), s)
    };
    PackedRgba::rgb(r, g, b)
}

fn palette_ocean(t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    // Deep blue -> cyan -> turquoise -> seafoam
    let (r, g, b) = if t < 0.5 {
        let s = t / 0.5;
        lerp_rgb((10, 30, 100), (30, 180, 220), s)
    } else {
        let s = (t - 0.5) / 0.5;
        lerp_rgb((30, 180, 220), (150, 255, 200), s)
    };
    PackedRgba::rgb(r, g, b)
}

fn palette_fire(t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    // Black -> dark red -> orange -> yellow -> white
    let (r, g, b) = if t < 0.2 {
        let s = t / 0.2;
        lerp_rgb((0, 0, 0), (80, 10, 0), s)
    } else if t < 0.4 {
        let s = (t - 0.2) / 0.2;
        lerp_rgb((80, 10, 0), (200, 50, 0), s)
    } else if t < 0.6 {
        let s = (t - 0.4) / 0.2;
        lerp_rgb((200, 50, 0), (255, 150, 20), s)
    } else if t < 0.8 {
        let s = (t - 0.6) / 0.2;
        lerp_rgb((255, 150, 20), (255, 230, 100), s)
    } else {
        let s = (t - 0.8) / 0.2;
        lerp_rgb((255, 230, 100), (255, 255, 220), s)
    };
    PackedRgba::rgb(r, g, b)
}

fn palette_neon(t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    // Cycling through neon colors
    let hue = t * 360.0;
    let (r, g, b) = hsv_to_rgb(hue, 1.0, 1.0);
    PackedRgba::rgb(r, g, b)
}

fn palette_cyberpunk(t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    // Hot pink -> cyan with purple undertones
    let (r, g, b) = if t < 0.5 {
        let s = t / 0.5;
        lerp_rgb((255, 20, 150), (150, 50, 200), s)
    } else {
        let s = (t - 0.5) / 0.5;
        lerp_rgb((150, 50, 200), (50, 220, 255), s)
    };
    PackedRgba::rgb(r, g, b)
}

fn palette_doom_wall(idx: usize) -> PackedRgba {
    // Muted browns/greys reminiscent of classic Doom E1M1 (brighter for visibility).
    const PALETTE: &[(u8, u8, u8)] = &[
        (96, 66, 44),
        (128, 88, 60),
        (160, 112, 76),
        (196, 136, 96),
        (118, 118, 118),
        (170, 170, 170),
        (122, 86, 58),
        (188, 138, 96),
        (216, 164, 120),
        (140, 100, 72),
        (164, 118, 84),
        (198, 144, 104),
    ];
    let (r, g, b) = PALETTE[idx % PALETTE.len()];
    PackedRgba::rgb(r, g, b)
}

fn palette_quake_stone(t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    // Quake palette approximation (muddy browns + stone greys), brightened for legibility.
    let c1 = (78, 70, 60);
    let c2 = (108, 98, 86);
    let c3 = (148, 136, 124);
    let c4 = (126, 116, 104);

    let (r, g, b) = if t < 0.33 {
        let s = t / 0.33;
        lerp_rgb(c1, c2, s)
    } else if t < 0.66 {
        let s = (t - 0.33) / 0.33;
        lerp_rgb(c2, c3, s)
    } else {
        let s = (t - 0.66) / 0.34;
        lerp_rgb(c3, c4, s)
    };
    PackedRgba::rgb(r, g, b)
}

fn palette_quake_floor(t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    let c1 = (86, 72, 58);
    let c2 = (116, 96, 76);
    let c3 = (148, 122, 96);
    let c4 = (128, 108, 88);

    let (r, g, b) = if t < 0.33 {
        let s = t / 0.33;
        lerp_rgb(c1, c2, s)
    } else if t < 0.66 {
        let s = (t - 0.33) / 0.33;
        lerp_rgb(c2, c3, s)
    } else {
        let s = (t - 0.66) / 0.34;
        lerp_rgb(c3, c4, s)
    };
    PackedRgba::rgb(r, g, b)
}

fn palette_quake_ceiling(t: f64) -> PackedRgba {
    let t = t.clamp(0.0, 1.0);
    let c1 = (70, 76, 92);
    let c2 = (98, 106, 124);
    let c3 = (128, 138, 158);
    let c4 = (110, 118, 136);

    let (r, g, b) = if t < 0.33 {
        let s = t / 0.33;
        lerp_rgb(c1, c2, s)
    } else if t < 0.66 {
        let s = (t - 0.33) / 0.33;
        lerp_rgb(c2, c3, s)
    } else {
        let s = (t - 0.66) / 0.34;
        lerp_rgb(c3, c4, s)
    };
    PackedRgba::rgb(r, g, b)
}

fn lerp_rgb(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    (
        (a.0 as f64 + (b.0 as f64 - a.0 as f64) * t) as u8,
        (a.1 as f64 + (b.1 as f64 - a.1 as f64) * t) as u8,
        (a.2 as f64 + (b.2 as f64 - a.2 as f64) * t) as u8,
    )
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> (u8, u8, u8) {
    let h = h % 360.0;
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let m = v - c;

    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };

    (
        ((r + m) * 255.0) as u8,
        ((g + m) * 255.0) as u8,
        ((b + m) * 255.0) as u8,
    )
}

const PLASMA_PALETTES: [PlasmaPalette; 6] = [
    PlasmaPalette::Sunset,
    PlasmaPalette::Ocean,
    PlasmaPalette::Fire,
    PlasmaPalette::Neon,
    PlasmaPalette::Cyberpunk,
    PlasmaPalette::Galaxy,
];

fn current_fx_theme() -> ThemeInputs {
    ThemeInputs::from(theme::palette(theme::current_theme()))
}

fn next_plasma_palette(current: PlasmaPalette) -> PlasmaPalette {
    let idx = PLASMA_PALETTES
        .iter()
        .position(|&palette| palette == current)
        .unwrap_or(0);
    PLASMA_PALETTES[(idx + 1) % PLASMA_PALETTES.len()]
}

// =============================================================================
// 3D Wireframe - Multiple shapes with starfield background
// =============================================================================

#[derive(Debug, Clone)]
struct Star {
    x: f64,
    y: f64,
    z: f64,
    brightness: f64,
}

#[derive(Debug, Clone)]
struct Shape3DState {
    rotation_x: f64,
    rotation_y: f64,
    rotation_z: f64,
    shape: Shape3DType,
    stars: Vec<Star>,
    camera_z: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape3DType {
    Cube,
    Octahedron,
    Icosahedron,
    Torus,
}

impl Shape3DType {
    fn next(self) -> Self {
        match self {
            Self::Cube => Self::Octahedron,
            Self::Octahedron => Self::Icosahedron,
            Self::Icosahedron => Self::Torus,
            Self::Torus => Self::Cube,
        }
    }
}

impl Default for Shape3DState {
    fn default() -> Self {
        // 300+ stars for denser starfield (bd-3vbf.27 polish)
        let mut stars = Vec::with_capacity(350);
        for _ in 0..350 {
            stars.push(Star {
                x: rand_simple() * 2.0 - 1.0,
                y: rand_simple() * 2.0 - 1.0,
                z: rand_simple() * 5.0 + 1.0,
                brightness: rand_simple() * 0.5 + 0.5,
            });
        }
        Self {
            rotation_x: 0.0,
            rotation_y: 0.0,
            rotation_z: 0.0,
            shape: Shape3DType::Cube,
            stars,
            camera_z: 0.0,
        }
    }
}

impl Shape3DState {
    fn update(&mut self) {
        self.rotation_x += 0.018;
        self.rotation_y += 0.028;
        self.rotation_z += 0.010;
        self.camera_z += 0.05;

        // Move stars towards camera (warp effect)
        for star in &mut self.stars {
            star.z -= 0.03;
            if star.z < 0.1 {
                star.z = 5.0 + rand_simple();
                star.x = rand_simple() * 2.0 - 1.0;
                star.y = rand_simple() * 2.0 - 1.0;
            }
        }
    }

    fn project(&self, x: f64, y: f64, z: f64, width: f64, height: f64) -> (i32, i32, f64) {
        // Rotate around X
        let (y1, z1) = (
            y * self.rotation_x.cos() - z * self.rotation_x.sin(),
            y * self.rotation_x.sin() + z * self.rotation_x.cos(),
        );
        // Rotate around Y
        let (x2, z2) = (
            x * self.rotation_y.cos() + z1 * self.rotation_y.sin(),
            -x * self.rotation_y.sin() + z1 * self.rotation_y.cos(),
        );
        // Rotate around Z
        let (x3, y3) = (
            x2 * self.rotation_z.cos() - y1 * self.rotation_z.sin(),
            x2 * self.rotation_z.sin() + y1 * self.rotation_z.cos(),
        );

        // Perspective projection
        let fov = 2.0;
        let z_offset = 4.0;
        let scale = fov / (z2 + z_offset);

        let screen_x = (width / 2.0 + x3 * scale * width * 0.4) as i32;
        let screen_y = (height / 2.0 + y3 * scale * height * 0.4) as i32;

        (screen_x, screen_y, z2)
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16, time: f64) {
        let w = width as f64;
        let h = height as f64;

        // Render starfield first
        for star in &self.stars {
            let scale = 1.0 / star.z;
            let sx = (w / 2.0 + star.x * scale * w * 0.5) as i32;
            let sy = (h / 2.0 + star.y * scale * h * 0.5) as i32;

            let depth_fade = 1.0 - star.z / 6.0;
            let brightness = (star.brightness * depth_fade * 255.0) as u8;
            let twinkle = (1.0 + 0.35 * (time * 6.0 + star.x * 12.0 + star.y * 8.0).sin()) / 1.35;
            let b = (brightness as f64 * twinkle) as u8;

            if sx >= 0 && sx < width as i32 && sy >= 0 && sy < height as i32 {
                // Distant stars get a subtle blue tint for depth
                let blue_tint = ((1.0 - depth_fade) * 40.0) as u8;
                painter.point_colored(
                    sx,
                    sy,
                    PackedRgba::rgb(b, b, b.saturating_add(30 + blue_tint)),
                );
            }
        }

        // Render shape
        match self.shape {
            Shape3DType::Cube => self.render_cube(painter, w, h, time),
            Shape3DType::Octahedron => self.render_octahedron(painter, w, h, time),
            Shape3DType::Icosahedron => self.render_icosahedron(painter, w, h, time),
            Shape3DType::Torus => self.render_torus(painter, w, h, time),
        }
    }

    /// Draw a line with depth-based thickness (bd-3vbf.27 polish).
    /// Closer edges (lower z) are thicker (up to 3 pixels wide).
    #[allow(clippy::too_many_arguments)]
    fn draw_thick_line(
        &self,
        painter: &mut Painter,
        x0: i32,
        y0: i32,
        x1: i32,
        y1: i32,
        depth: f64,
        color: PackedRgba,
    ) {
        // Closer edges (lower depth) get thicker lines
        // z ranges roughly from -2 to 2, normalize to thickness 1-3
        let thickness = if depth < -0.5 {
            3 // Very close: 3-pixel wide
        } else if depth < 0.5 {
            2 // Medium: 2-pixel wide
        } else {
            1 // Far: 1-pixel wide
        };

        painter.line_colored(x0, y0, x1, y1, Some(color));

        if thickness >= 2 {
            // Draw parallel lines for thickness
            painter.line_colored(x0 + 1, y0, x1 + 1, y1, Some(color));
            painter.line_colored(x0, y0 + 1, x1, y1 + 1, Some(color));
        }
        if thickness >= 3 {
            painter.line_colored(x0 - 1, y0, x1 - 1, y1, Some(color));
            painter.line_colored(x0, y0 - 1, x1, y1 - 1, Some(color));
        }
    }

    fn render_cube(&self, painter: &mut Painter, w: f64, h: f64, time: f64) {
        let vertices = [
            (-1.0, -1.0, -1.0),
            (1.0, -1.0, -1.0),
            (1.0, 1.0, -1.0),
            (-1.0, 1.0, -1.0),
            (-1.0, -1.0, 1.0),
            (1.0, -1.0, 1.0),
            (1.0, 1.0, 1.0),
            (-1.0, 1.0, 1.0),
        ];
        let edges = [
            (0, 1),
            (1, 2),
            (2, 3),
            (3, 0),
            (4, 5),
            (5, 6),
            (6, 7),
            (7, 4),
            (0, 4),
            (1, 5),
            (2, 6),
            (3, 7),
        ];

        let projected: Vec<_> = vertices
            .iter()
            .map(|&(x, y, z)| self.project(x, y, z, w, h))
            .collect();

        for (i, &(a, b)) in edges.iter().enumerate() {
            let (x0, y0, z0) = projected[a];
            let (x1, y1, z1) = projected[b];
            let avg_z = (z0 + z1) / 2.0;
            let brightness = (1.0 - avg_z / 4.0).clamp(0.3, 1.0);

            let hue = (i as f64 / edges.len() as f64 + time * 0.1) % 1.0;
            let (r, g, b) = hsv_to_rgb(hue * 360.0, 0.8, brightness);

            self.draw_thick_line(painter, x0, y0, x1, y1, avg_z, PackedRgba::rgb(r, g, b));
        }
    }

    fn render_octahedron(&self, painter: &mut Painter, w: f64, h: f64, time: f64) {
        let s = 1.5;
        let vertices = [
            (0.0, -s, 0.0),
            (0.0, s, 0.0),
            (s, 0.0, 0.0),
            (-s, 0.0, 0.0),
            (0.0, 0.0, s),
            (0.0, 0.0, -s),
        ];
        let edges = [
            (0, 2),
            (0, 3),
            (0, 4),
            (0, 5),
            (1, 2),
            (1, 3),
            (1, 4),
            (1, 5),
            (2, 4),
            (4, 3),
            (3, 5),
            (5, 2),
        ];

        let projected: Vec<_> = vertices
            .iter()
            .map(|&(x, y, z)| self.project(x, y, z, w, h))
            .collect();

        for (i, &(a, b)) in edges.iter().enumerate() {
            let (x0, y0, z0) = projected[a];
            let (x1, y1, z1) = projected[b];
            let avg_z = (z0 + z1) / 2.0;
            let brightness = (1.0 - avg_z / 4.0).clamp(0.3, 1.0);

            let color = palette_ocean((i as f64 / edges.len() as f64 + time * 0.05) % 1.0);
            let (r, g, b_val) = (
                (color.r() as f64 * brightness) as u8,
                (color.g() as f64 * brightness) as u8,
                (color.b() as f64 * brightness) as u8,
            );

            // Depth-based line thickness (bd-3vbf.27 polish)
            self.draw_thick_line(painter, x0, y0, x1, y1, avg_z, PackedRgba::rgb(r, g, b_val));
        }
    }

    fn render_icosahedron(&self, painter: &mut Painter, w: f64, h: f64, time: f64) {
        let phi = (1.0 + 5.0_f64.sqrt()) / 2.0;
        let vertices = [
            (-1.0, phi, 0.0),
            (1.0, phi, 0.0),
            (-1.0, -phi, 0.0),
            (1.0, -phi, 0.0),
            (0.0, -1.0, phi),
            (0.0, 1.0, phi),
            (0.0, -1.0, -phi),
            (0.0, 1.0, -phi),
            (phi, 0.0, -1.0),
            (phi, 0.0, 1.0),
            (-phi, 0.0, -1.0),
            (-phi, 0.0, 1.0),
        ];
        let edges = [
            (0, 1),
            (0, 5),
            (0, 7),
            (0, 10),
            (0, 11),
            (1, 5),
            (1, 7),
            (1, 8),
            (1, 9),
            (2, 3),
            (2, 4),
            (2, 6),
            (2, 10),
            (2, 11),
            (3, 4),
            (3, 6),
            (3, 8),
            (3, 9),
            (4, 5),
            (4, 9),
            (4, 11),
            (5, 9),
            (5, 11),
            (6, 7),
            (6, 8),
            (6, 10),
            (7, 8),
            (7, 10),
            (8, 9),
            (10, 11),
        ];

        let projected: Vec<_> = vertices
            .iter()
            .map(|&(x, y, z)| self.project(x, y, z, w, h))
            .collect();

        for (i, &(a, b)) in edges.iter().enumerate() {
            let (x0, y0, z0) = projected[a];
            let (x1, y1, z1) = projected[b];
            let avg_z = (z0 + z1) / 2.0;
            let brightness = (1.0 - avg_z / 5.0).clamp(0.3, 1.0);

            let color = palette_sunset((i as f64 / edges.len() as f64 + time * 0.03) % 1.0);
            let (r, g, b_val) = (
                (color.r() as f64 * brightness) as u8,
                (color.g() as f64 * brightness) as u8,
                (color.b() as f64 * brightness) as u8,
            );

            // Depth-based line thickness (bd-3vbf.27 polish)
            self.draw_thick_line(painter, x0, y0, x1, y1, avg_z, PackedRgba::rgb(r, g, b_val));
        }
    }

    fn render_torus(&self, painter: &mut Painter, w: f64, h: f64, time: f64) {
        let major_r = 1.2;
        let minor_r = 0.5;
        let u_steps = 24;
        let v_steps = 12;

        let mut points = Vec::new();
        for u in 0..u_steps {
            for v in 0..v_steps {
                let u_angle = (u as f64 / u_steps as f64) * TAU;
                let v_angle = (v as f64 / v_steps as f64) * TAU;

                let x = (major_r + minor_r * v_angle.cos()) * u_angle.cos();
                let y = (major_r + minor_r * v_angle.cos()) * u_angle.sin();
                let z = minor_r * v_angle.sin();

                points.push((u, v, self.project(x, y, z, w, h)));
            }
        }

        // Draw rings
        for u in 0..u_steps {
            for v in 0..v_steps {
                let idx = u * v_steps + v;
                let next_v = u * v_steps + (v + 1) % v_steps;
                let next_u = ((u + 1) % u_steps) * v_steps + v;

                let (_, _, (x0, y0, z0)) = points[idx];
                let (_, _, (x1, y1, _z1)) = points[next_v];
                let (_, _, (x2, y2, _z2)) = points[next_u];

                let brightness1 = (1.0 - z0 / 4.0).clamp(0.3, 1.0);
                let hue = (u as f64 / u_steps as f64 + time * 0.05) % 1.0;
                let color = palette_cyberpunk(hue);
                let (r, g, b) = (
                    (color.r() as f64 * brightness1) as u8,
                    (color.g() as f64 * brightness1) as u8,
                    (color.b() as f64 * brightness1) as u8,
                );

                painter.line_colored(x0, y0, x1, y1, Some(PackedRgba::rgb(r, g, b)));
                painter.line_colored(x0, y0, x2, y2, Some(PackedRgba::rgb(r, g, b)));
            }
        }
    }
}

// =============================================================================
// Particle Fireworks - Explosions with trails
// =============================================================================

#[derive(Debug, Clone)]
struct Particle {
    x: f64,
    y: f64,
    vx: f64,
    vy: f64,
    life: f64,
    max_life: f64,
    hue: f64,
    is_rocket: bool,
    trail: Vec<(f64, f64)>,
}

#[derive(Debug, Clone)]
struct ParticleState {
    particles: Vec<Particle>,
    spawn_timer: f64,
}

impl Default for ParticleState {
    fn default() -> Self {
        Self {
            particles: Vec::with_capacity(2000),
            spawn_timer: 0.0,
        }
    }
}

impl ParticleState {
    fn update_with_quality(&mut self, quality: FxQuality) {
        if matches!(quality, FxQuality::Off) {
            return;
        }

        let spawn_threshold = match quality {
            FxQuality::Full => 22.0,
            FxQuality::Reduced => 26.0,
            FxQuality::Minimal => 32.0,
            FxQuality::Off => f64::INFINITY,
        };
        let max_particles = match quality {
            FxQuality::Full => 1500,
            FxQuality::Reduced => 1100,
            FxQuality::Minimal => 700,
            FxQuality::Off => 0,
        };
        let max_trail = match quality {
            FxQuality::Full => 14,
            FxQuality::Reduced => 10,
            FxQuality::Minimal => 6,
            FxQuality::Off => 0,
        };
        let min_explosion = match quality {
            FxQuality::Full => 80,
            FxQuality::Reduced => 60,
            FxQuality::Minimal => 40,
            FxQuality::Off => 0,
        };
        let extra_explosion = match quality {
            FxQuality::Full => 60,
            FxQuality::Reduced => 40,
            FxQuality::Minimal => 25,
            FxQuality::Off => 0,
        };

        self.spawn_timer += 1.0;

        // Launch rockets periodically
        if self.spawn_timer >= spawn_threshold {
            self.spawn_timer = 0.0;
            let x = 0.2 + rand_simple() * 0.6;
            self.particles.push(Particle {
                x,
                y: 1.0,
                vx: (rand_simple() - 0.5) * 0.01,
                vy: -0.025 - rand_simple() * 0.015,
                life: 1.0,
                max_life: 1.0,
                hue: rand_simple(),
                is_rocket: true,
                trail: Vec::new(),
            });
        }

        // Update particles
        let mut new_particles = Vec::new();

        for p in &mut self.particles {
            if max_trail > 0 && p.trail.len() > max_trail {
                let excess = p.trail.len() - max_trail;
                p.trail.drain(0..excess);
            }
            // Store trail position - longer trails for more dramatic effect (bd-3vbf.27 polish)
            if p.trail.len() < max_trail {
                p.trail.push((p.x, p.y));
            } else {
                p.trail.remove(0);
                p.trail.push((p.x, p.y));
            }

            p.x += p.vx;
            p.y += p.vy;

            if p.is_rocket {
                p.vy += 0.0005; // Less gravity for rockets
                p.life -= 0.015;

                // Explode when rocket dies or reaches apex
                if p.life <= 0.0 || p.vy > 0.0 {
                    // Create explosion with variety (bd-3vbf.27 polish)
                    // More particles (80-140 range) with varied patterns
                    let num_particles =
                        min_explosion + (rand_simple() * extra_explosion as f64) as usize;
                    let pattern = (rand_simple() * 3.0) as u8; // 0=circular, 1=ring, 2=starburst

                    for i in 0..num_particles {
                        let angle = match pattern {
                            0 => rand_simple() * TAU, // Circular: random angles
                            1 => (i as f64 / num_particles as f64) * TAU + rand_simple() * 0.1, // Ring: evenly spaced
                            _ => {
                                // Starburst: concentrated in rays
                                let ray = (rand_simple() * 8.0).floor();
                                ray / 8.0 * TAU + (rand_simple() - 0.5) * 0.15
                            }
                        };

                        // More varied speeds for dynamic look
                        let base_speed = match pattern {
                            1 => 0.015 + rand_simple() * 0.01,  // Ring: tighter speed range
                            _ => 0.008 + rand_simple() * 0.025, // Others: wider speed range
                        };

                        let hue_variation = p.hue + (rand_simple() - 0.5) * 0.15; // Slightly more color variation
                        let life_variation = 0.8 + rand_simple() * 0.4; // Varied lifetimes

                        new_particles.push(Particle {
                            x: p.x,
                            y: p.y,
                            vx: angle.cos() * base_speed,
                            vy: angle.sin() * base_speed,
                            life: life_variation,
                            max_life: life_variation,
                            hue: hue_variation.rem_euclid(1.0),
                            is_rocket: false,
                            trail: Vec::new(),
                        });
                    }
                    p.life = 0.0;
                }
            } else {
                p.vy += 0.0006; // Gravity
                p.life -= 0.012;
                p.vx *= 0.99; // Air resistance
                p.vy *= 0.99;
            }
        }

        // Remove dead particles and add new ones
        self.particles.retain(|p| p.life > 0.0);
        self.particles.extend(new_particles);

        // Limit total particles
        if self.particles.len() > max_particles {
            let drain = (self.particles.len() - max_particles).min(500);
            self.particles.drain(0..drain);
        }
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        for p in &self.particles {
            // Draw trail with hue shift along length for sparkle effect
            for (i, &(tx, ty)) in p.trail.iter().enumerate() {
                let trail_frac = i as f64 / p.trail.len() as f64;
                let trail_life = trail_frac * p.life;
                if trail_life > 0.1 {
                    let px = (tx * width as f64) as i32;
                    let py = (ty * height as f64) as i32;
                    let trail_hue = (p.hue + trail_frac * 0.08).rem_euclid(1.0);
                    let (r, g, b) = hsv_to_rgb(trail_hue * 360.0, 0.75, trail_life * 0.55);
                    painter.point_colored(px, py, PackedRgba::rgb(r, g, b));
                }
            }

            // Draw particle
            let px = (p.x * width as f64) as i32;
            let py = (p.y * height as f64) as i32;

            let brightness = p.life / p.max_life;
            let saturation = if p.is_rocket { 0.3 } else { 0.9 };
            let (r, g, b) = hsv_to_rgb(p.hue * 360.0, saturation, brightness);

            painter.point_colored(px, py, PackedRgba::rgb(r, g, b));

            // Glow for bright particles — warm-tinted halo
            if brightness > 0.65 && !p.is_rocket {
                let glow_v = brightness * 0.45;
                let (gr, gg, gb) = hsv_to_rgb(p.hue * 360.0, 0.3, glow_v);
                let glow = PackedRgba::rgb(gr, gg, gb);
                painter.point_colored(px + 1, py, glow);
                painter.point_colored(px - 1, py, glow);
                painter.point_colored(px, py + 1, glow);
                painter.point_colored(px, py - 1, glow);
            }
        }
    }
}

// =============================================================================
// Matrix Rain - Digital rain effect
// =============================================================================

#[derive(Debug, Clone)]
struct MatrixDrop {
    x: usize,
    y: f64,
    speed: f64,
    length: usize,
    chars: Vec<u8>,
}

#[derive(Debug, Clone, Default)]
struct MatrixState {
    drops: Vec<MatrixDrop>,
    width: usize,
    initialized: bool,
}

impl MatrixState {
    fn init(&mut self, width: usize) {
        if self.initialized && self.width == width {
            return;
        }
        self.width = width;
        self.drops.clear();

        // Create drops for each column
        for x in 0..width {
            if rand_simple() < 0.7 {
                self.spawn_drop(x);
            }
        }
        self.initialized = true;
    }

    fn spawn_drop(&mut self, x: usize) {
        let length = 8 + (rand_simple() * 20.0) as usize;
        let mut chars = Vec::with_capacity(length);
        for _ in 0..length {
            chars.push((rand_simple() * 94.0 + 33.0) as u8); // Printable ASCII
        }
        self.drops.push(MatrixDrop {
            x,
            y: -(rand_simple() * 20.0),
            speed: 0.3 + rand_simple() * 0.4,
            length,
            chars,
        });
    }

    fn update(&mut self, height: usize) {
        if !self.initialized {
            return;
        }

        for drop in &mut self.drops {
            drop.y += drop.speed;

            // Randomly change characters
            if rand_simple() < 0.1 {
                let idx = (rand_simple() * drop.chars.len() as f64) as usize;
                if idx < drop.chars.len() {
                    drop.chars[idx] = (rand_simple() * 94.0 + 33.0) as u8;
                }
            }
        }

        // Remove drops that are off screen and spawn new ones
        self.drops
            .retain(|d| (d.y as i32 - d.length as i32) < height as i32);

        for x in 0..self.width {
            if !self.drops.iter().any(|d| d.x == x) && rand_simple() < 0.03 {
                self.spawn_drop(x);
            }
        }
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        if !self.initialized {
            return;
        }

        // Scale from character columns to pixel columns
        let char_width = width as usize / self.width.max(1);

        for drop in &self.drops {
            let px = (drop.x * char_width.max(1)) as i32;

            for (i, &_ch) in drop.chars.iter().enumerate() {
                let char_y = drop.y as i32 - i as i32;
                if char_y < 0 || char_y >= height as i32 {
                    continue;
                }

                // Calculate brightness - head is brightest
                let brightness = if i == 0 {
                    1.0
                } else {
                    1.0 - (i as f64 / drop.length as f64)
                };

                let (r, g, b) = if i == 0 {
                    (200, 255, 200) // White-green head
                } else {
                    let g = (brightness * 200.0) as u8;
                    (0, g, (g / 4).min(50))
                };

                // Draw the "character" as a small cluster of braille dots
                for dy in 0..2 {
                    for dx in 0..char_width.min(2) as i32 {
                        painter.point_colored(px + dx, char_y * 2 + dy, PackedRgba::rgb(r, g, b));
                    }
                }
            }
        }
    }
}

// =============================================================================
// Tunnel - Flying through a tunnel effect
// =============================================================================

#[derive(Debug, Clone, Default)]
struct TunnelState {
    offset: f64,
}

impl TunnelState {
    fn update(&mut self) {
        self.offset += 0.08;
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        let cx = width as f64 / 2.0;
        let cy = height as f64 / 2.0;
        let max_dist = (cx * cx + cy * cy).sqrt();

        for py in 0..height as i32 {
            for px in 0..width as i32 {
                let dx = px as f64 - cx;
                let dy = py as f64 - cy;
                let dist = (dx * dx + dy * dy).sqrt();
                let angle = dy.atan2(dx);

                if dist < 2.0 {
                    continue; // Center void
                }

                // Tunnel depth based on distance from center
                let depth = max_dist / dist;
                let u = angle / TAU + 0.5;
                let v = depth * 0.5 + self.offset;

                // Create tunnel rings pattern
                let ring = ((v * 8.0).floor() as i32).rem_euclid(2) as f64;
                let segment = ((u * 16.0).floor() as i32).rem_euclid(2) as f64;
                let checker = (ring + segment).rem_euclid(2.0);

                // Color based on depth and pattern
                let depth_fade = (1.0 - 1.0 / depth.max(1.0)).clamp(0.0, 1.0);
                let base_brightness = 0.2 + 0.6 * checker;
                let brightness = base_brightness * depth_fade;

                let hue = (u + self.offset * 0.1) % 1.0;
                let color = palette_cyberpunk(hue);
                let r = (color.r() as f64 * brightness) as u8;
                let g = (color.g() as f64 * brightness) as u8;
                let b = (color.b() as f64 * brightness) as u8;

                painter.point_colored(px, py, PackedRgba::rgb(r, g, b));
            }
        }
    }
}

// =============================================================================
// Fire - Realistic fire simulation
// =============================================================================

#[derive(Debug, Clone, Default)]
struct FireState {
    buffer: Vec<f64>,
    width: usize,
    height: usize,
    initialized: bool,
}

impl FireState {
    fn init(&mut self, width: usize, height: usize) {
        if self.initialized && self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        self.buffer = vec![0.0; width * height];
        self.initialized = true;
    }

    fn update(&mut self) {
        if !self.initialized || self.width == 0 || self.height == 0 {
            return;
        }

        // Set fire source at bottom
        let bottom = self.height - 1;
        for x in 0..self.width {
            self.buffer[bottom * self.width + x] = 0.8 + rand_simple() * 0.2;
        }

        // Propagate fire upward with cooling
        for y in 0..self.height - 1 {
            for x in 0..self.width {
                let below = (y + 1) * self.width + x;
                let left = if x > 0 {
                    (y + 1) * self.width + x - 1
                } else {
                    below
                };
                let right = if x < self.width - 1 {
                    (y + 1) * self.width + x + 1
                } else {
                    below
                };
                let below2 = if y + 2 < self.height {
                    (y + 2) * self.width + x
                } else {
                    below
                };

                // Average with neighbors and cool down
                let avg = (self.buffer[below]
                    + self.buffer[left]
                    + self.buffer[right]
                    + self.buffer[below2])
                    / 4.0;

                // Random cooling factor
                let cooling = 0.02 + rand_simple() * 0.03;
                let new_val = (avg - cooling).max(0.0);

                // Add some turbulence
                let turbulence = if rand_simple() < 0.1 {
                    (rand_simple() - 0.5) * 0.1
                } else {
                    0.0
                };

                self.buffer[y * self.width + x] = (new_val + turbulence).clamp(0.0, 1.0);
            }
        }
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        if !self.initialized || self.width == 0 || self.height == 0 || width == 0 || height == 0 {
            return;
        }

        let scale_x = self.width as f64 / width as f64;
        let scale_y = self.height as f64 / height as f64;

        for py in 0..height as i32 {
            for px in 0..width as i32 {
                let gx = ((px as f64 * scale_x) as usize).min(self.width - 1);
                let gy = ((py as f64 * scale_y) as usize).min(self.height - 1);
                let val = self.buffer[gy * self.width + gx];

                if val > 0.01 {
                    let color = palette_fire(val);
                    painter.point_colored(px, py, color);
                }
            }
        }
    }
}

// =============================================================================
// Reaction-Diffusion (Gray-Scott) - Turing pattern morphogenesis
// =============================================================================

#[derive(Debug, Clone)]
struct ReactionDiffusionState {
    // Chemical concentrations: U (activator) and V (inhibitor)
    u: Vec<f64>,
    v: Vec<f64>,
    u_next: Vec<f64>,
    v_next: Vec<f64>,
    width: usize,
    height: usize,
    initialized: bool,
    // Gray-Scott parameters - these create beautiful organic patterns
    feed: f64, // Feed rate (F)
    kill: f64, // Kill rate (k)
    du: f64,   // Diffusion rate of U
    dv: f64,   // Diffusion rate of V
}

impl Default for ReactionDiffusionState {
    fn default() -> Self {
        Self {
            u: Vec::new(),
            v: Vec::new(),
            u_next: Vec::new(),
            v_next: Vec::new(),
            width: 0,
            height: 0,
            initialized: false,
            // Classic coral/maze pattern parameters
            feed: 0.055,
            kill: 0.062,
            du: 1.0,
            dv: 0.5,
        }
    }
}

impl ReactionDiffusionState {
    fn init(&mut self, width: usize, height: usize) {
        if self.initialized && self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        let size = width * height;
        self.u = vec![1.0; size];
        self.v = vec![0.0; size];
        self.u_next = vec![0.0; size];
        self.v_next = vec![0.0; size];

        // Seed with random spots of V
        for _ in 0..15 {
            let cx = (rand_simple() * width as f64) as usize;
            let cy = (rand_simple() * height as f64) as usize;
            let r = 3 + (rand_simple() * 5.0) as usize;
            for dy in 0..r * 2 {
                for dx in 0..r * 2 {
                    let x = (cx + dx).saturating_sub(r);
                    let y = (cy + dy).saturating_sub(r);
                    if x < width && y < height {
                        let dist =
                            ((dx as i32 - r as i32).pow(2) + (dy as i32 - r as i32).pow(2)) as f64;
                        if dist < (r * r) as f64 {
                            let idx = y * width + x;
                            self.u[idx] = 0.5;
                            self.v[idx] = 0.25;
                        }
                    }
                }
            }
        }
        self.initialized = true;
    }

    fn update(&mut self) {
        if !self.initialized || self.width < 3 || self.height < 3 {
            return;
        }

        let w = self.width;
        let h = self.height;
        let size = w * h;
        if self.u_next.len() != size {
            self.u_next = vec![0.0; size];
            self.v_next = vec![0.0; size];
        }
        self.u_next.copy_from_slice(&self.u);
        self.v_next.copy_from_slice(&self.v);
        {
            let (u, v) = (&self.u, &self.v);
            let (new_u, new_v) = (&mut self.u_next, &mut self.v_next);

            // Gray-Scott reaction-diffusion equations
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    let idx = y * w + x;
                    let u_val = u[idx];
                    let v_val = v[idx];

                    // Laplacian (5-point stencil)
                    let lap_u = u[idx - 1] + u[idx + 1] + u[idx - w] + u[idx + w] - 4.0 * u_val;
                    let lap_v = v[idx - 1] + v[idx + 1] + v[idx - w] + v[idx + w] - 4.0 * v_val;

                    // Reaction terms
                    let uvv = u_val * v_val * v_val;
                    let du_dt = self.du * lap_u - uvv + self.feed * (1.0 - u_val);
                    let dv_dt = self.dv * lap_v + uvv - (self.feed + self.kill) * v_val;

                    // Euler integration with dt = 1.0
                    new_u[idx] = (u_val + du_dt).clamp(0.0, 1.0);
                    new_v[idx] = (v_val + dv_dt).clamp(0.0, 1.0);
                }
            }
        }

        std::mem::swap(&mut self.u, &mut self.u_next);
        std::mem::swap(&mut self.v, &mut self.v_next);
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        if !self.initialized || self.width == 0 || self.height == 0 {
            return;
        }

        let scale_x = self.width as f64 / width as f64;
        let scale_y = self.height as f64 / height as f64;

        for py in 0..height as i32 {
            for px in 0..width as i32 {
                let gx = ((px as f64 * scale_x) as usize).min(self.width - 1);
                let gy = ((py as f64 * scale_y) as usize).min(self.height - 1);
                let idx = gy * self.width + gx;

                let v = self.v[idx];
                if v > 0.05 {
                    // Beautiful organic palette based on concentration
                    let color = palette_ocean(v * 1.5);
                    painter.point_colored(px, py, color);
                }
            }
        }
    }
}

// =============================================================================
// Strange Attractor - Clifford attractor with beautiful chaos
// =============================================================================

#[derive(Debug, Clone)]
struct AttractorPoint {
    x: f64,
    y: f64,
    age: f64,
    hue: f64,
}

#[derive(Debug, Clone)]
struct AttractorState {
    points: Vec<AttractorPoint>,
    // Clifford attractor parameters: x' = sin(a*y) + c*cos(a*x), y' = sin(b*x) + d*cos(b*y)
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    time: f64,
}

impl Default for AttractorState {
    fn default() -> Self {
        Self {
            points: Vec::new(),
            // Beautiful swirling parameters
            a: -1.4,
            b: 1.6,
            c: 1.0,
            d: 0.7,
            time: 0.0,
        }
    }
}

impl AttractorState {
    fn update(&mut self) {
        self.time += 0.002;

        // Slowly evolve parameters for morphing patterns
        let t = self.time;
        self.a = -1.4 + 0.3 * (t * 0.7).sin();
        self.b = 1.6 + 0.2 * (t * 0.5).cos();
        self.c = 1.0 + 0.3 * (t * 0.3).sin();
        self.d = 0.7 + 0.2 * (t * 0.4).cos();

        // Spawn new points
        if self.points.len() < 5000 {
            for _ in 0..50 {
                self.points.push(AttractorPoint {
                    x: rand_simple() * 4.0 - 2.0,
                    y: rand_simple() * 4.0 - 2.0,
                    age: 0.0,
                    hue: rand_simple(),
                });
            }
        }

        // Update points using Clifford attractor equations
        for p in &mut self.points {
            let new_x = (self.a * p.y).sin() + self.c * (self.a * p.x).cos();
            let new_y = (self.b * p.x).sin() + self.d * (self.b * p.y).cos();
            p.x = new_x;
            p.y = new_y;
            p.age += 0.01;
            p.hue = (p.hue + 0.001) % 1.0;
        }

        // Remove old points
        self.points.retain(|p| p.age < 2.0);
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        let w = width as f64;
        let h = height as f64;
        let scale = (w.min(h) / 5.0).max(1.0);
        let cx = w / 2.0;
        let cy = h / 2.0;

        for p in &self.points {
            let sx = (cx + p.x * scale) as i32;
            let sy = (cy + p.y * scale) as i32;

            if sx >= 0 && sx < width as i32 && sy >= 0 && sy < height as i32 {
                let brightness = 1.0 - (p.age / 2.0).min(1.0);
                let (r, g, b) = hsv_to_rgb(p.hue * 360.0, 0.9, brightness);
                painter.point_colored(sx, sy, PackedRgba::rgb(r, g, b));
            }
        }
    }
}

// =============================================================================
// Mandelbrot - Deep zoom into the fractal with smooth coloring
// =============================================================================

#[derive(Debug, Clone)]
struct MandelbrotState {
    center_x: f64,
    center_y: f64,
    zoom: f64,
    max_iter: u32,
    time: f64,
}

impl Default for MandelbrotState {
    fn default() -> Self {
        Self {
            // Interesting zoom target near the "seahorse valley"
            center_x: -0.743643887037151,
            center_y: 0.131825904205330,
            zoom: 1.0,
            max_iter: 100,
            time: 0.0,
        }
    }
}

impl MandelbrotState {
    fn update(&mut self) {
        self.time += 0.02;
        // Continuous zoom with periodic reset
        self.zoom *= 1.015;
        if self.zoom > 1e8 {
            self.zoom = 1.0;
            // Pick a new interesting location
            let locations = [
                (-0.743643887037151, 0.131825904205330), // Seahorse valley
                (-0.16, 1.0405),                         // Branch tip
                (-1.25066, 0.02012),                     // Mini Mandelbrot
                (-0.77568377, 0.13646737),               // Spiral
            ];
            let idx = (rand_simple() * locations.len() as f64) as usize;
            let loc = locations[idx.min(locations.len() - 1)];
            self.center_x = loc.0;
            self.center_y = loc.1;
        }
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        if width == 0 || height == 0 {
            return;
        }

        let w = width as f64;
        let h = height as f64;
        let scale = 3.5 / (self.zoom * w.min(h));

        for py in 0..height as i32 {
            for px in 0..width as i32 {
                // Map pixel to complex plane
                let x0 = self.center_x + (px as f64 - w / 2.0) * scale;
                let y0 = self.center_y + (py as f64 - h / 2.0) * scale;

                // Mandelbrot iteration with smooth coloring
                let mut x = 0.0;
                let mut y = 0.0;
                let mut iter = 0u32;

                while x * x + y * y <= 256.0 && iter < self.max_iter {
                    let xtemp = x * x - y * y + x0;
                    y = 2.0 * x * y + y0;
                    x = xtemp;
                    iter += 1;
                }

                if iter < self.max_iter {
                    // Smooth coloring using escape-time algorithm
                    let log_zn = (x * x + y * y).ln() / 2.0;
                    let nu = (log_zn / 2.0_f64.ln()).ln() / 2.0_f64.ln();
                    let smooth_iter = iter as f64 + 1.0 - nu;

                    // Color based on iteration count with time-shifting hue
                    let t = (smooth_iter / 50.0 + self.time * 0.1) % 1.0;
                    let color = palette_sunset(t);
                    painter.point_colored(px, py, color);
                }
            }
        }
    }
}

// =============================================================================
// Lissajous - Harmonograph-style overlapping harmonic curves
// =============================================================================

#[derive(Debug, Clone)]
struct LissajousCurve {
    freq_x: f64,
    freq_y: f64,
    phase_x: f64,
    phase_y: f64,
    decay: f64,
    hue: f64,
}

#[derive(Debug, Clone)]
struct LissajousState {
    curves: Vec<LissajousCurve>,
    time: f64,
    trail_buffer: Vec<(i32, i32, f64, f64)>, // x, y, age, hue
}

impl Default for LissajousState {
    fn default() -> Self {
        // Create multiple overlapping harmonograph curves
        let curves = vec![
            LissajousCurve {
                freq_x: 3.0,
                freq_y: 2.0,
                phase_x: 0.0,
                phase_y: TAU / 4.0,
                decay: 0.002,
                hue: 0.0,
            },
            LissajousCurve {
                freq_x: 5.0,
                freq_y: 4.0,
                phase_x: TAU / 3.0,
                phase_y: 0.0,
                decay: 0.003,
                hue: 0.33,
            },
            LissajousCurve {
                freq_x: 7.0,
                freq_y: 6.0,
                phase_x: TAU / 6.0,
                phase_y: TAU / 2.0,
                decay: 0.001,
                hue: 0.66,
            },
        ];
        Self {
            curves,
            time: 0.0,
            trail_buffer: Vec::with_capacity(10000),
        }
    }
}

impl LissajousState {
    fn update(&mut self) {
        self.time += 0.05;

        // Age and remove old trail points
        for point in &mut self.trail_buffer {
            point.2 += 0.02; // age
        }
        self.trail_buffer.retain(|p| p.2 < 1.5);
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        let w = width as f64;
        let h = height as f64;
        let cx = w / 2.0;
        let cy = h / 2.0;
        let scale = (w.min(h) * 0.4).max(1.0);

        // Draw trail buffer
        for &(px, py, age, hue) in &self.trail_buffer {
            if px >= 0 && px < width as i32 && py >= 0 && py < height as i32 {
                let brightness = (1.0 - age / 1.5).max(0.0);
                let (r, g, b) = hsv_to_rgb(hue * 360.0, 0.8, brightness);
                painter.point_colored(px, py, PackedRgba::rgb(r, g, b));
            }
        }

        // Draw each curve with many sample points
        let t = self.time;
        for curve in &self.curves {
            let decay_factor = (-curve.decay * t).exp();
            for i in 0..500 {
                let ti = t - i as f64 * 0.02;
                if ti < 0.0 {
                    continue;
                }
                let d = (-curve.decay * ti).exp();

                let x = (curve.freq_x * ti + curve.phase_x).sin() * d;
                let y = (curve.freq_y * ti + curve.phase_y).sin() * d;

                let sx = (cx + x * scale) as i32;
                let sy = (cy + y * scale) as i32;

                if sx >= 0 && sx < width as i32 && sy >= 0 && sy < height as i32 {
                    let brightness = (d / decay_factor).min(1.0) * (1.0 - i as f64 / 500.0);
                    let hue = (curve.hue + ti * 0.01) % 1.0;
                    let (r, g, b) = hsv_to_rgb(hue * 360.0, 0.9, brightness);
                    painter.point_colored(sx, sy, PackedRgba::rgb(r, g, b));
                }
            }
        }
    }
}

// =============================================================================
// Flow Field - Particles following Perlin noise vector field
// =============================================================================

#[derive(Debug, Clone)]
struct FlowParticle {
    x: f64,
    y: f64,
    prev_x: f64,
    prev_y: f64,
    hue: f64,
    age: f64,
}

#[derive(Debug, Clone)]
struct FlowFieldState {
    particles: Vec<FlowParticle>,
    time: f64,
    noise_scale: f64,
}

impl Default for FlowFieldState {
    fn default() -> Self {
        Self {
            particles: Vec::with_capacity(2000),
            time: 0.0,
            noise_scale: 0.008,
        }
    }
}

impl FlowFieldState {
    fn update_with_quality(&mut self, quality: FxQuality) {
        if matches!(quality, FxQuality::Off) {
            return;
        }

        let target_particles = match quality {
            FxQuality::Full => 1500,
            FxQuality::Reduced => 1100,
            FxQuality::Minimal => 700,
            FxQuality::Off => 0,
        };

        if self.particles.len() > target_particles {
            self.particles.truncate(target_particles);
        }

        self.time += 0.03;

        // Spawn new particles
        while self.particles.len() < target_particles {
            let x = rand_simple();
            let y = rand_simple();
            self.particles.push(FlowParticle {
                x,
                y,
                prev_x: x,
                prev_y: y,
                hue: rand_simple(),
                age: 0.0,
            });
        }

        // Update particles following the flow field
        let time = self.time;
        let noise_scale = self.noise_scale;
        for p in &mut self.particles {
            p.prev_x = p.x;
            p.prev_y = p.y;

            // Sample noise field to get flow direction (inline to avoid borrow issues)
            let nx = p.x / noise_scale + time;
            let ny = p.y / noise_scale;
            let n1 = (nx * 1.0 + ny * 1.7).sin();
            let n2 = (nx * 2.3 - ny * 1.2 + time * 0.5).sin() * 0.5;
            let n3 = (nx * 4.1 + ny * 3.7 - time * 0.3).sin() * 0.25;
            let n4 = ((nx + ny) * 0.7 + time * 0.2).cos() * 0.3;
            let noise = (n1 + n2 + n3 + n4) / 2.05;
            let angle = noise * TAU * 2.0;

            // Move particle along flow
            let speed = 0.005;
            p.x += angle.cos() * speed;
            p.y += angle.sin() * speed;
            p.age += 0.01;
            p.hue = (p.hue + 0.001) % 1.0;

            // Wrap around edges
            if p.x < 0.0 {
                p.x = 1.0;
                p.prev_x = 1.0;
            }
            if p.x > 1.0 {
                p.x = 0.0;
                p.prev_x = 0.0;
            }
            if p.y < 0.0 {
                p.y = 1.0;
                p.prev_y = 1.0;
            }
            if p.y > 1.0 {
                p.y = 0.0;
                p.prev_y = 0.0;
            }
        }

        // Remove old particles
        self.particles.retain(|p| p.age < 3.0);
        if self.particles.len() > target_particles {
            self.particles.truncate(target_particles);
        }
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        let w = width as f64;
        let h = height as f64;

        for p in &self.particles {
            let sx = (p.x * w) as i32;
            let sy = (p.y * h) as i32;
            let px = (p.prev_x * w) as i32;
            let py = (p.prev_y * h) as i32;

            // Only draw if within bounds and not wrapping
            let dx = (sx - px).abs();
            let dy = (sy - py).abs();
            if dx < width as i32 / 2 && dy < height as i32 / 2 {
                let brightness = (1.0 - p.age / 3.0).max(0.0);
                let (r, g, b) = hsv_to_rgb(p.hue * 360.0, 0.85, brightness);
                let color = PackedRgba::rgb(r, g, b);

                // Draw line from previous to current position
                painter.line_colored(px, py, sx, sy, Some(color));
            }
        }
    }
}

// =============================================================================
// Julia Set - Animated companion to Mandelbrot with morphing c parameter
// =============================================================================

#[derive(Debug, Clone)]
struct JuliaState {
    // Complex c parameter that animates around interesting values
    c_real: f64,
    c_imag: f64,
    time: f64,
    max_iter: u32,
    zoom: f64,
}

impl Default for JuliaState {
    fn default() -> Self {
        Self {
            c_real: -0.7269,
            c_imag: 0.1889,
            time: 0.0,
            max_iter: 80,
            zoom: 1.0,
        }
    }
}

impl JuliaState {
    fn update(&mut self) {
        self.time += 0.015;
        // Animate c along a lemniscate (figure-8 curve) for beautiful morphing
        let t = self.time;
        self.c_real = 0.7885 * (t * 0.4).cos();
        self.c_imag = 0.7885 * (t * 0.4).sin() * (2.0 * t * 0.4).cos();
        // Gentle zoom oscillation
        self.zoom = 1.0 + 0.3 * (t * 0.2).sin();
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16) {
        if width == 0 || height == 0 {
            return;
        }

        let w = width as f64;
        let h = height as f64;
        let scale = 3.0 / (self.zoom * w.min(h));

        for py in 0..height as i32 {
            for px in 0..width as i32 {
                // Map pixel to complex plane centered at origin
                let mut x = (px as f64 - w / 2.0) * scale;
                let mut y = (py as f64 - h / 2.0) * scale;

                let mut iter = 0u32;
                while x * x + y * y <= 256.0 && iter < self.max_iter {
                    let xtemp = x * x - y * y + self.c_real;
                    y = 2.0 * x * y + self.c_imag;
                    x = xtemp;
                    iter += 1;
                }

                if iter < self.max_iter {
                    // Smooth coloring
                    let log_zn = (x * x + y * y).ln() / 2.0;
                    let nu = (log_zn / 2.0_f64.ln()).ln() / 2.0_f64.ln();
                    let smooth_iter = iter as f64 + 1.0 - nu;
                    let t = (smooth_iter / 40.0 + self.time * 0.05) % 1.0;
                    let color = palette_cyberpunk(t);
                    painter.point_colored(px, py, color);
                }
            }
        }
    }
}

// =============================================================================
// Wave Interference - Multiple wave sources creating interference patterns
// =============================================================================

#[derive(Debug, Clone)]
struct WaveSource {
    x: f64,
    y: f64,
    freq: f64,
    phase: f64,
    amplitude: f64,
}

#[derive(Debug, Clone)]
struct WaveInterferenceState {
    sources: Vec<WaveSource>,
    time: f64,
}

impl Default for WaveInterferenceState {
    fn default() -> Self {
        Self {
            sources: vec![
                WaveSource {
                    x: 0.25,
                    y: 0.5,
                    freq: 15.0,
                    phase: 0.0,
                    amplitude: 1.0,
                },
                WaveSource {
                    x: 0.75,
                    y: 0.5,
                    freq: 15.0,
                    phase: 0.0,
                    amplitude: 1.0,
                },
                WaveSource {
                    x: 0.5,
                    y: 0.25,
                    freq: 12.0,
                    phase: TAU / 3.0,
                    amplitude: 0.8,
                },
                WaveSource {
                    x: 0.5,
                    y: 0.75,
                    freq: 12.0,
                    phase: TAU / 3.0,
                    amplitude: 0.8,
                },
            ],
            time: 0.0,
        }
    }
}

impl WaveInterferenceState {
    fn update(&mut self) {
        if self.sources.len() < 2 {
            return;
        }
        self.time += 0.08;
        // Slowly move sources in circular patterns
        let t = self.time * 0.1;
        self.sources[0].x = 0.25 + 0.1 * t.cos();
        self.sources[0].y = 0.5 + 0.1 * t.sin();
        self.sources[1].x = 0.75 + 0.1 * (t + TAU / 2.0).cos();
        self.sources[1].y = 0.5 + 0.1 * (t + TAU / 2.0).sin();
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16, quality: FxQuality) {
        if width == 0 || height == 0 {
            return;
        }
        if self.sources.is_empty() {
            return;
        }

        let stride = fx_stride_for_area(quality, width, height, 12_000);
        if stride == 0 {
            return;
        }

        let w = width as f64;
        let h = height as f64;
        let inv_w = 1.0 / w;
        let inv_h = 1.0 / h;
        let sources = &self.sources;
        let denom = sources.len() as f64;

        for py in (0..height as usize).step_by(stride) {
            for px in (0..width as usize).step_by(stride) {
                let x = px as f64 * inv_w;
                let y = py as f64 * inv_h;

                // Sum waves from all sources (superposition principle)
                let mut sum = 0.0;
                for source in sources {
                    let dx = x - source.x;
                    let dy = y - source.y;
                    let dist = (dx * dx + dy * dy).sqrt();
                    // Wave equation: A * sin(k*r - omega*t + phi)
                    let wave =
                        source.amplitude * (source.freq * dist - self.time + source.phase).sin();
                    sum += wave;
                }

                // Normalize and map to color
                let normalized = (sum / denom + 1.0) / 2.0;
                let color = palette_ocean(normalized);
                painter.point_colored(px as i32, py as i32, color);
            }
        }
    }
}

// =============================================================================
// Spiral Galaxy - Logarithmic spirals with rotating star field
// =============================================================================

#[derive(Debug, Clone)]
struct SpiralStar {
    angle: f64, // Position along spiral
    arm: usize, // Which spiral arm
    radial_offset: f64,
    brightness: f64,
    hue_offset: f64,
}

#[derive(Debug, Clone)]
struct SpiralState {
    stars: Vec<SpiralStar>,
    rotation: f64,
    num_arms: usize,
    spiral_tightness: f64,
}

impl Default for SpiralState {
    fn default() -> Self {
        let num_arms = 4;
        let mut stars = Vec::with_capacity(3000);

        for _ in 0..3000 {
            stars.push(SpiralStar {
                angle: rand_simple() * TAU * 3.0, // Multiple rotations
                arm: (rand_simple() * num_arms as f64) as usize,
                radial_offset: (rand_simple() - 0.5) * 0.15,
                brightness: 0.3 + rand_simple() * 0.7,
                hue_offset: rand_simple() * 0.1,
            });
        }

        Self {
            stars,
            rotation: 0.0,
            num_arms,
            spiral_tightness: 0.3,
        }
    }
}

impl SpiralState {
    fn update(&mut self) {
        self.rotation += 0.008;
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16, quality: FxQuality) {
        if width == 0 || height == 0 {
            return;
        }

        let step = fx_stride_for_area(quality, width, height, 12_000);
        if step == 0 {
            return;
        }

        let w = width as f64;
        let h = height as f64;
        let cx = w / 2.0;
        let cy = h / 2.0;
        let scale = w.min(h) * 0.4;

        let max_stars = match quality {
            FxQuality::Full => 3000,
            FxQuality::Reduced => 2000,
            FxQuality::Minimal => 1200,
            FxQuality::Off => 0,
        };
        for (idx, star) in self.stars.iter().take(max_stars).enumerate() {
            if idx % step != 0 {
                continue;
            }
            // Logarithmic spiral: r = a * e^(b*theta)
            let arm_angle = (star.arm as f64 / self.num_arms as f64) * TAU;
            let theta = star.angle + arm_angle + self.rotation;
            // Clamp exponent to prevent infinity/overflow which can cause render artifacts or hangs
            let exponent = (self.spiral_tightness * star.angle).min(50.0);
            let r = 0.05 * exponent.exp() + star.radial_offset;

            // Clamp radius
            if r > 1.0 {
                continue;
            }

            let sx = cx + r * scale * theta.cos();
            let sy = cy + r * scale * theta.sin();

            let px = sx as i32;
            let py = sy as i32;

            if px >= 0 && px < width as i32 && py >= 0 && py < height as i32 {
                // Color based on radius and arm
                let hue = (r + star.hue_offset + self.rotation * 0.1) % 1.0;
                let brightness = star.brightness * (1.0 - r * 0.5);
                let (red, g, b) = hsv_to_rgb(hue * 360.0 * 0.3 + 200.0, 0.6, brightness);
                painter.point_colored(px, py, PackedRgba::rgb(red, g, b));
            }
        }

        // Draw bright galactic core
        for dy in -3..=3 {
            for dx in -3..=3 {
                let dist = ((dx * dx + dy * dy) as f64).sqrt();
                if dist < 3.0 {
                    let brightness = (1.0 - dist / 3.0) * 255.0;
                    let b = brightness as u8;
                    let px = (cx as i32 + dx).clamp(0, width as i32 - 1);
                    let py = (cy as i32 + dy).clamp(0, height as i32 - 1);
                    painter.point_colored(px, py, PackedRgba::rgb(255, b.saturating_add(100), b));
                }
            }
        }
    }
}

// =============================================================================
// Spin Lattice - Landau-Lifshitz spin dynamics on a 2D lattice
// =============================================================================

#[derive(Debug, Clone)]
struct SpinLatticeState {
    // Spin vectors stored as (theta, phi) spherical coordinates
    theta: Vec<f64>, // Polar angle
    phi: Vec<f64>,   // Azimuthal angle
    theta_next: Vec<f64>,
    phi_next: Vec<f64>,
    width: usize,
    height: usize,
    initialized: bool,
    // Physical parameters
    exchange_j: f64,   // Exchange coupling
    anisotropy_k: f64, // Uniaxial anisotropy
    damping: f64,      // Gilbert damping
    temperature: f64,  // Thermal noise
    time: f64,
}

impl Default for SpinLatticeState {
    fn default() -> Self {
        Self {
            theta: Vec::new(),
            phi: Vec::new(),
            theta_next: Vec::new(),
            phi_next: Vec::new(),
            width: 0,
            height: 0,
            initialized: false,
            exchange_j: 1.0,
            anisotropy_k: 0.3,
            damping: 0.1,
            temperature: 0.05,
            time: 0.0,
        }
    }
}

impl SpinLatticeState {
    fn init(&mut self, width: usize, height: usize) {
        if self.initialized && self.width == width && self.height == height {
            return;
        }
        self.width = width;
        self.height = height;
        let size = width * height;

        // Initialize with slightly perturbed ferromagnetic state + domain walls
        self.theta = vec![0.0; size];
        self.phi = vec![0.0; size];
        self.theta_next = vec![0.0; size];
        self.phi_next = vec![0.0; size];

        for y in 0..height {
            for x in 0..width {
                let idx = y * width + x;
                // Create initial domain structure
                let domain = ((x / 10) + (y / 10)) % 2;
                self.theta[idx] = if domain == 0 { 0.2 } else { TAU / 2.0 - 0.2 };
                self.phi[idx] = rand_simple() * TAU;
            }
        }
        self.initialized = true;
    }

    fn update(&mut self) {
        if !self.initialized || self.width < 3 || self.height < 3 {
            return;
        }

        self.time += 0.05;
        let w = self.width;
        let h = self.height;
        let dt = 0.1;

        let size = w * h;
        if self.theta_next.len() != size {
            self.theta_next = vec![0.0; size];
            self.phi_next = vec![0.0; size];
        }
        self.theta_next.copy_from_slice(&self.theta);
        self.phi_next.copy_from_slice(&self.phi);
        {
            let (theta, phi) = (&self.theta, &self.phi);
            let (new_theta, new_phi) = (&mut self.theta_next, &mut self.phi_next);

            // Landau-Lifshitz-Gilbert dynamics
            for y in 1..h - 1 {
                for x in 1..w - 1 {
                    let idx = y * w + x;

                    // Current spin in Cartesian
                    let theta_val = theta[idx];
                    let phi_val = phi[idx];
                    let sx = theta_val.sin() * phi_val.cos();
                    let sy = theta_val.sin() * phi_val.sin();
                    let sz = theta_val.cos();

                    // Effective field from exchange (sum of neighbor spins)
                    let mut hx = 0.0;
                    let mut hy = 0.0;
                    let mut hz = 0.0;

                    for &(dx, dy) in &[(1i32, 0i32), (-1, 0), (0, 1), (0, -1)] {
                        let nx = (x as i32 + dx) as usize;
                        let ny = (y as i32 + dy) as usize;
                        let nidx = ny * w + nx;
                        let nt = theta[nidx];
                        let np = phi[nidx];
                        hx += self.exchange_j * nt.sin() * np.cos();
                        hy += self.exchange_j * nt.sin() * np.sin();
                        hz += self.exchange_j * nt.cos();
                    }

                    // Anisotropy field (easy axis along z)
                    hz += self.anisotropy_k * sz;

                    // Thermal noise
                    hx += (rand_simple() - 0.5) * self.temperature;
                    hy += (rand_simple() - 0.5) * self.temperature;
                    hz += (rand_simple() - 0.5) * self.temperature;

                    // Torque: -S x H
                    let tx = sy * hz - sz * hy;
                    let ty = sz * hx - sx * hz;
                    let tz = sx * hy - sy * hx;

                    // Damping torque: -alpha * S x (S x H)
                    let dtx = self.damping * (sy * tz - sz * ty);
                    let dty = self.damping * (sz * tx - sx * tz);
                    let dtz = self.damping * (sx * ty - sy * tx);

                    // Update spin
                    let new_sx = sx + dt * (tx + dtx);
                    let new_sy = sy + dt * (ty + dty);
                    let new_sz = sz + dt * (tz + dtz);

                    // Normalize and convert back to spherical
                    let norm = (new_sx * new_sx + new_sy * new_sy + new_sz * new_sz).sqrt();
                    if norm > 0.001 {
                        let nsx = new_sx / norm;
                        let nsy = new_sy / norm;
                        let nsz = new_sz / norm;

                        new_theta[idx] = nsz.clamp(-1.0, 1.0).acos();
                        new_phi[idx] = nsy.atan2(nsx);
                    }
                }
            }
        }

        std::mem::swap(&mut self.theta, &mut self.theta_next);
        std::mem::swap(&mut self.phi, &mut self.phi_next);
    }

    fn render(&self, painter: &mut Painter, width: u16, height: u16, quality: FxQuality) {
        if !self.initialized || self.width == 0 || self.height == 0 {
            return;
        }
        if width == 0 || height == 0 {
            return;
        }

        let stride = fx_stride(quality);
        if stride == 0 {
            return;
        }

        let scale_x = self.width as f64 / width as f64;
        let scale_y = self.height as f64 / height as f64;

        for py in (0..height as usize).step_by(stride) {
            for px in (0..width as usize).step_by(stride) {
                let gx = ((px as f64 * scale_x) as usize).min(self.width - 1);
                let gy = ((py as f64 * scale_y) as usize).min(self.height - 1);
                let idx = gy * self.width + gx;

                let theta = self.theta[idx];
                let phi = self.phi[idx];

                // Color based on spin direction
                // z-component (theta) maps to brightness
                // xy-plane angle (phi) maps to hue
                let sz = theta.cos();
                let brightness = (sz + 1.0) / 2.0; // Map [-1,1] to [0,1]
                let hue = (phi / TAU + 0.5) % 1.0;

                let (r, g, b) = hsv_to_rgb(hue * 360.0, 0.9, brightness);
                painter.point_colored(px as i32, py as i32, PackedRgba::rgb(r, g, b));
            }
        }
    }
}

// =============================================================================
// Doom E1M1 - First-person braille raycaster
// =============================================================================

const DOOM_FOV: f32 = 1.2;
const DOOM_WALL_HEIGHT: f32 = 128.0;
const DOOM_GRAVITY: f32 = -4.2;
const DOOM_JUMP_VELOCITY: f32 = 28.0;
const DOOM_COLLISION_RADIUS: f32 = 20.0;
const DOOM_COLLISION_PAD: f32 = 2.0;
const DOOM_MOVE_STEP: f32 = 2.8;
const DOOM_STRAFE_STEP: f32 = 2.4;
const DOOM_TURN_RATE: f32 = 0.07;
const DOOM_SUBSTEP: f32 = 1.2;
const DOOM_GRID_CELL: f32 = 64.0;

#[derive(Debug, Clone, Copy)]
struct DoomLine {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    vx: f32,
    vy: f32,
    len_sq: f32,
}

impl DoomLine {
    fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        let vx = x2 - x1;
        let vy = y2 - y1;
        let len_sq = vx * vx + vy * vy;
        Self {
            x1,
            y1,
            x2,
            y2,
            vx,
            vy,
            len_sq,
        }
    }

    fn distance_sq(self, px: f32, py: f32) -> f32 {
        if self.len_sq <= 1e-6 {
            let dx = px - self.x1;
            let dy = py - self.y1;
            return dx * dx + dy * dy;
        }
        let wx = px - self.x1;
        let wy = py - self.y1;
        let t = ((wx * self.vx) + (wy * self.vy)) / self.len_sq;
        let t = t.clamp(0.0, 1.0);
        let proj_x = self.x1 + t * self.vx;
        let proj_y = self.y1 + t * self.vy;
        let dx = px - proj_x;
        let dy = py - proj_y;
        dx * dx + dy * dy
    }
}

#[derive(Debug, Clone)]
struct DoomGrid {
    origin_x: f32,
    origin_y: f32,
    cols: i32,
    rows: i32,
    cell_size: f32,
    cells: Vec<Vec<usize>>,
}

impl DoomGrid {
    fn new(lines: &[DoomLine], cell_size: f32) -> Self {
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for line in lines {
            min_x = min_x.min(line.x1.min(line.x2));
            min_y = min_y.min(line.y1.min(line.y2));
            max_x = max_x.max(line.x1.max(line.x2));
            max_y = max_y.max(line.y1.max(line.y2));
        }
        if !min_x.is_finite() || !min_y.is_finite() {
            return Self {
                origin_x: 0.0,
                origin_y: 0.0,
                cols: 0,
                rows: 0,
                cell_size,
                cells: Vec::new(),
            };
        }

        let span_x = (max_x - min_x).max(1.0);
        let span_y = (max_y - min_y).max(1.0);
        let cols = ((span_x / cell_size).ceil() as i32).max(1) + 1;
        let rows = ((span_y / cell_size).ceil() as i32).max(1) + 1;
        let mut cells = vec![Vec::new(); (cols * rows) as usize];

        for (idx, line) in lines.iter().enumerate() {
            let min_lx = line.x1.min(line.x2);
            let max_lx = line.x1.max(line.x2);
            let min_ly = line.y1.min(line.y2);
            let max_ly = line.y1.max(line.y2);

            let min_cx = ((min_lx - min_x) / cell_size).floor() as i32;
            let max_cx = ((max_lx - min_x) / cell_size).floor() as i32;
            let min_cy = ((min_ly - min_y) / cell_size).floor() as i32;
            let max_cy = ((max_ly - min_y) / cell_size).floor() as i32;

            let min_cx = min_cx.clamp(0, cols - 1);
            let max_cx = max_cx.clamp(0, cols - 1);
            let min_cy = min_cy.clamp(0, rows - 1);
            let max_cy = max_cy.clamp(0, rows - 1);

            for cy in min_cy..=max_cy {
                for cx in min_cx..=max_cx {
                    let idx_cell = (cy * cols + cx) as usize;
                    cells[idx_cell].push(idx);
                }
            }
        }

        Self {
            origin_x: min_x,
            origin_y: min_y,
            cols,
            rows,
            cell_size,
            cells,
        }
    }

    fn in_bounds(&self, cx: i32, cy: i32) -> bool {
        cx >= 0 && cy >= 0 && cx < self.cols && cy < self.rows
    }

    fn cell_indices(&self, cx: i32, cy: i32) -> &[usize] {
        let idx = (cy * self.cols + cx) as usize;
        &self.cells[idx]
    }
}

#[derive(Debug, Clone, Copy)]
struct DoomRay {
    sin_t: f32,
    cos_t: f32,
}

#[derive(Debug, Clone)]
struct DoomPlayer {
    x: f32,
    y: f32,
    yaw: f32,
    pitch: f32,
    jump_z: f32,
    vel_z: f32,
    grounded: bool,
}

impl Default for DoomPlayer {
    fn default() -> Self {
        let (px, py) = FREEDOOM_E1M1_PLAYER_START;
        Self {
            x: px as f32,
            y: py as f32,
            yaw: 0.0,
            pitch: 0.0,
            jump_z: 0.0,
            vel_z: 0.0,
            grounded: true,
        }
    }
}

#[derive(Debug, Clone)]
struct DoomE1M1State {
    time: f32,
    fire_flash: f32,
    player: DoomPlayer,
    walk_phase: f32,
    walk_intensity: f32,
    lines: Vec<DoomLine>,
    grid: DoomGrid,
    ray_cache: RefCell<Vec<DoomRay>>,
    ray_width: Cell<u16>,
    ray_marks: RefCell<Vec<u32>>,
    ray_stamp: Cell<u32>,
}

impl Default for DoomE1M1State {
    fn default() -> Self {
        let lines = build_doom_lines();
        let grid = DoomGrid::new(&lines, DOOM_GRID_CELL);
        let marks = vec![0; lines.len()];
        Self {
            time: 0.0,
            fire_flash: 0.0,
            player: DoomPlayer::default(),
            walk_phase: 0.0,
            walk_intensity: 0.0,
            lines,
            grid,
            ray_cache: RefCell::new(Vec::new()),
            ray_width: Cell::new(0),
            ray_marks: RefCell::new(marks),
            ray_stamp: Cell::new(1),
        }
    }
}

impl DoomE1M1State {
    fn ensure_ray_cache(&self, width: u16) {
        if width == 0 || self.ray_width.get() == width {
            return;
        }
        let mut ray_cache = self.ray_cache.borrow_mut();
        ray_cache.clear();
        let w = width as f32;
        let denom = (w - 1.0).max(1.0);
        ray_cache.reserve(width as usize);
        for px in 0..width {
            let x = px as f32;
            let t = (x / denom - 0.5) * DOOM_FOV;
            let (sin_t, cos_t) = t.sin_cos();
            ray_cache.push(DoomRay { sin_t, cos_t });
        }
        self.ray_width.set(width);
    }
    fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.player.yaw = (self.player.yaw + yaw_delta) % TAU as f32;
        self.player.pitch = (self.player.pitch + pitch_delta).clamp(-0.6, 0.6);
    }

    fn jump(&mut self) {
        if self.player.grounded {
            self.player.vel_z = DOOM_JUMP_VELOCITY;
            self.player.grounded = false;
        }
    }

    fn fire(&mut self) {
        self.fire_flash = 1.0;
    }

    fn move_forward(&mut self, amount: f32) {
        let dx = amount * self.player.yaw.cos();
        let dy = amount * self.player.yaw.sin();
        self.try_move(dx, dy);
        let stride = amount.abs();
        if stride > 0.0 {
            self.walk_phase += stride * 0.08;
            self.walk_intensity = (self.walk_intensity + stride * 0.02).min(1.0);
        }
    }

    fn strafe(&mut self, amount: f32) {
        let dx = amount * (self.player.yaw + std::f32::consts::FRAC_PI_2).cos();
        let dy = amount * (self.player.yaw + std::f32::consts::FRAC_PI_2).sin();
        self.try_move(dx, dy);
        let stride = amount.abs();
        if stride > 0.0 {
            self.walk_phase += stride * 0.07;
            self.walk_intensity = (self.walk_intensity + stride * 0.018).min(1.0);
        }
    }

    fn try_move(&mut self, dx: f32, dy: f32) {
        let max_step = DOOM_SUBSTEP.max(0.1);
        let mut steps = ((dx.abs().max(dy.abs())) / max_step).ceil() as u32;
        steps = steps.clamp(1, 12);
        let step_x = dx / steps as f32;
        let step_y = dy / steps as f32;
        for _ in 0..steps {
            if !self.try_move_step(step_x, step_y) {
                break;
            }
        }
    }

    fn try_move_step(&mut self, dx: f32, dy: f32) -> bool {
        let mut nx = (self.player.x + dx).clamp(0.0, 2048.0);
        let mut ny = (self.player.y + dy).clamp(0.0, 2048.0);

        if self.collides(nx, ny) {
            nx = (self.player.x + dx).clamp(0.0, 2048.0);
            ny = self.player.y;
            if self.collides(nx, ny) {
                nx = self.player.x;
                ny = (self.player.y + dy).clamp(0.0, 2048.0);
                if self.collides(nx, ny) {
                    return false;
                }
            }
        }

        self.player.x = nx;
        self.player.y = ny;
        true
    }

    fn collides(&self, x: f32, y: f32) -> bool {
        let radius = DOOM_COLLISION_RADIUS + DOOM_COLLISION_PAD;
        let radius_sq = radius * radius;
        for line in &self.lines {
            if line.distance_sq(x, y) < radius_sq {
                return true;
            }
        }
        false
    }

    fn update(&mut self) {
        self.time += 0.1;
        if self.fire_flash > 0.0 {
            self.fire_flash = (self.fire_flash - 0.12).max(0.0);
        }
        self.walk_intensity *= 0.88;
        if !self.player.grounded {
            self.player.vel_z += DOOM_GRAVITY * 0.1;
            self.player.jump_z += self.player.vel_z * 0.1;
            if self.player.jump_z <= 0.0 {
                self.player.jump_z = 0.0;
                self.player.vel_z = 0.0;
                self.player.grounded = true;
            }
        }
    }

    fn raycast(&self, ox: f32, oy: f32, dx: f32, dy: f32) -> Option<(f32, usize, f32, f32)> {
        let mut best_t = f32::INFINITY;
        let mut best_idx = 0usize;
        let mut best_side = 0.0f32;
        let mut best_u = 0.0f32;

        let grid = &self.grid;
        if grid.cols == 0 || grid.rows == 0 {
            return None;
        }

        let mut stamp = self.ray_stamp.get().wrapping_add(1);
        let mut marks = self.ray_marks.borrow_mut();
        if stamp == 0 {
            marks.fill(0);
            stamp = 1;
        }
        self.ray_stamp.set(stamp);

        let eps = 1e-6;
        let cell_size = grid.cell_size;
        let mut cell_x = ((ox - grid.origin_x) / cell_size).floor() as i32;
        let mut cell_y = ((oy - grid.origin_y) / cell_size).floor() as i32;
        if !grid.in_bounds(cell_x, cell_y) {
            return None;
        }

        let step_x = if dx >= 0.0 { 1 } else { -1 };
        let step_y = if dy >= 0.0 { 1 } else { -1 };
        let next_x = if dx >= 0.0 {
            grid.origin_x + (cell_x as f32 + 1.0) * cell_size
        } else {
            grid.origin_x + (cell_x as f32) * cell_size
        };
        let next_y = if dy >= 0.0 {
            grid.origin_y + (cell_y as f32 + 1.0) * cell_size
        } else {
            grid.origin_y + (cell_y as f32) * cell_size
        };

        let mut t_max_x = if dx.abs() < eps {
            f32::INFINITY
        } else {
            (next_x - ox) / dx
        };
        let mut t_max_y = if dy.abs() < eps {
            f32::INFINITY
        } else {
            (next_y - oy) / dy
        };
        let t_delta_x = if dx.abs() < eps {
            f32::INFINITY
        } else {
            cell_size / dx.abs()
        };
        let t_delta_y = if dy.abs() < eps {
            f32::INFINITY
        } else {
            cell_size / dy.abs()
        };

        let mut t_min = 0.0f32;
        loop {
            if !grid.in_bounds(cell_x, cell_y) {
                break;
            }

            for &idx in grid.cell_indices(cell_x, cell_y) {
                if marks[idx] == stamp {
                    continue;
                }
                marks[idx] = stamp;
                let line = &self.lines[idx];
                let denom = cross2(dx, dy, line.vx, line.vy);
                if denom.abs() < 1e-5 {
                    continue;
                }
                let px = line.x1 - ox;
                let py = line.y1 - oy;
                let t = cross2(px, py, line.vx, line.vy) / denom;
                let u = cross2(px, py, dx, dy) / denom;
                if t > 0.0
                    && (0.0..=1.0).contains(&u)
                    && (t < best_t || (t == best_t && idx < best_idx))
                {
                    best_t = t;
                    best_idx = idx;
                    best_side = if denom > 0.0 { 1.0 } else { -1.0 };
                    best_u = u;
                }
            }

            if t_min > best_t {
                break;
            }

            if t_max_x < t_max_y {
                cell_x += step_x;
                t_min = t_max_x;
                t_max_x += t_delta_x;
            } else {
                cell_y += step_y;
                t_min = t_max_y;
                t_max_y += t_delta_y;
            }
        }

        if best_t.is_finite() {
            Some((best_t, best_idx, best_side, best_u))
        } else {
            None
        }
    }

    fn render(
        &self,
        painter: &mut Painter,
        width: u16,
        height: u16,
        quality: FxQuality,
        _time: f64,
        frame: u64,
    ) {
        if width == 0 || height == 0 {
            return;
        }
        let stride = match quality {
            FxQuality::Off => 0,
            _ => 1,
        };
        if stride == 0 {
            return;
        }

        self.ensure_ray_cache(width);
        let ray_cache = self.ray_cache.borrow();
        let h = height as f32;
        let half_h = h * 0.5;
        let pitch_offset = -self.player.pitch * (h * 0.4);
        let jump_offset = self.player.jump_z * 0.2;
        let bob = (self.walk_phase).sin() * (2.0 + self.walk_intensity * 3.2);
        let mut center_y = half_h + pitch_offset + jump_offset + bob;
        center_y = center_y.clamp(0.0, (height.saturating_sub(1)) as f32);
        let proj_scale = h * 0.95;

        // Paint a Doom-like sky/floor gradient under the walls (brighter for visibility).
        let horizon = center_y.round() as i32;
        let max_y = height as i32 - 1;
        let sky_top = (90, 130, 200);
        let sky_bottom = (170, 205, 235);
        let floor_top = (182, 138, 100);
        let floor_bottom = (120, 86, 58);
        let fill_stride = 1;
        for py in (0..=max_y).step_by(fill_stride) {
            let (r, g, b) = if py <= horizon {
                let denom = horizon.max(1) as f64;
                let t = (py as f64 / denom).clamp(0.0, 1.0);
                lerp_rgb(sky_top, sky_bottom, t)
            } else {
                let denom = (max_y - horizon).max(1) as f64;
                let t = ((py - horizon) as f64 / denom).clamp(0.0, 1.0);
                lerp_rgb(floor_top, floor_bottom, t)
            };
            for px in (0..width as i32).step_by(fill_stride) {
                let jitter = ((px + py + frame as i32) & 3) as f32;
                let shade = 0.95 + jitter * 0.04;
                let rr = (r as f32 * shade).clamp(0.0, 255.0) as u8;
                let gg = (g as f32 * shade).clamp(0.0, 255.0) as u8;
                let bb = (b as f32 * shade).clamp(0.0, 255.0) as u8;
                painter.point_colored(px, py, PackedRgba::rgb(rr, gg, bb));
            }
        }

        let (sy, cy) = self.player.yaw.sin_cos();
        for px in (0..width as usize).step_by(stride) {
            let ray_params = ray_cache[px];
            let dir_x = cy * ray_params.cos_t - sy * ray_params.sin_t;
            let dir_y = sy * ray_params.cos_t + cy * ray_params.sin_t;
            let hit = self.raycast(self.player.x, self.player.y, dir_x, dir_y);

            let (dist, hit_idx, side, hit_u) = if let Some(hit) = hit {
                hit
            } else {
                continue;
            };

            let corrected = (dist * ray_params.cos_t).max(1.0);
            let wall_height = (DOOM_WALL_HEIGHT / corrected) * proj_scale;
            let top = (center_y - wall_height).round() as i32;
            let bottom = (center_y + wall_height).round() as i32;

            let mut base = palette_doom_wall(hit_idx);
            if side > 0.0 {
                base = palette_doom_wall(hit_idx + 3);
            }

            let fog = (corrected / 850.0).clamp(0.0, 1.0);
            let mut brightness = (0.65 + (1.0 - fog).powf(1.0)).clamp(0.0, 2.1);
            if self.fire_flash > 0.0 {
                brightness = (brightness + self.fire_flash * 0.35).min(1.6);
            }

            let tex_band = ((hit_u * 32.0).floor() as i32) & 3;
            let tex_boost = match tex_band {
                0 => 1.05,
                1 => 1.18,
                2 => 1.28,
                _ => 1.38,
            };

            let grain =
                (((px as u64).wrapping_mul(113) ^ (frame.wrapping_mul(131)) ^ hit_idx as u64) & 7)
                    as f32
                    / 80.0;
            brightness = (brightness * tex_boost + grain + 0.42).clamp(0.65, 2.2);

            let r = (base.r() as f32 * brightness).min(255.0) as u8;
            let g = (base.g() as f32 * brightness).min(255.0) as u8;
            let b = (base.b() as f32 * brightness).min(255.0) as u8;
            let wall_color = PackedRgba::rgb(r, g, b);

            let sky_base = PackedRgba::rgb(100, 140, 200);
            let floor_base = PackedRgba::rgb(140, 102, 72);
            let sky_fade = fog.clamp(0.0, 1.0);
            let floor_fade = fog.clamp(0.0, 1.0);
            let ceiling_color = PackedRgba::rgb(
                ((sky_base.r() as f32) * (1.0 - sky_fade)) as u8,
                ((sky_base.g() as f32) * (1.0 - sky_fade)) as u8,
                ((sky_base.b() as f32) * (1.0 - sky_fade)) as u8,
            );
            let floor_color = PackedRgba::rgb(
                ((floor_base.r() as f32) * (1.0 - floor_fade)) as u8,
                ((floor_base.g() as f32) * (1.0 - floor_fade)) as u8,
                ((floor_base.b() as f32) * (1.0 - floor_fade)) as u8,
            );

            let top_line = top.clamp(0, height as i32 - 1);
            let bottom_line = bottom.clamp(0, height as i32 - 1);

            if top_line > 0 {
                painter.line_colored(px as i32, 0, px as i32, top_line, Some(ceiling_color));
            }
            if bottom_line < height as i32 - 1 {
                painter.line_colored(
                    px as i32,
                    bottom_line,
                    px as i32,
                    height as i32 - 1,
                    Some(floor_color),
                );
            }

            painter.line_colored(
                px as i32,
                top_line,
                px as i32,
                bottom_line,
                Some(wall_color),
            );
        }

        // Simple crosshair + muzzle flash
        let cx = (width / 2) as i32;
        let cy = (center_y.round() as i32).clamp(0, height as i32 - 1);
        let flash = self.fire_flash;
        let cross_r = (220.0 + flash * 30.0).min(255.0) as u8;
        let cross_color = PackedRgba::rgb(cross_r, 240, 240);
        painter.line_colored(cx - 3, cy, cx + 3, cy, Some(cross_color));
        painter.line_colored(cx, cy - 2, cx, cy + 2, Some(cross_color));

        // Simple weapon silhouette at the bottom to sell the FPS vibe.
        if height > 6 {
            let gun_y = height as i32 - 3;
            let gun_x = cx - 8;
            let gun_color = PackedRgba::rgb(70, 54, 38);
            let gun_high = PackedRgba::rgb(110, 90, 60);
            painter.line_colored(gun_x, gun_y, gun_x + 16, gun_y, Some(gun_color));
            painter.line_colored(gun_x + 3, gun_y - 1, gun_x + 13, gun_y - 1, Some(gun_high));
            painter.line_colored(gun_x + 5, gun_y - 2, gun_x + 11, gun_y - 2, Some(gun_color));
            if flash > 0.0 {
                let flash_color = PackedRgba::rgb((240.0 * flash + 120.0) as u8, 200, 120);
                painter.line_colored(cx - 1, gun_y - 4, cx + 1, gun_y - 4, Some(flash_color));
            }
        }
    }
}

// =============================================================================
// Quake E1M1 - True 3D braille rasterizer
// =============================================================================

#[derive(Debug, Clone, Copy)]
struct Vec3 {
    x: f32,
    y: f32,
    z: f32,
}

impl Vec3 {
    const fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }

    fn dot(self, other: Self) -> f32 {
        self.x * other.x + self.y * other.y + self.z * other.z
    }

    fn cross(self, other: Self) -> Self {
        Self {
            x: self.y * other.z - self.z * other.y,
            y: self.z * other.x - self.x * other.z,
            z: self.x * other.y - self.y * other.x,
        }
    }

    fn len(self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
    }

    fn normalized(self) -> Self {
        let len = self.len();
        if len > 0.0 {
            Self::new(self.x / len, self.y / len, self.z / len)
        } else {
            self
        }
    }
}

impl core::ops::Add for Vec3 {
    type Output = Self;
    fn add(self, other: Self) -> Self {
        Self::new(self.x + other.x, self.y + other.y, self.z + other.z)
    }
}

impl core::ops::Sub for Vec3 {
    type Output = Self;
    fn sub(self, other: Self) -> Self {
        Self::new(self.x - other.x, self.y - other.y, self.z - other.z)
    }
}

impl core::ops::Mul<f32> for Vec3 {
    type Output = Self;
    fn mul(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
}

const QUAKE_EYE_HEIGHT: f32 = 0.18;
const QUAKE_GRAVITY: f32 = -0.34;
const QUAKE_JUMP_VELOCITY: f32 = 0.24;
const QUAKE_COLLISION_RADIUS: f32 = 0.075;
const QUAKE_FOV: f32 = 1.5;
const QUAKE_MOVE_STEP: f32 = 0.012;
const QUAKE_STRAFE_STEP: f32 = 0.01;
const QUAKE_TURN_RATE: f32 = 0.055;
const QUAKE_STEP_HEIGHT: f32 = 0.06;
const QUAKE_SUBSTEP: f32 = 0.02;

#[derive(Debug, Clone, Copy)]
struct WallSeg {
    x1: f32,
    y1: f32,
    x2: f32,
    y2: f32,
    vx: f32,
    vy: f32,
    len_sq: f32,
    inv_len_sq: f32,
}

impl WallSeg {
    fn new(a: Vec3, b: Vec3) -> Option<Self> {
        let vx = b.x - a.x;
        let vy = b.y - a.y;
        let len_sq = vx * vx + vy * vy;
        if len_sq <= 1e-6 {
            return None;
        }
        Some(Self {
            x1: a.x,
            y1: a.y,
            x2: b.x,
            y2: b.y,
            vx,
            vy,
            len_sq,
            inv_len_sq: 1.0 / len_sq,
        })
    }

    #[inline]
    fn distance_sq(self, px: f32, py: f32) -> f32 {
        debug_assert!(self.len_sq > 1e-6);
        let wx = px - self.x1;
        let wy = py - self.y1;
        let t = ((wx * self.vx) + (wy * self.vy)) * self.inv_len_sq;
        let t = t.clamp(0.0, 1.0);
        let proj_x = self.x1 + t * self.vx;
        let proj_y = self.y1 + t * self.vy;
        let dx = px - proj_x;
        let dy = py - proj_y;
        dx * dx + dy * dy
    }
}

#[derive(Debug, Clone)]
struct FloorTri {
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    min_x: f32,
    max_x: f32,
    min_y: f32,
    max_y: f32,
    area: f32,
}

impl FloorTri {
    fn new(v0: Vec3, v1: Vec3, v2: Vec3) -> Option<Self> {
        let area = cross2(v1.x - v0.x, v1.y - v0.y, v2.x - v0.x, v2.y - v0.y);
        if area.abs() <= 1e-6 {
            return None;
        }
        let min_x = v0.x.min(v1.x).min(v2.x);
        let max_x = v0.x.max(v1.x).max(v2.x);
        let min_y = v0.y.min(v1.y).min(v2.y);
        let max_y = v0.y.max(v1.y).max(v2.y);
        Some(Self {
            v0,
            v1,
            v2,
            min_x,
            max_x,
            min_y,
            max_y,
            area,
        })
    }
}

#[derive(Debug, Clone, Copy)]
struct QuakeTri {
    v0: Vec3,
    v1: Vec3,
    v2: Vec3,
    normal: Vec3,
    base: PackedRgba,
    is_floor: bool,
    is_ceiling: bool,
    ambient: f32,
    diffuse_scale: f32,
    diffuse: f32,
}

#[derive(Debug, Clone)]
struct QuakePlayer {
    pos: Vec3,
    yaw: f32,
    pitch: f32,
    vel_z: f32,
    grounded: bool,
}

impl QuakePlayer {
    fn new(pos: Vec3) -> Self {
        Self {
            pos,
            yaw: 0.0,
            pitch: 0.0,
            vel_z: 0.0,
            grounded: true,
        }
    }
}

#[derive(Debug, Clone)]
struct QuakeE1M1State {
    player: QuakePlayer,
    fire_flash: f32,
    bounds_min: Vec3,
    bounds_max: Vec3,
    wall_segments: Vec<WallSeg>,
    floor_tris: Vec<FloorTri>,
    quake_tris: Vec<QuakeTri>,
    depth: Vec<f32>,
    depth_stamp: Vec<u32>,
    depth_epoch: u32,
    depth_w: u16,
    depth_h: u16,
    walk_phase: f32,
    walk_intensity: f32,
}

impl Default for QuakeE1M1State {
    fn default() -> Self {
        let (min, max) = QuakeE1M1State::compute_bounds();
        let (wall_segments, floor_tris, quake_tris) = QuakeE1M1State::build_collision(min, max);
        let center_x = (min.x + max.x) * 0.5;
        let center_y = (min.y + max.y) * 0.5;
        let start = Vec3::new(center_x, center_y, min.z + QUAKE_EYE_HEIGHT);
        let mut state = Self {
            player: QuakePlayer::new(start),
            fire_flash: 0.0,
            bounds_min: min,
            bounds_max: max,
            wall_segments,
            floor_tris,
            quake_tris,
            depth: Vec::new(),
            depth_stamp: Vec::new(),
            depth_epoch: 1,
            depth_w: 0,
            depth_h: 0,
            walk_phase: 0.0,
            walk_intensity: 0.0,
        };
        state.player.pos = state.pick_spawn();
        state
    }
}

impl QuakeE1M1State {
    fn compute_bounds() -> (Vec3, Vec3) {
        let inv_scale = 1.0 / 1024.0;
        let mut min = Vec3::new(f32::INFINITY, f32::INFINITY, f32::INFINITY);
        let mut max = Vec3::new(f32::NEG_INFINITY, f32::NEG_INFINITY, f32::NEG_INFINITY);
        for (x, y, z) in QUAKE_E1M1_VERTS {
            let wx = *x as f32 * inv_scale;
            let wy = *y as f32 * inv_scale;
            let wz = *z as f32 * inv_scale;
            min.x = min.x.min(wx);
            min.y = min.y.min(wy);
            min.z = min.z.min(wz);
            max.x = max.x.max(wx);
            max.y = max.y.max(wy);
            max.z = max.z.max(wz);
        }
        (min, max)
    }

    fn build_collision(
        bounds_min: Vec3,
        bounds_max: Vec3,
    ) -> (Vec<WallSeg>, Vec<FloorTri>, Vec<QuakeTri>) {
        let inv_scale = 1.0 / 1024.0;
        let mut walls = Vec::new();
        let mut floors = Vec::new();
        let mut tris = Vec::with_capacity(QUAKE_E1M1_TRIS.len());
        let light_dir = Vec3::new(0.3, -0.45, 0.85).normalized();
        let height_span = (bounds_max.z - bounds_min.z).max(0.001);

        let mut seen_edges: HashSet<(u16, u16)> = HashSet::new();
        let mut push_edge = |ia: u16, ib: u16, a: Vec3, b: Vec3| {
            let key = if ia < ib { (ia, ib) } else { (ib, ia) };
            if !seen_edges.insert(key) {
                return;
            }
            if let Some(seg) = WallSeg::new(a, b) {
                walls.push(seg);
            }
        };

        for (i0, i1, i2) in QUAKE_E1M1_TRIS.iter().copied() {
            let v0 = QUAKE_E1M1_VERTS[i0 as usize];
            let v1 = QUAKE_E1M1_VERTS[i1 as usize];
            let v2 = QUAKE_E1M1_VERTS[i2 as usize];

            let w0 = Vec3::new(
                v0.0 as f32 * inv_scale,
                v0.1 as f32 * inv_scale,
                v0.2 as f32 * inv_scale,
            );
            let w1 = Vec3::new(
                v1.0 as f32 * inv_scale,
                v1.1 as f32 * inv_scale,
                v1.2 as f32 * inv_scale,
            );
            let w2 = Vec3::new(
                v2.0 as f32 * inv_scale,
                v2.1 as f32 * inv_scale,
                v2.2 as f32 * inv_scale,
            );

            let n = (w1 - w0).cross(w2 - w0);
            let len = n.len();
            if len <= 1e-6 {
                continue;
            }
            let normal = n * (1.0 / len);

            if normal.z.abs() < 0.35 {
                push_edge(i0, i1, w0, w1);
                push_edge(i1, i2, w1, w2);
                push_edge(i2, i0, w2, w0);
            }

            if normal.z > 0.35
                && let Some(tri) = FloorTri::new(w0, w1, w2)
            {
                floors.push(tri);
            }

            let height_t = ((w0.z - bounds_min.z) / height_span).clamp(0.0, 1.0);
            let is_floor = normal.z > 0.55;
            let is_ceiling = normal.z < -0.55;
            let base = if is_floor {
                palette_quake_floor(height_t as f64)
            } else if is_ceiling {
                palette_quake_ceiling(height_t as f64)
            } else {
                palette_quake_stone(height_t as f64)
            };
            let ambient = if is_floor {
                0.58
            } else if is_ceiling {
                0.45
            } else {
                0.50
            };
            let diffuse_scale = if is_floor || is_ceiling { 1.0 } else { 1.1 };
            let diffuse = normal.dot(light_dir).abs();

            tris.push(QuakeTri {
                v0: w0,
                v1: w1,
                v2: w2,
                normal,
                base,
                is_floor,
                is_ceiling,
                ambient,
                diffuse_scale,
                diffuse,
            });
        }

        (walls, floors, tris)
    }

    fn pick_spawn(&self) -> Vec3 {
        let center_x = (self.bounds_min.x + self.bounds_max.x) * 0.5;
        let center_y = (self.bounds_min.y + self.bounds_max.y) * 0.5;
        let mut best_score = f32::NEG_INFINITY;
        let mut best = None;
        let min_clear_sq = (QUAKE_COLLISION_RADIUS * 2.2).powi(2);

        for tri in &self.floor_tris {
            let cx = (tri.v0.x + tri.v1.x + tri.v2.x) / 3.0;
            let cy = (tri.v0.y + tri.v1.y + tri.v2.y) / 3.0;
            if cx <= self.bounds_min.x
                || cx >= self.bounds_max.x
                || cy <= self.bounds_min.y
                || cy >= self.bounds_max.y
            {
                continue;
            }
            let dist_sq = min_wall_distance_sq_bounded(cx, cy, &self.wall_segments, min_clear_sq);
            if dist_sq < min_clear_sq {
                continue;
            }
            let z = self.ground_eye_height(cx, cy);
            let score = dist_sq + (z - self.bounds_min.z) * 0.15;
            if score > best_score {
                best_score = score;
                best = Some(Vec3::new(cx, cy, z));
            }
        }

        best.unwrap_or_else(|| {
            Vec3::new(
                center_x,
                center_y,
                self.ground_eye_height(center_x, center_y),
            )
        })
    }

    fn ground_height_at(&self, x: f32, y: f32) -> Option<f32> {
        let mut best = None;
        let eps = 1e-3;

        for tri in &self.floor_tris {
            if x < tri.min_x || x > tri.max_x || y < tri.min_y || y > tri.max_y {
                continue;
            }

            let w0 = cross2(tri.v1.x - x, tri.v1.y - y, tri.v2.x - x, tri.v2.y - y) / tri.area;
            let w1 = cross2(tri.v2.x - x, tri.v2.y - y, tri.v0.x - x, tri.v0.y - y) / tri.area;
            let w2 = 1.0 - w0 - w1;

            if w0 >= -eps && w1 >= -eps && w2 >= -eps {
                let z = w0 * tri.v0.z + w1 * tri.v1.z + w2 * tri.v2.z;
                if best.is_none_or(|best_z| z > best_z) {
                    best = Some(z);
                }
            }
        }

        best
    }

    fn ground_eye_height(&self, x: f32, y: f32) -> f32 {
        let ground = self.ground_height_at(x, y).unwrap_or(self.bounds_min.z);
        ground + QUAKE_EYE_HEIGHT
    }

    fn snap_to_ground(&mut self, prev_pos: Vec3) -> bool {
        if !self.player.grounded {
            return true;
        }
        let ground = self.ground_eye_height(self.player.pos.x, self.player.pos.y);
        let dz = ground - prev_pos.z;
        if dz > QUAKE_STEP_HEIGHT {
            self.player.pos = prev_pos;
            return false;
        }
        if dz < -QUAKE_STEP_HEIGHT {
            self.player.pos.z = prev_pos.z;
            self.player.grounded = false;
            return true;
        }
        self.player.pos.z = ground;
        true
    }

    fn collides(&self, x: f32, y: f32) -> bool {
        let radius_sq = QUAKE_COLLISION_RADIUS * QUAKE_COLLISION_RADIUS;
        for seg in &self.wall_segments {
            let dist_sq = seg.distance_sq(x, y);
            if dist_sq < radius_sq {
                return true;
            }
        }
        false
    }

    fn look(&mut self, yaw_delta: f32, pitch_delta: f32) {
        self.player.yaw = (self.player.yaw + yaw_delta) % TAU as f32;
        self.player.pitch = (self.player.pitch + pitch_delta).clamp(-0.9, 0.9);
    }

    fn move_forward(&mut self, amount: f32) {
        let (sy, cy) = self.player.yaw.sin_cos();
        let dx = cy * amount;
        let dy = sy * amount;
        self.try_move(dx, dy);
        let stride = amount.abs();
        if stride > 0.0 {
            self.walk_phase += stride * 7.0;
            self.walk_intensity = (self.walk_intensity + stride * 0.6).min(1.0);
        }
    }

    fn strafe(&mut self, amount: f32) {
        let (sy, cy) = self.player.yaw.sin_cos();
        let dx = -sy * amount;
        let dy = cy * amount;
        self.try_move(dx, dy);
        let stride = amount.abs();
        if stride > 0.0 {
            self.walk_phase += stride * 6.0;
            self.walk_intensity = (self.walk_intensity + stride * 0.5).min(1.0);
        }
    }

    fn try_move(&mut self, dx: f32, dy: f32) {
        let margin = (QUAKE_COLLISION_RADIUS + 0.02).max(0.04);
        let min_x = self.bounds_min.x + margin;
        let max_x = self.bounds_max.x - margin;
        let min_y = self.bounds_min.y + margin;
        let max_y = self.bounds_max.y - margin;

        let max_step = QUAKE_SUBSTEP.max(0.001);
        let mut steps = ((dx.abs().max(dy.abs())) / max_step).ceil() as u32;
        steps = steps.clamp(1, 16);
        let step_x = dx / steps as f32;
        let step_y = dy / steps as f32;

        for _ in 0..steps {
            let prev = self.player.pos;
            let mut nx = (self.player.pos.x + step_x).clamp(min_x, max_x);
            let mut ny = (self.player.pos.y + step_y).clamp(min_y, max_y);

            if self.collides(nx, ny) {
                nx = (self.player.pos.x + step_x).clamp(min_x, max_x);
                ny = self.player.pos.y;
                if self.collides(nx, ny) {
                    nx = self.player.pos.x;
                    ny = (self.player.pos.y + step_y).clamp(min_y, max_y);
                    if self.collides(nx, ny) {
                        break;
                    }
                }
            }

            self.player.pos.x = nx;
            self.player.pos.y = ny;
            if !self.snap_to_ground(prev) {
                break;
            }
        }
    }

    fn jump(&mut self) {
        if self.player.grounded {
            self.player.vel_z = QUAKE_JUMP_VELOCITY;
            self.player.grounded = false;
        }
    }

    fn fire(&mut self) {
        self.fire_flash = 1.0;
    }

    fn update(&mut self) {
        if self.fire_flash > 0.0 {
            self.fire_flash = (self.fire_flash - 0.1).max(0.0);
        }
        self.walk_intensity *= 0.86;

        let ground = self.ground_eye_height(self.player.pos.x, self.player.pos.y);
        if self.player.grounded {
            let dz = ground - self.player.pos.z;
            if dz.abs() <= QUAKE_STEP_HEIGHT {
                self.player.pos.z = ground;
            } else if dz < -QUAKE_STEP_HEIGHT {
                self.player.grounded = false;
            } else {
                self.player.pos.z = ground;
            }
        }

        if !self.player.grounded {
            self.player.vel_z += QUAKE_GRAVITY * 0.1;
            self.player.pos.z += self.player.vel_z * 0.1;
            if self.player.pos.z <= ground {
                self.player.pos.z = ground;
                self.player.vel_z = 0.0;
                self.player.grounded = true;
            }
        }
    }

    fn ensure_depth(&mut self, width: u16, height: u16) {
        let len = width as usize * height as usize;
        if len > self.depth.len() {
            self.depth.resize(len, f32::INFINITY);
        }
        if len > self.depth_stamp.len() {
            self.depth_stamp.resize(len, 0);
        }
        self.depth_w = width;
        self.depth_h = height;
    }

    fn clear_depth(&mut self) {
        self.depth_epoch = self.depth_epoch.wrapping_add(1);
        if self.depth_epoch == 0 {
            self.depth_stamp.fill(0);
            self.depth_epoch = 1;
        }
    }

    fn render(
        &mut self,
        painter: &mut Painter,
        width: u16,
        height: u16,
        quality: FxQuality,
        _time: f64,
        frame: u64,
    ) {
        if width == 0 || height == 0 {
            return;
        }

        let stride = match quality {
            FxQuality::Off => 0,
            _ => 1,
        };
        if stride == 0 {
            return;
        }

        self.ensure_depth(width, height);
        self.clear_depth();

        let w = width as f32;
        let h = height as f32;
        let width_usize = width as usize;
        let center = Vec3::new(w * 0.5, h * 0.5, 0.0);
        let bob = (self.walk_phase).sin() * (0.015 + self.walk_intensity * 0.025);
        let eye = Vec3::new(
            self.player.pos.x,
            self.player.pos.y,
            self.player.pos.z + bob,
        );
        let (sy, cy) = self.player.yaw.sin_cos();
        let (sp, cp) = self.player.pitch.sin_cos();
        let forward = Vec3::new(cy * cp, sy * cp, sp).normalized();
        let right = Vec3::new(-sy, cy, 0.0).normalized();
        let up = right.cross(forward).normalized();

        let proj_scale = (w.min(h) * 0.5) / (QUAKE_FOV * 0.5).tan();
        let near = 0.02f32;
        let far = 10.0f32;
        let fog_color = PackedRgba::rgb(130, 140, 160);

        let horizon = (h * 0.5 - self.player.pitch * (h * 0.35) + bob * proj_scale * 0.8)
            .clamp(0.0, h - 1.0)
            .round() as i32;
        let max_y = height as i32 - 1;
        let sky_top = (92, 112, 140);
        let sky_bottom = (150, 175, 210);
        let floor_top = (160, 128, 96);
        let floor_bottom = (110, 86, 62);
        let fill_stride = 1;
        for py in (0..=max_y).step_by(fill_stride) {
            let (r, g, b) = if py <= horizon {
                let denom = horizon.max(1) as f64;
                let t = (py as f64 / denom).clamp(0.0, 1.0);
                lerp_rgb(sky_top, sky_bottom, t)
            } else {
                let denom = (max_y - horizon).max(1) as f64;
                let t = ((py - horizon) as f64 / denom).clamp(0.0, 1.0);
                lerp_rgb(floor_top, floor_bottom, t)
            };
            for px in (0..width as i32).step_by(fill_stride) {
                let jitter = ((px * 3 + py * 5 + frame as i32) & 3) as f32;
                let shade = 0.92 + jitter * 0.05;
                let rr = (r as f32 * shade).clamp(0.0, 255.0) as u8;
                let gg = (g as f32 * shade).clamp(0.0, 255.0) as u8;
                let bb = (b as f32 * shade).clamp(0.0, 255.0) as u8;
                painter.point_colored(px, py, PackedRgba::rgb(rr, gg, bb));
            }
        }

        let tri_step = match quality {
            FxQuality::Off => 0,
            _ => 1,
        };
        let edge_stride = if tri_step > 1 { tri_step * 2 } else { 1 };
        let inv_floor_tile = 1.0 / 0.35;
        let inv_ceiling_tile = 1.0 / 0.45;
        let inv_wall_stripe_x = 1.0 / 0.25;
        let inv_wall_stripe_z = 1.0 / 0.18;

        let edge = |ax: f32, ay: f32, bx: f32, by: f32, cx: f32, cy: f32| {
            (cx - ax) * (by - ay) - (cy - ay) * (bx - ax)
        };

        for (tri_idx, tri) in self.quake_tris.iter().enumerate().step_by(tri_step) {
            let world0 = tri.v0;
            let world1 = tri.v1;
            let world2 = tri.v2;

            let n = tri.normal;
            let view_dir = (eye - world0).normalized();
            let facing = n.dot(view_dir);
            if facing.abs() <= 0.02 {
                continue;
            }
            let facing = facing.abs();
            let rim = (1.0 - facing.clamp(0.0, 1.0)).powf(2.0) * 0.45;

            let is_floor = tri.is_floor;
            let is_ceiling = tri.is_ceiling;
            let base = tri.base;
            let light =
                (tri.ambient + tri.diffuse * tri.diffuse_scale + rim + 0.08).clamp(0.0, 1.6);

            let cam0 = Vec3::new(
                (world0 - eye).dot(right),
                (world0 - eye).dot(up),
                (world0 - eye).dot(forward),
            );
            let cam1 = Vec3::new(
                (world1 - eye).dot(right),
                (world1 - eye).dot(up),
                (world1 - eye).dot(forward),
            );
            let cam2 = Vec3::new(
                (world2 - eye).dot(right),
                (world2 - eye).dot(up),
                (world2 - eye).dot(forward),
            );
            let tri_depth = (cam0.z + cam1.z + cam2.z) / 3.0;
            let edge_fade = ((tri_depth - near) / (far - near)).clamp(0.0, 1.0);
            let mut clipped = [Vec3::new(0.0, 0.0, 0.0); 4];
            let clipped_len = clip_triangle_near(cam0, cam1, cam2, near, &mut clipped);
            if clipped_len < 3 {
                continue;
            }

            let mut draw_tri = |a: Vec3, b: Vec3, c: Vec3| {
                let sx0 = center.x + (a.x / a.z) * proj_scale;
                let sy0 = center.y - (a.y / a.z) * proj_scale;
                let sx1 = center.x + (b.x / b.z) * proj_scale;
                let sy1 = center.y - (b.y / b.z) * proj_scale;
                let sx2 = center.x + (c.x / c.z) * proj_scale;
                let sy2 = center.y - (c.y / c.z) * proj_scale;

                let minx = sx0.min(sx1).min(sx2).floor().max(0.0) as i32;
                let maxx = sx0.max(sx1).max(sx2).ceil().min(w - 1.0) as i32;
                let miny = sy0.min(sy1).min(sy2).floor().max(0.0) as i32;
                let maxy = sy0.max(sy1).max(sy2).ceil().min(h - 1.0) as i32;

                if minx > maxx || miny > maxy {
                    return;
                }

                let area = edge(sx0, sy0, sx1, sy1, sx2, sy2);
                if area.abs() < 1e-5 {
                    return;
                }

                let inv_area = 1.0 / area;
                let stride_usize = stride;
                let e0_dx = sy1 - sy2;
                let e0_dy = -(sx1 - sx2);
                let e1_dx = sy2 - sy0;
                let e1_dy = -(sx2 - sx0);
                let e2_dx = sy0 - sy1;
                let e2_dy = -(sx0 - sx1);
                let start_x = minx as f32;
                let start_y = miny as f32;
                let mut w0_row = edge(sx1, sy1, sx2, sy2, start_x, start_y);
                let mut w1_row = edge(sx2, sy2, sx0, sy0, start_x, start_y);
                let mut w2_row = edge(sx0, sy0, sx1, sy1, start_x, start_y);

                for py in (miny..=maxy).step_by(stride_usize) {
                    let mut w0e = w0_row;
                    let mut w1e = w1_row;
                    let mut w2e = w2_row;
                    for px in (minx..=maxx).step_by(stride_usize) {
                        if (w0e * area) < 0.0 || (w1e * area) < 0.0 || (w2e * area) < 0.0 {
                            w0e += e0_dx;
                            w1e += e1_dx;
                            w2e += e2_dx;
                            continue;
                        }

                        let b0 = w0e * inv_area;
                        let b1 = w1e * inv_area;
                        let b2 = w2e * inv_area;
                        let z = b0 * a.z + b1 * b.z + b2 * c.z;

                        let idx = py as usize * width_usize + px as usize;
                        let prior = if self.depth_stamp[idx] == self.depth_epoch {
                            self.depth[idx]
                        } else {
                            f32::INFINITY
                        };
                        if z >= prior {
                            w0e += e0_dx;
                            w1e += e1_dx;
                            w2e += e2_dx;
                            continue;
                        }
                        self.depth_stamp[idx] = self.depth_epoch;
                        self.depth[idx] = z;

                        let wx = world0.x * b0 + world1.x * b1 + world2.x * b2;
                        let wy = world0.y * b0 + world1.y * b1 + world2.y * b2;
                        let wz = world0.z * b0 + world1.z * b1 + world2.z * b2;

                        let fog = ((z - near) / (far - near)).clamp(0.0, 1.0);
                        let fade = (1.0 - fog).powf(1.35);
                        let pattern = if is_floor {
                            let tile = ((wx * inv_floor_tile).floor() as i32
                                + (wy * inv_floor_tile).floor() as i32)
                                & 1;
                            if tile == 0 { 0.92 } else { 1.05 }
                        } else if is_ceiling {
                            let tile = ((wx * inv_ceiling_tile).floor() as i32
                                + (wy * inv_ceiling_tile).floor() as i32)
                                & 1;
                            if tile == 0 { 0.95 } else { 1.03 }
                        } else {
                            let stripe = ((wx * inv_wall_stripe_x).floor() as i32
                                + (wz * inv_wall_stripe_z).floor() as i32)
                                & 1;
                            if stripe == 0 { 0.9 } else { 1.08 }
                        };
                        let grain = (((px as u64).wrapping_mul(73856093)
                            ^ (py as u64).wrapping_mul(19349663)
                            ^ frame)
                            & 3) as f32
                            / 20.0;
                        let mut brightness =
                            (light * fade * pattern + grain + 0.65).clamp(0.55, 2.1);
                        if self.fire_flash > 0.0 {
                            brightness = (brightness + self.fire_flash * 0.5).min(1.9);
                        }

                        let mut r = base.r() as f32 * brightness;
                        let mut g = base.g() as f32 * brightness;
                        let mut b = base.b() as f32 * brightness;
                        r += (fog_color.r() as f32 - r) * fog;
                        g += (fog_color.g() as f32 - g) * fog;
                        b += (fog_color.b() as f32 - b) * fog;
                        let r = r.clamp(0.0, 255.0) as u8;
                        let g = g.clamp(0.0, 255.0) as u8;
                        let b = b.clamp(0.0, 255.0) as u8;
                        painter.point_colored(px, py, PackedRgba::rgb(r, g, b));

                        w0e += e0_dx;
                        w1e += e1_dx;
                        w2e += e2_dx;
                    }

                    w0_row += e0_dy;
                    w1_row += e1_dy;
                    w2_row += e2_dy;
                }

                if tri_idx % edge_stride == 0 && edge_fade < 0.55 {
                    let edge_boost = (light + 0.25).clamp(0.0, 1.2);
                    let edge_scale = (1.0 - edge_fade).powf(1.4);
                    let er = (base.r() as f32 * edge_boost * edge_scale).min(255.0) as u8;
                    let eg = (base.g() as f32 * edge_boost * edge_scale).min(255.0) as u8;
                    let eb = (base.b() as f32 * edge_boost * edge_scale).min(255.0) as u8;
                    let edge_color = PackedRgba::rgb(er, eg, eb);

                    painter.line_colored(
                        sx0 as i32,
                        sy0 as i32,
                        sx1 as i32,
                        sy1 as i32,
                        Some(edge_color),
                    );
                    painter.line_colored(
                        sx1 as i32,
                        sy1 as i32,
                        sx2 as i32,
                        sy2 as i32,
                        Some(edge_color),
                    );
                    painter.line_colored(
                        sx2 as i32,
                        sy2 as i32,
                        sx0 as i32,
                        sy0 as i32,
                        Some(edge_color),
                    );
                }
            };

            if clipped_len == 3 {
                draw_tri(clipped[0], clipped[1], clipped[2]);
            } else {
                for i in 1..(clipped_len - 1) {
                    draw_tri(clipped[0], clipped[i], clipped[i + 1]);
                }
            }
        }

        // Crosshair
        let cx = (width / 2) as i32;
        let cy = (height / 2) as i32;
        let flash = self.fire_flash;
        let cross_r = (200.0 + flash * 40.0).min(255.0) as u8;
        let cross = PackedRgba::rgb(cross_r, 240, 240);
        painter.line_colored(cx - 3, cy, cx + 3, cy, Some(cross));
        painter.line_colored(cx, cy - 2, cx, cy + 2, Some(cross));

        if height > 7 {
            let gun_y = height as i32 - 3;
            let gun_x = cx - 9;
            let gun_dark = PackedRgba::rgb(58, 54, 52);
            let gun_mid = PackedRgba::rgb(92, 84, 76);
            painter.line_colored(gun_x, gun_y, gun_x + 18, gun_y, Some(gun_dark));
            painter.line_colored(gun_x + 2, gun_y - 1, gun_x + 16, gun_y - 1, Some(gun_mid));
            painter.line_colored(gun_x + 6, gun_y - 2, gun_x + 12, gun_y - 2, Some(gun_dark));
            if flash > 0.0 {
                let flash_color = PackedRgba::rgb((240.0 * flash + 100.0) as u8, 210, 140);
                painter.line_colored(cx - 2, gun_y - 4, cx + 2, gun_y - 4, Some(flash_color));
            }
        }
    }
}

// =============================================================================
// Helper functions
// =============================================================================

fn build_doom_lines() -> Vec<DoomLine> {
    let mut lines = Vec::with_capacity(FREEDOOM_E1M1_LINES.len());
    for (x1, y1, x2, y2) in FREEDOOM_E1M1_LINES {
        lines.push(DoomLine::new(
            *x1 as f32, *y1 as f32, *x2 as f32, *y2 as f32,
        ));
    }
    lines
}

static RAND_STATE: AtomicU64 = AtomicU64::new(12345);

fn rand_simple() -> f64 {
    let old = RAND_STATE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |s| {
            Some(s.wrapping_mul(6364136223846793005).wrapping_add(1))
        })
        .unwrap();
    let new = old.wrapping_mul(6364136223846793005).wrapping_add(1);
    (new >> 33) as f64 / (1u64 << 31) as f64
}

fn cross2(ax: f32, ay: f32, bx: f32, by: f32) -> f32 {
    ax * by - ay * bx
}

fn min_wall_distance_sq(x: f32, y: f32, walls: &[WallSeg]) -> f32 {
    let mut best = f32::INFINITY;
    for seg in walls {
        let dist_sq = seg.distance_sq(x, y);
        if dist_sq < best {
            best = dist_sq;
        }
    }
    best
}

fn min_wall_distance_sq_bounded(x: f32, y: f32, walls: &[WallSeg], bound_sq: f32) -> f32 {
    let mut best = f32::INFINITY;
    for seg in walls {
        let dist_sq = seg.distance_sq(x, y);
        if dist_sq < best {
            best = dist_sq;
            if best <= bound_sq {
                break;
            }
        }
    }
    best
}

fn clip_triangle_near(a: Vec3, b: Vec3, c: Vec3, near: f32, out: &mut [Vec3; 4]) -> usize {
    let verts = [a, b, c];
    let mut count = 0usize;
    let mut prev = verts[2];
    let mut prev_inside = prev.z >= near;

    for &curr in &verts {
        let curr_inside = curr.z >= near;
        if prev_inside && curr_inside {
            out[count] = curr;
            count += 1;
        } else if prev_inside && !curr_inside {
            let denom = curr.z - prev.z;
            if denom.abs() > 1e-6 {
                let t = (near - prev.z) / denom;
                out[count] = Vec3::new(
                    prev.x + (curr.x - prev.x) * t,
                    prev.y + (curr.y - prev.y) * t,
                    near,
                );
                count += 1;
            }
        } else if !prev_inside && curr_inside {
            let denom = curr.z - prev.z;
            if denom.abs() > 1e-6 {
                let t = (near - prev.z) / denom;
                out[count] = Vec3::new(
                    prev.x + (curr.x - prev.x) * t,
                    prev.y + (curr.y - prev.y) * t,
                    near,
                );
                count += 1;
            }
            out[count] = curr;
            count += 1;
        }

        prev = curr;
        prev_inside = curr_inside;
    }

    count
}

fn fx_stride(quality: FxQuality) -> usize {
    match quality {
        FxQuality::Full => 1,
        FxQuality::Reduced => 2,
        FxQuality::Minimal => 3,
        FxQuality::Off => 0,
    }
}

fn fx_stride_for_area(quality: FxQuality, width: u16, height: u16, max_samples: usize) -> usize {
    let mut stride = fx_stride(quality);
    if stride == 0 {
        return 0;
    }
    if width == 0 || height == 0 || max_samples == 0 {
        return stride;
    }

    let area = width as usize * height as usize;
    if area <= max_samples {
        return stride;
    }

    let desired = ((area as f64 / max_samples as f64).sqrt().ceil() as usize).max(1);
    stride = stride.max(desired);
    stride
}

// =============================================================================
// Screen implementation
// =============================================================================

impl Default for VisualEffectsScreen {
    fn default() -> Self {
        let plasma_palette = PlasmaPalette::Sunset;
        let markdown_panel = render_markdown(MARKDOWN_OVERLAY);
        let effect = initial_effect_from_env().unwrap_or(EffectType::Metaballs);

        Self {
            effect,
            frame: 0,
            time: 0.0,
            metaballs_adapter: RefCell::new(MetaballsCanvasAdapter::new()),
            shape3d: Shape3DState::default(),
            plasma_adapter: RefCell::new(PlasmaCanvasAdapter::new(plasma_palette)),
            plasma_palette,
            particles: ParticleState::default(),
            matrix: MatrixState::default(),
            tunnel: TunnelState::default(),
            fire: FireState::default(),
            reaction_diffusion: ReactionDiffusionState::default(),
            attractor: AttractorState::default(),
            mandelbrot: MandelbrotState::default(),
            lissajous: LissajousState::default(),
            flow_field: FlowFieldState::default(),
            julia: JuliaState::default(),
            wave_interference: WaveInterferenceState::default(),
            spiral: SpiralState::default(),
            spin_lattice: SpinLatticeState::default(),
            doom_e1m1: RefCell::new(None),
            quake_e1m1: RefCell::new(None),
            // FPS tracking
            frame_times: VecDeque::with_capacity(60),
            last_frame: None,
            fps: 0.0,
            avg_frame_time_us: 0.0,
            min_frame_time_us: 0.0,
            max_frame_time_us: 0.0,
            // Transition overlay
            transition: TransitionState::new(),
            painter: RefCell::new(Painter::new(0, 0, Mode::Braille)),
            last_quality: Cell::new(FxQuality::Full),
            effect_panic: Cell::new(None),
            // Text effects demo (bd-2b82)
            demo_mode: DemoMode::Canvas,
            text_effects: TextEffectsDemo::default(),
            markdown_panel,
            fps_input: FpsInputState::default(),
            fps_last_mouse: None,
            fps_mouse_sensitivity: 0.014,
            deterministic_mode: false,
            deterministic_tick_ms: 0,
        }
    }
}

fn initial_effect_from_env() -> Option<EffectType> {
    let raw = env::var("FTUI_DEMO_VFX_EFFECT")
        .or_else(|_| env::var("FTUI_VFX_EFFECT"))
        .ok()?;
    EffectType::from_name(&raw)
}

impl VisualEffectsScreen {
    fn with_doom_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut DoomE1M1State) -> R,
    {
        let mut guard = self.doom_e1m1.borrow_mut();
        if guard.is_none() {
            *guard = Some(DoomE1M1State::default());
        }
        f(guard.as_mut().expect("doom state should be initialized"))
    }

    fn with_quake_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut QuakeE1M1State) -> R,
    {
        let mut guard = self.quake_e1m1.borrow_mut();
        if guard.is_none() {
            *guard = Some(QuakeE1M1State::default());
        }
        f(guard.as_mut().expect("quake state should be initialized"))
    }

    fn is_fps_effect(&self) -> bool {
        matches!(self.effect, EffectType::DoomE1M1 | EffectType::QuakeE1M1)
    }

    fn canvas_mode_for_effect(&self, _quality: FxQuality, _area_cells: usize) -> Mode {
        match self.effect {
            // FPS effects render in full braille resolution.
            EffectType::DoomE1M1 | EffectType::QuakeE1M1 => Mode::Braille,
            _ => Mode::Braille,
        }
    }

    fn switch_effect(&mut self, effect: EffectType) {
        self.effect = effect;
        self.fps_last_mouse = None;
        self.fps_input = FpsInputState::default();
        self.start_transition();
    }

    pub(crate) fn set_effect_by_name(&mut self, name: &str) -> bool {
        let Some(effect) = EffectType::from_name(name) else {
            return false;
        };
        self.effect = effect;
        self.fps_last_mouse = None;
        self.fps_input = FpsInputState::default();
        true
    }

    pub(crate) fn effect_key(&self) -> &'static str {
        self.effect.key()
    }

    pub(crate) fn enable_deterministic_mode(&mut self, tick_ms: u64) {
        let tick_ms = tick_ms.max(1);
        if self.deterministic_mode && self.deterministic_tick_ms == tick_ms {
            return;
        }
        self.deterministic_mode = true;
        self.deterministic_tick_ms = tick_ms;
        self.frame_times.clear();
        self.last_frame = None;
    }

    pub(crate) fn sim_time(&self) -> f64 {
        self.time
    }

    fn update_fps_input(&mut self, code: KeyCode, kind: KeyEventKind) {
        let is_down = matches!(kind, KeyEventKind::Press | KeyEventKind::Repeat);
        match code {
            KeyCode::Char('w') | KeyCode::Char('W') => self.fps_input.forward = is_down,
            KeyCode::Char('s') | KeyCode::Char('S') => self.fps_input.back = is_down,
            KeyCode::Char('a') | KeyCode::Char('A') => self.fps_input.strafe_left = is_down,
            KeyCode::Char('d') | KeyCode::Char('D') => self.fps_input.strafe_right = is_down,
            _ => {}
        }
    }

    fn apply_fps_movement(&mut self) {
        if !self.is_fps_effect() {
            return;
        }

        let mut forward = (self.fps_input.forward as i8 - self.fps_input.back as i8) as f32;
        let mut strafe =
            (self.fps_input.strafe_right as i8 - self.fps_input.strafe_left as i8) as f32;
        let turn = (self.fps_input.turn_right as i8 - self.fps_input.turn_left as i8) as f32;

        let mag = (forward * forward + strafe * strafe).sqrt();
        if mag > 1.0 {
            forward /= mag;
            strafe /= mag;
        }

        match self.effect {
            EffectType::DoomE1M1 => {
                self.with_doom_mut(|doom| {
                    if forward != 0.0 {
                        doom.move_forward(forward * DOOM_MOVE_STEP);
                    }
                    if strafe != 0.0 {
                        doom.strafe(strafe * DOOM_STRAFE_STEP);
                    }
                    if turn != 0.0 {
                        doom.look(turn * DOOM_TURN_RATE, 0.0);
                    }
                });
            }
            EffectType::QuakeE1M1 => {
                self.with_quake_mut(|quake| {
                    if forward != 0.0 {
                        quake.move_forward(forward * QUAKE_MOVE_STEP);
                    }
                    if strafe != 0.0 {
                        quake.strafe(strafe * QUAKE_STRAFE_STEP);
                    }
                    if turn != 0.0 {
                        quake.look(turn * QUAKE_TURN_RATE, 0.0);
                    }
                });
            }
            _ => {}
        }
    }

    fn handle_fps_key(&mut self, code: KeyCode, kind: KeyEventKind) {
        self.update_fps_input(code, kind);
        if !matches!(kind, KeyEventKind::Press) {
            return;
        }

        match code {
            KeyCode::Char(' ') => match self.effect {
                EffectType::DoomE1M1 => self.with_doom_mut(|doom| doom.jump()),
                EffectType::QuakeE1M1 => self.with_quake_mut(|quake| quake.jump()),
                _ => {}
            },
            KeyCode::Left => {
                let effect = self.effect.prev();
                self.switch_effect(effect);
            }
            KeyCode::Right => {
                let effect = self.effect.next();
                self.switch_effect(effect);
            }
            KeyCode::Char('[') => {
                let effect = self.effect.prev();
                self.switch_effect(effect);
            }
            KeyCode::Char(']') => {
                let effect = self.effect.next();
                self.switch_effect(effect);
            }
            _ => {}
        }
    }

    fn handle_fps_mouse(&mut self, kind: MouseEventKind, x: u16, y: u16) {
        match kind {
            MouseEventKind::Moved | MouseEventKind::Drag(_) => {
                if let Some((lx, ly)) = self.fps_last_mouse {
                    let dx = x as i32 - lx as i32;
                    let dy = y as i32 - ly as i32;
                    let yaw_delta = dx as f32 * self.fps_mouse_sensitivity;
                    let pitch_delta = dy as f32 * self.fps_mouse_sensitivity;
                    match self.effect {
                        EffectType::DoomE1M1 => {
                            self.with_doom_mut(|doom| doom.look(yaw_delta, pitch_delta))
                        }
                        EffectType::QuakeE1M1 => {
                            self.with_quake_mut(|quake| quake.look(yaw_delta, pitch_delta));
                        }
                        _ => {}
                    }
                }
                self.fps_last_mouse = Some((x, y));
            }
            MouseEventKind::Down(MouseButton::Left) => match self.effect {
                EffectType::DoomE1M1 => self.with_doom_mut(|doom| doom.fire()),
                EffectType::QuakeE1M1 => self.with_quake_mut(|quake| quake.fire()),
                _ => {}
            },
            _ => {}
        }
    }

    /// Start a transition overlay for the current effect.
    fn start_transition(&mut self) {
        // Use a rainbow gradient for the transition
        self.transition.start_with_gradient(
            self.effect.name(),
            self.effect.description(),
            ColorGradient::cyberpunk(),
        );
        self.transition.set_speed(0.04); // Smooth transition
    }

    fn cycle_plasma_palette(&mut self) {
        self.plasma_palette = next_plasma_palette(self.plasma_palette);
        self.plasma_adapter
            .borrow_mut()
            .set_palette(self.plasma_palette);
    }

    /// Start a transition overlay for text effects.
    fn start_text_effects_transition(&mut self) {
        self.transition.start_with_gradient(
            self.text_effects.current_effect_name(),
            self.text_effects.current_effect_description(),
            ColorGradient::rainbow(),
        );
        self.transition.set_speed(0.05);
    }

    fn render_markdown_overlay(&self, frame: &mut Frame, area: Rect) {
        let min_width = 32u16;
        let min_height = 10u16;
        if area.width < min_width || area.height < min_height {
            return;
        }

        let max_width = area.width.saturating_sub(4).max(min_width);
        let max_height = area.height.saturating_sub(4).max(min_height);
        let panel_width = ((area.width as f32) * 0.45).round() as u16;
        let panel_height = ((area.height as f32) * 0.6).round() as u16;
        let panel_width = panel_width.clamp(min_width, max_width);
        let panel_height = panel_height.clamp(min_height, max_height);

        let x = area.x + (area.width - panel_width) / 2;
        let y = area.y + (area.height - panel_height) / 2;
        let panel_area = Rect::new(x, y, panel_width, panel_height);

        let border_style = Style::new().fg(PackedRgba::rgb(140, 190, 255));
        let panel_style = Style::new()
            .fg(PackedRgba::rgb(220, 230, 255))
            .bg(PackedRgba::rgb(16, 18, 26));
        let block = Block::new()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .title("Markdown Overlay")
            .title_alignment(Alignment::Center)
            .border_style(border_style)
            .style(panel_style);

        let inner = block.inner(panel_area);
        block.render(panel_area, frame);

        if inner.is_empty() {
            return;
        }

        Paragraph::new(self.markdown_panel.clone())
            .wrap(WrapMode::Word)
            .render(inner, frame);
    }

    /// Render text effects demo area
    fn render_text_effects(&self, frame: &mut Frame, area: Rect) {
        if area.width < 10 || area.height < 5 {
            return;
        }

        // Tab bar at top
        let tab_bar_height = 2u16;
        let tab_bar_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: tab_bar_height,
        };
        self.render_text_effects_tabs(frame, tab_bar_area);

        // Demo content area
        let content_area = Rect {
            x: area.x,
            y: area.y + tab_bar_height,
            width: area.width,
            height: area.height.saturating_sub(tab_bar_height + 3),
        };
        self.render_text_effects_demo(frame, content_area);

        // Help bar at bottom
        let help_area = Rect {
            x: area.x,
            y: area.y + area.height.saturating_sub(2),
            width: area.width,
            height: 2,
        };
        self.render_text_effects_help(frame, help_area);
    }

    /// Render tab bar for text effects
    fn render_text_effects_tabs(&self, frame: &mut Frame, area: Rect) {
        let mut text = String::with_capacity(area.width as usize);
        text.push(' ');

        for (i, tab) in TextEffectsTab::ALL.iter().enumerate() {
            let is_active = *tab == self.text_effects.tab;
            let name = tab.name();

            if is_active {
                text.push_str(&format!("[{}] ", name));
            } else {
                text.push_str(&format!(" {}  ", name));
            }

            if i < 5 {
                text.push('│');
            }
        }

        let style = Style::new().bold().fg(PackedRgba::rgb(200, 200, 255));
        let para = Paragraph::new(text).style(style);
        para.render(area, frame);

        // Render tab numbers hint on second line
        if area.height > 1 {
            let hint_area = Rect {
                y: area.y + 1,
                height: 1,
                ..area
            };
            let hint = " [1-6] Switch tabs │ [Space] Cycle effect │ [t] Canvas mode";
            let hint_para =
                Paragraph::new(hint).style(Style::new().fg(PackedRgba::rgb(120, 120, 150)));
            hint_para.render(hint_area, frame);
        }
    }

    /// Render the main text effects demo content
    fn render_text_effects_demo(&self, frame: &mut Frame, area: Rect) {
        let slots = if area.width >= 100 {
            3
        } else if area.width >= 70 {
            2
        } else {
            1
        };

        let mut constraints = Vec::new();
        for _ in 0..slots {
            constraints.push(Constraint::Ratio(1, slots as u32));
        }
        let cols = Flex::horizontal().constraints(constraints).split(area);

        for (idx, col) in cols.iter().enumerate() {
            if col.width < 10 || col.height < 4 {
                continue;
            }

            let effect_idx =
                (self.text_effects.effect_idx + idx) % self.text_effects.tab.effect_count();
            let demo = self.text_effects.variant_with_effect(effect_idx);
            let rows = Flex::vertical()
                .constraints([
                    Constraint::Fixed(1),
                    Constraint::Min(2),
                    Constraint::Fixed(1),
                ])
                .split(*col);

            let label = format!("{} · {}", idx + 1, demo.current_effect_name());
            Paragraph::new(truncate_to_width(&label, rows[0].width.into()))
                .style(Style::new().fg(PackedRgba::rgb(160, 190, 220)))
                .render(rows[0], frame);

            self.render_text_effect_variant(frame, rows[1], &demo);

            let desc = truncate_to_width(demo.current_effect_description(), rows[2].width.into());
            Paragraph::new(desc)
                .style(Style::new().fg(PackedRgba::rgb(120, 140, 160)))
                .render(rows[2], frame);
        }
    }

    fn render_text_effect_variant(&self, frame: &mut Frame, area: Rect, demo: &TextEffectsDemo) {
        if area.is_empty() || area.height < 2 {
            return;
        }

        let demo_text = demo.demo_text;
        let text_y = area.y + area.height / 2;
        let text_width = display_width(demo_text) as u16;
        let text_x = area.x + (area.width.saturating_sub(text_width)) / 2;
        let text_area = Rect {
            x: text_x,
            y: text_y,
            width: area.width.saturating_sub(text_x - area.x),
            height: 3,
        };

        let effect = demo.build_effect();
        match demo.tab {
            TextEffectsTab::Typography => {
                self.render_typography_demo(frame, text_area, demo_text, demo);
            }
            TextEffectsTab::SpecialFx if demo.effect_idx >= 2 => {
                self.render_special_fx_demo(frame, text_area, demo_text, demo);
            }
            _ => {
                let styled = StyledText::new(demo_text)
                    .bold()
                    .effect(effect)
                    .time(demo.time + (demo.effect_idx as f64 * 0.1));
                styled.render(text_area, frame);
            }
        }
    }

    /// Render typography-specific demos (Shadow, Glow, Mirror, ASCII)
    fn render_typography_demo(
        &self,
        frame: &mut Frame,
        area: Rect,
        text: &str,
        demo: &TextEffectsDemo,
    ) {
        match demo.effect_idx {
            0 => {
                // Shadow effect - render shadow first, then main text
                let shadow_offset = 1;
                let shadow_area = Rect {
                    x: area.x + shadow_offset,
                    y: area.y + 1,
                    ..area
                };
                let shadow_styled = StyledText::new(text).base_color(PackedRgba::rgb(50, 50, 80));
                shadow_styled.render(shadow_area, frame);

                let main_styled = StyledText::new(text)
                    .bold()
                    .base_color(PackedRgba::rgb(255, 255, 255));
                main_styled.render(area, frame);
            }
            1 => {
                // Glow effect
                let glow_effect = TextEffect::PulsingGlow {
                    color: PackedRgba::rgb(100, 200, 255),
                    speed: 1.5,
                };
                let styled = StyledText::new(text)
                    .bold()
                    .effect(glow_effect)
                    .time(demo.time);
                styled.render(area, frame);
            }
            2 => {
                // Outline effect - approximated with bright text
                let outline_styled = StyledText::new(text)
                    .bold()
                    .base_color(PackedRgba::rgb(255, 255, 100));
                outline_styled.render(area, frame);
            }
            3 => {
                // Mirror reflection
                let reflection = Reflection {
                    gap: 1,
                    start_opacity: 0.5,
                    end_opacity: 0.1,
                    height_ratio: 1.0,
                    wave: 0.0,
                };
                let styled = StyledMultiLine::new(vec![text.to_string()])
                    .bold()
                    .base_color(PackedRgba::rgb(200, 220, 255))
                    .reflection(reflection)
                    .time(demo.time);
                styled.render(area, frame);
            }
            _ => {
                // ASCII Art
                let ascii = AsciiArtText::new(text, AsciiArtStyle::Block);
                let ascii_styled = StyledMultiLine::from_ascii_art(ascii)
                    .effect(TextEffect::RainbowGradient { speed: 0.3 })
                    .time(demo.time);
                ascii_styled.render(area, frame);
            }
        }
    }

    /// Render special FX demos (scanline, matrix style)
    fn render_special_fx_demo(
        &self,
        frame: &mut Frame,
        area: Rect,
        text: &str,
        demo: &TextEffectsDemo,
    ) {
        match demo.effect_idx {
            2 => {
                // Scanline effect - alternate brightness
                let scanline_time = (demo.time * 10.0) as usize;
                let brightness = if scanline_time.is_multiple_of(2) {
                    255u8
                } else {
                    180u8
                };
                let styled = StyledText::new(text)
                    .bold()
                    .base_color(PackedRgba::rgb(brightness, brightness, brightness));
                styled.render(area, frame);
            }
            _ => {
                // Matrix style - green on black
                let matrix_effect = TextEffect::HorizontalGradient {
                    gradient: ColorGradient::new(vec![
                        (0.0, PackedRgba::rgb(0, 100, 0)),
                        (0.5, PackedRgba::rgb(0, 255, 0)),
                        (1.0, PackedRgba::rgb(100, 255, 100)),
                    ]),
                };
                let styled = StyledText::new(text)
                    .bold()
                    .effect(matrix_effect)
                    .time(demo.time);
                styled.render(area, frame);
            }
        }
    }

    /// Render help bar for text effects
    fn render_text_effects_help(&self, frame: &mut Frame, area: Rect) {
        let help_text = match self.text_effects.tab {
            TextEffectsTab::Combinations => {
                format!(
                    "Combos: [1]Gradient:{} [2]Anim:{} [3]Typo:{} [4]FX:{}",
                    if self.text_effects.combo_enabled[0] {
                        "ON"
                    } else {
                        "off"
                    },
                    if self.text_effects.combo_enabled[1] {
                        "ON"
                    } else {
                        "off"
                    },
                    if self.text_effects.combo_enabled[2] {
                        "ON"
                    } else {
                        "off"
                    },
                    if self.text_effects.combo_enabled[3] {
                        "ON"
                    } else {
                        "off"
                    },
                )
            }
            _ => {
                format!(
                    "FPS: {:.1} │ Time: {:.2} │ Easing: {:?}",
                    self.fps, self.text_effects.time, self.text_effects.easing
                )
            }
        };

        let help_para = Paragraph::new(format!(" {} │ [e] Easing │ [t] Canvas mode", help_text))
            .style(Style::new().fg(PackedRgba::rgb(100, 100, 130)));
        help_para.render(area, frame);
    }
}

impl Screen for VisualEffectsScreen {
    type Message = ();

    fn update(&mut self, event: &Event) -> Cmd<Self::Message> {
        if let Event::Mouse(mouse) = event
            && matches!(self.demo_mode, DemoMode::Canvas)
            && self.is_fps_effect()
        {
            self.handle_fps_mouse(mouse.kind, mouse.x, mouse.y);
            return Cmd::None;
        }

        if let Event::Key(KeyEvent { code, kind, .. }) = event {
            // 't' toggles between Canvas and TextEffects modes
            if matches!(code, KeyCode::Char('t')) && matches!(kind, KeyEventKind::Press) {
                self.demo_mode = match self.demo_mode {
                    DemoMode::Canvas => DemoMode::TextEffects,
                    DemoMode::TextEffects => DemoMode::Canvas,
                };
                self.fps_last_mouse = None;
                self.fps_input = FpsInputState::default();
                return Cmd::None;
            }

            match self.demo_mode {
                DemoMode::Canvas => {
                    if self.is_fps_effect() {
                        self.handle_fps_key(*code, *kind);
                        return Cmd::None;
                    }
                    let is_press = matches!(kind, KeyEventKind::Press);
                    let is_repeat = matches!(kind, KeyEventKind::Repeat);
                    if !(is_press || is_repeat) {
                        return Cmd::None;
                    }
                    // Canvas mode key handling (original behavior)
                    match code {
                        KeyCode::Left | KeyCode::Char('h') => {
                            if is_press || is_repeat {
                                let effect = self.effect.prev();
                                self.switch_effect(effect);
                            }
                        }
                        KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(' ') => {
                            if is_press || is_repeat {
                                let effect = self.effect.next();
                                self.switch_effect(effect);
                            }
                        }
                        KeyCode::Char('p') => match self.effect {
                            EffectType::Shape3D => {
                                if is_press {
                                    self.shape3d.shape = self.shape3d.shape.next();
                                }
                            }
                            EffectType::Plasma => {
                                if is_press {
                                    self.cycle_plasma_palette();
                                }
                            }
                            _ => {}
                        },
                        _ => {}
                    }
                }
                DemoMode::TextEffects => {
                    let is_press = matches!(kind, KeyEventKind::Press);
                    let is_repeat = matches!(kind, KeyEventKind::Repeat);
                    if !(is_press || is_repeat) {
                        return Cmd::None;
                    }
                    // Text effects mode key handling
                    match code {
                        // 1-6 keys switch tabs
                        KeyCode::Char('1') => {
                            if self.text_effects.tab == TextEffectsTab::Combinations {
                                self.text_effects.toggle_combo(0);
                            } else if let Some(tab) = TextEffectsTab::from_key(1) {
                                self.text_effects.tab = tab;
                                self.text_effects.effect_idx = 0;
                                self.start_text_effects_transition();
                            }
                        }
                        KeyCode::Char('2') => {
                            if self.text_effects.tab == TextEffectsTab::Combinations {
                                self.text_effects.toggle_combo(1);
                            } else if let Some(tab) = TextEffectsTab::from_key(2) {
                                self.text_effects.tab = tab;
                                self.text_effects.effect_idx = 0;
                                self.start_text_effects_transition();
                            }
                        }
                        KeyCode::Char('3') => {
                            if self.text_effects.tab == TextEffectsTab::Combinations {
                                self.text_effects.toggle_combo(2);
                            } else if let Some(tab) = TextEffectsTab::from_key(3) {
                                self.text_effects.tab = tab;
                                self.text_effects.effect_idx = 0;
                                self.start_text_effects_transition();
                            }
                        }
                        KeyCode::Char('4') => {
                            if self.text_effects.tab == TextEffectsTab::Combinations {
                                self.text_effects.toggle_combo(3);
                            } else if let Some(tab) = TextEffectsTab::from_key(4) {
                                self.text_effects.tab = tab;
                                self.text_effects.effect_idx = 0;
                                self.start_text_effects_transition();
                            }
                        }
                        KeyCode::Char('5') => {
                            if let Some(tab) = TextEffectsTab::from_key(5) {
                                self.text_effects.tab = tab;
                                self.text_effects.effect_idx = 0;
                                self.start_text_effects_transition();
                            }
                        }
                        KeyCode::Char('6') => {
                            if let Some(tab) = TextEffectsTab::from_key(6) {
                                self.text_effects.tab = tab;
                                self.text_effects.effect_idx = 0;
                                self.start_text_effects_transition();
                            }
                        }
                        // Space cycles effects within tab
                        KeyCode::Char(' ') | KeyCode::Right => {
                            if is_press || is_repeat {
                                self.text_effects.next_effect();
                                self.start_text_effects_transition();
                            }
                        }
                        KeyCode::Left => {
                            if is_press || is_repeat {
                                self.text_effects.prev_effect();
                                self.start_text_effects_transition();
                            }
                        }
                        // 'e' cycles easing functions
                        KeyCode::Char('e') => {
                            if is_press {
                                self.text_effects.easing_mode = !self.text_effects.easing_mode;
                                self.text_effects.next_easing();
                            }
                        }
                        // 'c' jumps to combinations tab
                        KeyCode::Char('c') => {
                            if is_press {
                                self.text_effects.tab = TextEffectsTab::Combinations;
                                self.start_text_effects_transition();
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Cmd::None
    }

    fn view(&self, frame: &mut Frame, area: Rect) {
        if area.width < 10 || area.height < 5 {
            return;
        }

        // Branch based on demo mode
        match self.demo_mode {
            DemoMode::TextEffects => {
                // Render text effects demo
                self.render_text_effects(frame, area);

                // Render transition overlay if active
                if self.transition.is_visible() {
                    self.transition.overlay().render(area, frame);
                }
                return;
            }
            DemoMode::Canvas => {
                // Continue with canvas rendering below
            }
        }

        // Header with effect name, controls, and FPS stats
        let header_area = Rect {
            x: area.x,
            y: area.y,
            width: area.width,
            height: 1,
        };
        let space_hint = match self.effect {
            EffectType::Shape3D => " │ Space: Shape",
            EffectType::Plasma => " │ Space: Palette",
            _ => "",
        };
        // Build FPS stats string
        let fps_stats = format!(
            " │ {:.1} FPS │ {:.1}ms avg │ {:.1}/{:.1}ms",
            self.fps,
            self.avg_frame_time_us / 1000.0,
            self.min_frame_time_us / 1000.0,
            self.max_frame_time_us / 1000.0
        );
        let header_text = if self.is_fps_effect() {
            format!(
                " {} │ WASD move/strafe │ Mouse look │ Space jump │ Click fire │ ←/→ Switch │ [t] Text FX{}",
                self.effect.name(),
                fps_stats
            )
        } else {
            format!(
                " {} │ ←/→ Switch │ [t] Text FX{}{}",
                self.effect.name(),
                space_hint,
                fps_stats
            )
        };
        let header = Paragraph::new(header_text)
            .style(Style::new().bold().fg(PackedRgba::rgb(200, 200, 255)));
        header.render(header_area, frame);

        // Canvas area
        let canvas_area = Rect {
            x: area.x,
            y: area.y + 1,
            width: area.width,
            height: area.height.saturating_sub(1),
        };

        // Reuse cached painter (grow-only) and render at sub-pixel resolution.
        {
            let area_cells = canvas_area.width as usize * canvas_area.height as usize;
            let mut quality = FxQuality::from_degradation_with_area(frame.degradation, area_cells);
            if self.is_fps_effect() {
                quality = match quality {
                    FxQuality::Off => FxQuality::Minimal,
                    FxQuality::Minimal => FxQuality::Reduced,
                    other => other,
                };
            }
            self.last_quality.set(quality);
            let theme_inputs = current_fx_theme();

            let mut painter = self.painter.borrow_mut();
            let mode = self.canvas_mode_for_effect(quality, area_cells);
            painter.ensure_for_area(canvas_area, mode);
            painter.clear();
            let (pw, ph) = painter.size();

            let mut effect_panicked = false;
            if !matches!(quality, FxQuality::Off) {
                let result = catch_unwind(AssertUnwindSafe(|| match self.effect {
                    EffectType::Shape3D => self.shape3d.render(&mut painter, pw, ph, self.time),
                    EffectType::Particles => self.particles.render(&mut painter, pw, ph),
                    EffectType::Matrix => self.matrix.render(&mut painter, pw, ph),
                    EffectType::Tunnel => self.tunnel.render(&mut painter, pw, ph),
                    EffectType::Fire => self.fire.render(&mut painter, pw, ph),
                    EffectType::ReactionDiffusion => {
                        self.reaction_diffusion.render(&mut painter, pw, ph)
                    }
                    EffectType::StrangeAttractor => self.attractor.render(&mut painter, pw, ph),
                    EffectType::Mandelbrot => self.mandelbrot.render(&mut painter, pw, ph),
                    EffectType::Lissajous => self.lissajous.render(&mut painter, pw, ph),
                    EffectType::FlowField => self.flow_field.render(&mut painter, pw, ph),
                    EffectType::Julia => self.julia.render(&mut painter, pw, ph),
                    EffectType::WaveInterference => {
                        self.wave_interference.render(&mut painter, pw, ph, quality)
                    }
                    EffectType::Spiral => self.spiral.render(&mut painter, pw, ph, quality),
                    EffectType::SpinLattice => {
                        self.spin_lattice.render(&mut painter, pw, ph, quality)
                    }
                    EffectType::DoomE1M1 => {
                        self.with_doom_mut(|doom| {
                            doom.render(&mut painter, pw, ph, quality, self.time, self.frame);
                        });
                    }
                    EffectType::QuakeE1M1 => {
                        self.with_quake_mut(|quake| {
                            quake.render(&mut painter, pw, ph, quality, self.time, self.frame);
                        });
                    }
                    // Canvas adapters for metaballs and plasma (bd-l8x9.5.3)
                    EffectType::Metaballs => {
                        self.metaballs_adapter.borrow_mut().fill_frame(
                            &mut painter,
                            self.time,
                            quality,
                            &theme_inputs,
                        );
                    }
                    EffectType::Plasma => {
                        self.plasma_adapter.borrow_mut().fill(
                            &mut painter,
                            self.time,
                            quality,
                            &theme_inputs,
                        );
                    }
                }));
                if result.is_err() {
                    effect_panicked = true;
                    painter.clear();
                    self.effect_panic.set(Some(self.effect));
                }
            }

            // Render canvas to frame without cloning painter buffers.
            let canvas = CanvasRef::from_painter(&painter);
            canvas.render(canvas_area, frame);

            if effect_panicked {
                let crash_block = Block::new()
                    .borders(Borders::ALL)
                    .border_type(BorderType::Double)
                    .title("Effect Error")
                    .title_alignment(Alignment::Center)
                    .style(Style::new().fg(PackedRgba::rgb(255, 120, 120)));
                let crash_area = crash_block.inner(canvas_area);
                crash_block.render(canvas_area, frame);
                if !crash_area.is_empty() {
                    Paragraph::new(
                        "This effect panicked and was disabled.\nSwitch effects with ←/→.",
                    )
                    .style(Style::new().fg(PackedRgba::rgb(255, 220, 220)))
                    .render(crash_area, frame);
                }
            }
        }

        // Render markdown overlay for metaballs/plasma
        if matches!(self.effect, EffectType::Metaballs | EffectType::Plasma) {
            self.render_markdown_overlay(frame, canvas_area);
        }

        // Render transition overlay if active
        if self.transition.is_visible() {
            self.transition.overlay().render(canvas_area, frame);
        }
    }

    fn tick(&mut self, _tick_count: u64) {
        if let Some(failed) = self.effect_panic.replace(None) {
            // Fall back to a safe effect and show a transition banner.
            if self.effect == failed {
                self.effect = EffectType::Plasma;
                self.start_transition();
            }
        }

        // FPS tracking
        if self.deterministic_mode {
            let frame_us = self.deterministic_tick_ms.max(1) as f64 * 1000.0;
            self.avg_frame_time_us = frame_us;
            self.min_frame_time_us = frame_us;
            self.max_frame_time_us = frame_us;
            self.fps = if frame_us > 0.0 {
                1_000_000.0 / frame_us
            } else {
                0.0
            };
        } else {
            let now = Instant::now();
            if let Some(last) = self.last_frame {
                let elapsed_us = now.duration_since(last).as_micros() as u64;
                self.frame_times.push_back(elapsed_us);

                // Keep last 60 frames for averaging
                while self.frame_times.len() > 60 {
                    self.frame_times.pop_front();
                }

                // Calculate FPS stats
                if !self.frame_times.is_empty() {
                    let sum: u64 = self.frame_times.iter().sum();
                    self.avg_frame_time_us = sum as f64 / self.frame_times.len() as f64;
                    self.fps = if self.avg_frame_time_us > 0.0 {
                        1_000_000.0 / self.avg_frame_time_us
                    } else {
                        0.0
                    };

                    // Min/max over recent frames
                    self.min_frame_time_us = *self.frame_times.iter().min().unwrap_or(&0) as f64;
                    self.max_frame_time_us = *self.frame_times.iter().max().unwrap_or(&0) as f64;
                }
            }
            self.last_frame = Some(now);
        }

        self.frame += 1;
        self.time += 0.1;
        if matches!(self.demo_mode, DemoMode::Canvas) && self.is_fps_effect() {
            self.apply_fps_movement();
        }

        let quality = self.last_quality.get();
        let update_stride = fx_stride(quality) as u64;
        let update_this_frame =
            update_stride != 0 && (self.frame == 1 || self.frame.is_multiple_of(update_stride));
        let spin_update_stride = if update_stride == 0 {
            0
        } else {
            update_stride.saturating_mul(2).max(1)
        };
        let spin_update_this_frame = spin_update_stride != 0
            && (self.frame == 1 || self.frame.is_multiple_of(spin_update_stride));

        let (matrix_width, matrix_height) = match quality {
            FxQuality::Full => (80, 60),
            FxQuality::Reduced => (60, 45),
            FxQuality::Minimal => (45, 30),
            FxQuality::Off => (0, 0),
        };
        let (fire_width, fire_height) = match quality {
            FxQuality::Full => (80, 50),
            FxQuality::Reduced => (60, 36),
            FxQuality::Minimal => (40, 28),
            FxQuality::Off => (0, 0),
        };
        let (reaction_width, reaction_height) = match quality {
            FxQuality::Full => (100, 60),
            FxQuality::Reduced => (72, 45),
            FxQuality::Minimal => (50, 30),
            FxQuality::Off => (0, 0),
        };
        let (spin_width, spin_height) = match quality {
            FxQuality::Full => (60, 40),
            FxQuality::Reduced => (45, 30),
            FxQuality::Minimal => (30, 20),
            FxQuality::Off => (0, 0),
        };
        let fractal_iters = match quality {
            FxQuality::Full => 80,
            FxQuality::Reduced => 60,
            FxQuality::Minimal => 40,
            FxQuality::Off => 0,
        };
        self.mandelbrot.max_iter = fractal_iters;
        self.julia.max_iter = fractal_iters;

        // Update only the active effect to avoid heavy background work.
        match self.effect {
            EffectType::Shape3D => {
                if update_this_frame {
                    self.shape3d.update();
                }
            }
            EffectType::Particles => {
                if update_this_frame {
                    self.particles.update_with_quality(quality);
                }
            }
            EffectType::Matrix => {
                if matrix_width > 0
                    && (!self.matrix.initialized || self.matrix.width != matrix_width)
                {
                    self.matrix.init(matrix_width);
                }
                if update_this_frame && matrix_width > 0 && matrix_height > 0 {
                    self.matrix.update(matrix_height);
                }
            }
            EffectType::Tunnel => {
                if update_this_frame {
                    self.tunnel.update();
                }
            }
            EffectType::Fire => {
                if fire_width > 0
                    && fire_height > 0
                    && (!self.fire.initialized
                        || self.fire.width != fire_width
                        || self.fire.height != fire_height)
                {
                    self.fire.init(fire_width, fire_height);
                }
                if update_this_frame && fire_width > 0 && fire_height > 0 {
                    self.fire.update();
                }
            }
            EffectType::ReactionDiffusion => {
                if reaction_width > 0
                    && reaction_height > 0
                    && (!self.reaction_diffusion.initialized
                        || self.reaction_diffusion.width != reaction_width
                        || self.reaction_diffusion.height != reaction_height)
                {
                    self.reaction_diffusion
                        .init(reaction_width, reaction_height);
                }
                if update_this_frame && reaction_width > 0 && reaction_height > 0 {
                    let iterations = match quality {
                        FxQuality::Full => {
                            if self.frame.is_multiple_of(2) {
                                4
                            } else {
                                6
                            }
                        }
                        FxQuality::Reduced => 3,
                        FxQuality::Minimal => 2,
                        FxQuality::Off => 0,
                    };
                    for _ in 0..iterations {
                        self.reaction_diffusion.update();
                    }
                }
            }
            EffectType::StrangeAttractor => {
                if update_this_frame {
                    self.attractor.update();
                }
            }
            EffectType::Mandelbrot => {
                if update_this_frame {
                    self.mandelbrot.update();
                }
            }
            EffectType::Lissajous => {
                if update_this_frame {
                    self.lissajous.update();
                }
            }
            EffectType::FlowField => {
                if update_this_frame {
                    self.flow_field.update_with_quality(quality);
                }
            }
            EffectType::Julia => {
                if update_this_frame {
                    self.julia.update();
                }
            }
            EffectType::WaveInterference => {
                if update_this_frame {
                    self.wave_interference.update();
                }
            }
            EffectType::Spiral => {
                if update_this_frame {
                    self.spiral.update();
                }
            }
            EffectType::SpinLattice => {
                if spin_width > 0
                    && spin_height > 0
                    && (!self.spin_lattice.initialized
                        || self.spin_lattice.width != spin_width
                        || self.spin_lattice.height != spin_height)
                {
                    self.spin_lattice.init(spin_width, spin_height);
                }
                if spin_update_this_frame && spin_width > 0 && spin_height > 0 {
                    self.spin_lattice.update();
                }
            }
            EffectType::DoomE1M1 => {
                if update_this_frame {
                    self.with_doom_mut(|doom| doom.update());
                }
            }
            EffectType::QuakeE1M1 => {
                if update_this_frame {
                    self.with_quake_mut(|quake| quake.update());
                }
            }
            EffectType::Metaballs | EffectType::Plasma => {}
        }

        // Update text effects only when the text demo is visible (bd-2b82).
        if matches!(self.demo_mode, DemoMode::TextEffects) {
            self.text_effects.tick();
        }

        // Update transition overlay animation
        self.transition.tick();
    }

    fn keybindings(&self) -> Vec<HelpEntry> {
        match self.demo_mode {
            DemoMode::Canvas => {
                if self.is_fps_effect() {
                    vec![
                        HelpEntry {
                            key: "WASD",
                            action: "Move",
                        },
                        HelpEntry {
                            key: "Mouse",
                            action: "Look",
                        },
                        HelpEntry {
                            key: "Space",
                            action: "Jump",
                        },
                        HelpEntry {
                            key: "Click",
                            action: "Fire",
                        },
                        HelpEntry {
                            key: "←/→",
                            action: "Switch effect",
                        },
                        HelpEntry {
                            key: "t",
                            action: "Text Effects mode",
                        },
                    ]
                } else {
                    vec![
                        HelpEntry {
                            key: "Space/→",
                            action: "Next effect",
                        },
                        HelpEntry {
                            key: "←",
                            action: "Prev effect",
                        },
                        HelpEntry {
                            key: "p",
                            action: "Cycle options",
                        },
                        HelpEntry {
                            key: "t",
                            action: "Text Effects mode",
                        },
                    ]
                }
            }
            DemoMode::TextEffects => {
                vec![
                    HelpEntry {
                        key: "1-6",
                        action: "Switch tab",
                    },
                    HelpEntry {
                        key: "Space/→",
                        action: "Next effect",
                    },
                    HelpEntry {
                        key: "←",
                        action: "Prev effect",
                    },
                    HelpEntry {
                        key: "e",
                        action: "Cycle easing",
                    },
                    HelpEntry {
                        key: "c",
                        action: "Combos tab",
                    },
                    HelpEntry {
                        key: "t",
                        action: "Canvas mode",
                    },
                ]
            }
        }
    }

    fn title(&self) -> &'static str {
        "Visual Effects"
    }

    fn tab_label(&self) -> &'static str {
        "VFX"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ftui_render::frame::Frame;
    use ftui_render::grapheme_pool::GraphemePool;

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn painter_buffer_reused_at_stable_size() {
        let mut screen = VisualEffectsScreen::default();
        screen.effect = EffectType::Shape3D;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 20, &mut pool);
        let area = Rect::new(0, 0, 60, 20);

        screen.view(&mut frame, area);
        let len1 = screen.painter.borrow().buffer_len();
        screen.view(&mut frame, area);
        let len2 = screen.painter.borrow().buffer_len();

        assert_eq!(len1, len2);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)]
    fn painter_buffer_grows_only_on_resize() {
        let mut screen = VisualEffectsScreen::default();
        screen.effect = EffectType::Shape3D;
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 40, &mut pool);

        let small = Rect::new(0, 0, 30, 12);
        screen.view(&mut frame, small);
        let len_small = screen.painter.borrow().buffer_len();

        let large = Rect::new(0, 0, 80, 40);
        screen.view(&mut frame, large);
        let len_large = screen.painter.borrow().buffer_len();
        assert!(len_large >= len_small);

        let smaller = Rect::new(0, 0, 20, 8);
        screen.view(&mut frame, smaller);
        let len_after = screen.painter.borrow().buffer_len();
        assert_eq!(len_after, len_large);
    }

    // =========================================================================
    // bd-3vbf.27 Unit Tests for Visual Effects Polish
    // =========================================================================

    /// Verify metaballs renders without panicking and produces visible output.
    #[test]
    fn metaballs_render() {
        let screen = VisualEffectsScreen {
            effect: EffectType::Metaballs,
            ..Default::default()
        };
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);

        // Should not panic
        screen.view(&mut frame, area);

        // Should have rendered something (not all cells empty)
        let has_content = (0..area.height).any(|y| {
            (0..area.width).any(|x| {
                frame
                    .buffer
                    .get(area.x + x, area.y + y)
                    .map(|c| c.content.as_char() != Some(' '))
                    .unwrap_or(false)
            })
        });
        assert!(has_content, "Metaballs should render visible content");
    }

    /// Verify Doom renders visible output (not a blank screen).
    #[test]
    fn doom_render_visible() {
        let screen = VisualEffectsScreen {
            effect: EffectType::DoomE1M1,
            ..Default::default()
        };
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);

        screen.view(&mut frame, area);

        let has_content = (0..area.height).any(|y| {
            (0..area.width).any(|x| {
                frame
                    .buffer
                    .get(area.x + x, area.y + y)
                    .map(|c| c.content.as_char() != Some(' '))
                    .unwrap_or(false)
            })
        });
        assert!(has_content, "Doom should render visible content");
    }

    /// Verify Quake renders visible output (not a blank screen).
    #[test]
    fn quake_render_visible() {
        let screen = VisualEffectsScreen {
            effect: EffectType::QuakeE1M1,
            ..Default::default()
        };
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(80, 24, &mut pool);
        let area = Rect::new(0, 0, 80, 24);

        screen.view(&mut frame, area);

        let has_content = (0..area.height).any(|y| {
            (0..area.width).any(|x| {
                frame
                    .buffer
                    .get(area.x + x, area.y + y)
                    .map(|c| c.content.as_char() != Some(' '))
                    .unwrap_or(false)
            })
        });
        assert!(has_content, "Quake should render visible content");
    }

    fn rgb_tuple(color: PackedRgba) -> (u8, u8, u8) {
        (color.r(), color.g(), color.b())
    }

    fn pick_doom_line(lines: &[DoomLine]) -> DoomLine {
        for line in lines {
            let dx = line.x2 - line.x1;
            let dy = line.y2 - line.y1;
            let len = (dx * dx + dy * dy).sqrt();
            let mx = (line.x1 + line.x2) * 0.5;
            let my = (line.y1 + line.y2) * 0.5;
            if len > 48.0 && mx > 128.0 && mx < 1920.0 && my > 128.0 && my < 1920.0 {
                return *line;
            }
        }
        lines[0]
    }

    fn pick_quake_wall(walls: &[WallSeg], min: Vec3, max: Vec3) -> WallSeg {
        for seg in walls {
            let mx = (seg.x1 + seg.x2) * 0.5;
            let my = (seg.y1 + seg.y2) * 0.5;
            let len = (seg.vx * seg.vx + seg.vy * seg.vy).sqrt();
            if len > 0.12
                && mx > min.x + 0.15
                && mx < max.x - 0.15
                && my > min.y + 0.15
                && my < max.y - 0.15
            {
                return *seg;
            }
        }
        walls[0]
    }

    /// Doom jump should peak below wall height and settle back on the ground.
    #[test]
    fn doom_jump_height_and_landing() {
        let mut doom = DoomE1M1State::default();
        doom.jump();
        let mut max_jump = 0.0f32;
        for _ in 0..200 {
            doom.update();
            max_jump = max_jump.max(doom.player.jump_z);
        }
        assert!(doom.player.grounded);
        assert!(doom.player.jump_z.abs() <= 0.001);
        assert!(
            max_jump <= DOOM_WALL_HEIGHT * 0.9,
            "Jump apex {max_jump} should stay below wall height"
        );
    }

    /// Doom collision should prevent stepping through walls.
    #[test]
    fn doom_collision_blocks_wall_crossing() {
        let mut doom = DoomE1M1State::default();
        let line = pick_doom_line(&doom.lines);
        let vx = line.x2 - line.x1;
        let vy = line.y2 - line.y1;
        let len = (vx * vx + vy * vy).sqrt().max(1.0);
        let nx = -vy / len;
        let ny = vx / len;
        let radius = DOOM_COLLISION_RADIUS + DOOM_COLLISION_PAD;
        doom.player.x = (line.x1 + line.x2) * 0.5 + nx * (radius + 6.0);
        doom.player.y = (line.y1 + line.y2) * 0.5 + ny * (radius + 6.0);
        doom.try_move(-nx * (radius + 12.0), -ny * (radius + 12.0));
        let dist_sq = line.distance_sq(doom.player.x, doom.player.y);
        assert!(
            dist_sq >= (radius * radius) * 0.85,
            "Player clipped into wall: dist_sq={dist_sq}"
        );
    }

    /// Quake jump should return to ground without overshooting.
    #[test]
    fn quake_jump_returns_to_ground() {
        let mut quake = QuakeE1M1State::default();
        let ground = quake.ground_eye_height(quake.player.pos.x, quake.player.pos.y);
        quake.jump();
        let mut max_z = quake.player.pos.z;
        for _ in 0..200 {
            quake.update();
            max_z = max_z.max(quake.player.pos.z);
        }
        assert!(quake.player.grounded);
        assert!((quake.player.pos.z - ground).abs() <= 0.001);
        assert!(
            (max_z - ground) <= 0.25,
            "Quake jump apex {max_z} too high above ground {ground}"
        );
    }

    /// Quake collision should prevent stepping through wall segments.
    #[test]
    fn quake_collision_blocks_wall_crossing() {
        let mut quake = QuakeE1M1State::default();
        assert!(!quake.wall_segments.is_empty());
        let seg = pick_quake_wall(&quake.wall_segments, quake.bounds_min, quake.bounds_max);
        let len = (seg.vx * seg.vx + seg.vy * seg.vy).sqrt().max(0.0001);
        let nx = -seg.vy / len;
        let ny = seg.vx / len;
        let radius = QUAKE_COLLISION_RADIUS;
        let mid_x = (seg.x1 + seg.x2) * 0.5;
        let mid_y = (seg.y1 + seg.y2) * 0.5;
        quake.player.pos.x = mid_x + nx * (radius + 0.08);
        quake.player.pos.y = mid_y + ny * (radius + 0.08);
        quake.player.pos.z = quake.ground_eye_height(quake.player.pos.x, quake.player.pos.y);
        quake.try_move(-nx * (radius + 0.16), -ny * (radius + 0.16));
        let dist_sq = seg.distance_sq(quake.player.pos.x, quake.player.pos.y);
        assert!(
            dist_sq >= (radius * radius) * 0.85,
            "Player clipped into wall: dist_sq={dist_sq}"
        );
    }

    /// Palette swatches should stay aligned to Doom/Quake expectations.
    #[test]
    fn doom_quake_palette_swatches() {
        assert_eq!(rgb_tuple(palette_doom_wall(0)), (96, 66, 44));
        assert_eq!(rgb_tuple(palette_doom_wall(4)), (118, 118, 118));
        assert_eq!(rgb_tuple(palette_doom_wall(5)), (170, 170, 170));

        assert_eq!(rgb_tuple(palette_quake_floor(0.0)), (86, 72, 58));
        assert_eq!(rgb_tuple(palette_quake_floor(1.0)), (128, 108, 88));
        assert_eq!(rgb_tuple(palette_quake_ceiling(0.0)), (70, 76, 92));
        assert_eq!(rgb_tuple(palette_quake_ceiling(1.0)), (110, 118, 136));
        assert_eq!(rgb_tuple(palette_quake_stone(0.0)), (78, 70, 60));
        assert_eq!(rgb_tuple(palette_quake_stone(1.0)), (126, 116, 104));
    }

    /// Verify effect transitions work without panicking.
    #[test]
    fn effect_transitions_smoothly() {
        let mut screen = VisualEffectsScreen::default();
        let mut pool = GraphemePool::new();
        let mut frame = Frame::new(60, 20, &mut pool);
        let area = Rect::new(0, 0, 60, 20);

        // Cycle through all effects
        for _ in 0..EffectType::ALL.len() * 2 {
            screen.view(&mut frame, area);
            screen.effect = screen.effect.next();
        }

        // Cycle backwards
        for _ in 0..EffectType::ALL.len() {
            screen.view(&mut frame, area);
            screen.effect = screen.effect.prev();
        }
        // Should complete without panicking
    }

    /// Verify all effects handle resize gracefully (including edge cases).
    #[test]
    fn effects_handle_resize() {
        let mut screen = VisualEffectsScreen::default();
        let mut pool = GraphemePool::new();

        // Test each effect with various sizes
        for effect in EffectType::ALL {
            screen.effect = *effect;

            // Normal size
            let mut frame = Frame::new(80, 24, &mut pool);
            screen.view(&mut frame, Rect::new(0, 0, 80, 24));

            // Very small
            let mut frame = Frame::new(10, 5, &mut pool);
            screen.view(&mut frame, Rect::new(0, 0, 10, 5));

            // Minimum size (1x1)
            let mut frame = Frame::new(1, 1, &mut pool);
            screen.view(&mut frame, Rect::new(0, 0, 1, 1));

            // Large size
            let mut frame = Frame::new(200, 60, &mut pool);
            screen.view(&mut frame, Rect::new(0, 0, 200, 60));
        }
        // All effects should handle all sizes without panicking
    }

    /// Verify 3D shapes has the expected star count (300+ per bd-3vbf.27).
    #[test]
    fn shape3d_has_dense_starfield() {
        let state = Shape3DState::default();
        assert!(
            state.stars.len() >= 300,
            "3D shapes should have 300+ stars, got {}",
            state.stars.len()
        );
    }

    /// Verify particle trails are longer (12+ per bd-3vbf.27).
    #[test]
    fn particles_have_longer_trails() {
        let mut state = ParticleState::default();
        // Add a particle and update it many times to fill trail
        state.particles.push(Particle {
            x: 0.5,
            y: 0.5,
            vx: 0.01,
            vy: -0.01,
            life: 1.0,
            max_life: 1.0,
            hue: 0.5,
            is_rocket: false,
            trail: Vec::new(),
        });

        // Update many times to fill trail
        for _ in 0..20 {
            state.update_with_quality(FxQuality::Full);
        }

        // Check that some particle has a long trail
        let max_trail = state
            .particles
            .iter()
            .map(|p| p.trail.len())
            .max()
            .unwrap_or(0);
        assert!(
            max_trail >= 12,
            "Particles should have trails of 12+ points, got {}",
            max_trail
        );
    }
}
