use anyhow;
use esp_idf_svc::hal::{
    gpio::OutputPin,
    gpio::PinDriver,
    i2s::{I2sDriver, I2sTx},
};
use esp_idf_svc::http::client::{Configuration as HttpConfiguration, EspHttpConnection};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use crate::http_client::{read_response, send_multipart_request};
use crate::llm_intf::{ChatRole, LlmHelper};
use crate::tts::{TtsConfig, TtsEngine};

/// Define message types for the transcription thread
#[derive(Debug)]
pub enum TranscriptionMessage {
    TranscribeFile { path: String },
    RestartSession,
    Shutdown,
}

/// Worker function for the transcription thread
fn transcription_worker(
    rx: Receiver<TranscriptionMessage>,
    response_tx: Sender<String>,
    mut i2s_driver: I2sDriver<'static, I2sTx>,
    mut sd_pin_driver: PinDriver<'static, impl OutputPin, esp_idf_svc::hal::gpio::Output>,
) -> anyhow::Result<()> {
    log::info!("Transcription worker thread started");

    // Get token from environment variable at compile time
    let token = env!("LLM_AUTH_TOKEN");

    // Create and configure the LLM helper
    let mut llm = match LlmHelper::new(token, "deepseek-chat") {
        helper => {
            log::info!("LLM helper created successfully");
            helper
        }
    };

    // Configure with parameters suitable for embedded device
    llm.configure(
        Some(512), // Max tokens to generate in response
        Some(0.7), // Temperature - balanced between deterministic and creative
        Some(0.9), // Top-p - slightly more focused sampling
    );

    // Send initial system message to set context
    llm.send_message(
        "接下来的请求来自一个语音转文字服务，请小心中间可能有一些字词被识别成同音的字词。请不要使用列表，回答保持一个段落。"
            .to_string(),
        ChatRole::System,
    );

    log::info!("LLM helper initialized with system prompt");

    // Initialize TTS engine
    let mut tts_engine = match TtsEngine::new_with_config(TtsConfig {
        max_chunk_chars: 30, // Smaller chunks for embedded device
        chunk_delay_ms: 100, // Longer delay to allow watchdog reset
        speed: 3,
    }) {
        Ok(engine) => {
            log::info!("TTS engine initialized successfully with chunking configuration");
            engine
        }
        Err(e) => {
            log::error!("Failed to initialize TTS engine: {}", e);
            return Err(e);
        }
    };

    sd_pin_driver.set_high().unwrap();
    let _ = tts_engine.synthesize_and_play("你好，乐鑫", &mut i2s_driver);
    sd_pin_driver.set_low().unwrap();

    loop {
        match rx.recv() {
            Ok(TranscriptionMessage::TranscribeFile { path }) => {
                log::info!("Received request to transcribe file: {}", path);

                match transcribe_audio(&path) {
                    Ok(transcription) => {
                        log::info!("Transcription completed: {}", transcription);

                        if transcription != "" {
                            // Send the transcription back even if LLM fails
                            if let Err(e) = response_tx.send(transcription.clone()) {
                                log::error!("Failed to send transcription response: {}", e);
                            }

                            if transcription == "再见" {
                                sd_pin_driver.set_high().unwrap();
                                let _ =
                                    tts_engine.synthesize_and_play("再见", &mut i2s_driver);
                                sd_pin_driver.set_low().unwrap();
                                continue;
                            }

                            // Send the transcription to the LLM
                            log::info!("Sending transcription to LLM...");

                            let response = llm.send_message(transcription, ChatRole::User);

                            if response.starts_with("Error:") {
                                log::error!("LLM API error: {}", response);
                            } else {
                                log::info!("LLM response: {}", response);

                                // Convert LLM response to audio using TTS
                                log::info!("Converting LLM response to audio...");

                                sd_pin_driver.set_high().unwrap(); // Ensure SD pin is enabled
                                if let Err(e) =
                                    tts_engine.synthesize_and_play(&response, &mut i2s_driver)
                                {
                                    log::error!("Failed to synthesize and play audio: {}", e);
                                } else {
                                    log::info!(
                                        "Audio synthesis and playback completed successfully"
                                    );
                                }
                                sd_pin_driver.set_low().unwrap(); // Ensure SD pin is disabled
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to transcribe audio: {}", e);
                        // Send error message back
                        if let Err(e) = response_tx.send(format!("Error: {}", e)) {
                            log::error!("Failed to send error response: {}", e);
                        }
                    }
                }
            }
            Ok(TranscriptionMessage::RestartSession) => {
                log::info!("Received restart session request, clearing LLM history");
                llm.clear_history();
                // Re-add the system message
                llm.send_message(
                    "接下来的请求来自一个语音转文字服务，请小心中间可能有一些字词被识别成同音的字词。请不要使用列表，不要包含*，回答保持一个段落。"
                        .to_string(),
                    ChatRole::System,
                );
            }
            Ok(TranscriptionMessage::Shutdown) => {
                log::info!("Transcription worker received shutdown signal");
                break;
            }
            Err(e) => {
                log::error!("Error receiving message in transcription worker: {}", e);
                break;
            }
        }
    }

    log::info!("Transcription worker thread terminated");
    Ok(())
}

/// Function to create and start the transcription worker thread
pub fn start_transcription_worker(
    i2s_driver: I2sDriver<'static, I2sTx>,
    sd_pin_driver: PinDriver<'static, impl OutputPin, esp_idf_svc::hal::gpio::Output>,
) -> anyhow::Result<(Sender<TranscriptionMessage>, Receiver<String>)> {
    let (tx, rx) = mpsc::channel();
    let (response_tx, response_rx) = mpsc::channel();

    thread::Builder::new()
        .name("transcription_worker".to_string())
        .stack_size(16 * 1024) // Increase stack size for TTS operations
        .spawn(move || {
            if let Err(e) = transcription_worker(rx, response_tx, i2s_driver, sd_pin_driver) {
                log::error!("Transcription worker failed: {}", e);
            }
        })?;

    log::info!("Transcription worker thread created successfully");
    Ok((tx, response_rx))
}

/// Function to send WAV file to transcription API with improved structure
/// This now runs in the separate thread
fn transcribe_audio(file_path: &str) -> anyhow::Result<String> {
    log::info!("Transcribing audio file: {}", file_path);

    // Read the WAV file
    let file_data = std::fs::read(file_path)?;
    log::info!("Read {} bytes from WAV file", file_data.len());

    // Set up the API endpoint
    let transcription_api_url = env!("VOS_URL");

    // Create HTTP client
    let http_config = HttpConfiguration {
        timeout: Some(std::time::Duration::from_secs(30)),
        ..Default::default()
    };
    let mut client = EspHttpConnection::new(&http_config)?;

    // Send the multipart request and get response
    send_multipart_request(&mut client, transcription_api_url, file_path, &file_data)?;

    // Process the response
    let response_text = read_response(&mut client)?;

    Ok(response_text
        .trim_end_matches('"')
        .trim_start_matches('"')
        .to_string())
}
