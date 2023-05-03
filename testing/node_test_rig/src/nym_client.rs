//! Instance of nym client

use rand::Rng;
use std::{
    io::{BufRead, BufReader},
    net::TcpListener,
    ops::Range,
    process::{Child, Command},
};

const LOCALHOST: &str = "0.0.0.0";
const PORT_RANGE: Range<u16> = 15000..25000;

/// Pick a random port
fn pick_port() -> u16 {
    let mut rng = rand::thread_rng();

    loop {
        let port = rng.gen_range(PORT_RANGE);
        if TcpListener::bind(format!("{LOCALHOST}:{port}")).is_ok() {
            return port;
        }
    }
}

/// Instance of nym client
pub struct NymClient {
    ps: Child,
    port: u16,
}

impl NymClient {
    /// Get the websocket address of the client.
    pub fn ws(&self) -> String {
        format!("ws://{}:{}", LOCALHOST, self.port)
    }

    pub async fn new() -> Self {
        let port = pick_port();
        let id = port.to_string();

        // init nym client.
        Command::new("nym-client")
            .arg("init")
            .arg("--id")
            .arg(&id)
            .output()
            .expect("Failed to init nym-client");

        // start nym-client
        let mut ps = Command::new("nym-client")
            .arg("run")
            .arg("--id")
            .arg(&id)
            .arg("--port")
            .arg(&id)
            .spawn()
            .expect("Failed to start nym-client");

        // wait for connection established
        let stderr = ps.stderr.as_mut();
        let reader = BufReader::new(stderr.expect("Failed to get stderr"));
        for line in reader.lines().flatten() {
            if line.contains("The address of this client is") {
                println!("{line}");
                break;
            }
        }

        Self { ps, port }
    }
}

impl Drop for NymClient {
    fn drop(&mut self) {
        self.ps.kill().expect("Failed to kill process")
    }
}
