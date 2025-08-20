use anyhow;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::{
    eventloop::EspSystemEventLoop,
    nvs::EspDefaultNvsPartition,
    wifi::{AuthMethod, ClientConfiguration, Configuration, EspWifi},
};
use heapless;

/// Enhanced WiFi initialization function with better error handling and reconnection logic
pub fn initialize_wifi(modem: Modem) -> anyhow::Result<Box<EspWifi<'static>>> {
    // Get SSID and password from environment variables (compile-time)
    let ssid = env!("WIFI_SSID");
    let pass = env!("WIFI_PASS");

    log::info!("Connecting to WiFi network: {}", ssid);

    let sys_loop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;

    let mut wifi = EspWifi::new(modem, sys_loop.clone(), Some(nvs))?;

    let mut auth_method = AuthMethod::WPA2Personal;
    if pass.is_empty() {
        auth_method = AuthMethod::None;
        log::info!("Using open WiFi network (no password)");
    }

    let mut client_config = ClientConfiguration {
        ssid: heapless::String::new(),
        password: heapless::String::new(),
        auth_method,
        ..Default::default()
    };

    // Copy SSID and password into heapless Strings
    client_config
        .ssid
        .push_str(ssid)
        .map_err(|_| anyhow::anyhow!("SSID too long"))?;
    client_config
        .password
        .push_str(pass)
        .map_err(|_| anyhow::anyhow!("Password too long"))?;

    wifi.set_configuration(&Configuration::Client(client_config))?;

    wifi.start()?;
    log::info!("WiFi started, connecting...");

    // Try to connect with retries
    let max_retries = 3;
    let mut connected = false;

    for attempt in 1..=max_retries {
        match wifi.connect() {
            Ok(_) => {
                log::info!(
                    "WiFi connect initiated (attempt {}/{}), waiting for connection...",
                    attempt,
                    max_retries
                );

                // Wait for connection with timeout
                let max_wait_seconds = 15; // Increased timeout for DHCP
                let mut has_valid_ip = false;

                for _i in 1..=max_wait_seconds {
                    std::thread::sleep(std::time::Duration::from_secs(1));

                    // First check if connected
                    if let Ok(true) = wifi.is_connected() {
                        connected = true;

                        // Then verify we have a valid IP address (not 0.0.0.0)
                        if let Ok(ip_info) = wifi.sta_netif().get_ip_info() {
                            if ip_info.ip != std::net::Ipv4Addr::new(0, 0, 0, 0) {
                                log::info!("Valid IP address obtained: {}", ip_info.ip);
                                log::info!("Subnet mask: {}", ip_info.subnet);
                                log::info!("DNS: {:?}", ip_info.dns);

                                // Log successful connection but don't try to test TCP connectivity
                                has_valid_ip = true;
                                break; // Successfully connected with valid IP
                            } else {
                                log::debug!(
                                    "Connected but waiting for DHCP (IP: {})...",
                                    ip_info.ip
                                );
                            }
                        }
                    }
                }

                if connected && has_valid_ip {
                    log::info!("WiFi connected successfully with valid IP address!");
                    break;
                } else if connected {
                    log::warn!(
                        "Connected to WiFi but failed to get valid IP address after {} seconds",
                        max_wait_seconds
                    );
                    // Disconnect and retry to force new DHCP exchange
                    let _ = wifi.disconnect();
                    std::thread::sleep(std::time::Duration::from_secs(1));
                } else {
                    log::warn!(
                        "WiFi connection timed out after {} seconds",
                        max_wait_seconds
                    );
                }
            }
            Err(e) => {
                log::error!(
                    "Failed to connect to WiFi (attempt {}/{}): {}",
                    attempt,
                    max_retries,
                    e
                );
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
        }
    }

    if connected {
        log::info!("WiFi connected successfully!");
        match wifi.sta_netif().get_ip_info() {
            Ok(ip_info) => log::info!("IP info: {:?}", ip_info),
            Err(e) => log::warn!("Failed to get IP info: {}", e),
        }
        // Return the wifi object in a Box to maintain ownership
        Ok(Box::new(wifi))
    } else {
        let err_msg = format!(
            "Failed to connect to WiFi '{}' after {} attempts",
            ssid, max_retries
        );
        log::error!("{}", err_msg);
        Err(anyhow::anyhow!(err_msg))
    }
}
