use crate::paths::Paths;
use crate::process::{self, ProcessError, ProcessTable};
use crate::protocol::ProcessStatus;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{RwLock, watch};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const MEMORY_CHECK_INTERVAL: Duration = Duration::from_secs(5);
pub const STATS_POLL_INTERVAL: Duration = Duration::from_secs(2);

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

pub fn parse_memory_string(s: &str) -> Result<u64, ProcessError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(ProcessError::InvalidCommand(
            "empty memory string".to_string(),
        ));
    }

    // Find where the numeric part ends and suffix begins
    let suffix_start = s.find(|c: char| c.is_alphabetic()).unwrap_or(s.len());

    let num_part = &s[..suffix_start];
    let suffix = s[suffix_start..].trim();

    if num_part.is_empty() {
        return Err(ProcessError::InvalidCommand(format!(
            "no numeric value in memory string: {s}"
        )));
    }

    let value: f64 = num_part.parse().map_err(|_| {
        ProcessError::InvalidCommand(format!("invalid number in memory string: {num_part}"))
    })?;

    let multiplier: u64 = match suffix.to_uppercase().as_str() {
        "" => 1,
        "K" | "KB" => 1024,
        "M" | "MB" => 1024 * 1024,
        "G" | "GB" => 1024 * 1024 * 1024,
        other => {
            return Err(ProcessError::InvalidCommand(format!(
                "unknown memory suffix: {other}"
            )));
        }
    };

    Ok((value * multiplier as f64) as u64)
}

// ---------------------------------------------------------------------------
// RSS reading
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub async fn read_rss_bytes(pid: u32) -> Option<u64> {
    let output = tokio::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let kb: u64 = text.trim().parse().ok()?;
    Some(kb * 1024)
}

#[cfg(windows)]
pub async fn read_rss_bytes(_pid: u32) -> Option<u64> {
    None
}

// ---------------------------------------------------------------------------
// Process stats (CPU + memory)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct ProcessStats {
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
}

pub type StatsCache = HashMap<u32, ProcessStats>;

#[cfg(unix)]
pub async fn read_process_stats(pid: u32) -> Option<(f64, u64)> {
    let output = tokio::process::Command::new("ps")
        .args(["-o", "%cpu=,rss=", "-p", &pid.to_string()])
        .output()
        .await
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    let mut parts = text.split_whitespace();
    let cpu: f64 = parts.next()?.parse().ok()?;
    let rss_kb: u64 = parts.next()?.parse().ok()?;
    Some((cpu, rss_kb * 1024))
}

#[cfg(windows)]
pub async fn read_process_stats(_pid: u32) -> Option<(f64, u64)> {
    None
}

pub fn spawn_stats_collector(
    processes: Arc<RwLock<ProcessTable>>,
    stats_cache: Arc<RwLock<StatsCache>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = tokio::time::sleep(STATS_POLL_INTERVAL) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
            }

            if *shutdown_rx.borrow() {
                return;
            }

            // Collect PIDs of running processes
            let pids: Vec<(String, u32)> = {
                let table = processes.read().await;
                table
                    .iter()
                    .filter(|(_, m)| {
                        matches!(m.status, ProcessStatus::Online | ProcessStatus::Starting)
                    })
                    .filter_map(|(name, m)| m.pid.map(|pid| (name.clone(), pid)))
                    .collect()
            };

            let mut new_cache = HashMap::new();
            for (_name, pid) in &pids {
                if let Some((cpu, mem)) = read_process_stats(*pid).await {
                    new_cache.insert(
                        *pid,
                        ProcessStats {
                            cpu_percent: Some(cpu),
                            memory_bytes: Some(mem),
                        },
                    );
                }
            }

            *stats_cache.write().await = new_cache;
        }
    });
}

// ---------------------------------------------------------------------------
// Memory monitor task
// ---------------------------------------------------------------------------

pub fn spawn_memory_monitor(
    name: String,
    max_memory_str: String,
    processes: Arc<RwLock<ProcessTable>>,
    paths: Paths,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let max_bytes = match parse_memory_string(&max_memory_str) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("invalid max_memory for '{name}': {e}");
                return;
            }
        };

        loop {
            // Wait for next check interval, listening for shutdown
            tokio::select! {
                _ = tokio::time::sleep(MEMORY_CHECK_INTERVAL) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                }
            }

            // Check shutdown before proceeding
            if *shutdown_rx.borrow() {
                return;
            }

            // Read PID from process table
            let pid = {
                let table = processes.read().await;
                match table.get(&name) {
                    Some(managed)
                        if managed.status == ProcessStatus::Online
                            || managed.status == ProcessStatus::Starting =>
                    {
                        managed.pid
                    }
                    _ => return, // Process gone or stopped
                }
            };

            let pid = match pid {
                Some(p) => p,
                None => continue,
            };

            // Read RSS
            let rss = match read_rss_bytes(pid).await {
                Some(r) => r,
                None => continue,
            };

            if rss <= max_bytes {
                continue;
            }

            // Memory limit exceeded — kill and restart
            eprintln!(
                "memory limit exceeded for '{}': {} bytes > {} bytes, restarting",
                name, rss, max_bytes
            );

            // Acquire write lock, signal monitor_shutdown to prevent handle_child_exit from restarting
            let (config, old_restarts, raw_pid) = {
                let mut table = processes.write().await;
                let managed = match table.get_mut(&name) {
                    Some(m) => m,
                    None => return,
                };

                // Signal the process monitor not to auto-restart
                if let Some(ref tx) = managed.monitor_shutdown {
                    let _ = tx.send(true);
                }

                let config = managed.config.clone();
                let restarts = managed.restarts;
                let raw_pid = managed.pid;
                (config, restarts, raw_pid)
            };

            // Kill the process
            if let Some(raw_pid) = raw_pid {
                let signal_name = config
                    .kill_signal
                    .as_deref()
                    .unwrap_or(process::DEFAULT_KILL_SIGNAL);
                if let Ok(signal) = process::parse_signal(signal_name) {
                    let _ = crate::sys::send_signal(raw_pid, signal);

                    // Poll for process exit
                    let timeout_ms = config
                        .kill_timeout
                        .unwrap_or(process::DEFAULT_KILL_TIMEOUT_MS);
                    let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
                    while crate::sys::is_pid_alive(raw_pid) {
                        if tokio::time::Instant::now() >= deadline {
                            let _ = crate::sys::force_kill(raw_pid);
                            tokio::time::sleep(Duration::from_millis(100)).await;
                            break;
                        }
                        tokio::time::sleep(Duration::from_millis(50)).await;
                    }
                }
            }

            // Wait for handle_child_exit to mark process Stopped
            tokio::time::sleep(Duration::from_millis(200)).await;

            // Spawn replacement process (and attach monitors)
            match process::spawn_and_attach(
                name.clone(),
                config.clone(),
                old_restarts + 1,
                &processes,
                &paths,
            )
            .await
            {
                Ok(()) => return, // This monitor instance terminates; the new one takes over
                Err(e) => {
                    eprintln!("failed to restart '{name}' after memory limit: {e}");
                    let mut table = processes.write().await;
                    if let Some(managed) = table.get_mut(&name) {
                        managed.status = ProcessStatus::Errored;
                        managed.pid = None;
                    }
                    return;
                }
            }
        }
    });
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_megabytes() {
        assert_eq!(parse_memory_string("200M").unwrap(), 200 * 1024 * 1024);
    }

    #[test]
    fn test_parse_megabytes_mb() {
        assert_eq!(parse_memory_string("200MB").unwrap(), 200 * 1024 * 1024);
    }

    #[test]
    fn test_parse_gigabytes() {
        assert_eq!(parse_memory_string("1G").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn test_parse_gigabytes_gb() {
        assert_eq!(parse_memory_string("2GB").unwrap(), 2 * 1024 * 1024 * 1024);
    }

    #[test]
    fn test_parse_kilobytes() {
        assert_eq!(parse_memory_string("512K").unwrap(), 512 * 1024);
    }

    #[test]
    fn test_parse_kilobytes_kb() {
        assert_eq!(parse_memory_string("512KB").unwrap(), 512 * 1024);
    }

    #[test]
    fn test_parse_plain_bytes() {
        assert_eq!(parse_memory_string("1048576").unwrap(), 1048576);
    }

    #[test]
    fn test_parse_fractional() {
        let result = parse_memory_string("1.5G").unwrap();
        let expected = (1.5 * 1024.0 * 1024.0 * 1024.0) as u64;
        assert_eq!(result, expected);
    }

    #[test]
    fn test_parse_case_insensitive() {
        assert_eq!(parse_memory_string("200m").unwrap(), 200 * 1024 * 1024);
    }

    #[test]
    fn test_parse_with_whitespace() {
        assert_eq!(parse_memory_string("  200M  ").unwrap(), 200 * 1024 * 1024);
    }

    #[test]
    fn test_parse_empty_errors() {
        assert!(parse_memory_string("").is_err());
    }

    #[test]
    fn test_parse_invalid_suffix_errors() {
        assert!(parse_memory_string("200X").is_err());
    }

    #[test]
    fn test_parse_no_number_errors() {
        assert!(parse_memory_string("MB").is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_read_rss_current_process() {
        let pid = std::process::id();
        let rss = read_rss_bytes(pid).await;
        assert!(rss.is_some());
        assert!(rss.unwrap() > 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_read_rss_nonexistent_pid() {
        let rss = read_rss_bytes(999_999_999).await;
        assert!(rss.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_read_process_stats_current_process() {
        let pid = std::process::id();
        let stats = read_process_stats(pid).await;
        assert!(stats.is_some());
        let (cpu, mem) = stats.unwrap();
        assert!(cpu >= 0.0);
        assert!(mem > 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_read_process_stats_nonexistent_pid() {
        let stats = read_process_stats(999_999_999).await;
        assert!(stats.is_none());
    }
}
