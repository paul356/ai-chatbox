use anyhow;
use esp_idf_svc::{hal::{
    gpio::{Gpio41, Gpio42},
    i2s::I2S0,
}, sys::daddr_t};
use esp_idf_svc::sys;
use std::sync::mpsc::{Receiver, Sender};
use std::{ffi::c_void, os::raw::c_void as raw_c_void};
use sys::esp_sr;

use crate::audio_device::init_mic;
use crate::transcription::TranscriptionMessage;

/// Define the State enum
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    /// Waiting for the wake word to be detected
    WakeWordDetecting,
    /// Recording audio after wake word detected
    Recording,
}

impl State {
    /// Returns a human-readable description of the state
    pub fn description(&self) -> &'static str {
        match self {
            State::WakeWordDetecting => "Waiting for wake word",
            State::Recording => "Recording audio",
        }
    }

    /// Logs a state transition with appropriate log level
    pub fn log_transition(from: State, to: State, reason: &str) {
        if from == to {
            log::debug!(
                "State remains at {:?} ({}): {}",
                to,
                to.description(),
                reason
            );
        } else {
            log::info!(
                "State transition: {:?} -> {:?} ({} → {}): {}",
                from,
                to,
                from.description(),
                to.description(),
                reason
            );
        }
    }
}

/// Update FeedTaskArg to include only the necessary peripherals needed for the microphone
pub struct FeedTaskArg {
    pub afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    pub afe_data: *mut esp_sr::esp_afe_sr_data_t,
    // Add fields for the peripherals needed for the microphone
    pub i2s0: I2S0,
    pub gpio_clk: Gpio42,
    pub gpio_din: Gpio41,
}

pub struct FetchTaskArg {
    pub afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    pub afe_data: *mut esp_sr::esp_afe_sr_data_t,
    pub multinet: *mut esp_sr::esp_mn_iface_t,
    pub model_data: *mut esp_sr::model_iface_data_t,
    pub transcription_tx: Sender<TranscriptionMessage>,
    pub transcription_response_rx: Receiver<String>,
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

/// Modify inner_feed_proc to use peripherals from FeedTaskArg
fn inner_feed_proc(feed_arg: &mut Box<FeedTaskArg>) -> anyhow::Result<()> {
    // Get peripherals from the FeedTaskArg
    let mut mic = init_mic(
        &mut feed_arg.i2s0,
        &mut feed_arg.gpio_clk,
        &mut feed_arg.gpio_din,
    )?;

    let chunk_size = call_c_method!(feed_arg.afe_handle, get_feed_chunksize, feed_arg.afe_data)?;
    let channel_num = call_c_method!(feed_arg.afe_handle, get_feed_channel_num, feed_arg.afe_data)?;

    log::info!(
        "[INFO] chunk_size {}, channel_num {}",
        chunk_size,
        channel_num
    );

    let mut chunk = vec![0u8; 2 * chunk_size as usize * channel_num as usize];

    loop {
        mic.read(chunk.as_mut_slice(), 100)?;
        let _ = call_c_method!(
            feed_arg.afe_handle,
            feed,
            feed_arg.afe_data,
            chunk.as_ptr() as *const i16
        )?;
    }
}

extern "C" fn feed_proc(arg: *mut raw_c_void) {
    let mut feed_arg = unsafe { Box::from_raw(arg as *mut FeedTaskArg) };

    match inner_feed_proc(&mut feed_arg) {
        Ok(_) => log::info!("Feed task completed successfully"),
        Err(e) => log::error!("Feed task failed: {}", e),
    };
}

/// Helper function to flush FatFs filesystem with improved error handling
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
            }
            Err(e) => {
                log::error!("Failed to create temp file at {}: {}", flush_path, e);
                return Err(anyhow::anyhow!(
                    "Failed to create temp file for filesystem flush: {}",
                    e
                ));
            }
        }
    }

    // Remove the temporary file
    match std::fs::remove_file(&flush_path) {
        Ok(_) => {
            log::info!("Filesystem at {} flushed successfully", mount_point);
        }
        Err(e) => {
            log::warn!("Failed to remove temp file at {}: {}", flush_path, e);
            // Continue execution - this is not a critical error
        }
    }

    Ok(())
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

/// Modify the RECORDING state code to flush data after finalizing WAV file
fn inner_fetch_proc(arg: &Box<FetchTaskArg>) -> anyhow::Result<()> {
    use hound::{WavSpec, WavWriter};
    use std::sync::mpsc::TryRecvError;

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
    let frames_per_second = 16000 / 256; // Assuming 30ms frames at 16kHz (adjust based on your frame size)

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
                    let next_state = State::Recording;
                    State::log_transition(
                        state,
                        next_state,
                        "Wake word detected, starting continuous recording",
                    );

                    call_c_method!(afe_handle, disable_wakenet, afe_data)?;

                    // Send restart session message to clear LLM history
                    if let Err(e) = arg
                        .transcription_tx
                        .send(TranscriptionMessage::RestartSession)
                    {
                        log::error!("Failed to send restart session message: {}", e);
                    } else {
                        log::info!("Sent restart session message to transcription worker");
                    }

                    // Initialize WAV recording
                    let spec = WavSpec {
                        channels: 1,
                        sample_rate: 16000,
                        bits_per_sample: 16,
                        sample_format: hound::SampleFormat::Int,
                    };

                    let current_file_idx = file_idx;
                    file_idx += 1;

                    log::info!("Creating WAV file: /vfat/audio{}.wav", current_file_idx);
                    let writer = WavWriter::create(
                        std::format!("/vfat/audio{}.wav", current_file_idx),
                        spec,
                    )?;
                    wav_writer = Some(writer);
                    silence_frames = 0;

                    state = next_state;
                }
            }

            State::Recording => {
                // Check for transcription responses non-blockingly from the fixed channel
                match arg.transcription_response_rx.try_recv() {
                    Ok(transcription) => {
                        log::info!("Received transcription response: {}", transcription);

                        // Check if the transcription contains the exit command
                        if transcription == "再见" {
                            let next_state = State::WakeWordDetecting;
                            State::log_transition(state, next_state, "Exit command detected");

                            // Finalize current recording if active
                            if let Some(writer) = wav_writer.take() {
                                writer.finalize()?;
                                log::info!("Finalized current recording due to exit command");
                            }

                            file_idx += 1;

                            // Return to wake word detection
                            call_c_method!(afe_handle, enable_wakenet, afe_data)?;
                            state = next_state;
                            continue;
                        }
                    }
                    Err(TryRecvError::Disconnected) => {
                        log::warn!("Transcription response channel was closed");
                    }
                    Err(TryRecvError::Empty) => {
                        // No response yet, continue with recording
                    }
                }

                // Check VAD state
                let vad_state = unsafe { (*res).vad_state };

                if vad_state == sys::esp_sr::vad_state_t_VAD_SILENCE {
                    silence_frames += 1;

                    // Shorter silence detection for continuous conversation
                    if silence_frames >= frames_per_second * 2 {
                        // 1 second of silence
                        // Finalize current WAV file and start transcription
                        if let Some(writer) = wav_writer.take() {
                            log::info!(
                                "Finalizing WAV file after {} silent frames for transcription",
                                silence_frames
                            );

                            let has_data = writer.duration() > 0;

                            if has_data {
                                writer.finalize()?;

                                // Flush the filesystem to ensure all data is written
                                if let Err(e) = flush_filesystem("/vfat") {
                                    log::warn!("Failed to flush filesystem: {}", e);
                                } else {
                                    log::info!("Filesystem flushed successfully");
                                }

                                // Send transcription request
                                let file_path = format!("/vfat/audio{}.wav", file_idx - 1);

                                if let Err(e) = arg.transcription_tx.send(
                                    TranscriptionMessage::TranscribeFile {
                                        path: file_path.clone(),
                                    },
                                ) {
                                    log::error!("Failed to send transcription message: {}", e);
                                } else {
                                    log::info!("Sent audio file for transcription: {}", file_path);
                                }

                                // Start a new recording immediately for continuous conversation
                                let spec = WavSpec {
                                    channels: 1,
                                    sample_rate: 16000,
                                    bits_per_sample: 16,
                                    sample_format: hound::SampleFormat::Int,
                                };

                                let current_file_idx = file_idx;
                                file_idx += 1;

                                log::info!(
                                    "Creating new WAV file for continuous recording: /vfat/audio{}.wav",
                                    current_file_idx
                                );
                                let writer = WavWriter::create(
                                    std::format!("/vfat/audio{}.wav", current_file_idx),
                                    spec,
                                )?;
                                wav_writer = Some(writer);
                            } else {
                                log::warn!("WAV file duration is zero, skipping transcription");
                                wav_writer = Some(writer);
                            }
                        }

                        silence_frames = 0;
                    }
                } else {
                    // Write audio data to WAV file
                    if let Some(writer) = &mut wav_writer {
                        let cache_size = unsafe { (*res).vad_cache_size };

                        if cache_size > 0 {
                            let data_ptr = unsafe { (*res).vad_cache };
                            let data_size = cache_size / 2; // Convert bytes to samples (16-bit samples)
                            for i in 0..data_size {
                                let sample = unsafe { *data_ptr.offset(i as isize) };
                                writer.write_sample(sample)?;
                            }
                        }

                                                let data_ptr = unsafe { (*res).data };
                        let data_size = unsafe { (*res).data_size / 2 }; // Convert bytes to samples (16-bit samples)
                        // Assuming data is an array of i16 samples
                        for i in 0..data_size {
                            let sample = unsafe { *data_ptr.offset(i as isize) };
                            writer.write_sample(sample)?;
                        }
                    }

                    // Reset silence counter when we detect speech
                    if silence_frames > 0 {
                        log::debug!(
                            "Speech detected after {} silent frames, resetting silence counter",
                            silence_frames
                        );
                    }
                    silence_frames = 0;
                }
            }
        }
    }
}

extern "C" fn fetch_proc(arg: *mut raw_c_void) {
    let feed_arg = unsafe { Box::from_raw(arg as *mut FetchTaskArg) };

    let res = inner_fetch_proc(&feed_arg);
    match res {
        Ok(_) => log::info!("Fetch task completed successfully"),
        Err(e) => log::error!("Fetch task failed: {}", e),
    };
}

pub fn create_feed_task(
    afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    afe_data: *mut esp_sr::esp_afe_sr_data_t,
    i2s0: I2S0,
    gpio_clk: Gpio42,
    gpio_din: Gpio41,
) -> anyhow::Result<esp_idf_svc::sys::TaskHandle_t> {
    use esp_idf_svc::hal;
    use std::ffi::CString;

    // Create the feed task argument
    let feed_task_arg = Box::new(FeedTaskArg {
        afe_handle,
        afe_data,
        i2s0,
        gpio_clk,
        gpio_din,
    });

    // Create the feed task
    let feed_task = unsafe {
        hal::task::create(
            feed_proc,
            &*CString::new("feed_task").unwrap(),
            8 * 1024,
            Box::into_raw(feed_task_arg) as *mut c_void,
            5,
            None,
        )
    }?;

    log::info!("Feed task created successfully");
    Ok(feed_task)
}

pub fn create_fetch_task(
    afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    afe_data: *mut esp_sr::esp_afe_sr_data_t,
    multinet: *mut esp_sr::esp_mn_iface_t,
    model_data: *mut esp_sr::model_iface_data_t,
    transcription_tx: Sender<TranscriptionMessage>,
    transcription_response_rx: Receiver<String>,
) -> anyhow::Result<esp_idf_svc::sys::TaskHandle_t> {
    use esp_idf_svc::hal;
    use std::ffi::CString;

    // Create the fetch task argument with transcription channel
    let fetch_task_arg = Box::new(FetchTaskArg {
        afe_handle,
        afe_data,
        multinet,
        model_data,
        transcription_tx,
        transcription_response_rx,
    });

    // Create the fetch task
    let fetch_task = unsafe {
        hal::task::create(
            fetch_proc,
            &*CString::new("fetch_task").unwrap(),
            8 * 1024,
            Box::into_raw(fetch_task_arg) as *mut c_void,
            5,
            None,
        )
    }?;

    log::info!("Fetch task created successfully");
    Ok(fetch_task)
}
