use anyhow;
use esp_idf_svc::hal::{
    gpio::{InputPin, OutputPin, PinDriver},
    i2s::{
        config::{
            ClockSource, Config, DataBitWidth, MclkMultiple, PdmDownsample, PdmRxClkConfig,
            PdmRxConfig, PdmRxGpioConfig, PdmRxSlotConfig, SlotMode, StdClkConfig, StdConfig,
            StdGpioConfig, StdSlotConfig
        },
        I2s, I2sDriver, I2sRx, I2sTx, I2S1,
    },
    peripheral::Peripheral,
};

/// Initialize microphone with PDM configuration
pub fn init_mic<'d>(
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

/// Initialize I2S TX for audio output (MAX98357 compatible)
pub fn init_i2s_tx(
    i2s_slot: I2S1,
    bclk_pin: impl Peripheral<P = impl InputPin + OutputPin> + 'static,
    dout_pin: impl Peripheral<P = impl OutputPin> + 'static,
    ws_pin: impl Peripheral<P = impl InputPin + OutputPin> + 'static,
) -> anyhow::Result<I2sDriver<'static, I2sTx>> {
    log::info!("Starting MAX98357 I2S audio test");

    // Configure I2S for audio output
    let sample_rate = 16000u32;
    let i2s_config = StdConfig::new(
        Config::default(),
        StdClkConfig::from_sample_rate_hz(sample_rate),
        StdSlotConfig::philips_slot_default(DataBitWidth::Bits16, SlotMode::Mono),
        StdGpioConfig::default(),
    );

    let mut i2s_driver = I2sDriver::new_std_tx(
        i2s_slot,
        &i2s_config,
        bclk_pin,                                      // BCLK
        dout_pin,                                      // DOUT (connects to DIN on MAX98357)
        Option::<esp_idf_svc::hal::gpio::Gpio0>::None, // MCLK (not needed for MAX98357)
        ws_pin,                                        // WS (LRCLK)
    )?;

    log::info!("I2S driver initialized successfully");

    // Enable the I2S channel
    i2s_driver.tx_enable()?;
    log::info!("I2S TX channel enabled");

    Ok(i2s_driver)
}

/// Configure MAX98357 control pins
pub fn configure_max98357_pins(
    sd_pin: impl Peripheral<P = impl OutputPin> + 'static,
) -> anyhow::Result<PinDriver<'static, impl OutputPin, esp_idf_svc::hal::gpio::Output>> {
    // SD pin (GPIO5) - shutdown control (active low)
    let mut sd_pin_driver = PinDriver::output(sd_pin)?;
    sd_pin_driver.set_low()?; // Enable the amplifier (not shutdown)

    log::info!("MAX98357 control pins configured");

    Ok(sd_pin_driver)
}
