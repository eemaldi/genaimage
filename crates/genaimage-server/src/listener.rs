use std::{future::Future, io, net::SocketAddr, pin::pin, time::Duration};

use axum::{serve::Listener, Router};
use hyper::server::conn::http1;
use hyper_util::{rt::TokioIo, service::TowerToHyperService};
use tokio::{
    net::{lookup_host, TcpListener},
    task::JoinSet,
    time::timeout,
};

use crate::{build_app, AppState};

const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Serves on loopback until the process receives a shutdown signal, then
/// drains in-flight requests up to the connection drain deadline.
pub async fn serve(state: AppState) -> io::Result<()> {
    let harness = state.harness();
    let config = &harness.config().server;
    let listener = bind_loopback(&config.host, config.port).await?;
    let signal = shutdown_signal()?;
    serve_on_listener(state, listener, signal, DRAIN_TIMEOUT).await
}

/// Serves a pre-bound loopback listener using the caller's shutdown signal.
pub async fn serve_on_listener(
    state: AppState,
    listener: TcpListener,
    signal: impl Future<Output = ()>,
    drain_timeout: Duration,
) -> io::Result<()> {
    let address = listener.local_addr()?;
    verify_loopback(&address.ip().to_string(), &[address])?;
    if state
        .portal_dev
        .as_ref()
        .is_some_and(|proxy| proxy.port() == address.port())
    {
        return Err(io::Error::new(io::ErrorKind::InvalidInput,
            "--portal-dev-url points to the GenAImage server port; use the separate Vite port (normally 5173)."));
    }
    tracing::info!(%address, "GenAImage server listening");
    let app = build_app(state.clone(), address.port());
    serve_connections(listener, app, state, signal, drain_timeout).await;
    Ok(())
}

async fn serve_connections(
    mut listener: TcpListener,
    app: Router,
    state: AppState,
    signal: impl Future<Output = ()>,
    drain_timeout: Duration,
) {
    // Own the connection tasks so a stuck socket can be closed at the deadline.
    let mut connections = JoinSet::new();
    let mut signal = pin!(signal);
    loop {
        tokio::select! {
            biased;
            () = &mut signal => break,
            Some(result) = connections.join_next() => report_connection(result),
            (socket, peer) = Listener::accept(&mut listener) => {
                let app = app.clone();
                let state = state.clone();
                connections.spawn(async move {
                    let mut connection = pin!(http1::Builder::new().serve_connection(
                        TokioIo::new(socket), TowerToHyperService::new(app),
                    ));
                    let result = tokio::select! {
                        result = &mut connection => result,
                        () = state.wait_for_shutdown() => {
                            connection.as_mut().graceful_shutdown();
                            connection.await
                        }
                    };
                    if let Err(error) = result {
                        tracing::trace!(%peer, %error, "HTTP connection ended with an error");
                    }
                });
            }
        }
    }

    drop(listener);
    state.begin_shutdown();
    tracing::info!("Shutdown requested; draining in-flight requests");
    let drain_connections = async {
        if timeout(drain_timeout, async {
            while let Some(result) = connections.join_next().await {
                report_connection(result);
            }
        })
        .await
        .is_err()
        {
            tracing::warn!(
                remaining = connections.len(),
                "Connection drain timed out; closing remaining connections"
            );
            connections.shutdown().await;
        }
    };
    tokio::join!(drain_connections, state.drain_run_tasks(drain_timeout));
}

fn report_connection(result: Result<(), tokio::task::JoinError>) {
    if let Err(error) = result {
        tracing::warn!(%error, "HTTP connection task failed");
    }
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
    use std::{sync::Arc, time::Instant};

    use axum::{body::Body, middleware};
    use futures_util::StreamExt;
    use genaimage_comfyui::ComfyUiClient;
    use genaimage_core::{Event, Harness, HarnessConfig};
    use serde_json::json;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpSocket,
        sync::{oneshot, Notify},
    };

    use super::*;

    #[tokio::test]
    async fn shutdown_closes_a_backpressured_event_connection() {
        let workspace = tempfile::tempdir().unwrap();
        let config = HarnessConfig::load_with(workspace.path(), None, workspace.path(), |key| {
            (key == "HOME").then(|| workspace.path().to_str().unwrap().to_owned())
        })
        .unwrap();
        let backend = Arc::new(ComfyUiClient::new(&config.comfyui).unwrap());
        let core = Arc::new(Harness::new_async(config, backend).await.unwrap());
        let state = AppState::new(core.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let event_started = Arc::new(Notify::new());
        let app = build_app(state.clone(), addr.port()).layer(middleware::map_response({
            let event_started = event_started.clone();
            move |response: axum::response::Response| {
                let event_started = event_started.clone();
                async move {
                    response.map(|body| {
                        Body::from_stream(body.into_data_stream().inspect(move |frame| {
                            if frame.as_ref().is_ok_and(|bytes| bytes.len() > 4096) {
                                event_started.notify_one();
                            }
                        }))
                    })
                }
            }
        }));
        let (shutdown, shutdown_requested) = oneshot::channel();
        let drain = Duration::from_millis(150);
        let watchdog = Duration::from_secs(5);
        let server = tokio::spawn(serve_connections(
            listener,
            app,
            state,
            async {
                let _ = shutdown_requested.await;
            },
            drain,
        ));

        // The first replay frame alone exceeds the socket's buffering. Observe
        // it leaving the SSE body without ever reading from the client socket.
        core.events().publish(Event::SettingsChanged {
            settings: json!("x".repeat(8 * 1024 * 1024)),
        });
        for value in 0..64 {
            core.events().publish(Event::SettingsChanged {
                settings: json!(value),
            });
        }
        let socket = TcpSocket::new_v4().unwrap();
        socket.set_recv_buffer_size(4096).unwrap();
        let mut client = socket.connect(addr).await.unwrap();
        client
            .write_all(
                format!("GET /api/v1/events HTTP/1.1\r\nHost: {addr}\r\nLast-Event-ID: 0\r\n\r\n")
                    .as_bytes(),
            )
            .await
            .unwrap();
        timeout(watchdog, event_started.notified())
            .await
            .expect("SSE replay started");

        let requested_at = Instant::now();
        shutdown.send(()).unwrap();
        timeout(watchdog, server)
            .await
            .expect("server must stop with a stalled client")
            .unwrap();
        assert!(
            requested_at.elapsed() >= drain,
            "the stalled connection should reach the drain deadline"
        );
        // Reading only after shutdown must terminate with a truncated response,
        // not release an orphaned connection that is still writing the event.
        let mut received = Vec::new();
        let result = timeout(watchdog, client.read_to_end(&mut received))
            .await
            .expect("stalled socket must close");
        if let Err(error) = result {
            assert_eq!(error.kind(), io::ErrorKind::ConnectionReset);
        }
        assert!(
            received.len() < 8 * 1024 * 1024,
            "stalled connection finished writing its event"
        );
    }

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
