use midir::{MidiInput, MidiOutput};
use crossbeam_channel::{unbounded, Sender};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use log::error;
use crate::parameters::{
    MixerParameters, FaderTarget, NUM_PHYSICAL_FADERS, NUM_BANKS, NUM_STRIPS, NUM_BUSES,
};
use crate::meters::MeterState;

pub const NOTE_BANK_LEFT: u8 = 0x2E;
pub const NOTE_BANK_RIGHT: u8 = 0x2F;
pub const NOTE_MASTER_MUTE: u8 = 0x32;

pub const AUX_MODE_NOTES: [u8; NUM_BUSES] = [91, 92, 86, 95, 93, 94, 84, 85];

/// CC number of the 9th (master) V-Pot LED ring. Verify once with
/// `aseqdump -p <port> -N` by turning the 9th encoder. Standard MCU = 0x38.
/// Hardware without a 9th encoder silently ignores this message.
pub const CC_MASTER_VPOT: u8 = 0x38;

pub struct MidiController {
    _in_conn: midir::MidiInputConnection<()>,
    out_tx: Sender<Vec<u8>>,
}

impl MidiController {
    pub fn new(
        params: Arc<MixerParameters>,
        meters: Arc<MeterState>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let midi_in = MidiInput::new("xtouch_mixer_in")?;
        let midi_out = MidiOutput::new("xtouch_mixer_out")?;

        let in_ports = midi_in.ports();
        let out_ports = midi_out.ports();

        let in_port = in_ports.iter()
            .find(|p| midi_in.port_name(p).unwrap_or_default().to_uppercase().contains("X-TOUCH"))
            .ok_or("X-Touch input port not found")?;
        let out_port = out_ports.iter()
            .find(|p| midi_out.port_name(p).unwrap_or_default().to_uppercase().contains("X-TOUCH"))
            .ok_or("X-Touch output port not found")?;

        let (tx, rx) = unbounded::<Vec<u8>>();

        let mut out_conn = midi_out.connect(out_port, "xtouch_out")?;
        std::thread::spawn(move || {
            while let Ok(msg) = rx.recv() {
                if let Err(e) = out_conn.send(&msg) {
                    error!("MIDI output error: {}", e);
                }
            }
        });

        let params_clone = params.clone();
        let tx_clone = tx.clone();
        let mut held_selects = 0;

        let in_conn = midi_in.connect(in_port, "xtouch_in", move |_stamp, message, _| {
            handle_midi_message(message, &params_clone, &tx_clone, &mut held_selects);
        }, ())?;

        start_meter_thread(params.clone(), meters.clone(), tx.clone());

        let params_sync = params.clone();
        let tx_sync = tx.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(std::time::Duration::from_millis(500));
                send_bank_state(&params_sync, &tx_sync);
                send_master_mute_led(&params_sync, &tx_sync);
            }
        });

        send_bank_state(&params, &tx);
        send_master_mute_led(&params, &tx);

        Ok(MidiController { _in_conn: in_conn, out_tx: tx })
    }

    pub fn get_sender(&self) -> Sender<Vec<u8>> {
        self.out_tx.clone()
    }
}

fn master_fader_target(params: &MixerParameters) -> f32 {
    match params.master_fader_target_mode() {
        FaderTarget::Monitor  => MixerParameters::get_f32(&params.monitor_gain),
        FaderTarget::Bus(b)   => MixerParameters::get_f32(&params.bus_gain[b]),
        FaderTarget::Master   => MixerParameters::get_f32(&params.master_gain),
    }
}

fn start_meter_thread(
    params: Arc<MixerParameters>,
    meters: Arc<MeterState>,
    tx: Sender<Vec<u8>>,
) {
    std::thread::spawn(move || {
        let mut last_vpot_states = [255u8; NUM_PHYSICAL_FADERS];
        let mut last_bank = 255usize;
        let mut last_master_val: u16 = 0xFFFF;
        let mut last_monitor_vpot: u8 = 255;

        loop {
            std::thread::sleep(std::time::Duration::from_millis(100));
            let bank = params.bank_offset.load(Ordering::Relaxed);
            if bank != last_bank {
                last_vpot_states.fill(255);
                last_bank = bank;
            }

            // Contextual master fader.
            let target = master_fader_target(&params);
            let pb_val = (target * 16383.0) as u16;
            if pb_val != last_master_val {
                let _ = tx.send(vec![0xE8, (pb_val & 0x7F) as u8, (pb_val >> 7) as u8]);
                last_master_val = pb_val;
            }

            for physical in 0..NUM_PHYSICAL_FADERS {
                let logical = bank * NUM_PHYSICAL_FADERS + physical;
                if logical >= NUM_STRIPS { continue; }

                let cc_number = 0x30 + physical as u8;
                let vu = params.vu_mode[logical].load(Ordering::Relaxed);

                let msg_val: u8 = if vu {
                    let peak = MeterState::take_peak(&meters.strip_in_midi[logical]);
                    let level = (peak * 11.0).round() as u8;
                    0x20 | level.min(11)
                } else {
                    let pan = MixerParameters::get_f32(&params.pan[logical]);
                    let v = ((pan + 1.0) / 2.0 * 11.0).round() as u8;
                    0x10 | v.min(11)
                };

                if msg_val != last_vpot_states[physical] {
                    let _ = tx.send(vec![0xB0, cc_number, msg_val]);
                    last_vpot_states[physical] = msg_val;
                }
            }

            // 9th V-Pot: monitor meter (mode ON) or monitor-gain position (OFF).
            // Always drain the peak — even when off — so stale values don't
            // accumulate while monitor mode is disabled.
            let mon_peak = MeterState::take_peak(&meters.monitor_peak_midi);
            let mon_vpot: u8 = if params.monitor_mode.load(Ordering::Relaxed) {
                let level = (mon_peak * 11.0).round() as u8;
                0x20 | level.min(11)
            } else {
                let gain = MixerParameters::get_f32(&params.monitor_gain);
                let pos = (gain * 11.0).round() as u8;
                0x10 | pos.min(11)
            };
            if mon_vpot != last_monitor_vpot {
                let _ = tx.send(vec![0xB0, CC_MASTER_VPOT, mon_vpot]);
                last_monitor_vpot = mon_vpot;
            }
        }
    });
}

fn handle_midi_message(msg: &[u8], params: &Arc<MixerParameters>, tx: &Sender<Vec<u8>>, held_selects: &mut usize) {
    if msg.len() < 2 { return; }
    let status = msg[0];
    let msg_type = status & 0xF0;

    match msg_type {
        0xE0 => {
            if msg.len() < 3 { return; }
            let value = ((msg[2] as u16) << 7 | msg[1] as u16) as f32 / 16383.0;
            let channel = (status & 0x0F) as usize;

            if channel < NUM_PHYSICAL_FADERS {
                let bank = params.bank_offset.load(Ordering::Relaxed);
                let logical = bank * NUM_PHYSICAL_FADERS + channel;
                if logical < NUM_STRIPS {
                    let is_selected = params.select[logical].load(Ordering::Relaxed);
                    let send_mode = params.send_on_fader_mode.load(Ordering::Relaxed);
                    let bus = params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);

                    let old_val = if send_mode {
                        MixerParameters::get_f32(&params.send_levels[logical * NUM_BUSES + bus])
                    } else {
                        MixerParameters::get_f32(&params.gain[logical])
                    };
                    let delta = value - old_val;

                    if is_selected {
                        let bank_start = bank * NUM_PHYSICAL_FADERS;
                        let bank_end = bank_start + NUM_PHYSICAL_FADERS;
                        for i in 0..NUM_STRIPS {
                            if params.select[i].load(Ordering::Relaxed) {
                                if send_mode {
                                    let idx = i * NUM_BUSES + bus;
                                    let mut l = MixerParameters::get_f32(&params.send_levels[idx]);
                                    l = (l + delta).clamp(0.0, 1.0);
                                    MixerParameters::set_f32(&params.send_levels[idx], l);
                                } else {
                                    let mut g = MixerParameters::get_f32(&params.gain[i]);
                                    g = (g + delta).clamp(0.0, 1.0);
                                    MixerParameters::set_f32(&params.gain[i], g);
                                }
                                if i != logical && i >= bank_start && i < bank_end {
                                    let phys = (i - bank_start) as u8;
                                    let nv = if send_mode {
                                        MixerParameters::get_f32(&params.send_levels[i * NUM_BUSES + bus])
                                    } else {
                                        MixerParameters::get_f32(&params.gain[i])
                                    };
                                    let pb_value = (nv * 16383.0) as u16;
                                    let _ = tx.send(vec![0xE0 | phys, (pb_value & 0x7F) as u8, (pb_value >> 7) as u8]);
                                }
                            }
                        }
                    } else {
                        if send_mode {
                            MixerParameters::set_f32(&params.send_levels[logical * NUM_BUSES + bus], value);
                        } else {
                            MixerParameters::set_f32(&params.gain[logical], value);
                        }
                    }
                }
            } else if channel == 8 {
                // Contextual master fader — three-way dispatch.
                match params.master_fader_target_mode() {
                    FaderTarget::Monitor  => MixerParameters::set_f32(&params.monitor_gain, value),
                    FaderTarget::Bus(b)   => MixerParameters::set_f32(&params.bus_gain[b], value),
                    FaderTarget::Master   => MixerParameters::set_f32(&params.master_gain, value),
                }
            }
        },
        0xB0 => {
            if msg.len() < 3 { return; }
            let cc = msg[1];
            let value = msg[2];

            if (0x10..=0x17).contains(&cc) {
                let physical = (cc - 0x10) as usize;
                let bank = params.bank_offset.load(Ordering::Relaxed);
                let logical = bank * NUM_PHYSICAL_FADERS + physical;
                if logical < NUM_STRIPS {
                    let delta = relative_delta(value);
                    let mut pan = MixerParameters::get_f32(&params.pan[logical]);
                    pan = (pan + delta).clamp(-1.0, 1.0);
                    MixerParameters::set_f32(&params.pan[logical], pan);
                }
            }
        },
        0x90 => {
            if msg.len() < 3 { return; }
            let note = msg[1];
            let velocity = msg[2];

            if note <= 0x07 && velocity > 0 {
                let bank_index = note as usize;
                if bank_index < NUM_BANKS {
                    params.bank_offset.store(bank_index, Ordering::Relaxed);
                    send_bank_state(params, tx);
                }
            } else if note >= 0x10 && note <= 0x17 && velocity > 0 {
                let physical = (note - 0x10) as usize;
                let bank = params.bank_offset.load(Ordering::Relaxed);
                let logical = bank * NUM_PHYSICAL_FADERS + physical;
                if logical < NUM_STRIPS {
                    let new_val = !params.mute[logical].load(Ordering::Relaxed);
                    let is_selected = params.select[logical].load(Ordering::Relaxed);
                    if is_selected {
                        let bank_start = bank * NUM_PHYSICAL_FADERS;
                        let bank_end = bank_start + NUM_PHYSICAL_FADERS;
                        for i in 0..NUM_STRIPS {
                            if params.select[i].load(Ordering::Relaxed) {
                                params.mute[i].store(new_val, Ordering::Relaxed);
                                if i >= bank_start && i < bank_end {
                                    let _ = tx.send(vec![0x90, 0x10 + (i - bank_start) as u8, if new_val {127} else {0}]);
                                }
                            }
                        }
                    } else {
                        params.mute[logical].store(new_val, Ordering::Relaxed);
                        let _ = tx.send(vec![0x90, note, if new_val {127} else {0}]);
                    }
                }
            } else if note >= 0x08 && note <= 0x0F && velocity > 0 {
                let physical = (note - 0x08) as usize;
                let bank = params.bank_offset.load(Ordering::Relaxed);
                let logical = bank * NUM_PHYSICAL_FADERS + physical;
                if logical < NUM_STRIPS {
                    let new_val = !params.solo[logical].load(Ordering::Relaxed);
                    let is_selected = params.select[logical].load(Ordering::Relaxed);
                    if is_selected {
                        let bank_start = bank * NUM_PHYSICAL_FADERS;
                        let bank_end = bank_start + NUM_PHYSICAL_FADERS;
                        for i in 0..NUM_STRIPS {
                            if params.select[i].load(Ordering::Relaxed) {
                                params.solo[i].store(new_val, Ordering::Relaxed);
                                if i >= bank_start && i < bank_end {
                                    let _ = tx.send(vec![0x90, 0x08 + (i - bank_start) as u8, if new_val {127} else {0}]);
                                }
                            }
                        }
                    } else {
                        params.solo[logical].store(new_val, Ordering::Relaxed);
                        let _ = tx.send(vec![0x90, note, if new_val {127} else {0}]);
                    }
                }
            } else if note >= 0x18 && note <= 0x1F {
                let physical = (note - 0x18) as usize;
                if physical < NUM_PHYSICAL_FADERS {
                    let bank = params.bank_offset.load(Ordering::Relaxed);
                    let logical = bank * NUM_PHYSICAL_FADERS + physical;
                    if logical < NUM_STRIPS {
                        if velocity > 0 {
                            if *held_selects == 0 {
                                for i in 0..NUM_STRIPS {
                                    params.select[i].store(false, Ordering::Relaxed);
                                }
                            }
                            *held_selects += 1;
                            params.select[logical].store(true, Ordering::Relaxed);
                            params.selected_strip.store(logical, Ordering::Relaxed);
                        } else if *held_selects > 0 {
                            *held_selects -= 1;
                        }
                        send_select_leds(params, tx);
                    }
                }
            } else if note >= 0x20 && note <= 0x27 && velocity > 0 {
                let physical = (note - 0x20) as usize;
                let bank = params.bank_offset.load(Ordering::Relaxed);
                let logical = bank * NUM_PHYSICAL_FADERS + physical;
                if logical < NUM_STRIPS {
                    let new_val = !params.vu_mode[logical].load(Ordering::Relaxed);
                    params.vu_mode[logical].store(new_val, Ordering::Relaxed);
                }
            } else if let Some(bus) = AUX_MODE_NOTES.iter().position(|&n| n == note) {
                if velocity > 0 {
                    for i in 0..NUM_STRIPS {
                        params.vu_mode[i].store(false, Ordering::Relaxed);
                    }
                    let current_mode = params.send_on_fader_mode.load(Ordering::Relaxed);
                    let current_bus = params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);
                    if current_mode && current_bus == bus {
                        params.send_on_fader_mode.store(false, Ordering::Relaxed);
                    } else {
                        // Entering aux mode always exits monitor mode — the master
                        // fader is a single physical control with a single meaning.
                        params.monitor_mode.store(false, Ordering::Relaxed);
                        params.send_on_fader_mode.store(true, Ordering::Relaxed);
                        params.current_bus.store(bus, Ordering::Relaxed);
                    }
                    send_bank_state(params, tx);
                    send_master_mute_led(params, tx);
                }
            } else if note == NOTE_MASTER_MUTE && velocity > 0 {
                match params.master_fader_target_mode() {
                    FaderTarget::Monitor => {
                        let v = !params.monitor_mute.load(Ordering::Relaxed);
                        params.monitor_mute.store(v, Ordering::Relaxed);
                    }
                    FaderTarget::Bus(b) => {
                        let v = !params.bus_mute[b].load(Ordering::Relaxed);
                        params.bus_mute[b].store(v, Ordering::Relaxed);
                    }
                    FaderTarget::Master => {
                        let v = !params.master_mute.load(Ordering::Relaxed);
                        params.master_mute.store(v, Ordering::Relaxed);
                    }
                }
                send_master_mute_led(params, tx);
            } else if note == NOTE_BANK_LEFT && velocity > 0 {
                let mut bank = params.bank_offset.load(Ordering::Relaxed);
                if bank > 0 {
                    bank -= 1;
                    params.bank_offset.store(bank, Ordering::Relaxed);
                    send_bank_state(params, tx);
                }
            } else if note == NOTE_BANK_RIGHT && velocity > 0 {
                let mut bank = params.bank_offset.load(Ordering::Relaxed);
                if bank < NUM_BANKS - 1 {
                    bank += 1;
                    params.bank_offset.store(bank, Ordering::Relaxed);
                    send_bank_state(params, tx);
                }
            }
        },
        _ => {}
    }
}

fn relative_delta(value: u8) -> f32 {
    if (0x01..=0x3F).contains(&value) {
        value as f32 / 127.0 * 2.7
    } else if (0x41..=0x7F).contains(&value) {
        -((value - 0x40) as f32) / 127.0 * 2.7
    } else {
        0.0
    }
}

fn send_bank_state(params: &Arc<MixerParameters>, tx: &Sender<Vec<u8>>) {
    let bank = params.bank_offset.load(Ordering::Relaxed);
    let send_mode = params.send_on_fader_mode.load(Ordering::Relaxed);
    let current_bus = params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);

    for physical in 0..NUM_PHYSICAL_FADERS {
        let logical = bank * NUM_PHYSICAL_FADERS + physical;
        if logical < NUM_STRIPS {
            let value = if send_mode {
                MixerParameters::get_f32(&params.send_levels[logical * NUM_BUSES + current_bus])
            } else {
                MixerParameters::get_f32(&params.gain[logical])
            };
            let pb_value = (value * 16383.0) as u16;
            let _ = tx.send(vec![0xE0 | physical as u8, (pb_value & 0x7F) as u8, (pb_value >> 7) as u8]);
        }
    }

    for physical in 0..NUM_PHYSICAL_FADERS {
        let logical = bank * NUM_PHYSICAL_FADERS + physical;
        if logical < NUM_STRIPS {
            let mute = params.mute[logical].load(Ordering::Relaxed);
            let solo = params.solo[logical].load(Ordering::Relaxed);
            let _ = tx.send(vec![0x90, 0x10 + physical as u8, if mute {127} else {0}]);
            let _ = tx.send(vec![0x90, 0x08 + physical as u8, if solo {127} else {0}]);
        }
    }

    send_select_leds(params, tx);

    for i in 0..NUM_BANKS {
        let _ = tx.send(vec![0x90, 0x00 + i as u8, if i == bank {127} else {0}]);
    }

    for (i, &note) in AUX_MODE_NOTES.iter().enumerate() {
        let lit = send_mode && current_bus == i;
        let _ = tx.send(vec![0x90, note, if lit {127} else {0}]);
    }

    for physical in 0..NUM_PHYSICAL_FADERS {
        let logical = bank * NUM_PHYSICAL_FADERS + physical;
        let vu = params.vu_mode[logical].load(Ordering::Relaxed);
        if vu { continue; }
        let pan = MixerParameters::get_f32(&params.pan[logical]);
        let v = ((pan + 1.0) / 2.0 * 11.0).round() as u8;
        let msg_val = 0x10 | v.min(11);
        let _ = tx.send(vec![0xB0, 0x30 + physical as u8, msg_val]);
    }

    let master_target = master_fader_target(params);
    let pb_value = (master_target * 16383.0) as u16;
    let _ = tx.send(vec![0xE8, (pb_value & 0x7F) as u8, (pb_value >> 7) as u8]);
}

fn send_select_leds(params: &Arc<MixerParameters>, tx: &Sender<Vec<u8>>) {
    let bank = params.bank_offset.load(Ordering::Relaxed);
    for physical in 0..NUM_PHYSICAL_FADERS {
        let logical = bank * NUM_PHYSICAL_FADERS + physical;
        let state = params.select[logical].load(Ordering::Relaxed);
        let _ = tx.send(vec![0x90, 0x18 + physical as u8, if state {127} else {0}]);
    }
}

fn send_master_mute_led(params: &Arc<MixerParameters>, tx: &Sender<Vec<u8>>) {
    let state = match params.master_fader_target_mode() {
        FaderTarget::Monitor => params.monitor_mute.load(Ordering::Relaxed),
        FaderTarget::Bus(b)  => params.bus_mute[b].load(Ordering::Relaxed),
        FaderTarget::Master  => params.master_mute.load(Ordering::Relaxed),
    };
    let _ = tx.send(vec![0x90, NOTE_MASTER_MUTE, if state {127} else {0}]);
}
