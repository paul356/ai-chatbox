use anyhow;
use esp_idf_svc::hal::{
    peripherals::Peripherals,
};
use esp_idf_svc::sys;
use std::time::Instant;

mod audio_device;
mod audio_processing;
mod http_client;
mod llm_intf;
mod sd_card;
mod speech_recognition;
mod transcription;
mod tts;
mod wifi;

use audio_device::{configure_max98357_pins, init_i2s_tx};
use audio_processing::{create_feed_task, create_fetch_task};
use speech_recognition::init_speech_recognition;
use transcription::start_transcription_worker;
use wifi::initialize_wifi;

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    log::info!("Starting AI Chatbox application");

    // Create a performance timer to measure initialization time
    let init_timer = Instant::now();

    // Take peripherals once at the beginning
    let peripherals = match Peripherals::take() {
        Ok(p) => p,
        Err(e) => {
            log::error!("Failed to take peripherals: {}", e);
            return Err(anyhow::anyhow!("Failed to take peripherals: {}", e));
        }
    };

    // Connect to Wi-Fi and store the wifi object to maintain ownership throughout the program's lifetime
    let _wifi = match initialize_wifi(peripherals.modem) {
        Ok(wifi) => {
            log::info!("WiFi connected successfully");
            wifi
        }
        Err(e) => {
            log::error!("Failed to connect to WiFi: {}", e);
            return Err(e);
        }
    };

    // Configure MAX98357 control pins first
    let sd_pin_driver = configure_max98357_pins(peripherals.pins.gpio5)?;

    // Initialize I2S TX driver for audio output
    let i2s_tx_driver = init_i2s_tx(
        peripherals.i2s1,
        peripherals.pins.gpio2,
        peripherals.pins.gpio3,
        peripherals.pins.gpio1,
    )?;

    log::info!("I2S TX channel configured for audio output");

    // Test the LLM helper
    /*match test_llm_helper() {
        Ok(_) => log::info!("LLM test completed successfully"),
        Err(e) => log::error!("LLM test failed: {}", e),
    }*/

    // Mount SD card with proper error handling
    let mut sd = sd_card::SdCard::new("/vfat");
    if let Err(e) = sd.mount_spi() {
        log::error!("Failed to mount SD card: {}", e);
        return Err(anyhow::anyhow!("Failed to mount SD card: {}", e));
    }

    // Initialize speech recognition system
    let (afe_handle, afe_data, multinet, model_data) = init_speech_recognition()?;

    // Start the transcription worker thread
    let (transcription_tx, transcription_response_rx) = match start_transcription_worker(i2s_tx_driver, sd_pin_driver) {
        Ok((tx, rx)) => (tx, rx),
        Err(e) => {
            log::error!("Failed to start transcription worker: {}", e);
            return Err(anyhow::anyhow!(
                "Failed to start transcription worker: {}",
                e
            ));
        }
    };
    log::info!("Transcription worker started successfully");

    // Create the feed task
    let _feed_task = create_feed_task(
        afe_handle,
        afe_data,
        peripherals.i2s0,
        peripherals.pins.gpio42,
        peripherals.pins.gpio41,
    )?;

    // Create the fetch task
    let _fetch_task = create_fetch_task(
        afe_handle,
        afe_data,
        multinet,
        model_data,
        transcription_tx,
        transcription_response_rx,
    )?;

    // Log initialization time
    log::info!(
        "AI Chatbox initialization completed in {} ms",
        init_timer.elapsed().as_millis()
    );

    // Simple infinite loop for embedded application - this is standard practice
    // for embedded applications where the main thread can just sleep
    log::info!("Entering main loop");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
