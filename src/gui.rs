use eframe::egui;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use crossbeam_channel::Sender;
use crate::parameters::{
    MixerParameters, FaderTarget, NUM_PHYSICAL_FADERS, NUM_BANKS, NUM_STRIPS, NUM_BUSES,
};
use crate::meters::MeterState;
use crate::midi_control::{AUX_MODE_NOTES, NOTE_MASTER_MUTE};

/// egui 0.27 ships RED / GREEN / BLUE / YELLOW / LIGHT_BLUE / etc., but has no
/// `Color32::CYAN` constant. Define it once so monitor-mode accents stay
/// consistent across the file. `from_rgb` is const in 0.27.
const CYAN: egui::Color32 = egui::Color32::from_rgb(0, 220, 220);

fn draw_knob(
    ui: &mut egui::Ui,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    size: f32,
    step: f32,
    sensitivity: f32,
) -> egui::Response {
    let (rect, mut response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click_and_drag());
    let min = *range.start();
    let max = *range.end();

    if response.double_clicked() {
        *value = if min < 0.0 { 0.0 } else { min };
        response.mark_changed();
    }
    if response.dragged() {
        let drag_dist = -response.drag_delta().y * sensitivity;
        let range_size = max - min;
        let mut new_val = *value + drag_dist * range_size;
        // Snap relative to `min` so both endpoints are reachable.
        new_val = min + ((new_val - min) / step).round() * step;
        *value = new_val.clamp(min, max);
        response.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let center = rect.center();
        let radius = size / 2.0;
        let stroke = if response.dragged() {
            egui::Stroke::new(2.0_f32, egui::Color32::WHITE)
        } else if response.hovered() {
            egui::Stroke::new(1.5_f32, egui::Color32::LIGHT_GRAY)
        } else {
            egui::Stroke::new(1.0_f32, egui::Color32::GRAY)
        };
        ui.painter().circle_filled(center, radius, egui::Color32::from_gray(30));
        ui.painter().circle_stroke(center, radius, stroke);
        let normalized = (*value - min) / (max - min);
        let start_angle = std::f32::consts::PI * 0.75;
        let end_angle = std::f32::consts::PI * 2.25;
        let angle = start_angle + normalized * (end_angle - start_angle);
        let dir = egui::vec2(angle.cos(), angle.sin());
        ui.painter().line_segment(
            [center, center + dir * (radius * 0.7)],
            egui::Stroke::new(2.0_f32, egui::Color32::WHITE)
        );
    }
    response
}

pub struct MixerApp {
    params: Arc<MixerParameters>,
    meters: Arc<MeterState>,
    midi_tx: Sender<Vec<u8>>,
    strip_display: Vec<f32>,
    bus_display: Vec<f32>,
    master_l_display: f32,
    master_r_display: f32,
    monitor_display: f32,
    last_update: std::time::Instant,
}

impl MixerApp {
    pub fn new(params: Arc<MixerParameters>, meters: Arc<MeterState>, midi_tx: Sender<Vec<u8>>) -> Self {
        Self {
            params,
            meters,
            midi_tx,
            strip_display: vec![0.0; NUM_STRIPS],
            bus_display: vec![0.0; NUM_BUSES],
            master_l_display: 0.0,
            master_r_display: 0.0,
            monitor_display: 0.0,
            last_update: std::time::Instant::now(),
        }
    }

    fn update_meter_displays(&mut self) {
        let now = std::time::Instant::now();
        let dt = (now - self.last_update).as_secs_f32();
        self.last_update = now;
        let decay = 0.5f32.powf(dt / 0.3);

        for i in 0..NUM_STRIPS {
            let peak = MeterState::take_peak(&self.meters.strip_in[i]);
            if peak > self.strip_display[i] {
                self.strip_display[i] = peak;
            } else {
                self.strip_display[i] *= decay;
                if self.strip_display[i] < 0.001 { self.strip_display[i] = 0.0; }
            }
        }
        for i in 0..NUM_BUSES {
            let peak = MeterState::take_peak(&self.meters.bus_out[i]);
            if peak > self.bus_display[i] {
                self.bus_display[i] = peak;
            } else {
                self.bus_display[i] *= decay;
                if self.bus_display[i] < 0.001 { self.bus_display[i] = 0.0; }
            }
        }

        let peak_l = MeterState::take_peak(&self.meters.master_l);
        let peak_r = MeterState::take_peak(&self.meters.master_r);
        if peak_l > self.master_l_display { self.master_l_display = peak_l; } else { self.master_l_display *= decay; }
        if peak_r > self.master_r_display { self.master_r_display = peak_r; } else { self.master_r_display *= decay; }

        let mon_peak = MeterState::take_peak(&self.meters.monitor_peak);
        if mon_peak > self.monitor_display {
            self.monitor_display = mon_peak;
        } else {
            self.monitor_display *= decay;
            if self.monitor_display < 0.001 { self.monitor_display = 0.0; }
        }
    }

    fn send_master_mute_led(&self) {
        let state = match self.params.master_fader_target_mode() {
            FaderTarget::Monitor => self.params.monitor_mute.load(Ordering::Relaxed),
            FaderTarget::Bus(b)  => self.params.bus_mute[b].load(Ordering::Relaxed),
            FaderTarget::Master  => self.params.master_mute.load(Ordering::Relaxed),
        };
        let _ = self.midi_tx.send(vec![0x90, NOTE_MASTER_MUTE, if state {127} else {0}]);
    }

    fn toggle_aux_mode(&mut self, bus: usize) {
        let current_mode = self.params.send_on_fader_mode.load(Ordering::Relaxed);
        let current_bus = self.params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);
        if current_mode && current_bus == bus {
            self.params.send_on_fader_mode.store(false, Ordering::Relaxed);
        } else {
            self.params.monitor_mode.store(false, Ordering::Relaxed);
            self.params.send_on_fader_mode.store(true, Ordering::Relaxed);
            self.params.current_bus.store(bus, Ordering::Relaxed);
            for i in 0..NUM_STRIPS {
                self.params.vu_mode[i].store(false, Ordering::Relaxed);
            }
        }
        self.send_bank_state();
        self.send_master_mute_led();
    }

    fn toggle_monitor_mode(&mut self) {
        let entering = !self.params.monitor_mode.load(Ordering::Relaxed);
        self.params.monitor_mode.store(entering, Ordering::Relaxed);
        if entering {
            self.params.send_on_fader_mode.store(false, Ordering::Relaxed);
        }
        self.send_bank_state();
        self.send_master_mute_led();
    }

    fn draw_channel_strip(&mut self, ui: &mut egui::Ui, physical: usize) {
        let bank = self.params.bank_offset.load(Ordering::Relaxed);
        let logical = bank * NUM_PHYSICAL_FADERS + physical;
        if logical >= NUM_STRIPS { return; }

        let gain = MixerParameters::get_f32(&self.params.gain[logical]);
        let pan = MixerParameters::get_f32(&self.params.pan[logical]);
        let mute = self.params.mute[logical].load(Ordering::Relaxed);
        let solo = self.params.solo[logical].load(Ordering::Relaxed);
        let select = self.params.select[logical].load(Ordering::Relaxed);
        let send_mode = self.params.send_on_fader_mode.load(Ordering::Relaxed);
        let current_bus = self.params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);

        ui.allocate_ui(egui::vec2(62.0, 280.0), |ui| {
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new(format!("CH {}", logical + 1)).strong().size(11.0));

                let mut pan_val = pan;
                if draw_knob(ui, &mut pan_val, -1.0..=1.0, 24.0, 0.01, 0.0025).changed() {
                    MixerParameters::set_f32(&self.params.pan[logical], pan_val);
                }
                ui.add_space(4.0);

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 2.0;

                    if ui.add_sized([18.0, 18.0], egui::Button::new(egui::RichText::new("M").size(9.0)).fill(if mute { egui::Color32::RED } else { egui::Color32::DARK_GRAY })).clicked() {
                        let new_val = !mute;
                        if select {
                            let bank_start = bank * NUM_PHYSICAL_FADERS;
                            let bank_end = bank_start + NUM_PHYSICAL_FADERS;
                            for i in 0..NUM_STRIPS {
                                if self.params.select[i].load(Ordering::Relaxed) {
                                    self.params.mute[i].store(new_val, Ordering::Relaxed);
                                    if i >= bank_start && i < bank_end {
                                        let _ = self.midi_tx.send(vec![0x90, 0x10 + (i - bank_start) as u8, if new_val {127} else {0}]);
                                    }
                                }
                            }
                        } else {
                            self.params.mute[logical].store(new_val, Ordering::Relaxed);
                            let _ = self.midi_tx.send(vec![0x90, 0x10 + physical as u8, if new_val {127} else {0}]);
                        }
                    }

                    if ui.add_sized([18.0, 18.0], egui::Button::new(egui::RichText::new("S").size(9.0)).fill(if solo { egui::Color32::YELLOW } else { egui::Color32::DARK_GRAY })).clicked() {
                        let new_val = !solo;
                        if select {
                            let bank_start = bank * NUM_PHYSICAL_FADERS;
                            let bank_end = bank_start + NUM_PHYSICAL_FADERS;
                            for i in 0..NUM_STRIPS {
                                if self.params.select[i].load(Ordering::Relaxed) {
                                    self.params.solo[i].store(new_val, Ordering::Relaxed);
                                    if i >= bank_start && i < bank_end {
                                        let _ = self.midi_tx.send(vec![0x90, 0x08 + (i - bank_start) as u8, if new_val {127} else {0}]);
                                    }
                                }
                            }
                        } else {
                            self.params.solo[logical].store(new_val, Ordering::Relaxed);
                            let _ = self.midi_tx.send(vec![0x90, 0x08 + physical as u8, if new_val {127} else {0}]);
                        }
                    }

                    if ui.add_sized([22.0, 18.0], egui::Button::new(egui::RichText::new("Sel").size(9.0)).fill(if select { egui::Color32::LIGHT_BLUE } else { egui::Color32::DARK_GRAY })).clicked() {
                        let is_multi = ui.input(|i| i.modifiers.shift || i.modifiers.mac_cmd || i.modifiers.ctrl);
                        if is_multi {
                            let new_state = !select;
                            self.params.select[logical].store(new_state, Ordering::Relaxed);
                            if new_state { self.params.selected_strip.store(logical, Ordering::Relaxed); }
                        } else {
                            for i in 0..NUM_STRIPS { self.params.select[i].store(i == logical, Ordering::Relaxed); }
                            self.params.selected_strip.store(logical, Ordering::Relaxed);
                        }
                        for phys in 0..NUM_PHYSICAL_FADERS {
                            let l = bank * NUM_PHYSICAL_FADERS + phys;
                            let s = self.params.select[l].load(Ordering::Relaxed);
                            let _ = self.midi_tx.send(vec![0x90, 0x18 + phys as u8, if s {127} else {0}]);
                        }
                    }
                });

                ui.add_space(6.0);

                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let send_value = if send_mode {
                        MixerParameters::get_f32(&self.params.send_levels[logical * NUM_BUSES + current_bus])
                    } else {
                        gain
                    };
                    let mut slider_val = send_value;

                    ui.allocate_ui(egui::vec2(22.0, 180.0), |ui| {
                        let fader = egui::Slider::new(&mut slider_val, 0.0..=1.0).vertical().show_value(false);
                        if ui.add_sized([22.0, 180.0], fader).changed() {
                            if send_mode {
                                MixerParameters::set_f32(
                                    &self.params.send_levels[logical * NUM_BUSES + current_bus],
                                    slider_val,
                                );
                                let pb_value = (slider_val * 16383.0) as u16;
                                let _ = self.midi_tx.send(vec![0xE0 | physical as u8, (pb_value & 0x7F) as u8, (pb_value >> 7) as u8]);
                            } else {
                                let delta = slider_val - gain;
                                if select {
                                    let bank_start = bank * NUM_PHYSICAL_FADERS;
                                    let bank_end = bank_start + NUM_PHYSICAL_FADERS;
                                    for i in 0..NUM_STRIPS {
                                        if self.params.select[i].load(Ordering::Relaxed) {
                                            let mut g = MixerParameters::get_f32(&self.params.gain[i]);
                                            g = (g + delta).clamp(0.0, 1.0);
                                            MixerParameters::set_f32(&self.params.gain[i], g);
                                            if i >= bank_start && i < bank_end {
                                                let phys = (i - bank_start) as u8;
                                                let pb_value = (g * 16383.0) as u16;
                                                let _ = self.midi_tx.send(vec![0xE0 | phys, (pb_value & 0x7F) as u8, (pb_value >> 7) as u8]);
                                            }
                                        }
                                    }
                                } else {
                                    MixerParameters::set_f32(&self.params.gain[logical], slider_val);
                                    let pb_value = (slider_val * 16383.0) as u16;
                                    let _ = self.midi_tx.send(vec![0xE0 | physical as u8, (pb_value & 0x7F) as u8, (pb_value >> 7) as u8]);
                                }
                            }
                        }
                    });

                    let meter_height = 180.0;
                    let meter_width = 6.0;
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(meter_width, meter_height), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 1.0, egui::Color32::from_gray(30));

                    let level = self.strip_display[logical].clamp(0.0, 1.0);
                    if level > 0.001 {
                        let fill_height = meter_height * level;
                        let mut fill_rect = rect;
                        fill_rect.min.y = fill_rect.max.y - fill_height;
                        let color = if level > 0.9 { egui::Color32::RED } else if level > 0.7 { egui::Color32::YELLOW } else { egui::Color32::GREEN };
                        ui.painter().rect_filled(fill_rect, 1.0, color);
                    }
                });
            });
        });
    }

    fn draw_64ch_meter_bridge(&mut self, ui: &mut egui::Ui) {
        let active_bank = self.params.bank_offset.load(Ordering::Relaxed);
        let (bridge_rect, _) = ui.allocate_exact_size(egui::vec2(480.0, 310.0), egui::Sense::hover());

        ui.painter().rect_filled(bridge_rect, 6.0, egui::Color32::from_gray(18));
        ui.painter().rect_stroke(bridge_rect, 6.0, egui::Stroke::new(1.5_f32, egui::Color32::from_gray(50)));

        let title_pos = egui::pos2(bridge_rect.center().x, bridge_rect.min.y + 18.0);
        ui.painter().text(
            title_pos,
            egui::Align2::CENTER_CENTER,
            "64-CHANNEL MASTER METER BRIDGE",
            egui::FontId::proportional(14.0),
            egui::Color32::GOLD,
        );

        let margin_left = 14.0;
        let start_x = bridge_rect.min.x + margin_left;
        let start_y = bridge_rect.min.y + 36.0;
        let col_width = 54.0;
        let col_gap = 3.0;
        let col_height = 240.0;
        let segments_count = 15;
        let seg_height = 12.0;
        let seg_gap = 1.5;
        let bar_width = 5.0;

        for bank_idx in 0..NUM_BANKS {
            let is_current_bank = bank_idx == active_bank;
            let col_x = start_x + (bank_idx as f32 * (col_width + col_gap));
            let col_rect = egui::Rect::from_min_size(
                egui::pos2(col_x, start_y),
                egui::vec2(col_width, col_height),
            );
            let bg_color = if is_current_bank { egui::Color32::from_rgb(16, 46, 20) } else { egui::Color32::from_gray(24) };
            let border_stroke = if is_current_bank { egui::Stroke::new(2.0_f32, egui::Color32::GREEN) } else { egui::Stroke::new(1.0_f32, egui::Color32::from_gray(45)) };
            ui.painter().rect_filled(col_rect, 4.0, bg_color);
            ui.painter().rect_stroke(col_rect, 4.0, border_stroke);

            let header_pos = egui::pos2(col_rect.center().x, col_rect.min.y + 12.0);
            let text_color = if is_current_bank { egui::Color32::GOLD } else { egui::Color32::LIGHT_GRAY };
            ui.painter().text(
                header_pos,
                egui::Align2::CENTER_CENTER,
                format!("BANK {}", bank_idx + 1),
                egui::FontId::proportional(10.0),
                text_color,
            );

            let meter_start_x = col_rect.min.x + 3.0;
            let meter_start_y = col_rect.min.y + 30.0;

            for ch in 0..NUM_PHYSICAL_FADERS {
                let logical_ch = bank_idx * NUM_PHYSICAL_FADERS + ch;
                let level = self.strip_display[logical_ch].clamp(0.0, 1.0);
                let active_segs = (level * segments_count as f32).round() as usize;
                let bar_x = meter_start_x + (ch as f32 * (bar_width + 1.0));

                for seg in 0..segments_count {
                    let seg_inv = (segments_count - 1) - seg;
                    let seg_y = meter_start_y + (seg_inv as f32 * (seg_height + seg_gap));
                    let seg_rect = egui::Rect::from_min_size(egui::pos2(bar_x, seg_y), egui::vec2(bar_width, seg_height));
                    let is_on = seg < active_segs;
                    let seg_color = if !is_on {
                        egui::Color32::from_gray(38)
                    } else if seg >= 12 {
                        egui::Color32::RED
                    } else if seg >= 9 {
                        egui::Color32::YELLOW
                    } else {
                        egui::Color32::GREEN
                    };
                    ui.painter().rect_filled(seg_rect, 0.5, seg_color);
                }
            }
        }
    }

    fn draw_input_section(&mut self, ui: &mut egui::Ui) {
        let bank = self.params.bank_offset.load(Ordering::Relaxed);
        let send_mode = self.params.send_on_fader_mode.load(Ordering::Relaxed);
        let monitor_mode = self.params.monitor_mode.load(Ordering::Relaxed);
        let current_bus = self.params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);

        let panel_size = egui::vec2(560.0, 300.0);
        let (rect, _) = ui.allocate_exact_size(panel_size, egui::Sense::hover());

        let (bg_color, border) = if monitor_mode {
            (egui::Color32::from_rgb(15, 35, 45), egui::Stroke::new(3.0_f32, CYAN))
        } else if send_mode {
            (egui::Color32::from_rgb(45, 38, 10), egui::Stroke::new(3.0_f32, egui::Color32::YELLOW))
        } else {
            (egui::Color32::from_gray(18), egui::Stroke::new(1.5_f32, egui::Color32::from_gray(50)))
        };
        ui.painter().rect_filled(rect, 6.0, bg_color);
        ui.painter().rect_stroke(rect, 6.0, border);

        let title_pos = egui::pos2(rect.min.x + 10.0, rect.min.y + 10.0);
        let (title, title_size, title_color) = if monitor_mode {
            (
                "🔊 MONITOR MODE — MASTER FADER CONTROLS MONITOR".to_string(),
                14.0,
                CYAN,
            )
        } else if send_mode {
            (
                format!("🔊 AUX MODE — BUS {} — SEND ON FADERS", current_bus + 1),
                14.0,
                egui::Color32::YELLOW,
            )
        } else {
            (
                format!("INPUT CHANNELS (BANK {})", bank + 1),
                12.0,
                egui::Color32::LIGHT_GREEN,
            )
        };
        ui.painter().text(
            title_pos,
            egui::Align2::LEFT_TOP,
            title,
            egui::FontId::proportional(title_size),
            title_color,
        );

        let content_rect = rect.shrink(10.0).translate(egui::vec2(0.0, 18.0));
        ui.allocate_ui_at_rect(content_rect, |ui| {
            ui.horizontal_centered(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                for physical in 0..NUM_PHYSICAL_FADERS {
                    self.draw_channel_strip(ui, physical);
                    if physical < NUM_PHYSICAL_FADERS - 1 {
                        let (line_rect, _) = ui.allocate_exact_size(egui::vec2(1.0, 280.0), egui::Sense::hover());
                        let line_color = if monitor_mode {
                            egui::Color32::from_rgb(30, 80, 100)
                        } else if send_mode {
                            egui::Color32::from_rgb(120, 100, 20)
                        } else {
                            egui::Color32::from_gray(70)
                        };
                        ui.painter().vline(
                            line_rect.center().x,
                            line_rect.y_range(),
                            egui::Stroke::new(1.0_f32, line_color),
                        );
                    }
                }
            });
        });
    }

    fn draw_bus_controls(&mut self, ui: &mut egui::Ui) {
        let send_mode = self.params.send_on_fader_mode.load(Ordering::Relaxed);
        let monitor_mode = self.params.monitor_mode.load(Ordering::Relaxed);
        let current_bus = self.params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);

        ui.horizontal_centered(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;

            for bus in 0..NUM_BUSES {
                let is_aux = send_mode && !monitor_mode && bus == current_bus;
                let muted = self.params.bus_mute[bus].load(Ordering::Relaxed);
                let is_pre = self.params.bus_pre_post[bus].load(Ordering::Relaxed);

                ui.allocate_ui(egui::vec2(44.0, 112.0), |ui| {
                    ui.vertical_centered(|ui| {
                        let label_fill = if is_aux {
                            egui::Color32::from_rgb(120, 100, 20)
                        } else {
                            egui::Color32::DARK_GRAY
                        };
                        if ui
                            .add_sized(
                                [40.0, 20.0],
                                egui::Button::new(
                                    egui::RichText::new(format!("B{}", bus + 1))
                                        .size(11.0)
                                        .color(if is_aux { egui::Color32::YELLOW } else { egui::Color32::LIGHT_GRAY }),
                                )
                                .fill(label_fill),
                            )
                            .clicked()
                        {
                            self.toggle_aux_mode(bus);
                        }

                        if ui
                            .add_sized(
                                [40.0, 18.0],
                                egui::Button::new(egui::RichText::new("MUTE").size(8.0))
                                    .fill(if muted { egui::Color32::RED } else { egui::Color32::DARK_GRAY }),
                            )
                            .clicked()
                        {
                            let new_val = !muted;
                            self.params.bus_mute[bus].store(new_val, Ordering::Relaxed);
                            if is_aux {
                                self.send_master_mute_led();
                            }
                        }

                        if ui
                            .add_sized(
                                [40.0, 18.0],
                                egui::Button::new(egui::RichText::new(if is_pre { "PRE" } else { "PST" }).size(8.0))
                                    .fill(if is_pre { egui::Color32::from_rgb(40, 90, 140) } else { egui::Color32::DARK_GRAY }),
                            )
                            .clicked()
                        {
                            self.params.bus_pre_post[bus].store(!is_pre, Ordering::Relaxed);
                        }

                        let meter_h = 40.0;
                        let meter_w = 8.0;
                        let (meter_rect, _) = ui.allocate_exact_size(
                            egui::vec2(meter_w, meter_h),
                            egui::Sense::hover(),
                        );
                        ui.painter().rect_filled(meter_rect, 1.0, egui::Color32::from_gray(30));
                        let level = self.bus_display[bus].clamp(0.0, 1.0);
                        if level > 0.001 {
                            let fill_h = meter_h * level;
                            let mut fill_rect = meter_rect;
                            fill_rect.min.y = fill_rect.max.y - fill_h;
                            let color = if muted {
                                egui::Color32::from_gray(90)
                            } else if level > 0.9 {
                                egui::Color32::RED
                            } else if level > 0.7 {
                                egui::Color32::YELLOW
                            } else {
                                egui::Color32::GREEN
                            };
                            ui.painter().rect_filled(fill_rect, 1.0, color);
                        }
                    });
                });
            }

            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);

            // Contextual master column (mute + knob + value + optional monitor sub-meter).
            let target = self.params.master_fader_target_mode();
            let (mute_state, mute_label, label_text, label_color) = match target {
                FaderTarget::Monitor => (
                    self.params.monitor_mute.load(Ordering::Relaxed),
                    "MUTE MON".to_string(),
                    "MONITOR".to_string(),
                    CYAN,
                ),
                FaderTarget::Bus(b) => (
                    self.params.bus_mute[b].load(Ordering::Relaxed),
                    format!("MUTE B{}", b + 1),
                    format!("B{} MSTR", b + 1),
                    egui::Color32::YELLOW,
                ),
                FaderTarget::Master => (
                    self.params.master_mute.load(Ordering::Relaxed),
                    "MUTE MST".to_string(),
                    "MASTER".to_string(),
                    egui::Color32::GRAY,
                ),
            };

            ui.allocate_ui(egui::vec2(80.0, 112.0), |ui| {
                ui.vertical_centered(|ui| {
                    if ui
                        .add_sized(
                            [76.0, 24.0],
                            egui::Button::new(egui::RichText::new(&mute_label).size(9.0))
                                .fill(if mute_state { egui::Color32::RED } else { egui::Color32::DARK_GRAY }),
                        )
                        .clicked()
                    {
                        match target {
                            FaderTarget::Monitor => {
                                let v = !self.params.monitor_mute.load(Ordering::Relaxed);
                                self.params.monitor_mute.store(v, Ordering::Relaxed);
                            }
                            FaderTarget::Bus(b) => {
                                let v = !self.params.bus_mute[b].load(Ordering::Relaxed);
                                self.params.bus_mute[b].store(v, Ordering::Relaxed);
                            }
                            FaderTarget::Master => {
                                let v = !self.params.master_mute.load(Ordering::Relaxed);
                                self.params.master_mute.store(v, Ordering::Relaxed);
                            }
                        }
                        self.send_master_mute_led();
                    }
                    ui.add_space(4.0);

                    let start_val = match target {
                        FaderTarget::Monitor => MixerParameters::get_f32(&self.params.monitor_gain),
                        FaderTarget::Bus(b)  => MixerParameters::get_f32(&self.params.bus_gain[b]),
                        FaderTarget::Master  => MixerParameters::get_f32(&self.params.master_gain),
                    };
                    let mut gain = start_val;
                    ui.label(egui::RichText::new(&label_text).size(8.0).color(label_color));
                    if draw_knob(ui, &mut gain, 0.0..=1.0, 32.0, 0.01, 0.005).changed() {
                        match target {
                            FaderTarget::Monitor => MixerParameters::set_f32(&self.params.monitor_gain, gain),
                            FaderTarget::Bus(b)  => MixerParameters::set_f32(&self.params.bus_gain[b], gain),
                            FaderTarget::Master  => MixerParameters::set_f32(&self.params.master_gain, gain),
                        }
                        let pb_value = (gain * 16383.0) as u16;
                        let _ = self.midi_tx
                            .send(vec![0xE8, (pb_value & 0x7F) as u8, (pb_value >> 7) as u8]);
                    }
                    ui.label(egui::RichText::new(format!("{}", (gain * 100.0).round() as i32)).size(9.0));

                    if matches!(target, FaderTarget::Monitor) {
                        let meter_h = 14.0;
                        let meter_w = 8.0;
                        let (mr, _) = ui.allocate_exact_size(egui::vec2(meter_w, meter_h), egui::Sense::hover());
                        ui.painter().rect_filled(mr, 1.0, egui::Color32::from_gray(30));
                        let level = self.monitor_display.clamp(0.0, 1.0);
                        if level > 0.001 {
                            let mut fr = mr;
                            fr.min.y = fr.max.y - meter_h * level;
                            let color = if level > 0.9 { egui::Color32::RED }
                                        else if level > 0.7 { egui::Color32::YELLOW }
                                        else { CYAN };
                            ui.painter().rect_filled(fr, 1.0, color);
                        }
                    }
                });
            });

            ui.add_space(2.0);
            ui.separator();
            ui.add_space(2.0);

            // Master stereo L/R meter — always shows master_out_* peaks,
            // independent of which mode the master fader is currently driving.
            ui.allocate_ui(egui::vec2(34.0, 112.0), |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(egui::RichText::new("MST").size(9.0).strong().color(egui::Color32::LIGHT_GRAY));
                    ui.add_space(2.0);

                    let bar_h = 80.0;
                    let bar_w = 12.0;
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 3.0;

                        // Left channel
                        let (l_rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
                        ui.painter().rect_filled(l_rect, 1.0, egui::Color32::from_gray(30));
                        let lv = self.master_l_display.clamp(0.0, 1.0);
                        if lv > 0.001 {
                            let mut f = l_rect;
                            f.min.y = f.max.y - bar_h * lv;
                            let c = if lv > 0.9 { egui::Color32::RED }
                                    else if lv > 0.7 { egui::Color32::YELLOW }
                                    else { egui::Color32::GREEN };
                            ui.painter().rect_filled(f, 1.0, c);
                        }

                        // Right channel
                        let (r_rect, _) = ui.allocate_exact_size(egui::vec2(bar_w, bar_h), egui::Sense::hover());
                        ui.painter().rect_filled(r_rect, 1.0, egui::Color32::from_gray(30));
                        let rv = self.master_r_display.clamp(0.0, 1.0);
                        if rv > 0.001 {
                            let mut f = r_rect;
                            f.min.y = f.max.y - bar_h * rv;
                            let c = if rv > 0.9 { egui::Color32::RED }
                                    else if rv > 0.7 { egui::Color32::YELLOW }
                                    else { egui::Color32::GREEN };
                            ui.painter().rect_filled(f, 1.0, c);
                        }
                    });

                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        ui.allocate_ui(egui::vec2(bar_w, 10.0), |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(egui::RichText::new("L").size(7.0).color(egui::Color32::GRAY));
                            });
                        });
                        ui.add_space(3.0);
                        ui.allocate_ui(egui::vec2(bar_w, 10.0), |ui| {
                            ui.vertical_centered(|ui| {
                                ui.label(egui::RichText::new("R").size(7.0).color(egui::Color32::GRAY));
                            });
                        });
                    });
                });
            });
        });
    }

    fn draw_bus_panel(&mut self, ui: &mut egui::Ui) {
        let send_mode = self.params.send_on_fader_mode.load(Ordering::Relaxed);
        let monitor_mode = self.params.monitor_mode.load(Ordering::Relaxed);
        let current_bus = self.params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);

        let panel_size = egui::vec2(560.0, 160.0);
        let (rect, _) = ui.allocate_exact_size(panel_size, egui::Sense::hover());

        let (bg_color, border, title, title_color) = if monitor_mode {
            (
                egui::Color32::from_rgb(15, 35, 45),
                egui::Stroke::new(2.0_f32, CYAN),
                "BUS CONTROLS — MONITOR MODE".to_string(),
                CYAN,
            )
        } else if send_mode {
            (
                egui::Color32::from_rgb(35, 30, 10),
                egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(180, 150, 30)),
                format!("BUS CONTROLS — AUX BUS {} ACTIVE", current_bus + 1),
                egui::Color32::YELLOW,
            )
        } else {
            (
                egui::Color32::from_gray(18),
                egui::Stroke::new(1.5_f32, egui::Color32::from_gray(50)),
                "BUS CONTROLS".to_string(),
                egui::Color32::LIGHT_BLUE,
            )
        };

        ui.painter().rect_filled(rect, 6.0, bg_color);
        ui.painter().rect_stroke(rect, 6.0, border);

        let title_pos = egui::pos2(rect.min.x + 10.0, rect.min.y + 8.0);
        ui.painter().text(
            title_pos,
            egui::Align2::LEFT_TOP,
            title,
            egui::FontId::proportional(11.0),
            title_color,
        );

        let content_rect = rect.shrink(10.0).translate(egui::vec2(0.0, 20.0));
        ui.allocate_ui_at_rect(content_rect, |ui| {
            self.draw_bus_controls(ui);
        });
    }

    fn draw_bank_navigation(&mut self, ui: &mut egui::Ui) {
        let current_bank = self.params.bank_offset.load(Ordering::Relaxed);
        let send_mode = self.params.send_on_fader_mode.load(Ordering::Relaxed);
        let monitor_mode = self.params.monitor_mode.load(Ordering::Relaxed);
        let current_bus = self.params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);

        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(egui::RichText::new("BANK:").strong().size(12.0));
            ui.add_space(8.0);
            for i in 0..NUM_BANKS {
                let label = format!("{}", i + 1);
                let is_active = i == current_bank;
                if ui.add_sized([35.0, 24.0], egui::Button::new(label).fill(if is_active { egui::Color32::from_rgb(50, 150, 50) } else { egui::Color32::DARK_GRAY })).clicked() {
                    self.params.bank_offset.store(i, Ordering::Relaxed);
                    self.send_bank_state();
                }
            }

            ui.add_space(20.0);

            if send_mode {
                if ui
                    .add_sized(
                        [180.0, 24.0],
                        egui::Button::new(
                            egui::RichText::new(format!("AUX: BUS {} — EXIT", current_bus + 1))
                                .size(11.0)
                                .strong()
                                .color(egui::Color32::YELLOW),
                        )
                        .fill(egui::Color32::from_rgb(90, 75, 15)),
                    )
                    .clicked()
                {
                    self.toggle_aux_mode(current_bus);
                }
            }

            ui.add_space(20.0);

            let monitor_btn = egui::Button::new(
                egui::RichText::new(if monitor_mode { "■ MONITOR — ON" } else { "Monitor Mode" })
                    .size(11.0)
                    .strong()
                    .color(if monitor_mode { CYAN } else { egui::Color32::LIGHT_GRAY }),
            )
            .fill(if monitor_mode {
                egui::Color32::from_rgb(20, 70, 80)
            } else {
                egui::Color32::DARK_GRAY
            });
            if ui.add_sized([140.0, 24.0], monitor_btn).clicked() {
                self.toggle_monitor_mode();
            }

            ui.add_space(20.0);

            if ui.add_sized([90.0, 24.0], egui::Button::new("Save State")).clicked() {
                if let Err(e) = self.params.save_to_file("mixer_state.json") { eprintln!("Save error: {}", e); }
            }
            if ui.add_sized([90.0, 24.0], egui::Button::new("Load State")).clicked() {
                if let Err(e) = self.params.load_from_file("mixer_state.json") { eprintln!("Load error: {}", e); } else { self.send_bank_state(); }
            }
        });
    }

    fn send_bank_state(&self) {
        let bank = self.params.bank_offset.load(Ordering::Relaxed);
        let send_mode = self.params.send_on_fader_mode.load(Ordering::Relaxed);
        let current_bus = self.params.current_bus.load(Ordering::Relaxed).min(NUM_BUSES - 1);

        for physical in 0..NUM_PHYSICAL_FADERS {
            let logical = bank * NUM_PHYSICAL_FADERS + physical;
            if logical < NUM_STRIPS {
                let value = if send_mode {
                    MixerParameters::get_f32(&self.params.send_levels[logical * NUM_BUSES + current_bus])
                } else {
                    MixerParameters::get_f32(&self.params.gain[logical])
                };
                let pb_value = (value * 16383.0) as u16;
                let _ = self.midi_tx.send(vec![0xE0 | physical as u8, (pb_value & 0x7F) as u8, (pb_value >> 7) as u8]);
            }
        }
        for physical in 0..NUM_PHYSICAL_FADERS {
            let logical = bank * NUM_PHYSICAL_FADERS + physical;
            if logical < NUM_STRIPS {
                let mute = self.params.mute[logical].load(Ordering::Relaxed);
                let solo = self.params.solo[logical].load(Ordering::Relaxed);
                let select = self.params.select[logical].load(Ordering::Relaxed);
                let _ = self.midi_tx.send(vec![0x90, 0x10 + physical as u8, if mute {127} else {0}]);
                let _ = self.midi_tx.send(vec![0x90, 0x08 + physical as u8, if solo {127} else {0}]);
                let _ = self.midi_tx.send(vec![0x90, 0x18 + physical as u8, if select {127} else {0}]);
            }
        }
        for i in 0..NUM_BANKS {
            let _ = self.midi_tx.send(vec![0x90, 0x00 + i as u8, if i == bank {127} else {0}]);
        }
        for (i, &note) in AUX_MODE_NOTES.iter().enumerate() {
            let lit = send_mode && current_bus == i;
            let _ = self.midi_tx.send(vec![0x90, note, if lit {127} else {0}]);
        }

        for physical in 0..NUM_PHYSICAL_FADERS {
            let logical = bank * NUM_PHYSICAL_FADERS + physical;
            let vu = self.params.vu_mode[logical].load(Ordering::Relaxed);
            if vu { continue; }
            let pan = MixerParameters::get_f32(&self.params.pan[logical]);
            let v = ((pan + 1.0) / 2.0 * 11.0).round() as u8;
            let msg_val = 0x10 | v.min(11);
            let _ = self.midi_tx.send(vec![0xB0, 0x30 + physical as u8, msg_val]);
        }

        let master_target = match self.params.master_fader_target_mode() {
            FaderTarget::Monitor => MixerParameters::get_f32(&self.params.monitor_gain),
            FaderTarget::Bus(b)  => MixerParameters::get_f32(&self.params.bus_gain[b]),
            FaderTarget::Master  => MixerParameters::get_f32(&self.params.master_gain),
        };
        let pb_value = (master_target * 16383.0) as u16;
        let _ = self.midi_tx.send(vec![0xE8, (pb_value & 0x7F) as u8, (pb_value >> 7) as u8]);
    }
}

impl eframe::App for MixerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.update_meter_displays();

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.allocate_ui(egui::vec2(ui.available_width(), 35.0), |ui| {
                        self.draw_bank_navigation(ui);
                    });

                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 24.0;

                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 4.0;
                            self.draw_input_section(ui);
                            self.draw_bus_panel(ui);
                        });

                        ui.vertical(|ui| {
                            self.draw_64ch_meter_bridge(ui);
                        });
                    });
                });
        });

        ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Err(e) = self.params.save_to_file("mixer_state.json") {
            eprintln!("Auto‑save failed: {}", e);
        }
    }
}

pub fn run_gui(
    params: Arc<MixerParameters>,
    meters: Arc<MeterState>,
    midi_tx: Sender<Vec<u8>>,
) -> Result<(), eframe::Error> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 620.0]),
        ..Default::default()
    };
    eframe::run_native(
        "X-Touch Mixer 64",
        options,
        Box::new(|_cc| Box::new(MixerApp::new(params, meters, midi_tx))),
    )
}
