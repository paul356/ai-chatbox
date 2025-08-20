use anyhow;
use esp_idf_svc::sys::esp_sr;
use std::ffi::CString;

use crate::llm_intf::{ChatRole, LlmHelper};

/// Add this function to print all fields of afe_config
pub fn print_afe_config(afe_config: *const esp_sr::afe_config_t) {
    unsafe {
        log::info!("--- AFE Configuration ---");

        // AEC configuration
        log::info!(
            "AEC: init={}, mode={}, filter_length={}",
            (*afe_config).aec_init,
            (*afe_config).aec_mode,
            (*afe_config).aec_filter_length
        );

        // SE configuration
        log::info!("SE: init={}", (*afe_config).se_init);

        // NS configuration
        log::info!(
            "NS: init={}, mode={}",
            (*afe_config).ns_init,
            (*afe_config).afe_ns_mode
        );

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
        log::info!(
            "WakeNet: init={}, mode={}",
            (*afe_config).wakenet_init,
            (*afe_config).wakenet_mode
        );

        // AGC configuration
        log::info!(
            "AGC: init={}, mode={}, compression_gain_db={}, target_level_dbfs={}",
            (*afe_config).agc_init,
            (*afe_config).agc_mode,
            (*afe_config).agc_compression_gain_db,
            (*afe_config).agc_target_level_dbfs
        );

        // PCM configuration
        log::info!(
            "PCM: total_ch_num={}, mic_num={}, ref_num={}, sample_rate={}",
            (*afe_config).pcm_config.total_ch_num,
            (*afe_config).pcm_config.mic_num,
            (*afe_config).pcm_config.ref_num,
            (*afe_config).pcm_config.sample_rate
        );

        // General AFE configuration
        log::info!("General AFE: mode={}, type={}, preferred_core={}, preferred_priority={}, ringbuf_size={}, linear_gain={}",
            (*afe_config).afe_mode,
            (*afe_config).afe_type,
            (*afe_config).afe_perferred_core,
            (*afe_config).afe_perferred_priority,
            (*afe_config).afe_ringbuf_size,
            (*afe_config).afe_linear_gain);

        log::info!(
            "Memory allocation mode={}, debug_init={}, fixed_first_channel={}",
            (*afe_config).memory_alloc_mode,
            (*afe_config).debug_init,
            (*afe_config).fixed_first_channel
        );

        log::info!("--- End of AFE Configuration ---");
    }
}

/// Test function for LLM functionality with improved error handling
pub fn test_llm_helper() -> anyhow::Result<()> {
    // Get token from environment variable at compile time
    let token = env!("LLM_AUTH_TOKEN");

    log::info!("Creating LlmHelper instance to test DeepSeek API integration");

    // Create LLM helper with error handling
    let mut llm =
        match std::panic::catch_unwind(|| LlmHelper::new(token, "deepseek-chat")) {
            Ok(helper) => helper,
            Err(_) => return Err(anyhow::anyhow!("Failed to initialize LlmHelper")),
        };

    // Configure parameters with reasonable defaults for embedded use
    llm.configure(
        Some(256), // Smaller token count to conserve memory
        Some(0.7), // Temperature - balanced between deterministic and creative
        Some(0.9), // Top-p - slightly more focused sampling
    );

    // Send a test message
    log::info!("Sending test message to DeepSeek API");

    let response = llm.send_message(
        "Hello! I'm testing the ESP32-S3 integration with DeepSeek AI. Can you confirm this is working?".to_string(),
        ChatRole::User
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

/// Initialize speech recognition system and return handles
pub fn init_speech_recognition(
) -> anyhow::Result<(
    *mut esp_sr::esp_afe_sr_iface_t,
    *mut esp_sr::esp_afe_sr_data_t,
    *mut esp_sr::esp_mn_iface_t,
    *mut esp_sr::model_iface_data_t,
)> {
    use esp_idf_svc::sys::esp_sr::{
        afe_config_free, afe_config_init, esp_afe_handle_from_config, esp_mn_commands_add,
        esp_mn_commands_clear, esp_mn_commands_update, esp_mn_handle_from_name, esp_srmodel_filter,
        esp_srmodel_init,
    };

    // Initialize speech recognition models
    let part_name = CString::new("/vfat").unwrap();
    let models = unsafe { esp_srmodel_init(part_name.as_ptr()) };
    if models.is_null() {
        log::error!("Failed to initialize speech recognition models");
        return Err(anyhow::anyhow!(
            "Failed to initialize speech recognition models"
        ));
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

    // Use the macro defined in audio_processing.rs
    macro_rules! call_c_method {
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

    Ok((afe_handle, afe_data, multinet, model_data))
}
