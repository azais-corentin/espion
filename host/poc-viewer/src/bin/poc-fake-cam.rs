//! poc-fake-cam: the viewer's fake camera on its own, for `poc-viewer '[::1]:5009'`.

use std::net::SocketAddr;

use anyhow::{Context, Result};
use clap::Parser;
use poc_viewer::fake::{FakeCamera, FakeConfig};

/// A fake OV9281 camera board speaking the espion-proto camera stream.
#[derive(Debug, Parser)]
#[command(version, about)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "[::1]:5009")]
    bind: SocketAddr,
    /// Line-rate limit in Mbit/s (Ethernet bytes on the wire; the board's link: 100).
    #[arg(long, value_name = "MBPS")]
    mbps: Option<f64>,
    /// Fraction of datagrams lost (0..1).
    #[arg(long, value_name = "FRACTION", default_value_t = 0.0)]
    loss: f64,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let fake = FakeCamera::spawn(FakeConfig {
        bind: args.bind,
        mbps: args.mbps,
        loss: args.loss,
        ..FakeConfig::default()
    })
    .context("starting the fake camera")?;
    println!(
        "fake camera on {} (limit {}, loss {}); Ctrl-C stops it",
        fake.addr(),
        args.mbps
            .map_or("none".to_owned(), |m| format!("{m} Mbit/s")),
        args.loss
    );
    loop {
        std::thread::park();
    }
}
