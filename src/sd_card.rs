use std::ffi::CString;
use std::ffi::c_uint;
use std::ffi::c_void;
use esp_idf_svc::sys::esp;
use esp_idf_svc::sys::{
    esp_vfs_fat_sdcard_unmount, esp_vfs_fat_sdmmc_mount, esp_vfs_fat_sdmmc_mount_config_t,
    sdmmc_card_t, sdmmc_host_deinit, sdmmc_host_do_transaction, sdmmc_host_get_dma_info,
    sdmmc_host_get_real_freq, sdmmc_host_get_slot_width, sdmmc_host_init, sdmmc_host_io_int_enable,
    sdmmc_host_io_int_wait, sdmmc_host_set_bus_ddr_mode, sdmmc_host_set_bus_width,
    sdmmc_host_set_card_clk, sdmmc_host_set_cclk_always_on, sdmmc_host_set_input_delay,
    sdmmc_host_t, sdmmc_host_t__bindgen_ty_1, sdmmc_slot_config_t,
    sdmmc_slot_config_t__bindgen_ty_1, sdmmc_slot_config_t__bindgen_ty_2, ESP_OK,
};
use esp_idf_svc::sys::{
    sdspi_host_init, sdspi_host_set_card_clk, sdspi_host_do_transaction, sdspi_host_remove_device, sdspi_host_io_int_enable, sdspi_host_io_int_wait, sdspi_host_get_real_freq, sdspi_host_get_dma_info, spi_bus_initialize, esp_vfs_fat_sdspi_mount, sdspi_device_config_t, spi_bus_config_t,
};
use esp_idf_svc::sys;

const SDMMC_SLOT_FLAG_INTERNAL_PULLUP: c_uint = 1 << 0;
const SDMMC_HOST_FLAG_1BIT: c_uint = 1 << 0;
const SDMMC_HOST_FLAG_4BIT: c_uint = 1 << 1;
const SDMMC_HOST_FLAG_8BIT: c_uint = 1 << 2;
const SDMMC_HOST_FLAG_SPI: c_uint = 1 << 3;
const SDMMC_HOST_FLAG_DDR: c_uint = 1 << 4;
const SDMMC_HOST_FLAG_DEINIT_ARG: c_uint = 1 << 5;
const SDMMC_HOST_SLOT_1: i32 = 1;
const SDMMC_FREQ_DEFAULT: i32 = 20000;
const SDMMC_DELAY_PHASE_0: u32 = 0;
const SDSPI_DEFAULT_HOST: i32 = 2;
const SDSPI_DEFAULT_DMA: u32 = 3;

pub struct SdCard {
    mount_point: CString,
    card_handle: *mut sdmmc_card_t,
}

impl SdCard {
    pub fn new(mpoint: &str) -> Self {
        let mount_point = CString::new(mpoint).unwrap();
        let card_handle: *mut sdmmc_card_t = std::ptr::null_mut();

        Self {
            mount_point,
            card_handle,
        }
    }

    pub fn mount_sdmmc(&mut self) -> anyhow::Result<()> {
        let sdmmc_mount_config = esp_vfs_fat_sdmmc_mount_config_t {
            format_if_mount_failed: false,
            max_files: 4,
            allocation_unit_size: 16 * 1024,
            disk_status_check_enable: false,
            use_one_fat: false,
        };

        let sd_host = sdmmc_host_t {
            flags: SDMMC_HOST_FLAG_1BIT
                | SDMMC_HOST_FLAG_4BIT
                | SDMMC_HOST_FLAG_8BIT
                | SDMMC_HOST_FLAG_DDR,
            slot: SDMMC_HOST_SLOT_1,
            max_freq_khz: SDMMC_FREQ_DEFAULT,
            io_voltage: 3.3,
            init: Some(sdmmc_host_init),
            set_bus_width: Some(sdmmc_host_set_bus_width),
            get_bus_width: Some(sdmmc_host_get_slot_width),
            set_bus_ddr_mode: Some(sdmmc_host_set_bus_ddr_mode),
            set_card_clk: Some(sdmmc_host_set_card_clk),
            set_cclk_always_on: Some(sdmmc_host_set_cclk_always_on),
            do_transaction: Some(sdmmc_host_do_transaction),
            __bindgen_anon_1: sdmmc_host_t__bindgen_ty_1 {
                deinit: Some(sdmmc_host_deinit),
            },
            io_int_enable: Some(sdmmc_host_io_int_enable),
            io_int_wait: Some(sdmmc_host_io_int_wait),
            command_timeout_ms: 0,
            get_real_freq: Some(sdmmc_host_get_real_freq),
            input_delay_phase: SDMMC_DELAY_PHASE_0,
            set_input_delay: Some(sdmmc_host_set_input_delay),
            dma_aligned_buffer: std::ptr::null_mut(),
            pwr_ctrl_handle: std::ptr::null_mut(),
            get_dma_info: Some(sdmmc_host_get_dma_info),
        };

        let slot_config = sdmmc_slot_config_t {
            clk: 7,
            cmd: 15,
            d0: 6,
            d1: 5,
            d2: 17,
            d3: 16,
            d4: -1,
            d5: -1,
            d6: -1,
            d7: -1,
            __bindgen_anon_1: sdmmc_slot_config_t__bindgen_ty_1 { cd: 4 },
            __bindgen_anon_2: sdmmc_slot_config_t__bindgen_ty_2 { wp: -1 },
            width: 4,
            flags: 0,
        };

        let mut card_handle: *mut sdmmc_card_t = std::ptr::null_mut();

        let ret = unsafe {
            esp_vfs_fat_sdmmc_mount(
                self.mount_point.as_ptr(),
                &sd_host,
                &slot_config as *const sdmmc_slot_config_t as *const c_void,
                &sdmmc_mount_config,
                &mut card_handle,
            )
        };

        if ret != ESP_OK {
            log::error!("Failed to mount SD Card");
            esp! { ret }?;
        }

        Ok(())
    }

    pub fn mount_spi(&mut self) -> anyhow::Result<()> {
        let sdmmc_mount_config = esp_vfs_fat_sdmmc_mount_config_t {
            format_if_mount_failed: false,
            max_files: 4,
            allocation_unit_size: 16 * 1024,
            disk_status_check_enable: false,
            use_one_fat: false,
        };

        let sd_host = sdmmc_host_t {
            flags: SDMMC_HOST_FLAG_SPI | SDMMC_HOST_FLAG_DEINIT_ARG,
            slot: SDSPI_DEFAULT_HOST,
            max_freq_khz: SDMMC_FREQ_DEFAULT,
            io_voltage: 3.3,
            init: Some(sdspi_host_init),
            set_bus_width: None,
            get_bus_width: None,
            set_bus_ddr_mode: None,
            set_card_clk: Some(sdspi_host_set_card_clk),
            set_cclk_always_on: None,
            do_transaction: Some(sdspi_host_do_transaction),
            __bindgen_anon_1: sdmmc_host_t__bindgen_ty_1 {
                deinit_p: Some(sdspi_host_remove_device),
            },
            io_int_enable: Some(sdspi_host_io_int_enable),
            io_int_wait: Some(sdspi_host_io_int_wait),
            command_timeout_ms: 0,
            get_real_freq: Some(sdspi_host_get_real_freq),
            input_delay_phase: SDMMC_DELAY_PHASE_0,
            set_input_delay: None,
            dma_aligned_buffer: std::ptr::null_mut(),
            pwr_ctrl_handle: std::ptr::null_mut(),
            get_dma_info: Some(sdspi_host_get_dma_info),
        };

        let bus_cfg = spi_bus_config_t {
            __bindgen_anon_1: sys::spi_bus_config_t__bindgen_ty_1 {
                mosi_io_num: 9,
            },
            __bindgen_anon_2: sys::spi_bus_config_t__bindgen_ty_2 {
                miso_io_num: 8,
            },
            sclk_io_num: 7,
            __bindgen_anon_3: sys::spi_bus_config_t__bindgen_ty_3 {
                quadwp_io_num: -1,
            },
            __bindgen_anon_4: sys::spi_bus_config_t__bindgen_ty_4 {
                quadhd_io_num: -1,
            },
            data4_io_num: -1,
            data5_io_num: -1,
            data6_io_num: -1,
            data7_io_num: -1,
            max_transfer_sz: 0,
            flags: 0,
            isr_cpu_id: 0,
            intr_flags: 0,
        };

        let ret = unsafe { spi_bus_initialize(sd_host.slot as u32, &bus_cfg, SDSPI_DEFAULT_DMA) };
        if ret != ESP_OK {
            log::error!("Failed to initialize SPI bus");
            esp! { ret }?;
        }

        let slot_config = sdspi_device_config_t {
            host_id: sd_host.slot as u32,
            gpio_cs: 21,
            gpio_cd: -1,
            gpio_wp: -1,
            gpio_int: -1,
            gpio_wp_polarity: false,
        };

        let ret = unsafe { esp_vfs_fat_sdspi_mount(self.mount_point.as_ptr(), &sd_host, &slot_config, &sdmmc_mount_config, &mut self.card_handle) };
        if ret != ESP_OK {
            log::error!("Failed to mount SD Card");
            esp! { ret }?;
        }

        Ok(())
    }
}

impl Drop for SdCard {
    fn drop(&mut self) {
        if self.card_handle != std::ptr::null_mut() {
            unsafe { esp_vfs_fat_sdcard_unmount(self.mount_point.as_ptr(), self.card_handle); }
        }
    }
}
