use std::net::IpAddr;
use std::path::{Path, PathBuf};

use super::{CaptureSpec, DesiredCaptureRow};

impl CaptureSpec {
    pub(super) fn from_row(row: DesiredCaptureRow) -> Result<Self, String> {
        let container_id = row.container_id.trim().to_string();
        if container_id.is_empty() {
            return Err(format!(
                "service {} has an empty container id",
                row.service_id
            ));
        }
        let host_text = row.host.trim().to_string();
        let host = host_text
            .parse::<IpAddr>()
            .map_err(|_| format!("service {} has a non-IP capture host", row.service_id))?;
        let port = u16::try_from(row.port)
            .ok()
            .filter(|port| *port > 0)
            .ok_or_else(|| format!("service {} has an invalid capture port", row.service_id))?;
        Ok(Self {
            service_id: row.service_id,
            container_id,
            host_text,
            host,
            port,
            challenge_id: row.challenge_id,
            participation_id: row.participation_id,
        })
    }

    pub(super) fn output_dir(&self, storage_root: &Path) -> PathBuf {
        storage_root
            .join("capture")
            .join(self.challenge_id.to_string())
            .join(self.participation_id.to_string())
    }

    /// Scope both directions to one service endpoint. Filtering by port alone
    /// leaks traffic between teams whose services expose the same port.
    pub(super) fn bpf_filter(&self) -> String {
        format!("host {} and tcp port {}", self.host, self.port)
    }
}
