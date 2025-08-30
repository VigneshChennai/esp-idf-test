use std::convert::TryInto;

use anyhow::Result;
use embedded_svc::wifi::{AuthMethod, ClientConfiguration, Configuration};

use esp_idf_svc::eventloop::EspSystemEventLoop;
use esp_idf_svc::hal::modem::Modem;
use esp_idf_svc::hal::peripherals::Peripherals;
use esp_idf_svc::log::EspLogger;
use esp_idf_svc::nvs::EspDefaultNvsPartition;
use esp_idf_svc::sys::{
    esp, esp_vfs_eventfd_config_t, esp_vfs_eventfd_register
};
use esp_idf_svc::timer::EspTaskTimerService;
use esp_idf_svc::wifi::{AsyncWifi, EspWifi};

use log::info;


const SSID: &str = "NETGEAR13";
const PASSWORD: &str = "royalphoenix978";


fn tls_support() {
    use rustls::crypto::CryptoProvider;
    use rustls_rustcrypto::provider;
    CryptoProvider::install_default(provider()).unwrap();
}

async fn initialize_time() -> Result<()> {
    use esp_idf_svc::sntp::{EspSntp, SyncStatus};
    info!("Initializing SNTP and waiting for time sync...");
    // Create a new SNTP instance with default configuration
    let sntp = EspSntp::new_default()?;

    // Wait for synchronization to complete
    while sntp.get_sync_status() != SyncStatus::Completed {
        // The underlying service is trying to sync in the background
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }

    info!("Time synchronized");
    Ok(())
}

fn config_eventfd() -> Result<(), esp_idf_svc::sys::EspError> {
    // Register the eventfd VFS driver to allow async operations
    // This is needed for tokio to work.
    let config = esp_vfs_eventfd_config_t {
        max_fds: 1,
        ..Default::default()
    };
    esp! { unsafe { esp_vfs_eventfd_register(&config) } }
}

fn print_memory_info() {
    use esp_idf_svc::sys::{
        esp_get_free_heap_size, esp_get_minimum_free_heap_size,
        heap_caps_get_total_size, MALLOC_CAP_8BIT
    };
    // We need unsafe here as we are directly using function from esp-idf-sys crate.
    // which is a direct binding to the ESP-IDF C API.
    //
    // This is safe as long as we ensure that the ESP-IDF C API is used correctly.
    unsafe {
        // Get the total heap size available to the application.
        let total_heap = heap_caps_get_total_size(MALLOC_CAP_8BIT) as u32;
        info!("Total heap size: {} bytes", total_heap);

        // Get the current free heap size.
        let free_heap = esp_get_free_heap_size();
        info!("Current free heap size: {} bytes", free_heap);

        // Get the minimum free heap size that has been observed since
        // the application started. This is a good indicator of
        // worst-case memory usage.
        let min_free_heap = esp_get_minimum_free_heap_size();
        info!("Minimum free heap size: {} bytes", min_free_heap);

        // Example of a simple memory usage calculation
        let used_heap = total_heap - free_heap;
        info!("Currently used heap size: {} bytes", used_heap);
    }
}

fn main() -> Result<()> {
    esp_idf_svc::sys::link_patches(); // Required for compatibility
    EspLogger::initialize_default();  // Enable logging

    config_eventfd()?;

    tls_support();
    // Run the async main function
    tokio::runtime::Builder::new_current_thread()
      .thread_name("esp-tokio-rt".to_owned())
      .enable_all()
      .build()?
      .block_on(async_main())?;

    Ok(())
}

async fn async_main() -> Result<()> {
    // Take required peripherals
    let peripherals = Peripherals::take()?;
    let modem: Modem = peripherals.modem;
    let sysloop = EspSystemEventLoop::take()?;
    let nvs = EspDefaultNvsPartition::take()?;
    let timer = EspTaskTimerService::new()?;

    // Create the ESP WiFi driver
    let wifi_driver = EspWifi::new(modem, sysloop.clone(), Some(nvs))?;

    // Wrap it in AsyncWifi
    let mut wifi = AsyncWifi::wrap(wifi_driver, sysloop, timer)?;

    // Wi-Fi Configuration
    let config = Configuration::Client(ClientConfiguration {
        ssid: SSID.try_into().map_err(|_| anyhow::Error::msg("Error in SSID"))?,
        password: PASSWORD.try_into().map_err(|_| anyhow::Error::msg("Error in Password"))?,
        auth_method: AuthMethod::WPA2Personal,
        ..Default::default()
    });

    // Set config and start
    wifi.set_configuration(&config)?;
    info!("Wi-Fi configuration set.");

    wifi.start().await?;
    info!("Wi-Fi started.");

    wifi.connect().await?;
    info!("Wi-Fi connecting...");

    wifi.wait_netif_up().await?;
    info!("Wi-Fi connected!");

    initialize_time().await?;

    let mut first = true;
    loop {
        if first {
            first = false;
        } else {
            info!("Sleeping for 5 seconds");
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            info!("Looping again...");
        }

        let ip_info = wifi.wifi().sta_netif().get_ip_info()?;
        info!("IP Info: {:?}", ip_info);
        print_memory_info();
        // Leaking memory to testing 
        // 1. if memory tracking working as expected
        // 2. what happens on OOM

        // let _ = "New string object".to_owned().leak();

        // Testing if network access using standard client works.
        let response = reqwest::get("https://ifconfig.me/ip")
            .await;
        
        let data = match response {
            Err(e) => {
                info!("Error in network request: {:?}", e);
                continue;
            },
            Ok(resp) => resp.text().await?
        };

        let ip = data.trim();

        info!("Public IP: {}", ip);
    }
}