use crate::process::{ProcessError, ProcessTable};
use crate::protocol::ProcessStatus;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::sync::{RwLock, watch};

pub const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(1);
pub const HEALTH_CHECK_TIMEOUT_SECS: u64 = 30;
pub const HEALTH_CHECK_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq)]
pub enum HealthCheckTarget {
    Http(String),
    Tcp(String, u16),
}

pub fn parse_health_check(url: &str) -> Result<HealthCheckTarget, ProcessError> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(HealthCheckTarget::Http(url.to_string()))
    } else if let Some(rest) = url.strip_prefix("tcp://") {
        let (host, port_str) = if let Some(rest) = rest.strip_prefix('[') {
            // IPv6 bracketed: [host]:port
            rest.split_once("]:").ok_or_else(|| {
                ProcessError::InvalidCommand(format!(
                    "invalid TCP health check URL (bad IPv6 format): {url}"
                ))
            })?
        } else {
            // IPv4 / hostname: host:port
            rest.rsplit_once(':').ok_or_else(|| {
                ProcessError::InvalidCommand(format!(
                    "invalid TCP health check URL (missing port): {url}"
                ))
            })?
        };
        let port: u16 = port_str.parse().map_err(|_| {
            ProcessError::InvalidCommand(format!("invalid TCP health check port: {port_str}"))
        })?;
        Ok(HealthCheckTarget::Tcp(host.to_string(), port))
    } else {
        Err(ProcessError::InvalidCommand(format!(
            "unsupported health check scheme: {url}"
        )))
    }
}

async fn check_http(client: &reqwest::Client, url: &str) -> bool {
    match client.get(url).send().await {
        Ok(resp) => resp.status().is_success(),
        Err(_) => false,
    }
}

async fn check_tcp(host: &str, port: u16) -> bool {
    let addr = if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    };
    tokio::time::timeout(HEALTH_CHECK_ATTEMPT_TIMEOUT, TcpStream::connect(&addr))
        .await
        .map(|r| r.is_ok())
        .unwrap_or(false)
}

async fn check_target(client: &reqwest::Client, target: &HealthCheckTarget) -> bool {
    match target {
        HealthCheckTarget::Http(url) => check_http(client, url).await,
        HealthCheckTarget::Tcp(host, port) => check_tcp(host, *port).await,
    }
}

async fn set_unhealthy_if_starting(name: &str, processes: &Arc<RwLock<ProcessTable>>) {
    let mut table = processes.write().await;
    if let Some(managed) = table.get_mut(name)
        && managed.status == ProcessStatus::Starting
    {
        managed.status = ProcessStatus::Unhealthy;
    }
}

async fn set_online_if_starting(name: &str, processes: &Arc<RwLock<ProcessTable>>) {
    let mut table = processes.write().await;
    if let Some(managed) = table.get_mut(name)
        && managed.status == ProcessStatus::Starting
    {
        managed.status = ProcessStatus::Online;
    }
}

enum WaitOutcome {
    Passed,
    TimedOut,
    Aborted,
}

async fn wait_for_check_pass(
    name: &str,
    target: &HealthCheckTarget,
    timeout_secs: u64,
    processes: &Arc<RwLock<ProcessTable>>,
    client: &reqwest::Client,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> WaitOutcome {
    for _ in 0..timeout_secs {
        // Check shutdown signal
        if *shutdown_rx.borrow() {
            return WaitOutcome::Aborted;
        }

        // Check if process is still in Starting state
        {
            let table = processes.read().await;
            match table.get(name) {
                Some(managed) if managed.status == ProcessStatus::Starting => {}
                _ => return WaitOutcome::Aborted,
            }
        }

        if check_target(client, target).await {
            return WaitOutcome::Passed;
        }

        // Wait before next attempt, also listening for shutdown
        tokio::select! {
            _ = tokio::time::sleep(HEALTH_CHECK_INTERVAL) => {}
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    return WaitOutcome::Aborted;
                }
            }
        }
    }

    WaitOutcome::TimedOut
}

pub fn spawn_startup_checker(
    name: String,
    readiness_check: Option<String>,
    readiness_timeout_secs: Option<u64>,
    health_check: Option<String>,
    processes: Arc<RwLock<ProcessTable>>,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let mut parsed_checks: Vec<(&str, HealthCheckTarget, u64)> = Vec::new();

        if let Some(readiness_check) = readiness_check {
            let target = match parse_health_check(&readiness_check) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("invalid readiness check for '{name}': {e}");
                    set_unhealthy_if_starting(&name, &processes).await;
                    return;
                }
            };
            parsed_checks.push((
                "readiness",
                target,
                readiness_timeout_secs.unwrap_or(HEALTH_CHECK_TIMEOUT_SECS),
            ));
        }

        if let Some(health_check) = health_check {
            let target = match parse_health_check(&health_check) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("invalid health check for '{name}': {e}");
                    set_unhealthy_if_starting(&name, &processes).await;
                    return;
                }
            };
            parsed_checks.push(("health", target, HEALTH_CHECK_TIMEOUT_SECS));
        }

        if parsed_checks.is_empty() {
            return;
        }

        let client = match reqwest::Client::builder()
            .timeout(HEALTH_CHECK_ATTEMPT_TIMEOUT)
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                eprintln!("failed to build HTTP client for '{name}': {e}");
                set_unhealthy_if_starting(&name, &processes).await;
                return;
            }
        };

        for (check_kind, target, timeout_secs) in &parsed_checks {
            match wait_for_check_pass(
                &name,
                target,
                *timeout_secs,
                &processes,
                &client,
                &mut shutdown_rx,
            )
            .await
            {
                WaitOutcome::Passed => {}
                WaitOutcome::TimedOut => {
                    eprintln!("{check_kind} check timed out for '{name}' after {timeout_secs}s");
                    set_unhealthy_if_starting(&name, &processes).await;
                    return;
                }
                WaitOutcome::Aborted => return,
            }
        }

        set_online_if_starting(&name, &processes).await;
    });
}

pub fn spawn_health_checker(
    name: String,
    health_check: String,
    processes: Arc<RwLock<ProcessTable>>,
    shutdown_rx: watch::Receiver<bool>,
) {
    spawn_startup_checker(name, None, None, Some(health_check), processes, shutdown_rx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_http_url() {
        let result = parse_health_check("http://127.0.0.1:3000/health").unwrap();
        assert_eq!(
            result,
            HealthCheckTarget::Http("http://127.0.0.1:3000/health".to_string())
        );
    }

    #[test]
    fn test_parse_https_url() {
        let result = parse_health_check("https://localhost:8443/ready").unwrap();
        assert_eq!(
            result,
            HealthCheckTarget::Http("https://localhost:8443/ready".to_string())
        );
    }

    #[test]
    fn test_parse_tcp_url() {
        let result = parse_health_check("tcp://127.0.0.1:5432").unwrap();
        assert_eq!(
            result,
            HealthCheckTarget::Tcp("127.0.0.1".to_string(), 5432)
        );
    }

    #[test]
    fn test_parse_tcp_url_with_hostname() {
        let result = parse_health_check("tcp://localhost:6379").unwrap();
        assert_eq!(
            result,
            HealthCheckTarget::Tcp("localhost".to_string(), 6379)
        );
    }

    #[test]
    fn test_parse_invalid_scheme() {
        let result = parse_health_check("ftp://example.com");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_tcp_missing_port() {
        let result = parse_health_check("tcp://127.0.0.1");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_tcp_invalid_port() {
        let result = parse_health_check("tcp://127.0.0.1:abc");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_empty_string() {
        let result = parse_health_check("");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_tcp_ipv6_bracketed() {
        let result = parse_health_check("tcp://[::1]:5432").unwrap();
        assert_eq!(result, HealthCheckTarget::Tcp("::1".to_string(), 5432));
    }

    #[test]
    fn test_parse_tcp_ipv6_full() {
        let result = parse_health_check("tcp://[2001:db8::1]:8080").unwrap();
        assert_eq!(
            result,
            HealthCheckTarget::Tcp("2001:db8::1".to_string(), 8080)
        );
    }

    #[test]
    fn test_parse_tcp_ipv6_missing_bracket() {
        let result = parse_health_check("tcp://[::1:8080");
        assert!(result.is_err());
    }
}
