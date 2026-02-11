use crate::paths::Paths;
use crate::process::{self, ProcessTable};
use crate::protocol::ProcessStatus;
use chrono::Utc;
use cron::Schedule;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{RwLock, watch};
pub fn parse_cron_expression(expr: &str) -> Result<Schedule, process::ProcessError> {
    let trimmed = expr.trim();
    // The `cron` crate requires 6 or 7 fields (sec min hour day month dow [year]).
    // Standard cron uses 5 fields (min hour day month dow).
    // Auto-convert 5-field to 7-field by prepending "0" (seconds) and appending "*" (year).
    let field_count = trimmed.split_whitespace().count();
    let normalized = if field_count == 5 {
        format!("0 {} *", trimmed)
    } else {
        trimmed.to_string()
    };
    Schedule::from_str(&normalized).map_err(|e| {
        process::ProcessError::InvalidCommand(format!("invalid cron expression '{}': {}", expr, e))
    })
}

pub fn next_run_duration(schedule: &Schedule) -> Option<std::time::Duration> {
    let now = Utc::now();
    let next = schedule.upcoming(Utc).next()?;
    let delta = next - now;
    delta.to_std().ok()
}
pub fn spawn_cron_restart(
    name: String,
    cron_expr: String,
    processes: Arc<RwLock<ProcessTable>>,
    paths: Paths,
    mut shutdown_rx: watch::Receiver<bool>,
) {
    tokio::spawn(async move {
        let schedule = match parse_cron_expression(&cron_expr) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("invalid cron_restart for '{}': {}", name, e);
                return;
            }
        };

        loop {
            // Calculate duration until next run
            let sleep_dur = match next_run_duration(&schedule) {
                Some(d) => d,
                None => return,
            };

            // Sleep until next cron time, listening for shutdown
            tokio::select! {
                _ = tokio::time::sleep(sleep_dur) => {}
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        return;
                    }
                    continue;
                }
            }

            if *shutdown_rx.borrow() {
                return;
            }

            // Check process is still running
            {
                let table = processes.read().await;
                match table.get(&name) {
                    Some(managed)
                        if managed.status == ProcessStatus::Online
                            || managed.status == ProcessStatus::Starting =>
                    {
                        // Process is running, proceed
                    }
                    _ => return,
                }
            }

            eprintln!("cron restart triggered for '{}'", name);

            // Graceful stop
            let (old_config, old_restarts) = {
                let mut table = processes.write().await;
                let managed = match table.get_mut(&name) {
                    Some(m) => m,
                    None => return,
                };

                if let Some(ref tx) = managed.monitor_shutdown {
                    let _ = tx.send(true);
                }

                let cfg = managed.config.clone();
                let restarts = managed.restarts;

                let _ = managed.graceful_stop().await;
                if let Some(ref hook) = cfg.post_stop {
                    let _ = process::run_hook(hook, &name, cfg.cwd.as_deref(), &paths).await;
                }

                (cfg, restarts)
            };

            // Spawn replacement (and attach monitors)
            match process::spawn_and_attach(
                name.clone(),
                old_config.clone(),
                old_restarts + 1,
                &processes,
                &paths,
            )
            .await
            {
                Ok(()) => return, // This cron instance terminates; the new one takes over
                Err(e) => {
                    eprintln!("failed to restart '{}' on cron schedule: {}", name, e);
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
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_daily_at_3am() {
        let schedule = parse_cron_expression("0 3 * * *").unwrap();
        let next = schedule.upcoming(Utc).next();
        assert!(next.is_some());
    }

    #[test]
    fn test_parse_every_5_minutes() {
        let schedule = parse_cron_expression("*/5 * * * *").unwrap();
        let next = schedule.upcoming(Utc).next();
        assert!(next.is_some());
    }

    #[test]
    fn test_parse_every_minute() {
        let schedule = parse_cron_expression("* * * * *").unwrap();
        let next = schedule.upcoming(Utc).next();
        assert!(next.is_some());
    }

    #[test]
    fn test_parse_complex_expression() {
        let schedule = parse_cron_expression("0 0 1,15 * *").unwrap();
        let next = schedule.upcoming(Utc).next();
        assert!(next.is_some());
    }

    #[test]
    fn test_parse_invalid_expression() {
        assert!(parse_cron_expression("not a cron").is_err());
    }

    #[test]
    fn test_parse_empty_expression() {
        assert!(parse_cron_expression("").is_err());
    }

    #[test]
    fn test_next_run_duration_is_positive() {
        let schedule = parse_cron_expression("* * * * *").unwrap();
        let dur = next_run_duration(&schedule);
        assert!(dur.is_some());
        // Next minute should be within 60 seconds
        assert!(dur.unwrap().as_secs() <= 60);
    }

    #[test]
    fn test_next_run_duration_every_5_min() {
        let schedule = parse_cron_expression("*/5 * * * *").unwrap();
        let dur = next_run_duration(&schedule);
        assert!(dur.is_some());
        assert!(dur.unwrap().as_secs() <= 300);
    }
}
