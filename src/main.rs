//! RemoteDesktop portal backend for compositors that expose virtual input.

mod eis;
mod input;
mod keymap;
mod portal;

use std::io::{self, ErrorKind};
use std::thread;

use zbus::connection::Builder;

use crate::input::Boot;
use crate::portal::Portal;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> io::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let (boot_tx, boot_rx) = std::sync::mpsc::channel();
    let (exit_tx, exit_rx) = tokio::sync::oneshot::channel();
    thread::spawn(move || {
        let result = input::run(boot_tx);
        let _ = exit_tx.send(result);
    });

    let commander = match boot_rx.recv() {
        Ok(Boot::Ready(commander)) => commander,
        Ok(Boot::Failed(message)) => return Err(io::Error::other(message)),
        Err(_) => {
            return Err(io::Error::new(
                ErrorKind::ConnectionAborted,
                "input thread exited during startup",
            ));
        }
    };

    let connection = Builder::session()
        .map_err(io::Error::other)?
        .name("org.freedesktop.impl.portal.desktop.virtinput")
        .map_err(io::Error::other)?
        .serve_at("/org/freedesktop/portal/desktop", Portal::new(commander))
        .map_err(io::Error::other)?
        .build()
        .await
        .map_err(io::Error::other)?;

    log::info!("serving org.freedesktop.impl.portal.RemoteDesktop");

    tokio::select! {
        _ = tokio::signal::ctrl_c() => log::info!("received SIGINT"),
        _ = terminate() => log::info!("received SIGTERM"),
        result = exit_rx => {
            match result {
                Ok(Ok(())) => log::info!("input thread stopped"),
                Ok(Err(err)) => log::error!("input thread failed: {err}"),
                Err(_) => log::error!("input thread dropped its exit notification"),
            }
        }
    }
    drop(connection);
    Ok(())
}

async fn terminate() {
    use tokio::signal::unix::{SignalKind, signal};
    if let Ok(mut sigterm) = signal(SignalKind::terminate()) {
        sigterm.recv().await;
    }
}
