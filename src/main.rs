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
use std::sync::mpsc::{self, Sender, Receiver};

// Define the ActionRequest enum
enum ActionRequest {
    CommandDetected(i32), // Represents a detected command with an ID
    OtherAction(i32),     // Represents other actions with an ID
}

mod sd_card;

struct FeedTaskArg {
    afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    afe_data: *mut esp_sr::esp_afe_sr_data_t,
    multinet: *mut esp_sr::esp_mn_iface_t,
    model_data: *mut esp_sr::model_iface_data_t,
    receiver: Receiver<ActionRequest>, // Add Receiver to the feed task arguments
}

struct FetchTaskArg {
    afe_handle: *mut esp_sr::esp_afe_sr_iface_t,
    afe_data: *mut esp_sr::esp_afe_sr_data_t,
    multinet: *mut esp_sr::esp_mn_iface_t,
    model_data: *mut esp_sr::model_iface_data_t,
    sender: Sender<ActionRequest>, // Add Sender to the fetch task arguments
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

    //let mut file_idx = 0;
    loop {
        // Check for messages from the fetch task
        if let Ok(message) = feed_arg.receiver.try_recv() {
            match message {
                ActionRequest::CommandDetected(command_id) => {
                    log::info!("[FEED TASK] Command detected with ID: {}", command_id);
                    // Handle the command (e.g., adjust behavior based on the command ID)
                }
                ActionRequest::OtherAction(action_id) => {
                    log::info!("[FEED TASK] Other action requested with ID: {}", action_id);
                    // Handle other actions
                }
            }
        }

        mic.read(chunk.as_mut_slice(), 100)?;
        let _ = call_c_method!(feed_arg.afe_handle, feed, feed_arg.afe_data, chunk.as_ptr() as *const i16)?;

    }

    Ok(())
}

extern "C" fn feed_proc(arg: * mut std::ffi::c_void) {
    let feed_arg = unsafe { Box::from_raw(arg as *mut FeedTaskArg) };

    inner_feed_proc(&feed_arg).unwrap();
}

fn inner_fetch_proc(arg: &Box<FetchTaskArg>) -> anyhow::Result<()> {
    let afe_handle = arg.afe_handle;
    let afe_data = arg.afe_data;
    let multinet = arg.multinet;
    let model_data = arg.model_data;

    let mut detect_flag: bool = false;
    let mut count: usize = 0;
    log::info!("Starting detection loop");
    loop {
        let res = call_c_method!(afe_handle, fetch, afe_data)?;
        if res.is_null() || unsafe { (*res).ret_value } == esp_sr::ESP_FAIL {
            log::error!("Fetch error!");
            break;
        }

        if unsafe { (*res).wakeup_state } == esp_sr::wakenet_state_t_WAKENET_DETECTED {
            log::info!("Wakeword detected");
            call_c_method!(afe_handle, disable_wakenet, afe_data)?;
            call_c_method!(multinet, clean, model_data)?;
            detect_flag = true;
        } else if detect_flag != true && (count % 10) == 0 {
            unsafe {
                log::info!("[INFO] wakeup_state {}, vad_state {}", (*res).wakeup_state, (*res).vad_state);
            }
        }
        count += 1;

        if detect_flag {
            let mn_state = call_c_method!(multinet, detect, model_data, (*res).data)?;
            if mn_state == esp_sr::esp_mn_state_t_ESP_MN_STATE_DETECTED {
                let mn_result = call_c_method!(multinet, get_results, model_data)?;
                for i in 0..unsafe { (*mn_result).num as usize } {
                    let command_id = unsafe { (*mn_result).command_id[i] };
                    log::info!("Command detected: {}", command_id);

                    // Send the detected command as an ActionRequest to the feed task
                    if let Err(err) = arg.sender.send(ActionRequest::CommandDetected(command_id)) {
                        log::error!("[FETCH TASK] Failed to send message: {}", err);
                    }
                }
            } else if mn_state == esp_sr::esp_mn_state_t_ESP_MN_STATE_TIMEOUT {
                log::info!("Timeout, no command detected");
                call_c_method!(afe_handle, enable_wakenet, afe_data)?;
                detect_flag = false;
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
        esp_mn_commands_add(1, Vec::from(b"hao gao xiao\0").as_ptr() as *const i8);
        esp_mn_commands_add(2, Vec::from(b"ni zhen bang\0").as_ptr() as *const i8);
        esp_mn_commands_update();
    }

    // Create a communication channel
    let (sender, receiver) = mpsc::channel();

    let feed_task_arg = Box::new(FeedTaskArg {
        afe_handle,
        afe_data,
        multinet,
        model_data,
        receiver, // Pass the Receiver to the feed task
    });

    let _ = unsafe {
        hal::task::create(feed_proc, &*CString::new("feed_task").unwrap(), 8 * 1024, Box::into_raw(feed_task_arg) as *mut c_void, 5, None)
    }?;

    let fetch_task_arg = Box::new(FetchTaskArg {
        afe_handle,
        afe_data,
        multinet,
        model_data,
        sender, // Pass the Sender to the fetch task
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
