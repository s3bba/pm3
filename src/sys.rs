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

    // -- PTY support --

    use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd};
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use tokio::io::unix::AsyncFd;

    /// Async reader for the master side of a PTY.
    ///
    /// Wraps an `AsyncFd<OwnedFd>` and implements `AsyncRead`.
    /// Treats `EIO` as EOF (happens when the slave side closes).
    pub struct PtyReader {
        inner: AsyncFd<OwnedFd>,
    }

    impl tokio::io::AsyncRead for PtyReader {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            loop {
                let mut guard = match self.inner.poll_read_ready(cx) {
                    Poll::Ready(Ok(guard)) => guard,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Pending => return Poll::Pending,
                };

                let result = guard.try_io(|inner| {
                    let fd = unsafe { BorrowedFd::borrow_raw(inner.as_raw_fd()) };
                    let unfilled = buf.initialize_unfilled();
                    match nix::unistd::read(fd, unfilled) {
                        Ok(0) => Ok(0),
                        Ok(n) => {
                            buf.advance(n);
                            Ok(n)
                        }
                        Err(nix::errno::Errno::EIO) => {
                            // EIO on PTY master means the slave side closed — treat as EOF
                            Ok(0)
                        }
                        Err(nix::errno::Errno::EAGAIN) => {
                            Err(io::Error::from(io::ErrorKind::WouldBlock))
                        }
                        Err(e) => Err(io::Error::from(e)),
                    }
                });

                match result {
                    Ok(Ok(_n)) => return Poll::Ready(Ok(())),
                    Ok(Err(e)) => return Poll::Ready(Err(e)),
                    Err(_would_block) => continue, // readiness was a false positive, retry
                }
            }
        }
    }

    /// Create a PTY pair. Returns `(PtyReader, OwnedFd)` where the `OwnedFd`
    /// is the slave fd to be passed as the child's stdout.
    pub fn create_pty() -> io::Result<(PtyReader, OwnedFd)> {
        let pty = nix::pty::openpty(None, None).map_err(io::Error::from)?;
        let master: OwnedFd = pty.master;
        let slave: OwnedFd = pty.slave;

        // Set master fd to non-blocking for async I/O
        nix::fcntl::fcntl(
            master.as_fd(),
            nix::fcntl::FcntlArg::F_SETFL(nix::fcntl::OFlag::O_NONBLOCK),
        )
        .map_err(io::Error::from)?;

        let async_fd = AsyncFd::new(master)?;
        Ok((PtyReader { inner: async_fd }, slave))
    }

    fn to_pid(pid: u32) -> io::Result<nix::unistd::Pid> {
        let raw = i32::try_from(pid).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("PID {pid} exceeds i32::MAX"),
            )
        })?;
        Ok(nix::unistd::Pid::from_raw(raw))
    }

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
        nix::sys::signal::kill(to_pid(pid)?, signal).map_err(io::Error::other)
    }

    pub fn is_pid_alive(pid: u32) -> bool {
        let Ok(pid) = to_pid(pid) else { return false };
        match nix::sys::signal::kill(pid, None) {
            Ok(()) => true,
            Err(nix::errno::Errno::EPERM) => true,
            Err(_) => false,
        }
    }

    pub fn check_pid(pid: u32) -> Result<bool, io::Error> {
        match nix::sys::signal::kill(to_pid(pid)?, None) {
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
            if handle.is_null() {
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
            if handle.is_null() {
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;
    use tokio::io::AsyncReadExt;

    #[test]
    fn test_create_pty_returns_valid_fds() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        rt.block_on(async {
            let (_reader, slave) = create_pty().unwrap();
            assert!(slave.as_raw_fd() >= 0);
        });
    }

    #[tokio::test]
    async fn test_pty_reader_reads_written_data() {
        let (mut reader, slave) = create_pty().unwrap();

        // Write to the slave side (simulates child process stdout)
        let written = nix::unistd::write(&slave, b"hello from pty\n").unwrap();
        assert!(written > 0);

        let mut buf = vec![0u8; 256];
        let n = reader.read(&mut buf).await.unwrap();
        let output = String::from_utf8_lossy(&buf[..n]);
        assert!(
            output.contains("hello from pty"),
            "expected 'hello from pty' in output, got: {output:?}"
        );
    }

    #[tokio::test]
    async fn test_pty_reader_eof_on_slave_close() {
        let (mut reader, slave) = create_pty().unwrap();

        // Close the slave without writing — reader should get EOF (not hang)
        drop(slave);

        let mut buf = vec![0u8; 256];
        let result =
            tokio::time::timeout(std::time::Duration::from_secs(2), reader.read(&mut buf)).await;
        match result {
            Ok(Ok(0)) => {} // EOF — expected
            Ok(Ok(_)) => {} // some terminal init bytes — fine
            Ok(Err(_)) => {}
            Err(_) => panic!("reader should not hang after slave is closed"),
        }
    }

    #[tokio::test]
    async fn test_pty_reader_multiple_writes() {
        let (mut reader, slave) = create_pty().unwrap();

        for i in 0..5 {
            let msg = format!("line {i}\n");
            nix::unistd::write(&slave, msg.as_bytes()).unwrap();
        }

        // Read all lines
        let mut buf = vec![0u8; 4096];
        let mut collected = String::new();
        // Read in a loop with a short timeout to collect all available data
        loop {
            match tokio::time::timeout(std::time::Duration::from_millis(200), reader.read(&mut buf))
                .await
            {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => collected.push_str(&String::from_utf8_lossy(&buf[..n])),
                Ok(Err(_)) => break,
                Err(_) => break, // timeout — done reading
            }
        }

        for i in 0..5 {
            assert!(
                collected.contains(&format!("line {i}")),
                "missing 'line {i}' in output: {collected:?}"
            );
        }
    }

    #[tokio::test]
    async fn test_pty_slave_is_a_tty() {
        let (_reader, slave) = create_pty().unwrap();
        // The slave fd should report as a terminal
        let is_tty = unsafe { nix::libc::isatty(slave.as_raw_fd()) };
        assert_eq!(is_tty, 1, "slave fd should be a terminal");
    }

    #[tokio::test]
    async fn test_pty_child_sees_isatty_true() {
        let (mut reader, slave) = create_pty().unwrap();

        // Spawn a child that tests isatty on fd 1, then sleeps to keep slave open.
        // `test -t 1` checks if fd 1 is a terminal.
        let mut child = std::process::Command::new("sh")
            .args([
                "-c",
                "if [ -t 1 ]; then echo IS_TTY; else echo NOT_TTY; fi; sleep 2",
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(slave))
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn sh");

        // Read output while child is still alive (slave fd still open)
        let mut output = String::new();
        loop {
            let mut buf = vec![0u8; 256];
            match tokio::time::timeout(std::time::Duration::from_secs(2), reader.read(&mut buf))
                .await
            {
                Ok(Ok(0)) => break,
                Ok(Ok(n)) => {
                    output.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if output.contains("IS_TTY") || output.contains("NOT_TTY") {
                        break;
                    }
                }
                Ok(Err(_)) => break,
                Err(_) => break,
            }
        }

        // Clean up child
        let _ = child.kill();
        let _ = child.wait();

        assert!(
            output.contains("IS_TTY"),
            "child should see fd 1 as a terminal, got: {output:?}"
        );
    }
}
