use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use crate::parameters::{NUM_STRIPS, NUM_BUSES};

pub struct MeterState {
    pub strip_in: Vec<AtomicU32>,
    pub strip_in_midi: Vec<AtomicU32>,
    pub bus_out: Vec<AtomicU32>,
    pub master_l: AtomicU32,
    pub master_r: AtomicU32,
    // Dual atomics for the monitor meter — GUI consumer and MIDI 9th-vpot
    // consumer drain independently, mirroring the strip_in / strip_in_midi pattern.
    pub monitor_peak: AtomicU32,
    pub monitor_peak_midi: AtomicU32,
}

impl MeterState {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            strip_in: (0..NUM_STRIPS).map(|_| AtomicU32::new(0.0f32.to_bits())).collect(),
            strip_in_midi: (0..NUM_STRIPS).map(|_| AtomicU32::new(0.0f32.to_bits())).collect(),
            bus_out: (0..NUM_BUSES).map(|_| AtomicU32::new(0.0f32.to_bits())).collect(),
            master_l: AtomicU32::new(0.0f32.to_bits()),
            master_r: AtomicU32::new(0.0f32.to_bits()),
            monitor_peak: AtomicU32::new(0.0f32.to_bits()),
            monitor_peak_midi: AtomicU32::new(0.0f32.to_bits()),
        })
    }

    #[inline]
    pub fn update_max_peak(atomic: &AtomicU32, value: f32) {
        let mut current = atomic.load(Ordering::Relaxed);
        loop {
            if value <= f32::from_bits(current) {
                break;
            }
            match atomic.compare_exchange_weak(
                current,
                value.to_bits(),
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => current = actual,
            }
        }
    }

    #[inline]
    pub fn take_peak(atomic: &AtomicU32) -> f32 {
        f32::from_bits(atomic.swap(0.0f32.to_bits(), Ordering::Relaxed))
    }
}
