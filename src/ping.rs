use std::net::{IpAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use surge_ping::{Client, Config, ICMP, PingIdentifier, PingSequence, SurgeError};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const PING_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub struct PingTarget {
    pub name: String,
    pub host: String,
}

#[derive(Clone, Debug)]
pub enum PingResult {
    Ok(f64),
    Timeout,
    Error(String),
    Pending,
}

pub struct PingService {
    _runtime: Runtime,
    handle: Handle,
    interval_tx: watch::Sender<Duration>,
    state: Arc<Mutex<ServiceState>>,
    v4: Result<Client, String>,
    v6: Result<Client, String>,
}

struct ServiceState {
    slots: Vec<Slot>,
}

struct Slot {
    host: String,
    result: Arc<Mutex<PingResult>>,
    task: JoinHandle<()>,
}

impl PingService {
    pub fn new(initial_interval: Duration) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("build tokio runtime");
        let handle = runtime.handle().clone();
        let (interval_tx, _rx) = watch::channel(initial_interval);

        // surge_ping::Client::new spawns a recv task on the current tokio
        // runtime; bind it to ours by entering the handle first.
        let _guard = handle.enter();
        let v4 = Client::new(&Config::default()).map_err(|e| e.to_string());
        let v6 = Client::new(&Config::builder().kind(ICMP::V6).build()).map_err(|e| e.to_string());
        drop(_guard);

        Self {
            _runtime: runtime,
            handle,
            interval_tx,
            state: Arc::new(Mutex::new(ServiceState { slots: Vec::new() })),
            v4,
            v6,
        }
    }

    /// Replace the set of hosts being pinged. Unchanged hosts keep their
    /// current result and in-flight task; new hosts start in Pending; removed
    /// hosts are aborted.
    pub fn set_targets(&self, hosts: &[String]) {
        let mut state = self.state.lock().unwrap();

        // Drain existing slots into a map keyed by host so we can reuse them.
        let mut existing: Vec<Option<Slot>> = state.slots.drain(..).map(Some).collect();

        let mut new_slots: Vec<Slot> = Vec::with_capacity(hosts.len());
        for host in hosts {
            let reused = existing
                .iter_mut()
                .find(|s| s.as_ref().is_some_and(|s| &s.host == host))
                .and_then(|s| s.take());
            match reused {
                Some(slot) => new_slots.push(slot),
                None => {
                    let result = Arc::new(Mutex::new(PingResult::Pending));
                    let interval_rx = self.interval_tx.subscribe();
                    let clients = Clients {
                        v4: self.v4.clone(),
                        v6: self.v6.clone(),
                    };
                    let task = self.handle.spawn(target_loop(
                        host.clone(),
                        clients,
                        result.clone(),
                        interval_rx,
                    ));
                    new_slots.push(Slot {
                        host: host.clone(),
                        result,
                        task,
                    });
                }
            }
        }

        // Abort anything left over (removed targets).
        for leftover in existing.into_iter().flatten() {
            leftover.task.abort();
        }

        state.slots = new_slots;
    }

    pub fn set_interval(&self, interval: Duration) {
        let _ = self.interval_tx.send(interval);
    }

    pub fn snapshot(&self) -> Vec<PingResult> {
        let state = self.state.lock().unwrap();
        state
            .slots
            .iter()
            .map(|s| s.result.lock().unwrap().clone())
            .collect()
    }
}

#[derive(Clone)]
struct Clients {
    v4: Result<Client, String>,
    v6: Result<Client, String>,
}

async fn target_loop(
    host: String,
    clients: Clients,
    result: Arc<Mutex<PingResult>>,
    mut interval_rx: watch::Receiver<Duration>,
) {
    let mut seq: u16 = 0;
    loop {
        let start = Instant::now();
        let r = ping_once(&clients, seq, &host).await;
        seq = seq.wrapping_add(1);
        *result.lock().unwrap() = r;

        let want = *interval_rx.borrow();
        let elapsed = start.elapsed();
        if elapsed < want {
            let remaining = want - elapsed;
            tokio::select! {
                _ = tokio::time::sleep(remaining) => {}
                _ = interval_rx.changed() => {}
            }
        }
    }
}

async fn ping_once(clients: &Clients, seq: u16, host: &str) -> PingResult {
    let addr = match resolve(host) {
        Ok(a) => a,
        Err(e) => return PingResult::Error(e),
    };
    let client = match addr {
        IpAddr::V4(_) => clients.v4.as_ref(),
        IpAddr::V6(_) => clients.v6.as_ref(),
    };
    let client = match client {
        Ok(c) => c,
        Err(e) => return PingResult::Error(e.clone()),
    };
    let ident = PingIdentifier(rand::random());
    let mut pinger = client.pinger(addr, ident).await;
    pinger.timeout(PING_TIMEOUT);
    match pinger.ping(PingSequence(seq), &[0u8; 32]).await {
        Ok((_pkt, dur)) => PingResult::Ok(dur.as_secs_f64() * 1000.0),
        Err(SurgeError::Timeout { .. }) => PingResult::Timeout,
        Err(e) => PingResult::Error(e.to_string()),
    }
}

fn resolve(host: &str) -> Result<IpAddr, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    (host, 0u16)
        .to_socket_addrs()
        .map_err(|e| e.to_string())
        .and_then(|mut it| {
            it.next()
                .map(|s| s.ip())
                .ok_or_else(|| "no address".to_string())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pings_loopback() {
        let svc = PingService::new(Duration::from_millis(100));
        svc.set_targets(&["127.0.0.1".to_string()]);
        // Give the loop time to complete at least one ping.
        std::thread::sleep(Duration::from_secs(1));
        let snap = svc.snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0] {
            PingResult::Ok(ms) => assert!(*ms >= 0.0 && *ms < 1000.0, "got {ms}ms"),
            other => panic!("expected Ok, got {other:?}"),
        }
    }

    #[test]
    fn set_targets_reuses_unchanged_slots() {
        let svc = PingService::new(Duration::from_millis(100));
        svc.set_targets(&["127.0.0.1".to_string()]);
        std::thread::sleep(Duration::from_millis(500));
        let first = svc.snapshot();
        assert!(matches!(first[0], PingResult::Ok(_)));

        svc.set_targets(&["127.0.0.1".to_string(), "1.1.1.1".to_string()]);
        let immediate = svc.snapshot();
        assert!(matches!(immediate[0], PingResult::Ok(_)), "reused slot lost its result: {:?}", immediate[0]);
        assert!(matches!(immediate[1], PingResult::Pending));
    }
}
