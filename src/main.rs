use anyhow;
use esp_idf_svc::hal::{
    self,
    gpio::{Gpio41, Gpio42, InputPin, OutputPin},
    i2s::{
        config::{
            ClockSource, Config, DataBitWidth, MclkMultiple, PdmDownsample, PdmRxClkConfig,
            PdmRxConfig, PdmRxGpioConfig, PdmRxSlotConfig, SlotMode,
        },
        I2s, I2sDriver, I2sRx, I2S0,
    },
    peripheral::Peripheral,
    peripherals::Peripherals,
};
use esp_idf_svc::{
    sys,
    wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi},
    nvs::EspDefaultNvsPartition,
    eventloop::EspSystemEventLoop,
    http::client::{Configuration as HttpConfiguration, EspHttpConnection}, // Add HTTP client
    http::Method, // Add Method enum
};
use hound::{
    WavWriter,
    WavSpec,
};
use heapless;
use std::{ffi::CString, os::raw::c_void, time::Instant};
use sys::esp_sr::{
    self, afe_config_free, afe_config_init, esp_afe_handle_from_config, esp_mn_commands_add,
    esp_mn_commands_clear, esp_mn_commands_update, esp_mn_handle_from_name, esp_srmodel_filter,
    esp_srmodel_init
};
use std::sync::mpsc::{self, Sender, Receiver};
use std::thread;

mod sd_card;
mod llm_intf;

#[allow(unused_imports)]
use llm_intf::{LlmHelper, ChatRole};

// Helper function to send a multipart request with a file
fn send_multipart_request(
    client: &mut EspHttpConnection,
    url: &str,
    file_path: &str,
    file_data: &[u8]
) -> anyhow::Result<()> {
    // Create multipart form data boundary
    let boundary = "------------------------boundary";
    
    // Create request body
    let request_body = create_multipart_body(boundary, file_path, file_data);
    
    // Set up headers
    let content_type = format!("multipart/form-data; boundary={}", boundary);
    let content_length = request_body.len().to_string();
    
    let headers = [
        ("Content-Type", content_type.as_str()),
        ("Content-Length", content_length.as_str()),
    ];
    
    // Send the request
    if let Err(e) = client.initiate_request(Method::Post, url, &headers) {
        return Err(anyhow::anyhow!("Failed to initiate HTTP request: {}", e));
    }
    
    // Write the request body
    if let Err(e) = client.write(&request_body) {
        return Err(anyhow::anyhow!("Failed to write request body: {}", e));
    }
    
    // Finalize the request
    if let Err(e) = client.initiate_response() {
        return Err(anyhow::anyhow!("Failed to get response: {}", e));
    }
    
    Ok(())
}

// Helper function to create a multipart request body
fn create_multipart_body(boundary: &str, file_path: &str, file_data: &[u8]) -> Vec<u8> {
    let filename = file_path.split('/').last().unwrap_or("audio.wav");
    let content_disposition = format!("Content-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\n", filename);
    let content_type = "Content-Type: audio/wav\r\n\r\n";
    
    let mut request_body = Vec::new();
    
    // Add boundary start
    request_body.extend_from_slice(format!("--{}\r\n", boundary).as_bytes());
    
    // Add content disposition
    request_body.extend_from_slice(content_disposition.as_bytes());
    
    // Add content type
    request_body.extend_from_slice(content_type.as_bytes());
    
    // Add file data
    request_body.extend_from_slice(file_data);
    request_body.extend_from_slice(b"\r\n");
    
    // Add boundary end
    request_body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
    
    request_body
}

// Helper function to read response body
fn read_response_body(client: &mut EspHttpConnection) -> anyhow::Result<String> {
    let mut response_body = Vec::new();
    let mut buffer = [0u8; 1024];
    
    loop {
        match client.read(&mut buffer) {
            Ok(bytes_read) => {
                if bytes_read == 0 {
                    break;
                }
                response_body.extend_from_slice(&buffer[..bytes_read]);
            },
            Err(e) => {
                return Err(anyhow::anyhow!("Error reading response: {}", e));
            }
        }
    }
    
    Ok(String::from_utf8_lossy(&response_body).to_string())
}

// Helper function to read and process HTTP response
fn read_response(client: &mut EspHttpConnection) -> anyhow::Result<String> {
    // Get status code
    let status = client.status();
    log::info!("Response status: {}", status);
    
    if status != 200 {
        // Handle error response
        let error_text = read_response_body(client)?;
        return Err(anyhow::anyhow!("API error ({}): {}", status, error_text));
    }
    
    // Read successful response
    read_response_body(client)
}

// Update FeedTaskArg to include only the necessary peripherals needed for the microphone
struct FeedTaskArg {
    afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    afe_data: *mut esp_sr::esp_afe_sr_data_t,
    // Add fields for the peripherals needed for the microphone
    i2s0: I2S0,
    gpio_clk: Gpio42,
    gpio_din: Gpio41,
}

struct FetchTaskArg {
    afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    afe_data: *mut esp_sr::esp_afe_sr_data_t,
    multinet: *mut esp_sr::esp_mn_iface_t,
    model_data: *mut esp_sr::model_iface_data_t,
    transcription_tx: Sender<TranscriptionMessage>,
}

// Define message types for the transcription thread
enum TranscriptionMessage {
    TranscribeFile { path: String },
    Shutdown,
}

// Worker function for the transcription thread
fn transcription_worker(rx: Receiver<TranscriptionMessage>) -> anyhow::Result<()> {
    log::info!("Transcription worker thread started");
    
    // Get token from environment variable at compile time
    let token = env!("LLM_AUTH_TOKEN", "LLM authentication token must be set at compile time");
    
    // Create and configure the LLM helper
    let mut llm = match llm_intf::LlmHelper::new(token, "deepseek-chat") {
        helper => {
            log::info!("LLM helper created successfully");
            helper
        }
    };
    
    // Configure with parameters suitable for embedded device
    llm.configure(
        Some(512),  // Max tokens to generate in response
        Some(0.7),  // Temperature - balanced between deterministic and creative
        Some(0.9)   // Top-p - slightly more focused sampling
    );
    
    // Send initial system message to set context
    llm.send_message(
        "接下来的请求来自一个语音转文字服务，请小心中间可能有一些字词被识别成同音的字词。".to_string(),
        ChatRole::System
    );
    
    log::info!("LLM helper initialized with system prompt");
    
    loop {
        match rx.recv() {
            Ok(TranscriptionMessage::TranscribeFile { path }) => {
                log::info!("Received request to transcribe file: {}", path);
                
                match transcribe_audio(&path) {
                    Ok(transcription) => {
                        log::info!("Transcription completed: {}", transcription);
                        
                        // Send the transcription to the LLM
                        log::info!("Sending transcription to LLM...");
                        let response = llm.send_message(transcription, ChatRole::User);
                        
                        if response.starts_with("Error:") {
                            log::error!("LLM API error: {}", response);
                        } else {
                            log::info!("LLM response: {}", response);
                            
                            // Here you would send the response to a text-to-speech system
                            // For now, we just log it
                        }
                    },
                    Err(e) => {
                        log::error!("Failed to transcribe audio: {}", e);
                    }
                }
            },
            Ok(TranscriptionMessage::Shutdown) => {
                log::info!("Transcription worker received shutdown signal");
                break;
            },
            Err(e) => {
                log::error!("Error receiving message in transcription worker: {}", e);
                break;
            }
        }
    }
    
    log::info!("Transcription worker thread terminated");
    Ok(())
}

// Function to create and start the transcription worker thread
fn start_transcription_worker() -> anyhow::Result<Sender<TranscriptionMessage>> {
    let (tx, rx) = mpsc::channel();
    
    thread::Builder::new()
        .name("transcription_worker".to_string())
        .stack_size(8 * 1024) // Same stack size as other threads
        .spawn(move || {
            if let Err(e) = transcription_worker(rx) {
                log::error!("Transcription worker failed: {}", e);
            }
        })?;
    
    log::info!("Transcription worker thread created successfully");
    Ok(tx)
}

// Function to send WAV file to transcription API with improved structure
// This now runs in the separate thread
fn transcribe_audio(file_path: &str) -> anyhow::Result<String> {
    log::info!("Transcribing audio file: {}", file_path);
    
    // Read the WAV file
    let file_data = std::fs::read(file_path)?;
    log::info!("Read {} bytes from WAV file", file_data.len());
    
    // Set up the API endpoint
    let transcription_api_url = "http://192.168.1.4:5000/transcribe";
    
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
    
    Ok(response_text)
}

fn init_mic<'d>(
    i2s_slot: impl Peripheral<P = impl I2s> + 'd,
    clk: impl Peripheral<P = impl OutputPin> + 'd,
    din: impl Peripheral<P = impl InputPin> + 'd,
) -> anyhow::Result<I2sDriver<'d, I2sRx>> {
    let pdm_rx_cfg = PdmRxConfig::new(
        Config::default(),
        PdmRxClkConfig::from_sample_rate_hz(16000)
            .clk_src(ClockSource::Pll160M)
            .mclk_multiple(MclkMultiple::M256)
            .downsample_mode(PdmDownsample::Samples8),
        PdmRxSlotConfig::from_bits_per_sample_and_slot_mode(DataBitWidth::Bits16, SlotMode::Mono),
        PdmRxGpioConfig::new(false),
    );

    let mut pdm_driver = I2sDriver::<I2sRx>::new_pdm_rx(i2s_slot, &pdm_rx_cfg, clk, din)?;

    pdm_driver.rx_enable()?;

    Ok(pdm_driver)
}

macro_rules! call_c_method {
    ($c_ptr: expr, $method: ident) => {
        unsafe {
            if $c_ptr.is_null() {
                Err(anyhow::anyhow!("Null pointer provided to {}", stringify!($method)))
            } else if let Some(inner_func) = (*$c_ptr).$method {
                Some(inner_func())
            } else {
               Err(anyhow::anyhow!("Failed to call method {}", stringify!($method)))
            }
        }
    };
    ($c_ptr: expr, $method: ident, $($args: expr),*) => {
        unsafe {
            if $c_ptr.is_null() {
                Err(anyhow::anyhow!("Null pointer provided to {}", stringify!($method)))
            } else if let Some(inner_func) = (*$c_ptr).$method {
                Ok(inner_func($($args),*))
            } else {
                Err(anyhow::anyhow!("Failed to call method {}", stringify!($method)))
            }
        }
    };
}

// Modify inner_feed_proc to use peripherals from FeedTaskArg
fn inner_feed_proc(feed_arg: &mut Box<FeedTaskArg>) -> anyhow::Result<()> {
    // Get peripherals from the FeedTaskArg
    let mut mic = init_mic(
        &mut feed_arg.i2s0,
        &mut feed_arg.gpio_clk,
        &mut feed_arg.gpio_din,
    )?;

    let chunk_size = call_c_method!(feed_arg.afe_handle, get_feed_chunksize, feed_arg.afe_data)?;
    let channel_num = call_c_method!(feed_arg.afe_handle, get_feed_channel_num, feed_arg.afe_data)?;

    log::info!("[INFO] chunk_size {}, channel_num {}", chunk_size, channel_num);

    let mut chunk = vec![0u8; 2 * chunk_size as usize * channel_num as usize];

    loop {
        mic.read(chunk.as_mut_slice(), 100)?;
        let _ = call_c_method!(feed_arg.afe_handle, feed, feed_arg.afe_data, chunk.as_ptr() as *const i16)?;
    }
}

extern "C" fn feed_proc(arg: * mut std::ffi::c_void) {
    let mut feed_arg = unsafe { Box::from_raw(arg as *mut FeedTaskArg) };

    match inner_feed_proc(&mut feed_arg) {
        Ok(_) => log::info!("Feed task completed successfully"),
        Err(e) => log::error!("Feed task failed: {}", e),
    };
}

// Define the State enum
#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    /// Waiting for wake word activation
    WakeWordDetecting,
    /// Waiting for command after wake word detected
    CmdDetecting,
    /// Recording audio after command detection
    Recording,
}

impl State {
    /// Returns a human-readable description of the state
    fn description(&self) -> &'static str {
        match self {
            State::WakeWordDetecting => "Waiting for wake word",
            State::CmdDetecting => "Detecting command",
            State::Recording => "Recording audio",
        }
    }

    /// Logs a state transition with appropriate log level
    fn log_transition(from: State, to: State, reason: &str) {
        if from == to {
            log::debug!("State remains at {:?} ({}): {}", to, to.description(), reason);
        } else {
            log::info!("State transition: {:?} -> {:?} ({} → {}): {}",
                      from, to, from.description(), to.description(), reason);
        }
    }
}

#[allow(dead_code)]
fn print_fetch_result(res: *const esp_sr::afe_fetch_result_t) {
    unsafe {
        log::info!("--- AFE Fetch Result ---");
        log::info!("data_size: {} bytes", (*res).data_size);
        log::info!("vad_cache_size: {} bytes", (*res).vad_cache_size);
        log::info!("data_volume: {} dB", (*res).data_volume);
        log::info!("wakeup_state: {}", (*res).wakeup_state);
        log::info!("wake_word_index: {}", (*res).wake_word_index);
        log::info!("wakenet_model_index: {}", (*res).wakenet_model_index);
        log::info!("vad_state: {}", (*res).vad_state);
        log::info!("trigger_channel_id: {}", (*res).trigger_channel_id);
        log::info!("wake_word_length: {} samples", (*res).wake_word_length);
        log::info!("ret_value: {}", (*res).ret_value);
        log::info!("raw_data_channels: {}", (*res).raw_data_channels);
        log::info!("--- End of Fetch Result ---");
    }
}

// Helper function to flush FatFs filesystem with improved error handling
fn flush_filesystem(mount_point: &str) -> anyhow::Result<()> {
    // Create a temporary file to force a flush of the file system
    let flush_path = format!("{}/flush.tmp", mount_point);

    // Wrap the file operations in a separate scope to ensure file is closed before deletion
    {
        match std::fs::File::create(&flush_path) {
            Ok(file) => {
                // Sync the file to ensure data is written to disk
                if let Err(e) = file.sync_all() {
                    log::warn!("Failed to sync filesystem at {}: {}", mount_point, e);
                    return Err(anyhow::anyhow!("Failed to sync filesystem: {}", e));
                }
            },
            Err(e) => {
                log::error!("Failed to create temp file at {}: {}", flush_path, e);
                return Err(anyhow::anyhow!("Failed to create temp file for filesystem flush: {}", e));
            }
        }
    }

    // Remove the temporary file
    match std::fs::remove_file(&flush_path) {
        Ok(_) => {
            log::info!("Filesystem at {} flushed successfully", mount_point);
        },
        Err(e) => {
            log::warn!("Failed to remove temp file at {}: {}", flush_path, e);
            // Continue execution - this is not a critical error
        }
    }

   Ok(())
}

// Modify the RECORDING state code to flush data after finalizing WAV file
fn inner_fetch_proc(arg: &Box<FetchTaskArg>) -> anyhow::Result<()> {
    let afe_handle = arg.afe_handle;
    let afe_data = arg.afe_data;
    let multinet = arg.multinet;
    let model_data = arg.model_data;

    // Validate pointers before using them
    if afe_handle.is_null() {
        return Err(anyhow::anyhow!("AFE handle is null"));
    }

    if afe_data.is_null() {
        return Err(anyhow::anyhow!("AFE data is null"));
    }

    if multinet.is_null() {
        return Err(anyhow::anyhow!("Multinet handle is null"));
    }

    if model_data.is_null() {
        return Err(anyhow::anyhow!("Model data is null"));
    }

    // Initialize state
    let mut state = State::WakeWordDetecting;

    // For recording WAV files
    let mut file_idx = 0;
    let mut wav_writer: Option<WavWriter<std::io::BufWriter<std::fs::File>>> = None;

    // For tracking silence duration
    let mut silence_frames = 0;
    let frames_per_second = 16000 / 480; // Assuming 30ms frames at 16kHz (adjust based on your frame size)

    log::info!("Starting detection loop with initial state: {:?}", state);

    // Infinite loop for the state machine - this function never returns normally
    loop {
        // Always fetch data from AFE
        let res = call_c_method!(afe_handle, fetch, afe_data)?;

        if res.is_null() {
            log::error!("Fetch returned null result");
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        if unsafe { (*res).ret_value } == esp_sr::ESP_FAIL {
            log::error!("Fetch failed with ESP_FAIL");
            std::thread::sleep(std::time::Duration::from_millis(10));
            continue;
        }

        // Handle the data based on current state
        match state {
            State::WakeWordDetecting => {
                if unsafe { (*res).wakeup_state } == esp_sr::wakenet_state_t_WAKENET_DETECTED {
                    let next_state = State::CmdDetecting;
                    State::log_transition(state, next_state, "Wake word detected");

                    call_c_method!(afe_handle, disable_wakenet, afe_data)?;
                    call_c_method!(multinet, clean, model_data)?;
                    state = next_state;
                }
            },

            State::CmdDetecting => {
                let mn_state = call_c_method!(multinet, detect, model_data, (*res).data)?;
                if mn_state == esp_sr::esp_mn_state_t_ESP_MN_STATE_DETECTED {
                    let mn_result = call_c_method!(multinet, get_results, model_data)?;
                    let command_id_str = (0..unsafe { (*mn_result).num as usize })
                        .map(|i| unsafe { (*mn_result).command_id[i].to_string() })
                        .collect::<Vec<String>>()
                        .join(", ");

                    for i in 0..unsafe { (*mn_result).num as usize } {
                        let command_id = unsafe { (*mn_result).command_id[i] };
                        log::info!("Command detected: {}", command_id);
                    }

                    let next_state = State::Recording;
                    State::log_transition(state, next_state, &format!("Command detected (ID: {})", command_id_str));

                    // Initialize WAV recording
                    let spec = WavSpec {
                        channels: 1,
                        sample_rate: 16000,
                        bits_per_sample: 16,
                        sample_format: hound::SampleFormat::Int,
                    };

                    // Increment file_idx before creating the writer to avoid conflicts if finalization fails
                    let current_file_idx = file_idx;
                    file_idx += 1;

                    log::info!("Creating WAV file: /vfat/audio{}.wav", current_file_idx);
                    let writer = WavWriter::create(std::format!("/vfat/audio{}.wav", current_file_idx), spec)?;
                    wav_writer = Some(writer);
                    silence_frames = 0;

                    state = next_state;

                } else if mn_state == esp_sr::esp_mn_state_t_ESP_MN_STATE_TIMEOUT {
                    let next_state = State::WakeWordDetecting;
                    State::log_transition(state, next_state, "Command detection timeout");

                    call_c_method!(afe_handle, enable_wakenet, afe_data)?;
                    state = next_state;
                }
            },

            State::Recording => {
                // Check VAD state
                let vad_state = unsafe { (*res).vad_state };

                // Write audio data to WAV file
                if let Some(writer) = &mut wav_writer {
                    let data_ptr = unsafe { (*res).data };
                    let data_size = unsafe { (*res).data_size / 2 }; // Convert bytes to samples (16-bit samples)

                    // Assuming data is an array of i16 samples
                    for i in 0..data_size {
                        let sample = unsafe { *data_ptr.offset(i as isize) };
                        writer.write_sample(sample)?;
                    }
                }

                if vad_state == sys::esp_sr::vad_state_t_VAD_SILENCE {
                    silence_frames += 1;
                    
                    // Maybe increase this value to avoid cutting off speech too early
                    if silence_frames >= frames_per_second * 2 { // 2 seconds of silence 
                        let next_state = State::WakeWordDetecting;
                        State::log_transition(state, next_state, &format!("Detected {} frames of silence", silence_frames));

                        // Finalize WAV file
                        if let Some(writer) = wav_writer.take() {
                            log::info!("Finalizing WAV file after {} silent frames", silence_frames);
                            writer.finalize()?;

                            // Flush the filesystem to ensure all data is written
                            if let Err(e) = flush_filesystem("/vfat") {
                                log::warn!("Failed to flush filesystem: {}", e);
                            } else {
                                log::info!("Filesystem flushed successfully");
                                
                                // Send a message to the transcription thread to process the file
                                let file_path = format!("/vfat/audio{}.wav", file_idx - 1);
                                if let Err(e) = arg.transcription_tx.send(TranscriptionMessage::TranscribeFile { path: file_path }) {
                                    log::error!("Failed to send transcription message: {}", e);
                                } else {
                                    log::info!("Sent audio file for asynchronous transcription");
                                }
                            }
                        }

                        // Return to wake word detection
                        call_c_method!(afe_handle, enable_wakenet, afe_data)?;
                        state = next_state;
                    }
                } else {
                    // Reset silence counter when we detect speech
                    if silence_frames > 0 {
                        log::debug!("Speech detected after {} silent frames, resetting silence counter", silence_frames);
                    }
                    silence_frames = 0;
                }
            }
        }
    }
}

extern "C" fn fetch_proc(arg: * mut std::ffi::c_void) {
    let feed_arg = unsafe { Box::from_raw(arg as *mut FetchTaskArg) };

    let res = inner_fetch_proc(&feed_arg);
    match res {
        Ok(_) => log::info!("Fetch task completed successfully"),
        Err(e) => log::error!("Fetch task failed: {}", e),
    };
}

// Add this function to print all fields of afe_config
fn print_afe_config(afe_config: *const esp_sr::afe_config_t) {
    unsafe {
        log::info!("--- AFE Configuration ---");

        // AEC configuration
        log::info!("AEC: init={}, mode={}, filter_length={}",
            (*afe_config).aec_init,
            (*afe_config).aec_mode,
            (*afe_config).aec_filter_length);

        // SE configuration
        log::info!("SE: init={}", (*afe_config).se_init);

        // NS configuration
        log::info!("NS: init={}, mode={}",
            (*afe_config).ns_init,
            (*afe_config).afe_ns_mode);

        // VAD configuration
        log::info!("VAD: init={}, mode={}, min_speech_ms={}, min_noise_ms={}, delay_ms={}, mute_playback={}, enable_channel_trigger={}",
            (*afe_config).vad_init,
            (*afe_config).vad_mode,
            (*afe_config).vad_min_speech_ms,
            (*afe_config).vad_min_noise_ms,
            (*afe_config).vad_delay_ms,
            (*afe_config).vad_mute_playback,
            (*afe_config).vad_enable_channel_trigger);

        // WakeNet configuration
        log::info!("WakeNet: init={}, mode={}",
            (*afe_config).wakenet_init,
            (*afe_config).wakenet_mode);

        // AGC configuration
        log::info!("AGC: init={}, mode={}, compression_gain_db={}, target_level_dbfs={}",
            (*afe_config).agc_init,
            (*afe_config).agc_mode,
            (*afe_config).agc_compression_gain_db,
            (*afe_config).agc_target_level_dbfs);

        // PCM configuration
        log::info!("PCM: total_ch_num={}, mic_num={}, ref_num={}, sample_rate={}",
            (*afe_config).pcm_config.total_ch_num,
            (*afe_config).pcm_config.mic_num,
            (*afe_config).pcm_config.ref_num,
            (*afe_config).pcm_config.sample_rate);

        // General AFE configuration
        log::info!("General AFE: mode={}, type={}, preferred_core={}, preferred_priority={}, ringbuf_size={}, linear_gain={}",
            (*afe_config).afe_mode,
            (*afe_config).afe_type,
            (*afe_config).afe_perferred_core,
            (*afe_config).afe_perferred_priority,
            (*afe_config).afe_ringbuf_size,
            (*afe_config).afe_linear_gain);

        log::info!("Memory allocation mode={}, debug_init={}, fixed_first_channel={}",
            (*afe_config).memory_alloc_mode,
            (*afe_config).debug_init,
            (*afe_config).fixed_first_channel);

        log::info!("--- End of AFE Configuration ---");
    }
}

// Enhanced WiFi initialization function with better error handling and reconnection logic
fn initialize_wifi(modem: hal::modem::Modem) -> anyhow::Result<Box<EspWifi<'static>>> {
    // Get SSID and password from environment variables (mandatory)
    let ssid = env!("WIFI_SSID", "WIFI_SSID environment variable must be set");
    let pass = env!("WIFI_PASS", "WIFI_PASS environment variable must be set");

    log::info!("Connecting to WiFi network: {}", ssid);

    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let mut wifi = EspWifi::new(modem, sys_loop.clone(), Some(nvs))?;

    let mut auth_method = AuthMethod::WPA2Personal;
    if pass.is_empty() {
        auth_method = AuthMethod::None;
        log::info!("Using open WiFi network (no password)");
    }

    let mut client_config = ClientConfiguration {
        ssid: heapless::String::new(),
        password: heapless::String::new(),
        auth_method,
        ..Default::default()
    };

    // Copy SSID and password into heapless Strings
    client_config.ssid.push_str(ssid).map_err(|_| anyhow::anyhow!("SSID too long"))?;
    client_config.password.push_str(pass).map_err(|_| anyhow::anyhow!("Password too long"))?;

    wifi.set_configuration(&Configuration::Client(client_config))?;

    wifi.start()?;
    log::info!("WiFi started, connecting...");

    // Try to connect with retries
    let max_retries = 3;
    let mut connected = false;

    for attempt in 1..=max_retries {
        match wifi.connect() {
            Ok(_) => {
                log::info!("WiFi connect initiated (attempt {}/{}), waiting for connection...", attempt, max_retries);

                // Wait for connection with timeout
                let max_wait_seconds = 15; // Increased timeout for DHCP
                let mut has_valid_ip = false;

                for _i in 1..=max_wait_seconds {
                    std::thread::sleep(std::time::Duration::from_secs(1));

                    // First check if connected
                    if let Ok(true) = wifi.is_connected() {
                        connected = true;

                        // Then verify we have a valid IP address (not 0.0.0.0)
                        if let Ok(ip_info) = wifi.sta_netif().get_ip_info() {
                            if ip_info.ip != std::net::Ipv4Addr::new(0, 0, 0, 0) {
                                log::info!("Valid IP address obtained: {}", ip_info.ip);
                                log::info!("Subnet mask: {}", ip_info.subnet);
                                log::info!("DNS: {:?}", ip_info.dns);

                                // Log successful connection but don't try to test TCP connectivity
                                has_valid_ip = true;
                                break; // Successfully connected with valid IP
                            } else {
                                log::debug!("Connected but waiting for DHCP (IP: {})...", ip_info.ip);
                            }
                        }
                    }
                }

                if connected && has_valid_ip {
                    log::info!("WiFi connected successfully with valid IP address!");
                    break;
                } else if connected {
                    log::warn!("Connected to WiFi but failed to get valid IP address after {} seconds", max_wait_seconds);
                    // Disconnect and retry to force new DHCP exchange
                    let _ = wifi.disconnect();
                    std::thread::sleep(std::time::Duration::from_secs(1));
                } else {
                    log::warn!("WiFi connection timed out after {} seconds", max_wait_seconds);
                }
            },
            Err(e) => {
                log::error!("Failed to connect to WiFi (attempt {}/{}): {}", attempt, max_retries, e);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }

    if connected {
        log::info!("WiFi connected successfully!");
        match wifi.sta_netif().get_ip_info() {
            Ok(ip_info) => log::info!("IP info: {:?}", ip_info),
            Err(e) => log::warn!("Failed to get IP info: {}", e),
        }
        // Return the wifi object in a Box to maintain ownership
        Ok(Box::new(wifi))
    } else {
        let err_msg = format!("Failed to connect to WiFi '{}' after {} attempts", ssid, max_retries);
        log::error!("{}", err_msg);
        Err(anyhow::anyhow!(err_msg))
    }
}

// Test function for LLM functionality with improved error handling
fn test_llm_helper() -> anyhow::Result<()> {
    // Get token from environment variable at compile time
    let token = env!("LLM_AUTH_TOKEN", "LLM authentication token must be set at compile time");

    log::info!("Creating LlmHelper instance to test DeepSeek API integration");

    // Create LLM helper with error handling
    let mut llm = match std::panic::catch_unwind(|| {
        llm_intf::LlmHelper::new(token, "deepseek-chat")
    }) {
        Ok(helper) => helper,
        Err(_) => return Err(anyhow::anyhow!("Failed to initialize LlmHelper"))
    };

    // Configure parameters with reasonable defaults for embedded use
    llm.configure(
        Some(256),  // Smaller token count to conserve memory
        Some(0.7),  // Temperature - balanced between deterministic and creative
        Some(0.9)   // Top-p - slightly more focused sampling
    );

    // Send a test message
    log::info!("Sending test message to DeepSeek API");

    let response = llm.send_message(
        "Hello! I'm testing the ESP32-S3 integration with DeepSeek AI. Can you confirm this is working?".to_string(),
        llm_intf::ChatRole::User
    );

    if response.starts_with("Error:") {
        log::error!("LLM API error: {}", response);
        return Err(anyhow::anyhow!("LLM API request failed: {}", response));
    }

    log::info!("Received response from DeepSeek API: {}", response);

    // Get conversation history
    let history = llm.get_history();
    log::info!("Conversation history:");
    for msg in history {
        log::info!("  {}", msg);
    }

    Ok(())
}

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
        },
        Err(e) => {
            log::error!("Failed to connect to WiFi: {}", e);
            return Err(e);
        }
    };

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

    // No need for ResourceGuard - SdCard has Drop trait implemented
    // that will automatically unmount when sd goes out of scope

    // Initialize speech recognition models
    let part_name = CString::new("/vfat").unwrap();
    let models = unsafe { esp_srmodel_init(part_name.as_ptr()) };
    if models.is_null() {
        log::error!("Failed to initialize speech recognition models");
        return Err(anyhow::anyhow!("Failed to initialize speech recognition models"));
    }

    let input_format = CString::new("M").unwrap();
    let afe_config = unsafe {
        afe_config_init(
            input_format.as_ptr(),
            models,
            esp_sr::afe_type_t_AFE_TYPE_SR,
            esp_sr::afe_mode_t_AFE_MODE_LOW_COST,
        )
    };

    if afe_config.is_null() {
        log::error!("Failed to initialize AFE configuration");
        return Err(anyhow::anyhow!("Failed to initialize AFE configuration"));
    }

    // Print the AFE configuration
    print_afe_config(afe_config);

    // Initialize AFE
    let afe_handle = unsafe { esp_afe_handle_from_config(afe_config) };
    if afe_handle.is_null() {
        log::error!("Failed to create AFE handle from config");
        unsafe { afe_config_free(afe_config) };
        return Err(anyhow::anyhow!("Failed to create AFE handle"));
    }

    let afe_data = match call_c_method!(afe_handle, create_from_config, afe_config) {
        Ok(data) => data,
        Err(e) => {
            log::error!("Failed to create AFE data: {}", e);
            unsafe { afe_config_free(afe_config) };
            return Err(e);
        }
    };

    // Free config after use
    unsafe { afe_config_free(afe_config) };

    // Initialize multinet for command recognition
    let prefix_str = Vec::from(esp_sr::ESP_MN_PREFIX);
    let chinese_str = Vec::from(esp_sr::ESP_MN_CHINESE);
    let mn_name = unsafe {
        esp_srmodel_filter(
            models,
            prefix_str.as_ptr() as *const i8,
            chinese_str.as_ptr() as *const i8,
        )
    };

    if mn_name.is_null() {
        log::error!("Failed to filter speech recognition model");
        return Err(anyhow::anyhow!("Failed to filter speech recognition model"));
    }

    let multinet = unsafe { esp_mn_handle_from_name(mn_name) };
    if multinet.is_null() {
        log::error!("Failed to get multinet handle");
        return Err(anyhow::anyhow!("Failed to get multinet handle"));
    }

    let model_data = match call_c_method!(multinet, create, mn_name, 6000) {
        Ok(data) => data,
        Err(e) => {
            log::error!("Failed to create model data: {}", e);
            return Err(e);
        }
    };

    // Setup speech commands
    unsafe {
        esp_mn_commands_clear();
        esp_mn_commands_add(1, Vec::from(b"wo you ge wen ti\0").as_ptr() as *const i8);
        esp_mn_commands_update();
    }

    // Start the transcription worker thread
    let transcription_tx = match start_transcription_worker() {
        Ok(tx) => tx,
        Err(e) => {
            log::error!("Failed to start transcription worker: {}", e);
            return Err(anyhow::anyhow!("Failed to start transcription worker: {}", e));
        }
    };
    log::info!("Transcription worker started successfully");

    // Create the feed task argument
    let feed_task_arg = Box::new(FeedTaskArg {
        afe_handle,
        afe_data,
        i2s0: peripherals.i2s0,
        gpio_clk: peripherals.pins.gpio42,
        gpio_din: peripherals.pins.gpio41,
    });

    // Create the feed task
    let _feed_task = unsafe {
        hal::task::create(
            feed_proc,
            &*CString::new("feed_task").unwrap(),
            8 * 1024,
            Box::into_raw(feed_task_arg) as *mut c_void,
            5,
            None
        )
    }?;
    log::info!("Feed task created successfully");

    // Create the fetch task argument with transcription channel
    let fetch_task_arg = Box::new(FetchTaskArg {
        afe_handle,
        afe_data,
        multinet,
        model_data,
        transcription_tx,
    });

    // Create the fetch task
    let _fetch_task = unsafe {
        hal::task::create(
            fetch_proc,
            &*CString::new("fetch_task").unwrap(),
            8 * 1024,
            Box::into_raw(fetch_task_arg) as *mut c_void,
            5,
            None
        )
    }?;
    log::info!("Fetch task created successfully");

    // Log initialization time
    log::info!("AI Chatbox initialization completed in {} ms", init_timer.elapsed().as_millis());

    // Simple infinite loop for embedded application - this is standard practice
    // for embedded applications where the main thread can just sleep
    log::info!("Entering main loop");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
