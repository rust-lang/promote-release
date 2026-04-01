use anyhow::Error;
use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use std::thread::JoinHandle;
use std::time::Duration;
use std::{net::SocketAddr, sync::Arc};
use std::{path::PathBuf, process::Command};
use tempfile::TempDir;
use tokio::runtime::Runtime;

use crate::config::Channel;

pub(crate) struct SmokeTester {
    thread: JoinHandle<Runtime>,
    server_addr: SocketAddr,
    shutdown: tokio::sync::oneshot::Sender<()>,
}

impl SmokeTester {
    pub(crate) fn new(paths: &[PathBuf]) -> Result<Self, Error> {
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));

        let paths = Arc::new(paths.to_vec());
        let service = move || {
            let paths = paths.clone();
            hyper::service::service_fn(move |req: hyper::Request<hyper::body::Incoming>| {
                let paths = paths.clone();
                async move { server_handler(req, paths) }
            })
        };

        let server_mtx = std::sync::Arc::new(std::sync::Mutex::new(None));
        let server_mtx_external = server_mtx.clone();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::spawn(move || {
            runtime.block_on(async move {
                let listener = tokio::net::TcpListener::bind(addr)
                    .await
                    .unwrap_or_else(|e| {
                        panic!("Failed to bind to {addr:?}: {e:?}");
                    });
                let graceful = hyper_util::server::graceful::GracefulShutdown::new();
                let mut server = hyper::server::conn::http1::Builder::new();
                if cfg!(test) {
                    server.auto_date_header(false);
                }
                let server_addr = listener.local_addr().expect("local_addr successful");
                *server_mtx.lock().unwrap() = Some(server_addr);
                loop {
                    tokio::select! {
                        c = listener.accept() => {
                            let (stream, _peer_addr) = match c {
                                Ok(c) => c,
                                Err(e) => {
                                    eprintln!("accept error: {:?}", e);
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                    continue;
                                }
                            };
                            let stream = hyper_util::rt::TokioIo::new(Box::pin(stream));

                            let conn = server.serve_connection(stream, service());
                            let conn = graceful.watch(conn);

                            tokio::spawn(async move {
                                if let Err(e) = conn.await {
                                    eprintln!("connection error: {e:?}");
                                }
                            });
                        }
                        _ = &mut rx => {
                            graceful.shutdown().await;
                            break;
                        }
                    }
                }
            });
            runtime
        });

        let server_addr = loop {
            let value = server_mtx_external.lock().unwrap().take();
            match value {
                None => {
                    eprintln!("Waiting for server to boot...");
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Some(other) => break other,
            }
        };

        Ok(Self {
            thread,
            server_addr,
            shutdown: tx,
        })
    }

    pub(crate) fn server_addr(&self) -> SocketAddr {
        self.server_addr
    }

    pub(crate) fn test(self, channel: &Channel) -> Result<(), Error> {
        let tempdir = TempDir::new()?;
        let cargo_dir = tempdir.path().join("sample-crate");
        std::fs::create_dir_all(&cargo_dir)?;

        let cargo = |args: &[&str]| {
            crate::run(
                Command::new("cargo")
                    .arg(format!("+{channel}"))
                    .args(args)
                    .env("USER", "root")
                    .current_dir(&cargo_dir),
            )
        };
        let rustup = |args: &[&str]| {
            crate::run(
                Command::new("rustup")
                    .env("RUSTUP_DIST_SERVER", format!("http://{}", self.server_addr))
                    .args(args),
            )
        };

        rustup(&["toolchain", "remove", &channel.to_string()])?;
        rustup(&[
            "toolchain",
            "install",
            &channel.to_string(),
            "--profile",
            "minimal",
        ])?;
        cargo(&["init", "--bin", "."])?;
        cargo(&["run"])?;

        // Finally shut down the HTTP server and the tokio reactor.
        let _ = self.shutdown.send(());
        self.thread.join().unwrap().shutdown_background();

        Ok(())
    }
}

fn server_handler(
    req: Request<Incoming>,
    paths: Arc<Vec<PathBuf>>,
) -> Result<Response<Full<Bytes>>, Error> {
    let file_name = match req.uri().path().split('/').next_back() {
        Some(file_name) => file_name,
        None => return not_found(),
    };
    for directory in &*paths {
        let path = directory.join(file_name);
        if path.is_file() {
            let content = std::fs::read(&path)?;
            return Ok(Response::new(content.into()));
        }
    }
    not_found()
}

fn not_found() -> Result<Response<Full<Bytes>>, Error> {
    let mut response = Response::new("404: Not Found\n".into());
    *response.status_mut() = StatusCode::NOT_FOUND;
    Ok(response)
}

#[cfg(test)]
mod test;
