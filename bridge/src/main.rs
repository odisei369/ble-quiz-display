//! BLE bridge for the RustQuiz nRF52840 firmware.
//!
//! Polls the quiz server every ~2 seconds and forwards the latest vote
//! percentages to the BLE peripheral named "RustQuiz". On disconnect,
//! restarts the scan-connect loop until the device is found again.
//!
//! Behaviour summary
//! ─────────────────
//!   loop {
//!       scan for "RustQuiz" ─► connect ─► discover characteristics
//!       loop every POLL_INTERVAL {
//!           GET /results
//!           write votes  → Votes characteristic
//!           if revealed && new question: write [0x01, correct] → Control
//!       }
//!       // any BLE error: drop back to the outer scan loop
//!   }
//!
//! Configurable via env vars:
//!   QUIZ_URL          base URL of the quiz server (default http://localhost:3000)
//!   POLL_INTERVAL_MS  poll interval in ms (default 2000)
//!   DEVICE_NAME       BLE local name to look for (default "RustQuiz")
//!   RUST_LOG          tracing filter, e.g. "bridge=debug,btleplug=warn"

use anyhow::{anyhow, Context, Result};
use btleplug::api::{
    Central, CentralEvent, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use serde::Deserialize;
use std::env;
use std::time::Duration;
use tokio::time::{sleep, timeout};
use tracing::{debug, info, warn};
use uuid::Uuid;

const VOTES_CHAR: Uuid = Uuid::from_u128(0x00001001_b0cd_11ec_871f_d45ddf138840);
const CONTROL_CHAR: Uuid = Uuid::from_u128(0x00001002_b0cd_11ec_871f_d45ddf138840);

const CONTROL_BLINK_START: u8 = 0x01;
const CONTROL_BLINK_STOP: u8 = 0x02;

const SCAN_WINDOW: Duration = Duration::from_secs(30);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_BACKOFF: Duration = Duration::from_secs(2);
const HTTP_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
struct Results {
    votes: [u8; 4],
    correct: u8,
    revealed: bool,
}

struct Config {
    quiz_url: String,
    poll_interval: Duration,
    device_name: String,
}

impl Config {
    fn from_env() -> Self {
        let quiz_url = env::var("QUIZ_URL")
            .unwrap_or_else(|_| "http://localhost:3000".into())
            .trim_end_matches('/')
            .to_string();
        let poll_interval = env::var("POLL_INTERVAL_MS")
            .ok()
            .and_then(|s| s.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_millis(2000));
        let device_name = env::var("DEVICE_NAME").unwrap_or_else(|_| "RustQuiz".into());
        Self { quiz_url, poll_interval, device_name }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bridge=info,btleplug=warn".into()),
        )
        .init();

    let cfg = Config::from_env();
    info!(quiz_url = %cfg.quiz_url, device = %cfg.device_name,
          poll_ms = cfg.poll_interval.as_millis() as u64,
          "bridge starting");

    let manager = Manager::new().await.context("create BLE manager")?;
    let adapter = manager
        .adapters()
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("no Bluetooth adapter found"))?;
    info!("Bluetooth adapter ready");

    let http = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .context("build http client")?;
    let results_url = format!("{}/results", cfg.quiz_url);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            _ = &mut shutdown => {
                info!("ctrl-c received, exiting");
                return Ok(());
            }
            res = run_session(&adapter, &http, &results_url, &cfg) => {
                match res {
                    Ok(()) => info!("session ended cleanly, reconnecting…"),
                    Err(e) => warn!(error = format!("{e:#}"), "session error, retrying"),
                }
            }
        }
        sleep(RETRY_BACKOFF).await;
    }
}

async fn run_session(
    adapter: &Adapter,
    http: &reqwest::Client,
    results_url: &str,
    cfg: &Config,
) -> Result<()> {
    let peripheral = scan_for_device(adapter, &cfg.device_name).await?;
    let addr = peripheral.address();
    info!(%addr, "found device, connecting…");

    timeout(CONNECT_TIMEOUT, peripheral.connect())
        .await
        .map_err(|_| anyhow!("BLE connect timed out after {}s", CONNECT_TIMEOUT.as_secs()))?
        .context("BLE connect")?;
    info!(%addr, "connected, discovering services…");

    peripheral
        .discover_services()
        .await
        .context("discover services")?;

    let chars = peripheral.characteristics();
    let votes_char = chars
        .iter()
        .find(|c| c.uuid == VOTES_CHAR)
        .ok_or_else(|| anyhow!("votes characteristic not found"))?
        .clone();
    let control_char = chars
        .iter()
        .find(|c| c.uuid == CONTROL_CHAR)
        .ok_or_else(|| anyhow!("control characteristic not found"))?
        .clone();
    info!("characteristics resolved — entering poll/write loop");

    let mut last_blink_target: Option<u8> = None;
    let mut last_votes: Option<[u8; 4]> = None;

    loop {
        if !peripheral.is_connected().await.unwrap_or(false) {
            return Err(anyhow!("peripheral disconnected"));
        }

        let results = match fetch_results(http, results_url).await {
            Ok(r) => r,
            Err(e) => {
                warn!(error = format!("{e:#}"), "HTTP poll failed");
                sleep(cfg.poll_interval).await;
                continue;
            }
        };

        if last_votes != Some(results.votes) {
            debug!(votes = ?results.votes, "writing votes");
        }
        peripheral
            .write(&votes_char, &results.votes, WriteType::WithResponse)
            .await
            .context("write votes characteristic")?;
        last_votes = Some(results.votes);

        // Continuous-blink protocol: send a start command on revealed→true,
        // a stop on revealed→false, and re-send if the target changes.
        let desired = if results.revealed && results.correct < 4 {
            Some(results.correct)
        } else {
            None
        };

        if desired != last_blink_target {
            match desired {
                Some(target) => {
                    info!(target, "starting continuous blink");
                    peripheral
                        .write(
                            &control_char,
                            &[CONTROL_BLINK_START, target],
                            WriteType::WithResponse,
                        )
                        .await
                        .context("write blink start")?;
                }
                None => {
                    info!("stopping blink");
                    peripheral
                        .write(
                            &control_char,
                            &[CONTROL_BLINK_STOP, 0],
                            WriteType::WithResponse,
                        )
                        .await
                        .context("write blink stop")?;
                }
            }
            last_blink_target = desired;
        }

        sleep(cfg.poll_interval).await;
    }
}

async fn fetch_results(http: &reqwest::Client, url: &str) -> Result<Results> {
    let resp = http.get(url).send().await.context("HTTP GET /results")?;
    if !resp.status().is_success() {
        return Err(anyhow!("server returned {}", resp.status()));
    }
    let body: Results = resp.json().await.context("decode /results JSON")?;
    Ok(body)
}

async fn device_name(p: &Peripheral) -> Option<String> {
    p.properties().await.ok().flatten().and_then(|props| props.local_name)
}

async fn scan_for_device(adapter: &Adapter, target: &str) -> Result<Peripheral> {
    info!(target, "scanning…");
    adapter
        .start_scan(ScanFilter::default())
        .await
        .context("start_scan")?;

    // Wait for a fresh advertisement. Skipping `adapter.peripherals()` on
    // purpose — after a power-cycle of the peripheral, that cache returns a
    // stale handle that connect() can't actually reach.
    let mut events = adapter.events().await.context("subscribe to events")?;
    let found = timeout(SCAN_WINDOW, async {
        while let Some(evt) = events.next().await {
            if let CentralEvent::DeviceDiscovered(id) | CentralEvent::DeviceUpdated(id) = evt {
                if let Ok(p) = adapter.peripheral(&id).await {
                    if device_name(&p).await.as_deref() == Some(target) {
                        return Ok::<Peripheral, anyhow::Error>(p);
                    }
                }
            }
        }
        Err(anyhow!("event stream ended unexpectedly"))
    })
    .await;

    adapter.stop_scan().await.ok();

    match found {
        Ok(Ok(p)) => Ok(p),
        Ok(Err(e)) => Err(e),
        Err(_) => Err(anyhow!(
            "device {:?} not found within {} seconds",
            target,
            SCAN_WINDOW.as_secs()
        )),
    }
}
