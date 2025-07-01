use anyhow::Error;
use hyper::{Body, Request, Response, Server, StatusCode};
use std::thread::JoinHandle;
use std::{net::SocketAddr, sync::Arc};
use std::{path::PathBuf, process::Command};
use tempfile::TempDir;
use tokio::{runtime::Runtime, sync::oneshot::Sender};

use crate::config::Channel;

pub(crate) struct SmokeTester {
    runtime: JoinHandle<Runtime>,
    server_addr: SocketAddr,
    shutdown_send: Sender<()>,
}

impl SmokeTester {
    pub(crate) fn new(paths: &[PathBuf]) -> Result<Self, Error> {
        let addr = SocketAddr::from(([127, 0, 0, 1], 0));

        let paths = Arc::new(paths.to_vec());
        let service = hyper::service::make_service_fn(move |_| {
            let paths = paths.clone();
            async move {
                Ok::<_, Error>(hyper::service::service_fn(move |req| {
                    let paths = paths.clone();
                    async move { server_handler(req, paths) }
                }))
            }
        });

        let (shutdown_send, shutdown_recv) = tokio::sync::oneshot::channel::<()>();
        let server_mtx = std::sync::Arc::new(std::sync::Mutex::new(None));
        let server_mtx_external = server_mtx.clone();
        let runtime = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let _guard = runtime.enter();
            let server = Server::bind(&addr).serve(service);
            let server_addr = server.local_addr();
            *server_mtx.lock().unwrap() = Some(server_addr);
            let server = server.with_graceful_shutdown(async {
                shutdown_recv.await.unwrap();
                eprintln!("Shutting down smoke test server...");
            });
            runtime.block_on(server).unwrap();
            runtime
        });

        let server_addr = loop {
            let value = server_mtx_external.lock().unwrap().take();
            match value {
                None => {
                    eprintln!("Waiting for server to boot...");
                    std::thread::sleep(std::time::Duration::from_secs(1));
                }
                Some(other) => break other,
            }
        };

        Ok(Self {
            runtime,
            server_addr,
            shutdown_send,
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
        self.shutdown_send
            .send(())
            .expect("failed to send shutdown message to the server");
        self.runtime.join().unwrap().shutdown_background();

        Ok(())
    }
}

fn server_handler(req: Request<Body>, paths: Arc<Vec<PathBuf>>) -> Result<Response<Body>, Error> {
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

fn not_found() -> Result<Response<Body>, Error> {
    let mut response = Response::new("404: Not Found\n".into());
    *response.status_mut() = StatusCode::NOT_FOUND;
    Ok(response)
}
