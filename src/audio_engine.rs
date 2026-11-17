use jack::{
    AsyncClient, Client, ProcessHandler, ProcessScope, AudioIn, AudioOut, ClientOptions, Port,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use crate::parameters::{MixerParameters, NUM_STRIPS, NUM_BUSES};
use crate::meters::MeterState;

pub struct MixerAudioProcessor {
    params: Arc<MixerParameters>,
    meters: Arc<MeterState>,
    input_ports: Vec<Port<AudioIn>>,
    output_l: Port<AudioOut>,
    output_r: Port<AudioOut>,
    monitor_l: Port<AudioOut>,
    monitor_r: Port<AudioOut>,
    direct_out_ports: Vec<Port<AudioOut>>,
    bus_output_ports: Vec<Port<AudioOut>>,
}

impl ProcessHandler for MixerAudioProcessor {
    fn process(&mut self, _: &Client, ps: &ProcessScope) -> jack::Control {
        let n_frames = ps.n_frames() as usize;

        let out_l = self.output_l.as_mut_slice(ps);
        let out_r = self.output_r.as_mut_slice(ps);
        let mon_l = self.monitor_l.as_mut_slice(ps);
        let mon_r = self.monitor_r.as_mut_slice(ps);

        let mut in_iter = self.input_ports.iter();
        let input_slices: [&[f32]; NUM_STRIPS] =
            std::array::from_fn(|_| in_iter.next().unwrap().as_slice(ps));

        let mut direct_iter = self.direct_out_ports.iter_mut();
        let direct_out_slices: [&mut [f32]; NUM_STRIPS] =
            std::array::from_fn(|_| direct_iter.next().unwrap().as_mut_slice(ps));

        let mut out_iter = self.bus_output_ports.iter_mut();
        let bus_out_slices: [&mut [f32]; NUM_BUSES] =
            std::array::from_fn(|_| out_iter.next().unwrap().as_mut_slice(ps));

        let mut strip_peaks = [0.0f32; NUM_STRIPS];
        let mut bus_peaks = [0.0f32; NUM_BUSES];
        let mut master_l_peak = 0.0f32;
        let mut master_r_peak = 0.0f32;
        let mut monitor_peak_local = 0.0f32;

        let master_gain = MixerParameters::get_f32(&self.params.master_gain);
        let master_mute = self.params.master_mute.load(Ordering::Relaxed);
        let monitor_gain = MixerParameters::get_f32(&self.params.monitor_gain);
        let monitor_mute = self.params.monitor_mute.load(Ordering::Relaxed);
        let any_solo = self.params.any_solo();

        let bus_gain: [f32; NUM_BUSES] =
            std::array::from_fn(|b| MixerParameters::get_f32(&self.params.bus_gain[b]));
        let bus_mute: [bool; NUM_BUSES] =
            std::array::from_fn(|b| self.params.bus_mute[b].load(Ordering::Relaxed));
        let bus_pre: [bool; NUM_BUSES] =
            std::array::from_fn(|b| self.params.bus_pre_post[b].load(Ordering::Relaxed));

        let mut ch_gain = [0.0f32; NUM_STRIPS];
        let mut ch_pan_l = [0.0f32; NUM_STRIPS];
        let mut ch_pan_r = [0.0f32; NUM_STRIPS];
        let mut ch_mute = [false; NUM_STRIPS];
        let mut ch_solo = [false; NUM_STRIPS];
        let mut ch_sends = [[0.0f32; NUM_BUSES]; NUM_STRIPS];

        for ch in 0..NUM_STRIPS {
            ch_mute[ch] = self.params.mute[ch].load(Ordering::Relaxed);
            ch_solo[ch] = self.params.solo[ch].load(Ordering::Relaxed);
            ch_gain[ch] = MixerParameters::get_f32(&self.params.gain[ch]);
            let pan = MixerParameters::get_f32(&self.params.pan[ch]);

            let angle = (pan + 1.0) * std::f32::consts::PI / 4.0;
            ch_pan_l[ch] = angle.cos();
            ch_pan_r[ch] = angle.sin();

            for bus in 0..NUM_BUSES {
                let send_idx = ch * NUM_BUSES + bus;
                ch_sends[ch][bus] = MixerParameters::get_f32(&self.params.send_levels[send_idx]);
            }
        }

        // Monitor trim is a pure listening gain — applies only to monitor_out_*.
        let mon_trim = if monitor_mute { 0.0 } else { monitor_gain };

        for frame in 0..n_frames {
            let mut main_mix_l = 0.0f32;
            let mut main_mix_r = 0.0f32;
            let mut solo_mix_l = 0.0f32;
            let mut solo_mix_r = 0.0f32;
            let mut bus_sums = [0.0f32; NUM_BUSES];

            for ch in 0..NUM_STRIPS {
                let input_sample = input_slices[ch][frame];
                let processed = input_sample * ch_gain[ch];

                direct_out_slices[ch][frame] = if ch_mute[ch] { 0.0 } else { processed };

                let abs_processed = processed.abs();
                if abs_processed > strip_peaks[ch] {
                    strip_peaks[ch] = abs_processed;
                }

                let pan_l = processed * ch_pan_l[ch];
                let pan_r = processed * ch_pan_r[ch];

                if !ch_mute[ch] {
                    main_mix_l += pan_l;
                    main_mix_r += pan_r;

                    // Bus sends: post-mute. Pre-fader tap = raw input, post = post-gain.
                    for bus in 0..NUM_BUSES {
                        let tap = if bus_pre[bus] { input_sample } else { processed };
                        bus_sums[bus] += tap * ch_sends[ch][bus];
                    }
                }

                // Solo mix also respects mute (bug fix — previously mute + solo left
                // the channel audible through the monitor bus).
                if ch_solo[ch] && !ch_mute[ch] {
                    solo_mix_l += pan_l;
                    solo_mix_r += pan_r;
                }
            }

            let master_out_l = if master_mute { 0.0 } else { main_mix_l * master_gain };
            let master_out_r = if master_mute { 0.0 } else { main_mix_r * master_gain };

            out_l[frame] = master_out_l;
            out_r[frame] = master_out_r;

            // Solo path also respects master_gain + master_mute (bug fix — previously
            // solo bypassed both, so pressing solo blasted the monitors 8x louder
            // than the current master trim).
            let solo_out_l = if master_mute { 0.0 } else { solo_mix_l * master_gain };
            let solo_out_r = if master_mute { 0.0 } else { solo_mix_r * master_gain };

            let base_mon_l = if any_solo { solo_out_l } else { master_out_l };
            let base_mon_r = if any_solo { solo_out_r } else { master_out_r };

            let final_mon_l = base_mon_l * mon_trim;
            let final_mon_r = base_mon_r * mon_trim;

            mon_l[frame] = final_mon_l;
            mon_r[frame] = final_mon_r;

            let mon_abs = final_mon_l.abs().max(final_mon_r.abs());
            if mon_abs > monitor_peak_local { monitor_peak_local = mon_abs; }

            if master_out_l.abs() > master_l_peak { master_l_peak = master_out_l.abs(); }
            if master_out_r.abs() > master_r_peak { master_r_peak = master_out_r.abs(); }

            // Bus output: × bus_gain, gated by bus_mute. Meter shows post-gain pre-mute
            // so signal remains visible when the bus is muted.
            for bus in 0..NUM_BUSES {
                let gained = bus_sums[bus] * bus_gain[bus];
                let out = if bus_mute[bus] { 0.0 } else { gained };
                bus_out_slices[bus][frame] = out;
                let abs = gained.abs();
                if abs > bus_peaks[bus] { bus_peaks[bus] = abs; }
            }
        }

        for i in 0..NUM_STRIPS {
            if strip_peaks[i] > 0.0 {
                MeterState::update_max_peak(&self.meters.strip_in[i], strip_peaks[i]);
                MeterState::update_max_peak(&self.meters.strip_in_midi[i], strip_peaks[i]);
            }
        }
        for b in 0..NUM_BUSES {
            if bus_peaks[b] > 0.0 {
                MeterState::update_max_peak(&self.meters.bus_out[b], bus_peaks[b]);
            }
        }
        if master_l_peak > 0.0 { MeterState::update_max_peak(&self.meters.master_l, master_l_peak); }
        if master_r_peak > 0.0 { MeterState::update_max_peak(&self.meters.master_r, master_r_peak); }
        if monitor_peak_local > 0.0 {
            MeterState::update_max_peak(&self.meters.monitor_peak, monitor_peak_local);
            MeterState::update_max_peak(&self.meters.monitor_peak_midi, monitor_peak_local);
        }

        jack::Control::Continue
    }
}

pub fn init_jack(
    params: Arc<MixerParameters>,
    meters: Arc<MeterState>,
) -> Result<AsyncClient<(), MixerAudioProcessor>, jack::Error> {
    let (client, _status) = Client::new("xtouch_64ch_mixer", ClientOptions::NO_START_SERVER)?;

    let mut input_ports = Vec::new();
    for i in 0..NUM_STRIPS {
        let port = client.register_port(&format!("in_{}", i + 1), AudioIn)?;
        input_ports.push(port);
    }

    let output_l = client.register_port("master_out_l", AudioOut)?;
    let output_r = client.register_port("master_out_r", AudioOut)?;
    let monitor_l = client.register_port("monitor_out_l", AudioOut)?;
    let monitor_r = client.register_port("monitor_out_r", AudioOut)?;

    let mut direct_out_ports = Vec::new();
    for i in 0..NUM_STRIPS {
        let port = client.register_port(&format!("direct_out_{}", i + 1), AudioOut)?;
        direct_out_ports.push(port);
    }

    let mut bus_output_ports = Vec::new();
    for b in 0..NUM_BUSES {
        let port = client.register_port(&format!("bus_{}", b + 1), AudioOut)?;
        bus_output_ports.push(port);
    }

    let processor = MixerAudioProcessor {
        params,
        meters,
        input_ports,
        output_l,
        output_r,
        monitor_l,
        monitor_r,
        direct_out_ports,
        bus_output_ports,
    };

    let active_client = client.activate_async((), processor)?;
    Ok(active_client)
}
