use crate::paths::Paths;
use std::io;

// =========================================================================
// Unix implementation
// =========================================================================

#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::process::CommandExt;

    pub use nix::sys::signal::Signal;

    use crate::process::ProcessError;

    pub fn parse_signal(name: &str) -> Result<Signal, ProcessError> {
        use std::str::FromStr;
        let normalized = if name.starts_with("SIG") {
            name.to_string()
        } else {
            format!("SIG{name}")
        };
        Signal::from_str(&normalized).map_err(|_| ProcessError::InvalidSignal(name.to_string()))
    }

    pub fn send_signal(pid: u32, signal: Signal) -> io::Result<()> {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), signal)
            .map_err(|e| io::Error::other(e))
    }

    pub fn is_pid_alive(pid: u32) -> bool {
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_ok()
    }

    pub fn check_pid(pid: u32) -> Result<bool, io::Error> {
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None) {
            Ok(()) => Ok(true),
            Err(nix::errno::Errno::ESRCH) => Ok(false),
            Err(nix::errno::Errno::EPERM) => Ok(true),
            Err(e) => Err(io::Error::other(e)),
        }
    }

    pub fn force_kill(pid: u32) -> io::Result<()> {
        send_signal(pid, Signal::SIGKILL)
    }

    // -- IPC (async) --

    pub async fn ipc_bind(paths: &Paths) -> io::Result<tokio::net::UnixListener> {
        let socket_path = paths.socket_file();
        if socket_path.exists() {
            tokio::fs::remove_file(&socket_path).await?;
        }
        tokio::net::UnixListener::bind(&socket_path)
    }

    pub async fn ipc_cleanup(paths: &Paths) {
        let _ = tokio::fs::remove_file(paths.socket_file()).await;
    }

    pub fn ipc_exists(paths: &Paths) -> bool {
        paths.socket_file().exists()
    }

    // -- IPC (sync, client) --

    pub fn ipc_connect(paths: &Paths) -> io::Result<std::os::unix::net::UnixStream> {
        std::os::unix::net::UnixStream::connect(paths.socket_file())
    }

    // -- Daemon spawn helper --

    pub fn configure_daemon_cmd(cmd: &mut std::process::Command) {
        cmd.process_group(0);
    }

    // -- Signal shutdown (async) --

    pub async fn signal_shutdown() {
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
        let mut sigint =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();

        tokio::select! {
            _ = sigterm.recv() => {}
            _ = sigint.recv() => {}
        }
    }

    // -- Hook shell --

    pub fn hook_command(hook: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.arg("-c").arg(hook);
        cmd
    }
}

// =========================================================================
// Windows implementation
// =========================================================================

#[cfg(windows)]
mod platform {
    use super::*;

    use crate::process::ProcessError;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Signal {
        Term,
        Kill,
        Int,
    }

    impl std::fmt::Display for Signal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Signal::Term => write!(f, "SIGTERM"),
                Signal::Kill => write!(f, "SIGKILL"),
                Signal::Int => write!(f, "SIGINT"),
            }
        }
    }

    pub fn parse_signal(name: &str) -> Result<Signal, ProcessError> {
        let normalized = name.to_uppercase();
        let normalized = if normalized.starts_with("SIG") {
            &normalized[3..]
        } else {
            &normalized
        };
        match normalized {
            "TERM" => Ok(Signal::Term),
            "KILL" => Ok(Signal::Kill),
            "INT" => Ok(Signal::Int),
            _ => Err(ProcessError::InvalidSignal(name.to_string())),
        }
    }

    pub fn send_signal(pid: u32, _signal: Signal) -> io::Result<()> {
        // On Windows, we can only terminate a process (no fine-grained signals).
        terminate_process(pid)
    }

    pub fn is_pid_alive(pid: u32) -> bool {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            GetExitCodeProcess, OpenProcess, PROCESS_QUERY_INFORMATION,
        };

        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_INFORMATION, 0, pid);
            if handle == 0 {
                return false;
            }
            let mut exit_code: u32 = 0;
            let result = GetExitCodeProcess(handle, &mut exit_code);
            CloseHandle(handle);
            // STILL_ACTIVE = 259
            result != 0 && exit_code == 259
        }
    }

    pub fn check_pid(pid: u32) -> Result<bool, io::Error> {
        Ok(is_pid_alive(pid))
    }

    pub fn force_kill(pid: u32) -> io::Result<()> {
        terminate_process(pid)
    }

    fn terminate_process(pid: u32) -> io::Result<()> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_TERMINATE, TerminateProcess,
        };

        unsafe {
            let handle = OpenProcess(PROCESS_TERMINATE, 0, pid);
            if handle == 0 {
                return Err(io::Error::last_os_error());
            }
            let result = TerminateProcess(handle, 1);
            CloseHandle(handle);
            if result == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    // -- IPC (async) --

    pub async fn ipc_bind(paths: &Paths) -> io::Result<tokio::net::TcpListener> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let port = listener.local_addr()?.port();
        tokio::fs::write(paths.port_file(), port.to_string()).await?;
        Ok(listener)
    }

    pub async fn ipc_cleanup(paths: &Paths) {
        let _ = tokio::fs::remove_file(paths.port_file()).await;
    }

    pub fn ipc_exists(paths: &Paths) -> bool {
        paths.port_file().exists()
    }

    // -- IPC (sync, client) --

    pub fn ipc_connect(paths: &Paths) -> io::Result<std::net::TcpStream> {
        let port_str = std::fs::read_to_string(paths.port_file())?;
        let port: u16 = port_str
            .trim()
            .parse()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        std::net::TcpStream::connect(("127.0.0.1", port))
    }

    // -- Daemon spawn helper --

    pub fn configure_daemon_cmd(cmd: &mut std::process::Command) {
        use std::os::windows::process::CommandExt;
        // CREATE_NEW_PROCESS_GROUP = 0x00000200
        cmd.creation_flags(0x00000200);
    }

    // -- Signal shutdown (async) --

    pub async fn signal_shutdown() {
        tokio::signal::ctrl_c().await.ok();
    }

    // -- Hook shell --

    pub fn hook_command(hook: &str) -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("cmd");
        cmd.arg("/C").arg(hook);
        cmd
    }
}

// =========================================================================
// Re-exports
// =========================================================================

pub use platform::*;

// =========================================================================
// Type aliases for IPC streams that differ by platform
// =========================================================================

#[cfg(unix)]
pub type IpcListener = tokio::net::UnixListener;

#[cfg(windows)]
pub type IpcListener = tokio::net::TcpListener;

#[cfg(unix)]
pub type IpcStream = tokio::net::UnixStream;

#[cfg(windows)]
pub type IpcStream = tokio::net::TcpStream;

#[cfg(unix)]
pub type SyncIpcStream = std::os::unix::net::UnixStream;

#[cfg(windows)]
pub type SyncIpcStream = std::net::TcpStream;

// Helper to accept from an IpcListener returning an IpcStream
pub async fn ipc_accept(listener: &IpcListener) -> io::Result<IpcStream> {
    let (stream, _addr) = listener.accept().await?;
    Ok(stream)
}
