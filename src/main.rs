use anyhow;
use esp_idf_svc::hal::{
    self,
    gpio::{InputPin, OutputPin},
    i2s::{
        config::{
            ClockSource, Config, DataBitWidth, MclkMultiple, PdmDownsample, PdmRxClkConfig,
            PdmRxConfig, PdmRxGpioConfig, PdmRxSlotConfig, SlotMode,
        },
        I2s, I2sDriver, I2sRx,
    },
    peripheral::Peripheral,
    peripherals::{self, Peripherals},
};
use esp_idf_svc::{
    sys,
    wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi},
    nvs::EspDefaultNvsPartition,
    eventloop::EspSystemEventLoop,
};
use hound::{
    WavWriter,
    WavSpec,
};
use heapless;
use std::{ffi::CString, os::raw::c_void, time::Duration};
use std::sync::Arc;
use sys::esp_sr::{
    self, afe_config_free, afe_config_init, esp_afe_handle_from_config, esp_mn_commands_add,
    esp_mn_commands_clear, esp_mn_commands_update, esp_mn_handle_from_name, esp_srmodel_filter,
    esp_srmodel_init, esp_afe_sr_iface_t, esp_afe_sr_data_t, esp_mn_iface_t, model_iface_data_t
};
use sys::{configTICK_RATE_HZ, vTaskDelay};

mod sd_card;
mod llm_intf;

use llm_intf::{LlmHelper, ChatRole};

struct FeedTaskArg {
    afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    afe_data: *mut esp_sr::esp_afe_sr_data_t,
    multinet: *mut esp_sr::esp_mn_iface_t,
    model_data: *mut esp_sr::model_iface_data_t,
}

struct FetchTaskArg {
    afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    afe_data: *mut esp_sr::esp_afe_sr_data_t,
    multinet: *mut esp_sr::esp_mn_iface_t,
    model_data: *mut esp_sr::model_iface_data_t,
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
            if let Some(inner_func) = (*$c_ptr).$method {
                Some(inner_func())
            } else {
               Err(anyhow::anyhow!("Failed to call method {}", stringify!($method)))
            }
        }
    };
    ($c_ptr: expr, $method: ident, $($args: expr),*) => {
        unsafe {
            if let Some(inner_func) = (*$c_ptr).$method {
                Ok(inner_func($($args),*))
            } else {
                Err(anyhow::anyhow!("Failed to call method {}", stringify!($method)))
            }
        }
    };
}

fn inner_feed_proc(feed_arg: &Box<FeedTaskArg>) -> anyhow::Result<()> {
    let peripherals = Peripherals::take()?;
    let mut mic = init_mic(
        peripherals.i2s0,
        peripherals.pins.gpio42,
        peripherals.pins.gpio41,
    )?;

    let chunk_size = call_c_method!(feed_arg.afe_handle, get_feed_chunksize, feed_arg.afe_data)?;

    let channel_num = call_c_method!(feed_arg.afe_handle, get_feed_channel_num, feed_arg.afe_data)?;

    log::info!("[INFO] chunk_size {}, channel_num {}", chunk_size, channel_num);

    let mut chunk = vec![0u8; 2 * chunk_size as usize * channel_num as usize];

    loop {
        mic.read(chunk.as_mut_slice(), 100)?;
        let _ = call_c_method!(feed_arg.afe_handle, feed, feed_arg.afe_data, chunk.as_ptr() as *const i16)?;
    }

    Ok(())
}

extern "C" fn feed_proc(arg: * mut std::ffi::c_void) {
    let feed_arg = unsafe { Box::from_raw(arg as *mut FeedTaskArg) };

    inner_feed_proc(&feed_arg).unwrap();
}

// Define the State enum
#[derive(Debug, Clone, Copy, PartialEq)]
enum State {
    WAKE_WORD_DETECTING,
    CMD_DETECTING,
    RECORDING,
}

// Add a function to print afe_fetch_result_t fields
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

// Add this helper function to flush FatFs filesystem
fn flush_filesystem(mount_point: &str) -> anyhow::Result<()> {
    // Create a temporary file to force a flush of the file system
    let flush_path = format!("{}/flush.tmp", mount_point);
    {
        let file = std::fs::File::create(&flush_path)?;
        file.sync_all()?; // This calls fsync() which flushes dirty data
    }
    // Remove the temporary file
    std::fs::remove_file(&flush_path)?;

    // On ESP-IDF, we can also directly call the esp_vfs_fat_sdmmc_unmount function
    // But we need to use the unsafe FFI call
    unsafe {
        #[allow(non_snake_case)]
        extern "C" {
            fn esp_vfs_fat_sdcard_unmount(mount_point: *const std::os::raw::c_char, card: *mut std::os::raw::c_void) -> i32;
        }

        // This is a more aggressive approach - unmount and remount
        // Only use if the above sync_all approach doesn't work
        /*
        let c_mount_point = CString::new(mount_point)?;
        let result = esp_vfs_fat_sdcard_unmount(c_mount_point.as_ptr(), std::ptr::null_mut());
        if result != 0 {
            log::warn!("Failed to unmount SD card: {}", result);
        }

        // Remount the card
        let mut sd = sd_card::SdCard::new(mount_point);
        sd.mount_spi()?;
        */
    }

    log::info!("Filesystem at {} flushed successfully", mount_point);
    Ok(())
}

// Modify the RECORDING state code to flush data after finalizing WAV file
fn inner_fetch_proc(arg: &Box<FetchTaskArg>) -> anyhow::Result<()> {
    let afe_handle = arg.afe_handle;
    let afe_data = arg.afe_data;
    let multinet = arg.multinet;
    let model_data = arg.model_data;

    // Initialize state
    let mut state = State::WAKE_WORD_DETECTING;

    // For recording WAV files
    let mut file_idx = 0;
    let mut wav_writer: Option<WavWriter<std::io::BufWriter<std::fs::File>>> = None;

    // For tracking silence duration
    let mut silence_frames = 0;
    let frames_per_second = 16000 / 480; // Assuming 30ms frames at 16kHz (adjust based on your frame size)

    log::info!("Starting detection loop with initial state: {:?}", state);

    loop {
        // Always fetch data from AFE
        let res = call_c_method!(afe_handle, fetch, afe_data)?;
        if res.is_null() || unsafe { (*res).ret_value } == esp_sr::ESP_FAIL {
            log::error!("Fetch error!");
            break;
        }

        // Handle the data based on current state
        match state {
            State::WAKE_WORD_DETECTING => {
                if unsafe { (*res).wakeup_state } == esp_sr::wakenet_state_t_WAKENET_DETECTED {
                    let next_state = State::CMD_DETECTING;
                    log::info!("Wake word detected. State transition: {:?} -> {:?}", state, next_state);

                    call_c_method!(afe_handle, disable_wakenet, afe_data)?;
                    call_c_method!(multinet, clean, model_data)?;
                    state = next_state;
                }
            },

            State::CMD_DETECTING => {
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

                    let next_state = State::RECORDING;
                    log::info!("Command detected (ID: {}). State transition: {:?} -> {:?}",
                        command_id_str, state, next_state);

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
                    let next_state = State::WAKE_WORD_DETECTING;
                    log::info!("Command detection timeout. State transition: {:?} -> {:?}", state, next_state);

                    call_c_method!(afe_handle, enable_wakenet, afe_data)?;
                    state = next_state;
                }
            },

            State::RECORDING => {
                // Print details about the fetch result for debugging
                //print_fetch_result(res);

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
                    if silence_frames >= frames_per_second { // More than 1 second of silence
                        let next_state = State::WAKE_WORD_DETECTING;
                        log::info!("Detected {} frames of silence. State transition: {:?} -> {:?}",
                            silence_frames, state, next_state);

                        // Finalize WAV file
                        if let Some(mut writer) = wav_writer.take() {
                            log::info!("Finalizing WAV file after {} silent frames", silence_frames);
                            writer.finalize()?;

                            // Flush the filesystem to ensure all data is written
                            if let Err(e) = flush_filesystem("/vfat") {
                                log::warn!("Failed to flush filesystem: {}", e);
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

    Ok(())
}

extern "C" fn fetch_proc(arg: * mut std::ffi::c_void) {
    let feed_arg = unsafe { Box::from_raw(arg as *mut FetchTaskArg) };

    let res = inner_fetch_proc(&feed_arg);
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

// Update WiFi initialization function
fn initialize_wifi() -> anyhow::Result<()> {
    // Get SSID and password from compile-time environment variables
    let ssid = env!("WIFI_SSID");
    let pass = env!("WIFI_PASS");

    log::info!("Connecting to WiFi network: {}", ssid);

    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let peripherals = Peripherals::take()?;

    let mut wifi = EspWifi::new(peripherals.modem, sys_loop.clone(), Some(nvs))?;

    let mut auth_method = AuthMethod::WPA2Personal;
    if pass.is_empty() {
        auth_method = AuthMethod::None;
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

    wifi.connect()?;
    log::info!("Waiting for connection...");

    // Give it some time to connect
    std::thread::sleep(std::time::Duration::from_secs(5));

    if wifi.is_connected()? {
        log::info!("WiFi connected successfully!");
        let ip_info = wifi.sta_netif().get_ip_info()?;
        log::info!("IP info: {:?}", ip_info);
    } else {
        log::warn!("WiFi not connected after timeout!");
    }

    Ok(())
}

// Test function for LLM functionality
fn test_llm_helper() -> anyhow::Result<()> {
    let token = env!("LLM_AUTH_TOKEN");

    log::info!("Creating LlmHelper instance to test DeepSeek API integration");
    let mut llm = llm_intf::LlmHelper::new(token, "deepseek-chat");

    // Configure parameters
    llm.configure(Some(256), Some(0.7), Some(0.9));

    // Send a test message
    log::info!("Sending test message to DeepSeek API");
    let response = llm.send_message(
        "Hello! I'm testing the ESP32-S3 integration with DeepSeek AI. Can you confirm this is working?".to_string(),
        llm_intf::ChatRole::User
    );

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

    // Connect to Wi-Fi using compile-time environment variables
    match initialize_wifi() {
        Ok(_) => log::info!("WiFi connected successfully"),
        Err(e) => log::error!("Failed to connect to WiFi: {}", e),
    }

    // Test the LLM helper
    match test_llm_helper() {
        Ok(_) => log::info!("LLM test completed successfully"),
        Err(e) => log::error!("LLM test failed: {}", e),
    }

    let mut sd = sd_card::SdCard::new("/vfat");
    sd.mount_spi()?;

    let part_name = CString::new("model").unwrap();
    let models = unsafe { esp_srmodel_init(part_name.as_ptr()) };

    let input_format = CString::new("M").unwrap();
    let afe_config = unsafe {
        afe_config_init(
            input_format.as_ptr(),
            models,
            esp_sr::afe_type_t_AFE_TYPE_SR,
            esp_sr::afe_mode_t_AFE_MODE_LOW_COST,
        )
    };

    // Print the AFE configuration
    print_afe_config(afe_config);

    let afe_handle = unsafe { esp_afe_handle_from_config(afe_config) };
    let afe_data = call_c_method!(afe_handle, create_from_config, afe_config)?;
    unsafe { afe_config_free(afe_config) };

    //let multinet: *mut esp_sr::esp_mn_iface_t = std::ptr::null_mut();
    //let model_data: *mut esp_sr::model_iface_data_t = std::ptr::null_mut();

    let prefix_str = Vec::from(esp_sr::ESP_MN_PREFIX);
    let chinese_str = Vec::from(esp_sr::ESP_MN_CHINESE);
    let mn_name = unsafe {
        esp_srmodel_filter(
            models,
            prefix_str.as_ptr() as *const i8,
            chinese_str.as_ptr() as *const i8,
        )
    };

    let multinet = unsafe { esp_mn_handle_from_name(mn_name) };
    let model_data = call_c_method!(multinet, create, mn_name, 6000)?;

    unsafe {
        esp_mn_commands_clear();
        esp_mn_commands_add(1, Vec::from(b"wo you wen ti\0").as_ptr() as *const i8);
        esp_mn_commands_update();
    }

    let feed_task_arg = Box::new(FeedTaskArg {
        afe_handle,
        afe_data,
        multinet,
        model_data,
    });

    let _ = unsafe {
        hal::task::create(feed_proc, &*CString::new("feed_task").unwrap(), 8 * 1024, Box::into_raw(feed_task_arg) as *mut c_void, 5, None)
    }?;

    let fetch_task_arg = Box::new(FetchTaskArg {
        afe_handle,
        afe_data,
        multinet,
        model_data,
    });

    let _ = unsafe {
        hal::task::create(fetch_proc, &*CString::new("fetch_task").unwrap(), 8 * 1024, Box::into_raw(fetch_task_arg) as *mut c_void, 5, None)
    }?;

    loop {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    // Clean up resources
    let _ = call_c_method!(multinet, destroy, model_data);
    let _ = call_c_method!(afe_handle, destroy, afe_data);

    // Flush filesystem before exit
    let _ = flush_filesystem("/vfat");

    Ok(())
}
