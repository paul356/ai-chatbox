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
use esp_idf_svc::sys;
use hound::{
    WavWriter,
    WavSpec,
};
use std::{ffi::CString, os::raw::c_void};
use std::boxed;
use sys::esp_sr::{
    self, afe_config_free, afe_config_init, esp_afe_handle_from_config, esp_mn_commands_add,
    esp_mn_commands_clear, esp_mn_commands_update, esp_mn_handle_from_name, esp_srmodel_filter,
    esp_srmodel_init, esp_afe_sr_iface_t, esp_afe_sr_data_t, esp_mn_iface_t, model_iface_data_t
};
use sys::{configTICK_RATE_HZ, vTaskDelay};

mod sd_card;

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

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    //let mut sd = sd_card::SdCard::new("/vfat");
    //sd.mount_spi()?;

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

    Ok(())
}
