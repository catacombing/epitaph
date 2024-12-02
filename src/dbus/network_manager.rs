//! NetworkManager DBus interface.

use std::error::Error;
use std::thread;

use calloop::channel::{self, Channel, Sender};
use tokio::runtime::Builder;
use zbus::export::futures_util::stream::StreamExt;
use zbus::proxy::{PropertyChanged, PropertyStream};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Type};
use zbus::{proxy, Connection};

/// Wifi connection quality.
#[derive(PartialEq, Eq, Default, Copy, Clone, Debug)]
pub struct WifiConnection {
    /// Wifi is enabled.
    pub enabled: bool,

    /// AP signal strength in percent.
    pub strength: u8,

    /// Connection has internet access.
    pub connected: bool,
}

impl WifiConnection {
    /// Get current WiFi connection status.
    async fn new(
        connection: &Connection,
        network_manager: &NetworkManagerProxy<'_>,
        wireless_device: &WirelessDeviceProxy<'_>,
    ) -> Option<Self> {
        // Get the active access point.
        let active_ap = match wireless_device.active_access_point().await {
            // Filter out fallback AP `/`.
            Ok(path) if path.len() != 1 => {
                AccessPointProxy::builder(connection).path(path).ok()?.build().await.ok()?
            },
            _ => return None,
        };

        // Get signal strength from AP.
        let strength = active_ap.strength().await.ok()?;

        // Get connection status from NM.
        let connectivity = network_manager.connectivity().await.ok()?;
        let connected = connectivity == ConnectivityState::Full;

        // Get enabled status.
        let enabled = network_manager.wireless_enabled().await.ok()?;

        Some(Self { strength, connected, enabled })
    }
}

/// Set NetworkManager WiFi state.
pub fn set_enabled(enabled: bool) {
    // Async function for updating the WiFi state.
    let set_wifi_state = |enabled: bool| async move {
        let connection = Connection::system().await?;
        let network_manager = NetworkManagerProxy::new(&connection).await?;
        if let Err(err) = network_manager.set_wireless_enabled(enabled).await {
            eprintln!("WiFi state change failed: {err}");
        }
        Ok::<(), zbus::Error>(())
    };

    // Spawn async executor for the WiFi update on a new thread.
    thread::spawn(move || {
        let mut builder = Builder::new_current_thread();
        let runtime = builder.enable_all().build().expect("create tokio runtime");
        runtime.block_on(set_wifi_state(enabled)).expect("execute tokio runtime");
    });
}

/// Get calloop channel for wifi signal strength changes.
pub fn wifi_listener() -> Result<Channel<WifiConnection>, Box<dyn Error>> {
    let (tx, rx) = channel::channel();
    thread::spawn(|| {
        let mut builder = Builder::new_current_thread();
        let runtime = builder.enable_all().build().expect("create tokio runtime");
        runtime.block_on(run_dbus_loop(tx)).expect("execute tokio runtime");
    });
    Ok(rx)
}

/// Run the DBus WiFi event loop.
async fn run_dbus_loop(tx: Sender<WifiConnection>) -> Result<(), Box<dyn Error>> {
    let connection = Connection::system().await?;

    // Get network manager interface.
    let network_manager = NetworkManagerProxy::new(&connection).await?;

    // Get stream for WiFi device changes.
    let mut device_added_stream = network_manager.receive_device_added().await?;
    let mut device_removed_stream = network_manager.receive_device_added().await?;

    // Get WiFi device and update stream.
    let mut wireless_device = active_wireless_device(&connection, &network_manager).await;

    // Get stream for connectivity state changes.
    let mut connectivity_stream = network_manager.receive_connectivity_changed().await;

    // Initialize empty AP signal strength stream.
    let mut strength_stream: Option<PropertyStream<u8>> = None;

    loop {
        // Extract optional streams, since async Rust sucks.
        let strength_future = async {
            match &mut strength_stream {
                Some(strength_stream) => strength_stream.next().await,
                None => None,
            }
        };
        let active_ap_future = async {
            match &mut wireless_device {
                Some((_, active_ap_stream)) => active_ap_stream.next().await,
                None => None,
            }
        };

        let new_active_ap = tokio::select! {
            // Wait for NetworkManager device changes.
            Some(_) = device_added_stream.next() => {
                wireless_device = active_wireless_device(&connection, &network_manager).await;
                None
            },
            Some(_) = device_removed_stream.next() => {
                wireless_device = active_wireless_device(&connection, &network_manager).await;
                None
            },

            // Wait for AP changes.
            Some(new_active_ap) = active_ap_future => Some(new_active_ap),

            // Wait for connectivity/signal quality changes.
            Some(_) = connectivity_stream.next() => None,
            Some(_) = strength_future => None,

            else => continue,
        };

        // Handle active AP changes.
        if let Some(new_active_ap) = new_active_ap {
            strength_stream = ap_strength_stream(&connection, new_active_ap).await.ok();
        }

        // Get the active wireless device.
        let wireless_device = match &wireless_device {
            Some((wireless_device, _)) => wireless_device,
            None => {
                tx.send(WifiConnection::default())?;
                continue;
            },
        };

        // Update connection status.
        let wifi_connection = WifiConnection::new(&connection, &network_manager, wireless_device)
            .await
            .unwrap_or_default();
        tx.send(wifi_connection)?;
    }
}

/// Get signal strength stream for an AP.
async fn ap_strength_stream<'a>(
    connection: &'a Connection,
    ap_change: PropertyChanged<'a, OwnedObjectPath>,
) -> zbus::Result<PropertyStream<'a, u8>> {
    let ap_path = ap_change.get().await?;
    let ap = AccessPointProxy::builder(connection).path(ap_path)?.build().await?;
    Ok(ap.receive_strength_changed().await)
}

/// Get the active wireless device.
async fn active_wireless_device<'a>(
    connection: &'a Connection,
    network_manager: &'a NetworkManagerProxy<'a>,
) -> Option<(WirelessDeviceProxy<'a>, PropertyStream<'a, OwnedObjectPath>)> {
    // Get realized network devices.
    let device_paths = network_manager.get_devices().await.ok()?;

    // Find the first wifi network device.
    let mut active_wireless_device = None;
    for device_path in device_paths {
        let wireless_device = wireless_device_from_path(connection, device_path).await;
        if wireless_device.is_some() {
            active_wireless_device = wireless_device;
            break;
        }
    }

    // Get stream for active AP changes.
    let active_wireless_device = active_wireless_device?;
    let active_ap_stream = active_wireless_device.receive_active_access_point_changed().await;

    Some((active_wireless_device, active_ap_stream))
}

/// Try and convert a NetworkManager device path to a wireless device.
async fn wireless_device_from_path(
    connection: &Connection,
    device_path: OwnedObjectPath,
) -> Option<WirelessDeviceProxy> {
    // Resolve as generic device first.
    let device = DeviceProxy::builder(connection).path(&device_path).ok()?.build().await.ok()?;

    // Skip devices with incorrect type.
    if !matches!(device.device_type().await, Ok(DeviceType::Wifi)) {
        return None;
    }

    // Try ta resolve as wireless device.
    WirelessDeviceProxy::builder(connection).path(device_path).ok()?.build().await.ok()
}

#[proxy(assume_defaults = true)]
trait NetworkManager {
    /// Get the list of realized network devices.
    fn get_devices(&self) -> zbus::Result<Vec<OwnedObjectPath>>;

    /// Indicates if wireless is currently enabled or not.
    #[zbus(property)]
    fn wireless_enabled(&self) -> zbus::Result<bool>;

    /// Set if wireless is currently enabled or not.
    #[zbus(property)]
    fn set_wireless_enabled(&self, enabled: bool) -> zbus::Result<()>;

    /// The result of the last connectivity check. The connectivity check is
    /// triggered automatically when a default connection becomes available,
    /// periodically and by calling a CheckConnectivity() method.
    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<ConnectivityState>;

    /// DeviceAdded signal
    #[zbus(signal)]
    fn device_added(&self, device_path: zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;

    /// DeviceRemoved signal
    #[zbus(signal)]
    fn device_removed(&self, device_path: zbus::zvariant::ObjectPath<'_>) -> zbus::Result<()>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Device"
)]
trait Device {
    /// The general type of the network device; ie Ethernet, Wi-Fi, etc.
    #[zbus(property)]
    fn device_type(&self) -> zbus::Result<DeviceType>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/Device/Wireless"
)]
trait WirelessDevice {
    /// Object path of the access point currently used by the wireless device.
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<OwnedObjectPath>;
}

#[proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager/AccessPoint"
)]
trait AccessPoint {
    /// The Service Set Identifier identifying the access point.
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;

    /// The radio channel frequency in use by the access point, in MHz.
    #[zbus(property)]
    fn frequency(&self) -> zbus::Result<u32>;

    /// The hardware address (BSSID) of the access point.
    #[zbus(property)]
    fn hw_address(&self) -> zbus::Result<String>;

    /// The current signal quality of the access point, in percent.
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
}

/// NMDeviceType values indicate the type of hardware represented by a device
/// object.
#[derive(Type, OwnedValue, PartialEq, Debug)]
#[repr(u32)]
pub enum DeviceType {
    Wifi = 2,
    Modem = 8,
}

/// NetworkManager connectivity state.
#[derive(Type, OwnedValue, PartialEq, Debug)]
#[repr(u32)]
pub enum ConnectivityState {
    /// Network connectivity is unknown.
    ///
    /// This means the connectivity checks are disabled (e.g. on server
    /// installations) or has not run yet. The graphical shell should assume the
    /// Internet connection might be available and not present a captive portal
    /// window.
    Unknown = 0,
    /// The host is not connected to any network.
    ///
    /// There's no active connection that contains a default route to the
    /// internet and thus it makes no sense to even attempt a connectivity
    /// check. The graphical shell should use this state to indicate the network
    /// connection is unavailable.
    Disconnected = 1,
    /// The Internet connection is hijacked by a captive portal gateway.
    ///
    /// The graphical shell may open a sandboxed web browser window (because the
    /// captive portals typically attempt a man-in-the-middle attacks against
    /// the https connections) for the purpose of authenticating to a gateway
    /// and retrigger the connectivity check with CheckConnectivity() when the
    /// browser window is dismissed.
    Portal = 2,
    /// The host is connected to a network, does not appear to be able to reach
    /// the full Internet, but a captive portal has not been detected.
    Limited = 3,
    /// The host is connected to a network, and appears to be able to reach the
    /// full Internet.
    Full = 4,
}
