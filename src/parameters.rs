use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicBool, AtomicUsize, Ordering};
use serde::{Serialize, Deserialize};

pub const NUM_PHYSICAL_FADERS: usize = 8;
pub const NUM_BANKS: usize = 8;
pub const NUM_STRIPS: usize = NUM_PHYSICAL_FADERS * NUM_BANKS;
pub const NUM_BUSES: usize = 8;

pub const DEFAULT_BUS_GAIN: f32 = 0.75;

fn default_bus_gain() -> Vec<f32> { vec![DEFAULT_BUS_GAIN; NUM_BUSES] }
fn default_bus_bools() -> Vec<bool> { vec![false; NUM_BUSES] }
fn default_monitor_gain() -> f32 { 1.0 }

/// What the physical master fader currently represents.
/// Resolved by `MixerParameters::master_fader_target_mode()` so every
/// consumer (MIDI, audio, GUI) agrees on the current meaning.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum FaderTarget {
    Master,
    Bus(usize),
    Monitor,
}

#[derive(Serialize, Deserialize)]
pub struct MixerState {
    pub gain: Vec<f32>,
    pub pan: Vec<f32>,
    pub mute: Vec<bool>,
    pub solo: Vec<bool>,
    pub select: Vec<bool>,
    pub send_levels: Vec<f32>,
    pub master_gain: f32,
    pub master_mute: bool,
    pub bank_offset: usize,
    pub selected_strip: usize,
    pub current_bus: usize,
    pub send_on_fader_mode: bool,
    pub vu_mode: Vec<bool>,
    #[serde(default = "default_bus_gain")]
    pub bus_gain: Vec<f32>,
    #[serde(default = "default_bus_bools")]
    pub bus_mute: Vec<bool>,
    #[serde(default = "default_bus_bools")]
    pub bus_pre_post: Vec<bool>,
    // New in this version — optional so old state files still load.
    #[serde(default)]
    pub monitor_mode: bool,
    #[serde(default = "default_monitor_gain")]
    pub monitor_gain: f32,
    #[serde(default)]
    pub monitor_mute: bool,
}

pub struct MixerParameters {
    pub gain: Vec<AtomicU32>,
    pub pan: Vec<AtomicU32>,
    pub mute: Vec<AtomicBool>,
    pub solo: Vec<AtomicBool>,
    pub select: Vec<AtomicBool>,
    pub send_levels: Vec<AtomicU32>,
    pub master_gain: AtomicU32,
    pub master_mute: AtomicBool,
    pub bank_offset: AtomicUsize,
    pub selected_strip: AtomicUsize,
    pub current_bus: AtomicUsize,
    pub send_on_fader_mode: AtomicBool,
    pub vu_mode: Vec<AtomicBool>,
    pub bus_gain: Vec<AtomicU32>,
    pub bus_mute: Vec<AtomicBool>,
    pub bus_pre_post: Vec<AtomicBool>,
    pub monitor_mode: AtomicBool,
    pub monitor_gain: AtomicU32,
    pub monitor_mute: AtomicBool,
}

impl MixerParameters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            gain: (0..NUM_STRIPS).map(|_| AtomicU32::new(0.74f32.to_bits())).collect(),
            pan: (0..NUM_STRIPS).map(|_| AtomicU32::new(0.0f32.to_bits())).collect(),
            mute: (0..NUM_STRIPS).map(|_| AtomicBool::new(false)).collect(),
            solo: (0..NUM_STRIPS).map(|_| AtomicBool::new(false)).collect(),
            select: (0..NUM_STRIPS).map(|_| AtomicBool::new(false)).collect(),
            send_levels: (0..NUM_STRIPS * NUM_BUSES)
                .map(|_| AtomicU32::new(0.0f32.to_bits()))
                .collect(),
            master_gain: AtomicU32::new(0.74f32.to_bits()),
            master_mute: AtomicBool::new(false),
            bank_offset: AtomicUsize::new(0),
            selected_strip: AtomicUsize::new(0),
            current_bus: AtomicUsize::new(0),
            send_on_fader_mode: AtomicBool::new(false),
            vu_mode: (0..NUM_STRIPS).map(|_| AtomicBool::new(false)).collect(),
            bus_gain: (0..NUM_BUSES)
                .map(|_| AtomicU32::new(DEFAULT_BUS_GAIN.to_bits()))
                .collect(),
            bus_mute: (0..NUM_BUSES).map(|_| AtomicBool::new(false)).collect(),
            bus_pre_post: (0..NUM_BUSES).map(|_| AtomicBool::new(false)).collect(),
            monitor_mode: AtomicBool::new(false),
            monitor_gain: AtomicU32::new(1.0f32.to_bits()),
            monitor_mute: AtomicBool::new(false),
        })
    }

    pub fn any_solo(&self) -> bool {
        (0..NUM_STRIPS).any(|i| self.solo[i].load(Ordering::Relaxed))
    }

    /// Single source of truth for what the master fader controls right now.
    /// Monitor mode wins over aux mode; aux mode wins over plain master.
    pub fn master_fader_target_mode(&self) -> FaderTarget {
        if self.monitor_mode.load(Ordering::Relaxed) {
            FaderTarget::Monitor
        } else if self.send_on_fader_mode.load(Ordering::Relaxed) {
            FaderTarget::Bus(self.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1))
        } else {
            FaderTarget::Master
        }
    }

    #[inline]
    pub fn get_f32(atomic: &AtomicU32) -> f32 {
        f32::from_bits(atomic.load(Ordering::Relaxed))
    }

    #[inline]
    pub fn set_f32(atomic: &AtomicU32, value: f32) {
        atomic.store(value.to_bits(), Ordering::Relaxed);
    }

    pub fn save_to_file(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let state = MixerState {
            gain: (0..NUM_STRIPS).map(|i| Self::get_f32(&self.gain[i])).collect(),
            pan: (0..NUM_STRIPS).map(|i| Self::get_f32(&self.pan[i])).collect(),
            mute: (0..NUM_STRIPS).map(|i| self.mute[i].load(Ordering::Relaxed)).collect(),
            solo: (0..NUM_STRIPS).map(|i| self.solo[i].load(Ordering::Relaxed)).collect(),
            select: (0..NUM_STRIPS).map(|i| self.select[i].load(Ordering::Relaxed)).collect(),
            send_levels: (0..NUM_STRIPS * NUM_BUSES)
                .map(|i| Self::get_f32(&self.send_levels[i]))
                .collect(),
            master_gain: Self::get_f32(&self.master_gain),
            master_mute: self.master_mute.load(Ordering::Relaxed),
            bank_offset: self.bank_offset.load(Ordering::Relaxed),
            selected_strip: self.selected_strip.load(Ordering::Relaxed),
            current_bus: self.current_bus.load(Ordering::Relaxed),
            send_on_fader_mode: self.send_on_fader_mode.load(Ordering::Relaxed),
            vu_mode: (0..NUM_STRIPS).map(|i| self.vu_mode[i].load(Ordering::Relaxed)).collect(),
            bus_gain: (0..NUM_BUSES).map(|i| Self::get_f32(&self.bus_gain[i])).collect(),
            bus_mute: (0..NUM_BUSES).map(|i| self.bus_mute[i].load(Ordering::Relaxed)).collect(),
            bus_pre_post: (0..NUM_BUSES).map(|i| self.bus_pre_post[i].load(Ordering::Relaxed)).collect(),
            monitor_mode: self.monitor_mode.load(Ordering::Relaxed),
            monitor_gain: Self::get_f32(&self.monitor_gain),
            monitor_mute: self.monitor_mute.load(Ordering::Relaxed),
        };
        let json = serde_json::to_string_pretty(&state)?;
        let tmp = format!("{}.tmp", path);
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn load_from_file(&self, path: &str) -> Result<(), Box<dyn std::error::Error>> {
        let json = std::fs::read_to_string(path)?;
        let state: MixerState = serde_json::from_str(&json)?;

        if state.gain.len() != NUM_STRIPS ||
           state.pan.len() != NUM_STRIPS ||
           state.mute.len() != NUM_STRIPS ||
           state.solo.len() != NUM_STRIPS ||
           state.select.len() != NUM_STRIPS ||
           state.send_levels.len() != NUM_STRIPS * NUM_BUSES ||
           state.vu_mode.len() != NUM_STRIPS ||
           state.bus_gain.len() != NUM_BUSES ||
           state.bus_mute.len() != NUM_BUSES ||
           state.bus_pre_post.len() != NUM_BUSES {
            return Err("Invalid mixer state file: vector lengths mismatch".into());
        }
        if state.bank_offset >= NUM_BANKS {
            return Err(format!("Invalid mixer state: bank_offset {} >= {}", state.bank_offset, NUM_BANKS).into());
        }
        if state.current_bus >= NUM_BUSES {
            return Err(format!("Invalid mixer state: current_bus {} >= {}", state.current_bus, NUM_BUSES).into());
        }
        if state.selected_strip >= NUM_STRIPS {
            return Err(format!("Invalid mixer state: selected_strip {} >= {}", state.selected_strip, NUM_STRIPS).into());
        }

        for i in 0..NUM_STRIPS {
            Self::set_f32(&self.gain[i], state.gain[i]);
            Self::set_f32(&self.pan[i], state.pan[i]);
            self.mute[i].store(state.mute[i], Ordering::Relaxed);
            self.solo[i].store(state.solo[i], Ordering::Relaxed);
            self.select[i].store(state.select[i], Ordering::Relaxed);
            self.vu_mode[i].store(state.vu_mode[i], Ordering::Relaxed);
        }
        for i in 0..NUM_STRIPS * NUM_BUSES {
            Self::set_f32(&self.send_levels[i], state.send_levels[i]);
        }
        for b in 0..NUM_BUSES {
            Self::set_f32(&self.bus_gain[b], state.bus_gain[b]);
            self.bus_mute[b].store(state.bus_mute[b], Ordering::Relaxed);
            self.bus_pre_post[b].store(state.bus_pre_post[b], Ordering::Relaxed);
        }
        Self::set_f32(&self.master_gain, state.master_gain);
        self.master_mute.store(state.master_mute, Ordering::Relaxed);
        self.bank_offset.store(state.bank_offset, Ordering::Relaxed);
        self.selected_strip.store(state.selected_strip, Ordering::Relaxed);
        self.current_bus.store(state.current_bus, Ordering::Relaxed);
        self.send_on_fader_mode.store(state.send_on_fader_mode, Ordering::Relaxed);
        self.monitor_mode.store(state.monitor_mode, Ordering::Relaxed);
        Self::set_f32(&self.monitor_gain, state.monitor_gain);
        self.monitor_mute.store(state.monitor_mute, Ordering::Relaxed);

        Ok(())
    }
}
