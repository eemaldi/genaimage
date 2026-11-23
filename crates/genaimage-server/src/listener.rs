use std::{future::Future, io, net::SocketAddr};

use tokio::net::{lookup_host, TcpListener};

use crate::{build_app, AppState};

/// Serves on the configured loopback address until the process receives a
/// shutdown signal, then lets Axum drain in-flight requests.
pub async fn serve(state: AppState) -> io::Result<()> {
    let config = &state.harness.config().server;
    let listener = bind_loopback(&config.host, config.port).await?;
    let signal = shutdown_signal()?;
    let shutdown = state.shutdown.clone();
    tracing::info!(address = %listener.local_addr()?, "GenAImage server listening");
    axum::serve(listener, build_app(state))
        .with_graceful_shutdown(async move {
            signal.await;
            shutdown.send_replace(true);
            tracing::info!("Shutdown requested; draining in-flight requests");
        })
        .await
}

async fn bind_loopback(host: &str, port: u16) -> io::Result<TcpListener> {
    let addresses: Vec<_> = lookup_host((host, port))
        .await
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not resolve server.host '{host}': {error}; use 127.0.0.1 or ::1"),
            )
        })?
        .collect();
    verify_loopback(host, &addresses)?;
    // Bind only the checked socket addresses; never resolve the name again.
    TcpListener::bind(addresses.as_slice())
        .await
        .map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("could not bind server.host '{host}' on server.port {port}: {error}"),
            )
        })
}

fn verify_loopback(host: &str, addresses: &[SocketAddr]) -> io::Result<()> {
    if addresses.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("server.host '{host}' did not resolve; use 127.0.0.1 or ::1"),
        ));
    }
    if let Some(address) = addresses
        .iter()
        .find(|addr| !addr.ip().to_canonical().is_loopback())
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("refusing to bind server.host '{host}' to non-loopback address {address}; set server.host or GENAIMAGE_SERVER_HOST to 127.0.0.1 or ::1"),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn shutdown_signal() -> io::Result<impl Future<Output = ()>> {
    use tokio::signal::unix::{signal, SignalKind};

    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    Ok(async move {
        tokio::select! {
            _ = interrupt.recv() => {},
            _ = terminate.recv() => {},
        }
    })
}

#[cfg(not(unix))]
fn shutdown_signal() -> io::Result<impl Future<Output = ()>> {
    let mut interrupt = tokio::signal::windows::ctrl_c()?;
    Ok(async move {
        interrupt.recv().await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bind_refuses_non_loopback_addresses() {
        for host in ["0.0.0.0", "::", "192.0.2.1", "::ffff:192.0.2.1"] {
            let error = bind_loopback(host, 0).await.unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
            assert!(error.to_string().contains("non-loopback"));
            assert!(error.to_string().contains("GENAIMAGE_SERVER_HOST"));
        }
    }

    #[test]
    fn rejects_a_hostname_with_mixed_dns_results() {
        let addresses = [
            "127.0.0.1:3000".parse().unwrap(),
            "192.0.2.1:3000".parse().unwrap(),
        ];
        let error = verify_loopback("localhost", &addresses).unwrap_err();
        assert!(error.to_string().contains("192.0.2.1:3000"));
    }
}
