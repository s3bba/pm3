use crate::manager::Manager;
use crate::memory;
use crate::paths::Paths;
use crate::pid;
use crate::protocol::{self, Request};
use color_eyre::eyre::bail;
use tokio::fs;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::watch;

pub async fn run(paths: Paths) -> color_eyre::Result<()> {
    fs::create_dir_all(paths.data_dir()).await?;

    if pid::is_daemon_running(&paths).await? {
        bail!("daemon is already running");
    }

    pid::write_pid_file(&paths).await?;

    let socket_path = paths.socket_file();
    if socket_path.exists() {
        fs::remove_file(&socket_path).await?;
    }

    let listener = UnixListener::bind(&socket_path)?;

    let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
    let manager = Manager::new(paths.clone());

    manager.auto_restore().await;

    memory::spawn_stats_collector(
        manager.processes(),
        manager.stats_cache(),
        shutdown_tx.subscribe(),
    );

    let result = run_accept_loop(&listener, &shutdown_tx, &mut shutdown_rx, &manager).await;

    manager.shutdown_all().await;

    let _ = fs::remove_file(paths.socket_file()).await;
    pid::remove_pid_file(&paths).await;

    result
}

async fn run_accept_loop(
    listener: &UnixListener,
    shutdown_tx: &watch::Sender<bool>,
    shutdown_rx: &mut watch::Receiver<bool>,
    manager: &Manager,
) -> color_eyre::Result<()> {
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                let (stream, _addr) = accept_result?;
                let tx = shutdown_tx.clone();
                let mgr = manager.clone();
                tokio::spawn(async move {
                    if let Err(e) = handle_connection(stream, &tx, &mgr).await {
                        eprintln!("connection error: {e}");
                    }
                });
            }
            _ = shutdown_rx.changed() => {
                if *shutdown_rx.borrow() {
                    break;
                }
            }
            _ = signal_shutdown() => {
                break;
            }
        }
    }

    Ok(())
}

async fn signal_shutdown() {
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).unwrap();
    let mut sigint =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt()).unwrap();

    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    shutdown_tx: &watch::Sender<bool>,
    manager: &Manager,
) -> color_eyre::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    buf_reader.read_line(&mut line).await?;

    if line.is_empty() {
        return Ok(());
    }

    let request = protocol::decode_request(&line)?;

    if let Request::Log {
        ref name,
        lines,
        follow,
    } = request
    {
        manager
            .stream_logs(name.clone(), lines, follow, &mut writer)
            .await?;
        writer.shutdown().await?;
        return Ok(());
    }

    let response = manager.dispatch(request, shutdown_tx).await;
    let encoded = protocol::encode_response(&response)?;
    writer.write_all(&encoded).await?;
    writer.shutdown().await?;

    Ok(())
}
