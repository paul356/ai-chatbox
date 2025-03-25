use std::ffi::CString;
use esp_idf_svc::sys::esp_sr::{esp_srmodel_init, afe_config_init};

fn main() {
    // It is necessary to call this function once. Otherwise some patches to the runtime
    // implemented by esp-idf-sys might not link properly. See https://github.com/esp-rs/esp-idf-template/issues/71
    esp_idf_svc::sys::link_patches();

    // Bind the log crate to the ESP Logging facilities
    esp_idf_svc::log::EspLogger::initialize_default();


    let part_name = CString::new("model").unwrap();
    let models = unsafe { esp_srmodel_init(part_name.as_ptr()) };

    log::info!("Hello, world!");
}
