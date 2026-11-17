mod parameters;
mod meters;
mod audio_engine;
mod midi_control;
mod gui;

use std::error::Error;
use log::info;

fn main() -> Result<(), Box<dyn Error>> {
    env_logger::init();

    let params = parameters::MixerParameters::new();
    let meters = meters::MeterState::new();

    if std::path::Path::new("mixer_state.json").exists() {
        if let Err(e) = params.load_from_file("mixer_state.json") {
            eprintln!("Failed to load previous state: {}", e);
        }
    }

    info!("Starting JACK audio engine...");
    let _jack_client = audio_engine::init_jack(params.clone(), meters.clone())?;

    info!("Starting MIDI controller...");
    let midi = midi_control::MidiController::new(params.clone(), meters.clone())?;
    let midi_tx = midi.get_sender();

    info!("Starting GUI...");
    gui::run_gui(params, meters, midi_tx)?;

    Ok(())
}
