use crate::config::ProcessConfig;
use crate::paths::Paths;
use crate::protocol::{ProcessInfo, ProcessStatus};
use std::collections::HashMap;
use std::str::FromStr;
use tokio::fs;
use tokio::process::{Child, Command};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

pub const DEFAULT_KILL_TIMEOUT_MS: u64 = 5000;
pub const DEFAULT_KILL_SIGNAL: &str = "SIGTERM";

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    #[error("invalid command: {0}")]
    InvalidCommand(String),
    #[error("failed to spawn process: {0}")]
    SpawnFailed(#[from] std::io::Error),
    #[error("process not found: {0}")]
    NotFound(String),
    #[error("invalid signal: {0}")]
    InvalidSignal(String),
}

// ---------------------------------------------------------------------------
// Command parsing
// ---------------------------------------------------------------------------

pub fn parse_command(command: &str) -> Result<(String, Vec<String>), ProcessError> {
    let words = shell_words::split(command)
        .map_err(|e| ProcessError::InvalidCommand(format!("failed to parse: {e}")))?;

    if words.is_empty() {
        return Err(ProcessError::InvalidCommand("command is empty".to_string()));
    }

    let program = words[0].clone();
    let args = words[1..].to_vec();
    Ok((program, args))
}

// ---------------------------------------------------------------------------
// Signal parsing
// ---------------------------------------------------------------------------

pub fn parse_signal(name: &str) -> Result<nix::sys::signal::Signal, ProcessError> {
    let normalized = if name.starts_with("SIG") {
        name.to_string()
    } else {
        format!("SIG{name}")
    };
    nix::sys::signal::Signal::from_str(&normalized)
        .map_err(|_| ProcessError::InvalidSignal(name.to_string()))
}

// ---------------------------------------------------------------------------
// ManagedProcess
// ---------------------------------------------------------------------------

pub struct ManagedProcess {
    pub name: String,
    pub config: ProcessConfig,
    pub child: Child,
    pub status: ProcessStatus,
    pub started_at: tokio::time::Instant,
    pub restarts: u32,
}

impl ManagedProcess {
    pub fn to_process_info(&self) -> ProcessInfo {
        ProcessInfo {
            name: self.name.clone(),
            pid: self.child.id(),
            status: self.status,
            uptime: Some(self.started_at.elapsed().as_secs()),
            restarts: self.restarts,
            cpu_percent: None,
            memory_bytes: None,
            group: self.config.group.clone(),
        }
    }

    pub async fn graceful_stop(&mut self) -> Result<(), ProcessError> {
        let raw_pid = match self.child.id() {
            Some(pid) => pid,
            None => {
                // Process already exited
                self.status = ProcessStatus::Stopped;
                return Ok(());
            }
        };

        let signal_name = self
            .config
            .kill_signal
            .as_deref()
            .unwrap_or(DEFAULT_KILL_SIGNAL);
        let signal = parse_signal(signal_name)?;

        let timeout_ms = self.config.kill_timeout.unwrap_or(DEFAULT_KILL_TIMEOUT_MS);
        let duration = std::time::Duration::from_millis(timeout_ms);

        let pid = nix::unistd::Pid::from_raw(raw_pid as i32);
        let _ = nix::sys::signal::kill(pid, signal);

        match tokio::time::timeout(duration, self.child.wait()).await {
            Ok(_) => {
                // Process exited within timeout
                self.status = ProcessStatus::Stopped;
            }
            Err(_) => {
                // Timeout expired — escalate to SIGKILL
                let _ = nix::sys::signal::kill(pid, nix::sys::signal::Signal::SIGKILL);
                let _ = self.child.wait().await;
                self.status = ProcessStatus::Stopped;
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// ProcessTable
// ---------------------------------------------------------------------------

pub type ProcessTable = HashMap<String, ManagedProcess>;

// ---------------------------------------------------------------------------
// Spawning
// ---------------------------------------------------------------------------

pub async fn spawn_process(
    name: String,
    config: ProcessConfig,
    paths: &Paths,
) -> Result<ManagedProcess, ProcessError> {
    let (program, args) = parse_command(&config.command)?;

    fs::create_dir_all(paths.log_dir()).await?;

    let stdout_file = fs::File::create(paths.stdout_log(&name))
        .await?
        .into_std()
        .await;
    let stderr_file = fs::File::create(paths.stderr_log(&name))
        .await?
        .into_std()
        .await;

    let mut cmd = Command::new(&program);
    cmd.args(&args);

    if let Some(ref cwd) = config.cwd {
        cmd.current_dir(cwd);
    }

    cmd.stdin(std::process::Stdio::null());
    cmd.stdout(std::process::Stdio::from(stdout_file));
    cmd.stderr(std::process::Stdio::from(stderr_file));

    let child = cmd.spawn().map_err(ProcessError::SpawnFailed)?;

    Ok(ManagedProcess {
        name,
        config,
        child,
        status: ProcessStatus::Online,
        started_at: tokio::time::Instant::now(),
        restarts: 0,
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_command() {
        let (prog, args) = parse_command("node server.js").unwrap();
        assert_eq!(prog, "node");
        assert_eq!(args, vec!["server.js"]);
    }

    #[test]
    fn test_parse_command_no_args() {
        let (prog, args) = parse_command("sleep").unwrap();
        assert_eq!(prog, "sleep");
        assert!(args.is_empty());
    }

    #[test]
    fn test_parse_command_multiple_args() {
        let (prog, args) = parse_command("echo hello world").unwrap();
        assert_eq!(prog, "echo");
        assert_eq!(args, vec!["hello", "world"]);
    }

    #[test]
    fn test_parse_command_quoted_args() {
        let (prog, args) = parse_command(r#"bash -c "echo hello""#).unwrap();
        assert_eq!(prog, "bash");
        assert_eq!(args, vec!["-c", "echo hello"]);
    }

    #[test]
    fn test_parse_command_single_quotes() {
        let (prog, args) = parse_command("echo 'hello world'").unwrap();
        assert_eq!(prog, "echo");
        assert_eq!(args, vec!["hello world"]);
    }

    #[test]
    fn test_parse_empty_command() {
        let result = parse_command("");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProcessError::InvalidCommand(_)
        ));
    }

    #[test]
    fn test_parse_whitespace_only() {
        let result = parse_command("   ");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProcessError::InvalidCommand(_)
        ));
    }

    // -------------------------------------------------------------------
    // Graceful stop constants & parse_signal
    // -------------------------------------------------------------------

    #[test]
    fn test_default_kill_timeout() {
        assert_eq!(DEFAULT_KILL_TIMEOUT_MS, 5000);
    }

    #[test]
    fn test_default_kill_signal() {
        assert_eq!(DEFAULT_KILL_SIGNAL, "SIGTERM");
    }

    #[test]
    fn test_parse_signal_sigterm() {
        let sig = parse_signal("SIGTERM").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGTERM);
    }

    #[test]
    fn test_parse_signal_sigint() {
        let sig = parse_signal("SIGINT").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGINT);
    }

    #[test]
    fn test_parse_signal_sighup() {
        let sig = parse_signal("SIGHUP").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGHUP);
    }

    #[test]
    fn test_parse_signal_sigusr1() {
        let sig = parse_signal("SIGUSR1").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGUSR1);
    }

    #[test]
    fn test_parse_signal_sigusr2() {
        let sig = parse_signal("SIGUSR2").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGUSR2);
    }

    #[test]
    fn test_parse_signal_without_sig_prefix() {
        let sig = parse_signal("TERM").unwrap();
        assert_eq!(sig, nix::sys::signal::Signal::SIGTERM);
    }

    #[test]
    fn test_parse_signal_invalid() {
        let result = parse_signal("BOGUS");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProcessError::InvalidSignal(_)
        ));
    }

    #[test]
    fn test_parse_signal_empty() {
        let result = parse_signal("");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ProcessError::InvalidSignal(_)
        ));
    }

    #[test]
    fn test_config_kill_timeout_default_when_none() {
        let val: Option<u64> = None;
        assert_eq!(val.unwrap_or(DEFAULT_KILL_TIMEOUT_MS), 5000);
    }

    #[test]
    fn test_config_kill_signal_default_when_none() {
        let val: Option<&str> = None;
        assert_eq!(val.unwrap_or(DEFAULT_KILL_SIGNAL), "SIGTERM");
    }
}
