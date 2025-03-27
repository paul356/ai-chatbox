use anyhow;
use esp_idf_svc::hal::{
    gpio::{Gpio21, InputPin, OutputPin},
    i2s::{
        config::{
            ClockSource, Config, DataBitWidth, MclkMultiple, PdmDownsample, PdmRxClkConfig,
            PdmRxConfig, PdmRxGpioConfig, PdmRxSlotConfig, SlotMode,
        },
        I2s, I2sDriver, I2sRx, I2S0,
    },
    peripheral::Peripheral,
    peripherals::{self, Peripherals},
    sd::{config::Configuration, spi::SdSpiHostDriver, SdCardDriver},
    spi::{Dma, SpiDriver, SpiDriverConfig},
    sys::{self, esp},
};
use esp_idf_svc::sys::esp_sr::{afe_config_init, esp_srmodel_init};
use esp_idf_svc::sys::{configTICK_RATE_HZ, vTaskDelay};
use std::ffi::CString;

sys::esp_app_desc!();

fn init_mic<'d>(
    i2s_slot: impl Peripheral<P = impl I2s> + 'd,
    clk: impl Peripheral<P = impl OutputPin> + 'd,
    din: impl Peripheral<P = impl InputPin> + 'd,
) -> anyhow::Result<I2sDriver<'d, I2sRx>> {
    let pdm_rx_cfg = PdmRxConfig::new(
        Config::default(),
        PdmRxClkConfig::from_sample_rate_hz(44100)
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

fn main() -> anyhow::Result<()> {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();

    // let part_name = CString::new("model").unwrap();
    // let models = unsafe { esp_srmodel_init(part_name.as_ptr()) };

    let peripherals = Peripherals::take()?;
    let mut mic = init_mic(
        peripherals.i2s0,
        peripherals.pins.gpio42,
        peripherals.pins.gpio41,
    )?;

    let spi_driver_cfg = SpiDriverConfig::new().dma(Dma::Auto(4000));
    let spi_driver = SpiDriver::new(
        peripherals.spi2,
        peripherals.pins.gpio7,
        peripherals.pins.gpio9,
        Some(peripherals.pins.gpio8),
        &spi_driver_cfg,
    )?;

    log::info!("Before sd spi host driver");
    let sd_spi_driver = SdSpiHostDriver::new(
        &spi_driver,
        Some(peripherals.pins.gpio21),
        Option::<Gpio21>::None,
        Option::<Gpio21>::None,
        Option::<Gpio21>::None,
        Some(false),
    )?;

    let sdcard_cfg = Configuration::new();

    log::info!("Before sd card driver");
    let sdcard_driver = SdCardDriver::new_spi(sd_spi_driver, &sdcard_cfg)?;

    /*let mut sdcard = sd_card::SdCard::new("/vfat");
    sdcard.mount_spi()?;*/

    log::info!("Before entering loop");
    loop {
        let mut data = vec![0u8; 1024];
        let res = mic.read(data.as_mut_slice(), 100)?;
        log::info!("Read {} bytes from microphone", res);

        unsafe { vTaskDelay(1 * configTICK_RATE_HZ) };
    }

    Ok(())
}
