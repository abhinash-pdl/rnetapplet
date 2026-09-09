use anyhow::{Context, Result};

use super::NmClient;

impl NmClient {
    pub async fn get_wireless_enabled(&self) -> Result<bool> {
        self.nm
            .wireless_enabled()
            .await
            .context("reading NM WirelessEnabled")
    }

    pub async fn set_wireless_enabled(&self, enabled: bool) -> Result<()> {
        self.nm
            .set_wireless_enabled(enabled)
            .await
            .with_context(|| format!("setting NM WirelessEnabled={enabled}"))
    }

    pub async fn set_airplane_mode(&self, airplane: bool) -> Result<()> {
        self.nm
            .enable(!airplane)
            .await
            .with_context(|| format!("setting NM Enable(airplane={airplane})"))
    }
}
