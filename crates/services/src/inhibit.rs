//! Keep the screen awake and the computer from sleeping while a game runs,
//! through the desktop portal's Inhibit interface. KDE, GNOME and other
//! desktops implement it, Flatpak included; where nothing answers, games run
//! without it.

use std::time::Duration;

/// How long to wait for the portal before giving up.
const PORTAL_TIMEOUT: Duration = Duration::from_secs(3);

/// An active inhibition; [`Inhibitor::release`] ends it (so does Ember
/// exiting, which closes its session bus connection).
pub struct Inhibitor {
    #[cfg(target_os = "linux")]
    request: ashpd::desktop::Request<()>,
}

impl Inhibitor {
    /// Ask the desktop not to blank, lock or suspend while `reason` holds.
    pub async fn acquire(reason: &str) -> Option<Self> {
        match tokio::time::timeout(PORTAL_TIMEOUT, Self::request(reason)).await {
            Ok(Ok(inhibitor)) => Some(inhibitor),
            Ok(Err(e)) => {
                tracing::info!("cannot keep the screen awake: {e:#}");
                None
            }
            Err(_) => {
                tracing::info!("cannot keep the screen awake: the desktop portal did not answer");
                None
            }
        }
    }

    #[cfg(target_os = "linux")]
    async fn request(reason: &str) -> anyhow::Result<Self> {
        use ashpd::desktop::inhibit::{InhibitFlags, InhibitOptions, InhibitProxy};
        let proxy = InhibitProxy::new().await?;
        let request = proxy
            .inhibit(
                None,
                InhibitFlags::Idle | InhibitFlags::Suspend,
                InhibitOptions::default().set_reason(reason),
            )
            .await?;
        Ok(Self { request })
    }

    #[cfg(not(target_os = "linux"))]
    async fn request(_reason: &str) -> anyhow::Result<Self> {
        anyhow::bail!("not supported on this platform")
    }

    /// [`Inhibitor::acquire`] with an owned reason, for spawning.
    pub async fn acquire_owned(reason: String) -> Option<Self> {
        Self::acquire(&reason).await
    }

    /// End the inhibition.
    pub async fn release(self) {
        #[cfg(target_os = "linux")]
        if let Err(e) = self.request.close().await {
            tracing::debug!("ending the screen inhibition: {e:#}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Talks to the real desktop. Run with `cargo test -p ember-services --
    /// --ignored keeps_the_screen_awake` while watching the session bus
    /// (`dbus-monitor --session`): on KDE the portal turns it into
    /// `PolicyAgent.AddInhibition`, and release into `ReleaseInhibition`.
    #[tokio::test]
    #[ignore = "needs a desktop session with a portal"]
    async fn keeps_the_screen_awake() {
        let inhibitor = Inhibitor::acquire("Ember test")
            .await
            .expect("portal answered");
        inhibitor.release().await;
    }
}
