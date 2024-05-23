//! ModemManager DBus interface.

use std::error::Error;
use std::thread;

use calloop::channel::{self, Channel, Sender};
use tokio::runtime::Builder;
use zbus::export::futures_util::stream::StreamExt;
use zbus::fdo::ObjectManagerProxy;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type};
use zbus::{dbus_proxy, Connection, PropertyStream};

/// Cellular connection status.
#[derive(PartialEq, Eq, Default, Copy, Clone, Debug)]
pub struct ModemConnection {
    /// Modem is enabled.
    pub enabled: bool,

    /// Cellular signal qualit in percent.
    pub strength: u8,

    /// Modem state is at least 'registered'.
    pub registered: bool,
}

impl ModemConnection {
    /// Get current cellular connection status.
    async fn new(modem: &ModemProxy<'_>, modem3gpp: &Modem3gppProxy<'_>) -> Option<Self> {
        // Get the modem connection quality.
        let strength = modem.signal_quality().await.ok()?.0 as u8;

        // Get 3gpp registration status.
        let registration_state = modem3gpp.registration_state().await.ok()?;
        let registered = registration_state > RegistrationState::Idle;

        // Get modem status.
        let modem_state = modem.modem_state().await.ok()?;
        let enabled = modem_state >= ModemState::Enabled;

        Some(Self { strength, registered, enabled })
    }
}

/// Get calloop channel for cellular signal strength changes.
pub fn modem_listener() -> Result<Channel<ModemConnection>, Box<dyn Error>> {
    let (tx, rx) = channel::channel();
    thread::spawn(|| {
        let mut builder = Builder::new_current_thread();
        let runtime = builder.enable_all().build().expect("create tokio runtime");
        runtime.block_on(run_dbus_loop(tx)).expect("execute tokio runtime");
    });
    Ok(rx)
}

/// Set ModemManager modem states.
pub fn set_enabled(enabled: bool) {
    // Async function for updating the state of every modem.
    let set_modem_state = move || async move {
        // Get all active modems.
        let connection = Connection::system().await?;
        let object_manager = object_manager(&connection).await?;
        let modems = active_modems(&connection, &object_manager).await;

        // Set the state for each one.
        for (modem, _) in modems {
            // Ensure modem's power state is `On` before enabling it.
            if enabled {
                if let Err(err) = modem.set_power_state(PowerState::On as u32).await {
                    eprintln!("Could not power modem on: {err}");
                }
            }

            // Set the modem state.
            if let Err(err) = modem.enable(enabled).await {
                eprintln!("Modem state change failed: {err}");
            }

            // Set modem to lowest powerstate it can recover from.
            //
            // Setting it to `PowerState::Off` will prevent turning it back on in the
            // future.
            if !enabled {
                if let Err(err) = modem.set_power_state(PowerState::Low as u32).await {
                    eprintln!("Could not power modem off: {err}");
                }
            }
        }

        Ok::<(), zbus::Error>(())
    };

    // Spawn async executor for the WiFi update on a new thread.
    thread::spawn(move || {
        let mut builder = Builder::new_current_thread();
        let runtime = builder.enable_all().build().expect("create tokio runtime");
        runtime.block_on(set_modem_state()).expect("execute tokio runtime");
    });
}

/// Run the DBus cellular event loop.
async fn run_dbus_loop(tx: Sender<ModemConnection>) -> Result<(), Box<dyn Error>> {
    let connection = Connection::system().await?;

    // Create object manager for modem changes.
    let object_manager = object_manager(&connection).await?;

    // Fill list of active modems.
    let mut modems = active_modems(&connection, &object_manager).await;

    // Get stream for modem changes.
    let mut modem_added_stream = object_manager.receive_interfaces_added().await?;
    let mut modem_removed_stream = object_manager.receive_interfaces_removed().await?;

    // Initialize modem quality and connectivity streams.
    let mut modem_streams = primary_modem_streams(&modems).await;

    loop {
        // Extract optional streams, since async Rust sucks.
        let modem_future = async {
            match &mut modem_streams {
                Some((registration_stream, connectivity_stream, quality_stream)) => {
                    tokio::select! {
                        _ = registration_stream.next() => Some(()),
                        _ = connectivity_stream.next() => Some(()),
                        _ = quality_stream.next() => Some(()),
                    }
                },
                None => None,
            }
        };

        tokio::select! {
            // Wait for any connectivity/signal quality changes.
            Some(_) = modem_future => (),

            // Wait for new/removed modems.
            Some(_) = modem_added_stream.next() => {
                modems = active_modems(&connection, &object_manager).await;
                modem_streams = primary_modem_streams(&modems).await;
            },
            Some(_) = modem_removed_stream.next() => {
                modems = active_modems(&connection, &object_manager).await;
                modem_streams = primary_modem_streams(&modems).await;
            },

            else => continue,
        };

        // Get first available modem.
        let (modem, modem3gpp) = match modems.first() {
            Some(modem) => modem,
            None => {
                tx.send(ModemConnection::default())?;
                continue;
            },
        };

        // Update connection status.
        let modem_connection = ModemConnection::new(modem, modem3gpp).await.unwrap_or_default();
        tx.send(modem_connection)?;
    }
}

/// Create object manager for tracking DBus modem objects
async fn object_manager(connection: &Connection) -> zbus::Result<ObjectManagerProxy> {
    ObjectManagerProxy::builder(connection)
        .destination("org.freedesktop.ModemManager1")?
        .path("/org/freedesktop/ModemManager1")?
        .build()
        .await
}

/// Get all active modems from the object manager.
async fn active_modems<'a>(
    connection: &'a Connection,
    object_manager: &'a ObjectManagerProxy<'a>,
) -> Vec<(ModemProxy<'a>, Modem3gppProxy<'a>)> {
    let managed_objects = object_manager.get_managed_objects().await;

    let mut modems = Vec::new();
    for (path, _) in managed_objects.into_iter().flatten() {
        if path.starts_with("/org/freedesktop/ModemManager1/Modem/") {
            let (modem, modem3gpp) = tokio::join!(
                modem_from_path(connection, path.clone()),
                modem3gpp_from_path(connection, path),
            );

            if let (Ok(modem), Ok(modem3gpp)) = (modem, modem3gpp) {
                modems.push((modem, modem3gpp));
            }
        }
    }

    modems
}

/// Get modem state/signal quality streams.
async fn primary_modem_streams<'a>(
    modems: &[(ModemProxy<'a>, Modem3gppProxy<'a>)],
) -> Option<(
    PropertyStream<'a, RegistrationState>,
    PropertyStream<'a, ModemState>,
    PropertyStream<'a, (u32, bool)>,
)> {
    let (modem, modem3gpp) = modems.first()?;

    let registration_stream = modem3gpp.receive_registration_state_changed().await;
    let connectivity_stream = modem.receive_modem_state_changed().await;
    let quality_stream = modem.receive_signal_quality_changed().await;

    Some((registration_stream, connectivity_stream, quality_stream))
}

/// Try and convert a DBus device path to modem.
async fn modem_from_path(
    connection: &Connection,
    device_path: OwnedObjectPath,
) -> zbus::Result<ModemProxy> {
    ModemProxy::builder(connection).path(device_path)?.build().await
}

/// Try and convert a DBus device path to 3gpp modem.
async fn modem3gpp_from_path(
    connection: &Connection,
    device_path: OwnedObjectPath,
) -> zbus::Result<Modem3gppProxy> {
    Modem3gppProxy::builder(connection).path(device_path)?.build().await
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1"
)]
trait ModemManager1 {
    /// InhibitDevice method
    fn inhibit_device(&self, uid: &str, inhibit: bool) -> zbus::Result<()>;

    /// ReportKernelEvent method
    fn report_kernel_event(
        &self,
        properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<()>;

    /// ScanDevices method
    fn scan_devices(&self) -> zbus::Result<()>;

    /// SetLogging method
    fn set_logging(&self, level: &str) -> zbus::Result<()>;

    /// Version property
    #[dbus_proxy(property)]
    fn version(&self) -> zbus::Result<String>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Location",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Location {
    /// GetLocation method
    fn get_location(
        &self,
    ) -> zbus::Result<std::collections::HashMap<u32, zbus::zvariant::OwnedValue>>;

    /// InjectAssistanceData method
    fn inject_assistance_data(&self, data: &[u8]) -> zbus::Result<()>;

    /// SetGpsRefreshRate method
    fn set_gps_refresh_rate(&self, rate: u32) -> zbus::Result<()>;

    /// SetSuplServer method
    fn set_supl_server(&self, supl: &str) -> zbus::Result<()>;

    /// Setup method
    fn setup(&self, sources: u32, signal_location: bool) -> zbus::Result<()>;

    /// AssistanceDataServers property
    #[dbus_proxy(property)]
    fn assistance_data_servers(&self) -> zbus::Result<Vec<String>>;

    /// Capabilities property
    #[dbus_proxy(property)]
    fn capabilities(&self) -> zbus::Result<u32>;

    /// Enabled property
    #[dbus_proxy(property)]
    fn enabled(&self) -> zbus::Result<u32>;

    /// GpsRefreshRate property
    #[dbus_proxy(property)]
    fn gps_refresh_rate(&self) -> zbus::Result<u32>;

    /// Location property
    #[dbus_proxy(property)]
    fn location(&self) -> zbus::Result<std::collections::HashMap<u32, zbus::zvariant::OwnedValue>>;

    /// SignalsLocation property
    #[dbus_proxy(property)]
    fn signals_location(&self) -> zbus::Result<bool>;

    /// SuplServer property
    #[dbus_proxy(property)]
    fn supl_server(&self) -> zbus::Result<String>;

    /// SupportedAssistanceData property
    #[dbus_proxy(property)]
    fn supported_assistance_data(&self) -> zbus::Result<u32>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Signal",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Signal {
    /// Setup method
    fn setup(&self, rate: u32) -> zbus::Result<()>;

    /// SetupThresholds method
    fn setup_thresholds(
        &self,
        settings: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<()>;

    /// Cdma property
    #[dbus_proxy(property)]
    fn cdma(&self) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

    /// ErrorRateThreshold property
    #[dbus_proxy(property)]
    fn error_rate_threshold(&self) -> zbus::Result<bool>;

    /// Evdo property
    #[dbus_proxy(property)]
    fn evdo(&self) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

    /// Gsm property
    #[dbus_proxy(property)]
    fn gsm(&self) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

    /// Lte property
    #[dbus_proxy(property)]
    fn lte(&self) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

    /// Nr5g property
    #[dbus_proxy(property)]
    fn nr5g(&self) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

    /// Rate property
    #[dbus_proxy(property)]
    fn rate(&self) -> zbus::Result<u32>;

    /// RssiThreshold property
    #[dbus_proxy(property)]
    fn rssi_threshold(&self) -> zbus::Result<u32>;

    /// Umts property
    #[dbus_proxy(property)]
    fn umts(&self) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp.Ussd",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Ussd {
    /// Cancel method
    fn cancel(&self) -> zbus::Result<()>;

    /// Initiate method
    fn initiate(&self, command: &str) -> zbus::Result<String>;

    /// Respond method
    fn respond(&self, response: &str) -> zbus::Result<String>;

    /// NetworkNotification property
    #[dbus_proxy(property)]
    fn network_notification(&self) -> zbus::Result<String>;

    /// NetworkRequest property
    #[dbus_proxy(property)]
    fn network_request(&self) -> zbus::Result<String>;

    /// State property
    #[dbus_proxy(property)]
    fn state(&self) -> zbus::Result<u32>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Messaging",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Messaging {
    /// Create method
    fn create(
        &self,
        properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    /// Delete method
    fn delete(&self, path: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    /// List method
    fn list(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;

    /// Added signal
    #[dbus_proxy(signal)]
    fn added(&self, path: zbus::zvariant::ObjectPath<'_>, received: bool) -> zbus::Result<()>;

    /// Deleted signal
    #[dbus_proxy(signal)]
    fn deleted(&self, path: zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    /// DefaultStorage property
    #[dbus_proxy(property)]
    fn default_storage(&self) -> zbus::Result<u32>;

    /// Messages property
    #[dbus_proxy(property)]
    fn messages(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;

    /// SupportedStorages property
    #[dbus_proxy(property)]
    fn supported_storages(&self) -> zbus::Result<Vec<u32>>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Modem {
    /// Command method
    fn command(&self, cmd: &str, timeout: u32) -> zbus::Result<String>;

    /// CreateBearer method
    fn create_bearer(
        &self,
        properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    /// DeleteBearer method
    fn delete_bearer(&self, bearer: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    /// Enable method
    fn enable(&self, enable: bool) -> zbus::Result<()>;

    /// FactoryReset method
    fn factory_reset(&self, code: &str) -> zbus::Result<()>;

    /// GetCellInfo method
    fn get_cell_info(
        &self,
    ) -> zbus::Result<Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>>;

    /// ListBearers method
    fn list_bearers(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;

    /// Reset method
    fn reset(&self) -> zbus::Result<()>;

    /// SetCurrentBands method
    fn set_current_bands(&self, bands: &[u32]) -> zbus::Result<()>;

    /// SetCurrentCapabilities method
    fn set_current_capabilities(&self, capabilities: u32) -> zbus::Result<()>;

    /// SetCurrentModes method
    fn set_current_modes(&self, modes: &(u32, u32)) -> zbus::Result<()>;

    /// SetPowerState method
    fn set_power_state(&self, state: u32) -> zbus::Result<()>;

    /// SetPrimarySimSlot method
    fn set_primary_sim_slot(&self, sim_slot: u32) -> zbus::Result<()>;

    /// StateChanged signal
    #[dbus_proxy(signal)]
    fn state_changed(&self, old: i32, new: i32, reason: u32) -> zbus::Result<()>;

    /// AccessTechnologies property
    #[dbus_proxy(property)]
    fn access_technologies(&self) -> zbus::Result<u32>;

    /// Bearers property
    #[dbus_proxy(property)]
    fn bearers(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;

    /// CarrierConfiguration property
    #[dbus_proxy(property)]
    fn carrier_configuration(&self) -> zbus::Result<String>;

    /// CarrierConfigurationRevision property
    #[dbus_proxy(property)]
    fn carrier_configuration_revision(&self) -> zbus::Result<String>;

    /// CurrentBands property
    #[dbus_proxy(property)]
    fn current_bands(&self) -> zbus::Result<Vec<u32>>;

    /// CurrentCapabilities property
    #[dbus_proxy(property)]
    fn current_capabilities(&self) -> zbus::Result<u32>;

    /// CurrentModes property
    #[dbus_proxy(property)]
    fn current_modes(&self) -> zbus::Result<(u32, u32)>;

    /// Device property
    #[dbus_proxy(property)]
    fn device(&self) -> zbus::Result<String>;

    /// DeviceIdentifier property
    #[dbus_proxy(property)]
    fn device_identifier(&self) -> zbus::Result<String>;

    /// Drivers property
    #[dbus_proxy(property)]
    fn drivers(&self) -> zbus::Result<Vec<String>>;

    /// EquipmentIdentifier property
    #[dbus_proxy(property)]
    fn equipment_identifier(&self) -> zbus::Result<String>;

    /// HardwareRevision property
    #[dbus_proxy(property)]
    fn hardware_revision(&self) -> zbus::Result<String>;

    /// Manufacturer property
    #[dbus_proxy(property)]
    fn manufacturer(&self) -> zbus::Result<String>;

    /// MaxActiveBearers property
    #[dbus_proxy(property)]
    fn max_active_bearers(&self) -> zbus::Result<u32>;

    /// MaxActiveMultiplexedBearers property
    #[dbus_proxy(property)]
    fn max_active_multiplexed_bearers(&self) -> zbus::Result<u32>;

    /// MaxBearers property
    #[dbus_proxy(property)]
    fn max_bearers(&self) -> zbus::Result<u32>;

    /// Model property
    #[dbus_proxy(property)]
    fn model(&self) -> zbus::Result<String>;

    /// OwnNumbers property
    #[dbus_proxy(property)]
    fn own_numbers(&self) -> zbus::Result<Vec<String>>;

    /// Plugin property
    #[dbus_proxy(property)]
    fn plugin(&self) -> zbus::Result<String>;

    /// Ports property
    #[dbus_proxy(property)]
    fn ports(&self) -> zbus::Result<Vec<(String, u32)>>;

    /// PowerState property
    #[dbus_proxy(property)]
    fn power_state(&self) -> zbus::Result<PowerState>;

    /// PrimaryPort property
    #[dbus_proxy(property)]
    fn primary_port(&self) -> zbus::Result<String>;

    /// PrimarySimSlot property
    #[dbus_proxy(property)]
    fn primary_sim_slot(&self) -> zbus::Result<u32>;

    /// Revision property
    #[dbus_proxy(property)]
    fn revision(&self) -> zbus::Result<String>;

    /// SignalQuality property
    #[dbus_proxy(property)]
    fn signal_quality(&self) -> zbus::Result<(u32, bool)>;

    /// Sim property
    #[dbus_proxy(property)]
    fn sim(&self) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    /// SimSlots property
    #[dbus_proxy(property)]
    fn sim_slots(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;

    /// State property
    #[dbus_proxy(property, name = "State")]
    fn modem_state(&self) -> zbus::Result<ModemState>;

    /// StateFailedReason property
    #[dbus_proxy(property)]
    fn state_failed_reason(&self) -> zbus::Result<u32>;

    /// SupportedBands property
    #[dbus_proxy(property)]
    fn supported_bands(&self) -> zbus::Result<Vec<u32>>;

    /// SupportedCapabilities property
    #[dbus_proxy(property)]
    fn supported_capabilities(&self) -> zbus::Result<Vec<u32>>;

    /// SupportedIpFamilies property
    #[dbus_proxy(property)]
    fn supported_ip_families(&self) -> zbus::Result<u32>;

    /// SupportedModes property
    #[dbus_proxy(property)]
    fn supported_modes(&self) -> zbus::Result<Vec<(u32, u32)>>;

    /// UnlockRequired property
    #[dbus_proxy(property)]
    fn unlock_required(&self) -> zbus::Result<u32>;

    /// UnlockRetries property
    #[dbus_proxy(property)]
    fn unlock_retries(&self) -> zbus::Result<std::collections::HashMap<u32, u32>>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Time",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Time {
    /// GetNetworkTime method
    fn get_network_time(&self) -> zbus::Result<String>;

    /// NetworkTimeChanged signal
    #[dbus_proxy(signal)]
    fn network_time_changed(&self, time: &str) -> zbus::Result<()>;

    /// NetworkTimezone property
    #[dbus_proxy(property)]
    fn network_timezone(
        &self,
    ) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Firmware",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Firmware {
    /// List method
    fn list(
        &self,
    ) -> zbus::Result<(String, Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>)>;

    /// Select method
    fn select(&self, uniqueid: &str) -> zbus::Result<()>;

    /// UpdateSettings property
    #[dbus_proxy(property)]
    fn update_settings(
        &self,
    ) -> zbus::Result<(u32, std::collections::HashMap<String, zbus::zvariant::OwnedValue>)>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp.ProfileManager",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait ProfileManager {
    /// Delete method
    fn delete(
        &self,
        properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<()>;

    /// List method
    fn list(
        &self,
    ) -> zbus::Result<Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>>;

    /// Set method
    fn set(
        &self,
        requested_properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

    /// Updated signal
    #[dbus_proxy(signal)]
    fn updated(&self) -> zbus::Result<()>;

    /// IndexField property
    #[dbus_proxy(property)]
    fn index_field(&self) -> zbus::Result<String>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Sar",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Sar {
    /// Enable method
    fn enable(&self, enable: bool) -> zbus::Result<()>;

    /// SetPowerLevel method
    fn set_power_level(&self, level: u32) -> zbus::Result<()>;

    /// PowerLevel property
    #[dbus_proxy(property)]
    fn power_level(&self) -> zbus::Result<u32>;

    /// State property
    #[dbus_proxy(property)]
    fn state(&self) -> zbus::Result<bool>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Simple",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Simple {
    /// Connect method
    fn connect(
        &self,
        properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    /// Disconnect method
    fn disconnect(&self, bearer: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    /// GetStatus method
    fn get_status(
        &self,
    ) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Modem3gpp",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Modem3gpp {
    /// DisableFacilityLock method
    fn disable_facility_lock(&self, properties: &(u32, &str)) -> zbus::Result<()>;

    /// Register method
    fn register(&self, operator_id: &str) -> zbus::Result<()>;

    /// Scan method
    fn scan(
        &self,
    ) -> zbus::Result<Vec<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>>;

    /// SetEpsUeModeOperation method
    fn set_eps_ue_mode_operation(&self, mode: u32) -> zbus::Result<()>;

    /// SetInitialEpsBearerSettings method
    fn set_initial_eps_bearer_settings(
        &self,
        settings: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<()>;

    /// SetNr5gRegistrationSettings method
    fn set_nr5g_registration_settings(
        &self,
        properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<()>;

    /// SetPacketServiceState method
    fn set_packet_service_state(&self, state: u32) -> zbus::Result<()>;

    /// EnabledFacilityLocks property
    #[dbus_proxy(property)]
    fn enabled_facility_locks(&self) -> zbus::Result<u32>;

    /// EpsUeModeOperation property
    #[dbus_proxy(property)]
    fn eps_ue_mode_operation(&self) -> zbus::Result<u32>;

    /// Imei property
    #[dbus_proxy(property)]
    fn imei(&self) -> zbus::Result<String>;

    /// InitialEpsBearer property
    #[dbus_proxy(property)]
    fn initial_eps_bearer(&self) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    /// InitialEpsBearerSettings property
    #[dbus_proxy(property)]
    fn initial_eps_bearer_settings(
        &self,
    ) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

    /// Nr5gRegistrationSettings property
    #[dbus_proxy(property)]
    fn nr5g_registration_settings(
        &self,
    ) -> zbus::Result<std::collections::HashMap<String, zbus::zvariant::OwnedValue>>;

    /// OperatorCode property
    #[dbus_proxy(property)]
    fn operator_code(&self) -> zbus::Result<String>;

    /// OperatorName property
    #[dbus_proxy(property)]
    fn operator_name(&self) -> zbus::Result<String>;

    /// PacketServiceState property
    #[dbus_proxy(property)]
    fn packet_service_state(&self) -> zbus::Result<u32>;

    /// Pco property
    #[dbus_proxy(property)]
    fn pco(&self) -> zbus::Result<Vec<(u32, bool, Vec<u8>)>>;

    /// RegistrationState property
    #[dbus_proxy(property)]
    fn registration_state(&self) -> zbus::Result<RegistrationState>;

    /// SubscriptionState property
    #[dbus_proxy(property)]
    fn subscription_state(&self) -> zbus::Result<u32>;
}

#[dbus_proxy(
    interface = "org.freedesktop.ModemManager1.Modem.Voice",
    default_service = "org.freedesktop.ModemManager1",
    default_path = "/org/freedesktop/ModemManager1/Modem/0"
)]
trait Voice {
    /// CallWaitingQuery method
    fn call_waiting_query(&self) -> zbus::Result<bool>;

    /// CallWaitingSetup method
    fn call_waiting_setup(&self, enable: bool) -> zbus::Result<()>;

    /// CreateCall method
    fn create_call(
        &self,
        properties: std::collections::HashMap<&str, zbus::zvariant::Value<'_>>,
    ) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;

    /// DeleteCall method
    fn delete_call(&self, path: &zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    /// HangupAll method
    fn hangup_all(&self) -> zbus::Result<()>;

    /// HangupAndAccept method
    fn hangup_and_accept(&self) -> zbus::Result<()>;

    /// HoldAndAccept method
    fn hold_and_accept(&self) -> zbus::Result<()>;

    /// ListCalls method
    fn list_calls(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;

    /// Transfer method
    fn transfer(&self) -> zbus::Result<()>;

    /// CallAdded signal
    #[dbus_proxy(signal)]
    fn call_added(&self, path: zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    /// CallDeleted signal
    #[dbus_proxy(signal)]
    fn call_deleted(&self, path: zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    /// Calls property
    #[dbus_proxy(property)]
    fn calls(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;

    /// EmergencyOnly property
    #[dbus_proxy(property)]
    fn emergency_only(&self) -> zbus::Result<bool>;
}

/// ModemManager modem 3gpp state.
#[derive(Type, OwnedValue, PartialEq, Debug, PartialOrd)]
#[repr(u32)]
pub enum RegistrationState {
    // Not registered, not searching for new operator to register.
    Idle = 0,
    // Registered on home network.
    Home = 1,
    // Not registered, searching for new operator to register with.
    Searching = 2,
    // Registration denied.
    Denied = 3,
    // Unknown registration status.
    Unknown = 4,
    // Registered on a roaming network.
    Roaming = 5,
    // Registered for "SMS only", home network (applicable only when on LTE). Since 1.8.
    HomeSmsOnly = 6,
    // Registered for "SMS only", roaming network (applicable only when on LTE). Since 1.8.
    RoamingSmsOnly = 7,
    // Emergency services only. Since 1.8.
    EmergencyOnly = 8,
    // Registered for "CSFB not preferred", home network (applicable only when on LTE). Since 1.8.
    HomeCsfbNotPreferred = 9,
    // Registered for "CSFB not preferred", roaming network (applicable only when on LTE). Since
    // 1.8.
    RoamingCsfbNotPreferred = 10,
    // Attached for access to Restricted Local Operator Services (applicable only when on LTE).
    // Since 1.14.
    AttachedRlos = 11,
}

/// Power state of the modem.
#[derive(Type, OwnedValue, PartialEq, Debug, PartialOrd)]
#[repr(u32)]
pub enum PowerState {
    // Unknown power state.
    Unknown = 0,
    // Off.
    Off = 1,
    // Low-power mode.
    Low = 2,
    /// Full power mode.
    On = 3,
}

/// Enumeration of possible modem states.
#[derive(Type, OwnedValue, PartialEq, Debug, PartialOrd)]
#[repr(i32)]
pub enum ModemState {
    /// The modem is unusable.
    Failed = 0,
    /// State unknown or not reportable.
    Unknown = 1,
    /// The modem is currently being initialized.
    Initializing = 2,
    /// The modem needs to be unlocked.
    Locked = 3,
    /// The modem is not enabled and is powered down.
    Disabled = 4,
    /// The modem is currently transitioning to the [Self::Disabled] state.
    Disabling = 5,
    /// The modem is currently transitioning to the [Self::Enabled] state.
    Enabling = 6,
    /// The modem is enabled and powered on but not registered with a network
    /// provider and not available for data connections.
    Enabled = 7,
    /// The modem is searching for a network provider to register with.
    Searching = 8,
    /// The modem is registered with a network provider, and data connections
    /// and messaging may be available for use.
    Registered = 9,
    /// The modem is disconnecting and deactivating the last active packet data
    /// bearer. This state will not be entered if more than one packet data
    /// bearer is active and one of the active bearers is deactivated.
    Disconnecting = 10,
    /// The modem is activating and connecting the first packet data bearer.
    /// Subsequent bearer activations when another bearer is already active do
    /// not cause this state to be entered.
    Connecting = 11,
}
