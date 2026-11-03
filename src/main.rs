use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::process::ExitCode;
use std::time::Duration;

use nsh_pixelbridge::config::{Config, HELP};
use nsh_pixelbridge::{api, convert};
use tracing_subscriber::EnvFilter;

fn main() -> ExitCode {
    // No CLI framework: three tiny options, single dash like the other NashCom tools.
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        None => {}
        Some("-version") | Some("-v") => {
            println!("nsh-pixelbridge {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Some("-help") | Some("-h") => {
            print!("{HELP}");
            return ExitCode::SUCCESS;
        }
        Some("-health-check") => return health_check(),
        Some(other) => {
            eprintln!("unknown option '{other}' (try -help)");
            return ExitCode::from(2);
        }
    }

    let config = match Config::from_env() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("configuration error: {message}");
            return ExitCode::from(2);
        }
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .init();

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(e) => {
            eprintln!("cannot start async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(serve(config))
}

async fn serve(config: Config) -> ExitCode {
    let fonts = convert::load_fonts(&config.font_dir);
    if fonts.is_empty() {
        tracing::warn!("no fonts found in {}: SVG text will not render", config.font_dir.display());
    } else {
        tracing::info!("loaded {} font faces from {}", fonts.len(), config.font_dir.display());
    }

    if let Some(path) = &config.openapi_file {
        if let Err(message) = api::check_openapi_file(path, config.public_url.as_deref()) {
            tracing::error!("PIXELBRIDGE_OPENAPI_FILE: {message}");
            return ExitCode::FAILURE;
        }
        tracing::warn!("serving /openapi.json from {} (re-read on every request, testing mode)", path.display());
    }

    let listener = match tokio::net::TcpListener::bind(config.listen).await {
        Ok(listener) => listener,
        Err(e) => {
            tracing::error!("cannot listen on {}: {e}", config.listen);
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        "nsh-pixelbridge {} listening on http://{} (max request {} bytes, {} concurrent, {}s timeout)",
        env!("CARGO_PKG_VERSION"),
        config.listen,
        config.max_request_bytes,
        config.max_concurrent,
        config.timeout_seconds
    );

    let app = api::router(api::AppState::new(config, fonts));
    if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(shutdown_signal()).await {
        tracing::error!("server error: {e}");
        return ExitCode::FAILURE;
    }
    tracing::info!("stopped");
    ExitCode::SUCCESS
}

/// Completes on Ctrl-C or SIGTERM (what `docker stop` sends). As PID 1 in a
/// container the process ignores SIGTERM unless it installs a handler.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut stream) => {
                stream.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
    tracing::info!("shutdown requested");
}

/// `nsh-pixelbridge -health-check`: used by the container HEALTHCHECK, where
/// the scratch image has no curl. Plain std networking, no async needed.
fn health_check() -> ExitCode {
    let config = match Config::from_env() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("configuration error: {message}");
            return ExitCode::from(2);
        }
    };

    // A listener on 0.0.0.0 / [::] is reached through loopback.
    let ip = match config.listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    let target = SocketAddr::new(ip, config.listen.port());
    let timeout = Duration::from_secs(3);

    let outcome = (|| -> std::io::Result<String> {
        let mut stream = TcpStream::connect_timeout(&target, timeout)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        stream.write_all(b"GET /health HTTP/1.0\r\nHost: localhost\r\n\r\n")?;
        let mut reply = String::new();
        stream.take(1024).read_to_string(&mut reply)?;
        Ok(reply)
    })();

    match outcome {
        Ok(reply) if reply.starts_with("HTTP/1.1 200") || reply.starts_with("HTTP/1.0 200") => ExitCode::SUCCESS,
        Ok(_) => {
            eprintln!("health check: unexpected response from {target}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("health check: {target}: {e}");
            ExitCode::FAILURE
        }
    }
}
