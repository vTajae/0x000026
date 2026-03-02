//! systemd watchdog integration — sd_notify for health pings and readiness.
//!
//! When running under systemd with `WatchdogSec=N`, this module spawns a
//! background task that:
//! 1. Sends `READY=1` on startup to signal readiness.
//! 2. Periodically sends `WATCHDOG=1` if the daemon is healthy.
//! 3. If health checks fail, skips the notification — systemd will kill and
//!    restart the daemon after the watchdog timeout expires.
//!
//! On non-Linux platforms or when `NOTIFY_SOCKET` is not set, all operations
//! are no-ops.

use std::sync::Arc;
use tracing::{debug, info, warn};

/// Send an sd_notify message to systemd.
///
/// Returns `true` if the message was sent, `false` if NOTIFY_SOCKET is not set
/// or the platform doesn't support it.
#[cfg(target_os = "linux")]
fn sd_notify(msg: &str) -> bool {
    use std::os::unix::net::UnixDatagram;

    let socket_path = match std::env::var("NOTIFY_SOCKET") {
        Ok(p) if !p.is_empty() => p,
        _ => return false,
    };

    let sock = match UnixDatagram::unbound() {
        Ok(s) => s,
        Err(e) => {
            warn!("sd_notify: failed to create socket: {e}");
            return false;
        }
    };

    // systemd supports abstract sockets (prefixed with @) and filesystem paths.
    // std UnixDatagram::send_to works directly for filesystem paths.
    // For abstract sockets, send_to with the @-prefixed path is attempted.
    match sock.send_to(msg.as_bytes(), &socket_path) {
        Ok(_) => true,
        Err(e) => {
            warn!("sd_notify: send to {socket_path} failed: {e}");
            false
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn sd_notify(_msg: &str) -> bool {
    false
}

/// Notify systemd that the daemon is ready.
///
/// Call this after the kernel has fully booted and the HTTP server is listening.
pub fn notify_ready() {
    if sd_notify("READY=1") {
        info!("sd_notify: sent READY=1");
    }
}

/// Start the watchdog background task.
///
/// If `WATCHDOG_USEC` is set by systemd, spawns a tokio task that pings at
/// half the watchdog interval. The `health_check` closure is called each tick;
/// if it returns `false`, the watchdog ping is skipped and systemd will
/// eventually kill and restart the daemon.
///
/// This is a no-op when `WATCHDOG_USEC` is not set.
pub fn start_watchdog<F>(health_check: F)
where
    F: Fn() -> bool + Send + Sync + 'static,
{
    // Parse WATCHDOG_USEC (set by systemd when WatchdogSec is configured)
    let watchdog_usec: u64 = match std::env::var("WATCHDOG_USEC") {
        Ok(val) => match val.parse() {
            Ok(v) if v > 0 => v,
            _ => {
                debug!("WATCHDOG_USEC not a valid positive integer, watchdog disabled");
                return;
            }
        },
        Err(_) => {
            debug!("WATCHDOG_USEC not set, watchdog disabled");
            return;
        }
    };

    // Ping at half the watchdog interval (standard practice)
    let ping_interval = std::time::Duration::from_micros(watchdog_usec / 2);
    info!(
        "Watchdog enabled: pinging every {:?} (WatchdogSec={}s)",
        ping_interval,
        watchdog_usec / 1_000_000,
    );

    let health_check = Arc::new(health_check);

    tokio::spawn(async move {
        let mut interval = tokio::time::interval(ping_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            interval.tick().await;

            if (health_check)() {
                if sd_notify("WATCHDOG=1") {
                    debug!("sd_notify: sent WATCHDOG=1");
                }
            } else {
                warn!("Watchdog: health check failed, skipping sd_notify (systemd will restart)");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sd_notify_no_socket() {
        // Without NOTIFY_SOCKET set, sd_notify should return false gracefully
        std::env::remove_var("NOTIFY_SOCKET");
        assert!(!sd_notify("READY=1"));
        assert!(!sd_notify("WATCHDOG=1"));
    }

    #[test]
    fn test_notify_ready_no_panic() {
        // Should be a no-op without NOTIFY_SOCKET
        std::env::remove_var("NOTIFY_SOCKET");
        notify_ready();
    }
}
