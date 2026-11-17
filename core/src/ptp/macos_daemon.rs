//! macOS competing PTP daemon detection and lock recovery.
//!
//! On macOS, `ptpcamerad` and `mscamerad-xpc` can automatically open a newly connected
//! PTP/MTP camera over USB, causing `claim_interface` to fail with `Busy` or `Access`.
//! We inspect and cleanly release only when an actual claim conflict occurs.

#[cfg(target_os = "macos")]
use tracing::{info, warn};

#[cfg(target_os = "macos")]
const COMPETING_PROCESSES: &[&str] = &["ptpcamerad", "mscamerad-xpc", "PTPCamera"];

/// Returns names of known macOS PTP/camera daemons currently running on the system.
pub fn detect_competing_daemons() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        let mut active = Vec::new();
        for name in COMPETING_PROCESSES {
            if let Ok(output) = std::process::Command::new("pgrep")
                .args(["-x", name])
                .output()
            {
                if output.status.success() && !output.stdout.is_empty() {
                    active.push((*name).to_string());
                }
            }
        }
        active
    }

    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

/// Attempts to release competing macOS camera daemons after a USB claim conflict (`Busy`/`Access`).
/// Returns `true` if at least one competing daemon process was signaled to release the interface.
pub fn release_competing_daemons() -> bool {
    #[cfg(target_os = "macos")]
    {
        let mut released_any = false;
        for name in COMPETING_PROCESSES {
            if let Ok(output) = std::process::Command::new("killall")
                .args(["-TERM", name])
                .output()
            {
                if output.status.success() {
                    info!(daemon = name, "released competing macOS PTP daemon lock");
                    released_any = true;
                }
            }
        }
        if released_any {
            std::thread::sleep(std::time::Duration::from_millis(250));
        } else {
            warn!("USB interface claim conflicted, but no known macOS PTP daemon was running");
        }
        released_any
    }

    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}
