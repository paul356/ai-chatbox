use std::ffi::CString;
use esp_idf_svc::sys::esp_sr::{afe_config_init, esp_srmodel_init};
use esp_idf_svc::sys::{
    configTICK_RATE_HZ, i2s_chan_config_t, i2s_chan_config_t__bindgen_ty_1, i2s_chan_handle_t, i2s_channel_enable, i2s_channel_init_pdm_rx_mode, i2s_channel_read, i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT, i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256, i2s_new_channel, i2s_pdm_dsr_t_I2S_PDM_DSR_8S, i2s_pdm_rx_clk_config_t, i2s_pdm_rx_config_t, i2s_pdm_rx_gpio_config_t, i2s_pdm_rx_gpio_config_t__bindgen_ty_1, i2s_pdm_rx_gpio_config_t__bindgen_ty_2, i2s_pdm_rx_slot_config_t, i2s_pdm_slot_mask_t_I2S_PDM_SLOT_LEFT, i2s_port_t_I2S_NUM_0, i2s_port_t_I2S_NUM_AUTO, i2s_role_t_I2S_ROLE_MASTER, i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_AUTO, i2s_slot_mode_t_I2S_SLOT_MODE_MONO, soc_periph_i2s_clk_src_t_I2S_CLK_SRC_DEFAULT, vTaskDelay
};
use anyhow;

fn init_mic() -> anyhow::Result<i2s_chan_handle_t> {
    let chan_config = i2s_chan_config_t {
        id: i2s_port_t_I2S_NUM_0,
        role: i2s_role_t_I2S_ROLE_MASTER,
        dma_desc_num: 6,
        dma_frame_num: 240,
        __bindgen_anon_1: i2s_chan_config_t__bindgen_ty_1 {
            auto_clear_after_cb: false
        },
        auto_clear_before_cb: false,
        intr_priority: 0,
    };

    let  mut invert_flags = i2s_pdm_rx_gpio_config_t__bindgen_ty_2::default();
    invert_flags.set_clk_inv(0);

    let pdm_rx_cfg = i2s_pdm_rx_config_t {
        clk_cfg: i2s_pdm_rx_clk_config_t {
            sample_rate_hz: 44100,
            clk_src: soc_periph_i2s_clk_src_t_I2S_CLK_SRC_DEFAULT,
            mclk_multiple: i2s_mclk_multiple_t_I2S_MCLK_MULTIPLE_256,
            dn_sample_mode: i2s_pdm_dsr_t_I2S_PDM_DSR_8S,
            bclk_div: 8,
        },
        slot_cfg: i2s_pdm_rx_slot_config_t {
            data_bit_width: i2s_data_bit_width_t_I2S_DATA_BIT_WIDTH_16BIT,
            slot_bit_width: i2s_slot_bit_width_t_I2S_SLOT_BIT_WIDTH_AUTO,
            slot_mode: i2s_slot_mode_t_I2S_SLOT_MODE_MONO,
            slot_mask: i2s_pdm_slot_mask_t_I2S_PDM_SLOT_LEFT,
        },
        gpio_cfg: i2s_pdm_rx_gpio_config_t {
            clk: 42,
            __bindgen_anon_1: i2s_pdm_rx_gpio_config_t__bindgen_ty_1 {
                din: 41,
            },
            invert_flags: invert_flags,
        },
    };

    let mut rx_handle: i2s_chan_handle_t = std::ptr::null_mut();
    let res = unsafe { i2s_new_channel(& chan_config, std::ptr::null_mut(), &mut rx_handle) };
    if res != 0 {
        return Err(anyhow::anyhow!("Failed to create I2S channel"));
    }

    let res = unsafe { i2s_channel_init_pdm_rx_mode(rx_handle, &pdm_rx_cfg) };
    if res != 0 {
        return Err(anyhow::anyhow!("Failed to init pdm rx mode"));
    }

    let res = unsafe { i2s_channel_enable(rx_handle) };
    if res != 0 {
        return Err(anyhow::anyhow!("Failed to enable I2S channel"));
    }

    Ok(rx_handle)
}

fn main() {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();


    // let part_name = CString::new("model").unwrap();
    // let models = unsafe { esp_srmodel_init(part_name.as_ptr()) };

    let mic = init_mic();
    if let Err(err) = init_mic() {
        log::error!("Failed to initialize microphone: {:?}", err);
        return;
    }

    let mic = mic.unwrap();
    while true {
        let mut data = vec![0u8; 1024];
        let mut bytes_read: usize = 0;
        let res = unsafe { i2s_channel_read(mic, data.as_mut_ptr() as * mut std::ffi::c_void, 1024, &mut bytes_read, 100) };
        if res != 0 {
            log::error!("Failed to read data from microphone");
            return;
        }
        log::info!("Read {} bytes from microphone", bytes_read);

        unsafe { vTaskDelay(1 * configTICK_RATE_HZ) };
    }
    log::info!("Hello, world!");
}
