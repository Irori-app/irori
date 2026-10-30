//! Whether the connection costs money per byte, for automatic updates. Linux
//! asks the desktop portal and Windows the system's connectivity hint. macOS
//! has no answer without Network.framework, so there it stays unknown, as it
//! does wherever nothing answers.

/// `Some(true)` on a metered connection, `None` when the system can't tell.
pub(crate) async fn is_metered() -> Option<bool> {
    let answer = tokio::time::timeout(std::time::Duration::from_secs(3), ask());
    answer.await.ok().flatten()
}

#[cfg(target_os = "linux")]
async fn ask() -> Option<bool> {
    use ashpd::desktop::network_monitor::NetworkMonitor;
    let monitor = NetworkMonitor::new().await.ok()?;
    monitor.is_metered().await.ok()
}

#[cfg(windows)]
async fn ask() -> Option<bool> {
    use windows_sys::Win32::NetworkManagement::IpHelper::GetNetworkConnectivityHint;
    use windows_sys::Win32::Networking::WinSock::{
        NL_NETWORK_CONNECTIVITY_HINT, NetworkConnectivityCostHintFixed,
        NetworkConnectivityCostHintUnrestricted, NetworkConnectivityCostHintVariable,
    };
    // SAFETY: an all-zero hint is a valid value of this plain C struct.
    let mut hint: NL_NETWORK_CONNECTIVITY_HINT = unsafe { std::mem::zeroed() };
    // SAFETY: `hint` is a live, writable struct of the type the call fills in.
    if unsafe { GetNetworkConnectivityHint(&mut hint) } != 0 {
        return None;
    }
    match hint.ConnectivityCost {
        NetworkConnectivityCostHintFixed | NetworkConnectivityCostHintVariable => Some(true),
        NetworkConnectivityCostHintUnrestricted => Some(false),
        _ => None,
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
async fn ask() -> Option<bool> {
    None
}
