use std::collections::VecDeque;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use surge_ping::{Client, Config, ICMP, PingIdentifier, PingSequence, Pinger, SurgeError};
use tokio::runtime::{Handle, Runtime};
use tokio::sync::watch;
use tokio::task::JoinHandle;

const PING_TIMEOUT: Duration = Duration::from_secs(10);
const TCP_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// Ports raced by the TCP prober — chosen because home routers commonly
/// expose at least one (HTTP UI, HTTPS UI, DNS forwarder, TR-069/CWMP).
/// A `ECONNREFUSED` counts as a success: it still proves the router
/// responded, which is the only thing we're measuring.
const TCP_PROBE_PORTS: &[u16] = &[80, 443, 53, 7547];
const HISTORY_RETENTION: Duration = Duration::from_secs(5 * 60);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeKind {
    Icmp,
    Tcp,
}

#[derive(Clone, Debug)]
pub struct Probe {
    pub host: String,
    pub kind: ProbeKind,
}

#[derive(Clone, Debug)]
pub struct PingTarget {
    pub host: String,
}

#[derive(Clone, Debug)]
pub enum PingResult {
    Ok(f64),
    Timeout,
    Error(String),
    Pending,
}

#[derive(Clone, Debug)]
pub struct Sample {
    pub at: Instant,
    pub result: PingResult,
}

#[derive(Clone, Debug)]
pub struct HostSnapshot {
    #[allow(dead_code)] // useful for upcoming stats work; identifies the host.
    pub host: String,
    pub current: PingResult,
    pub samples: Vec<Sample>,
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
    kind: ProbeKind,
    result: Arc<Mutex<PingResult>>,
    samples: Arc<Mutex<VecDeque<Sample>>>,
    task: JoinHandle<()>,
}

/// The per-target probing strategy. Built once at slot-creation time and
/// reused for every tick of `target_loop`.
enum Prober {
    Icmp(Pinger),
    Tcp(IpAddr),
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

    /// Replace the set of probes being run. Unchanged (host, kind) entries
    /// keep their current result and in-flight task; new entries start in
    /// Pending; removed entries are aborted.
    pub fn set_targets(&self, probes: &[Probe]) {
        let mut state = self.state.lock().unwrap();

        // Drain existing slots into a map keyed by host so we can reuse them.
        let mut existing: Vec<Option<Slot>> = state.slots.drain(..).map(Some).collect();

        let mut new_slots: Vec<Slot> = Vec::with_capacity(probes.len());
        for probe in probes {
            let reused = existing
                .iter_mut()
                .find(|s| {
                    s.as_ref()
                        .is_some_and(|s| s.host == probe.host && s.kind == probe.kind)
                })
                .and_then(|s| s.take());
            match reused {
                Some(slot) => new_slots.push(slot),
                None => {
                    let result = Arc::new(Mutex::new(PingResult::Pending));
                    let samples = Arc::new(Mutex::new(VecDeque::new()));
                    let interval_rx = self.interval_tx.subscribe();
                    // Build the Prober here, not inside the task. Surge-ping's
                    // `Drop for Client` unconditionally marks the shared reply
                    // map as destroyed; if we cloned `Client` into the task
                    // and the task ever ended (e.g. set_targets aborts it on
                    // a router IP change), every other in-flight ping would
                    // start returning `ClientDestroyed`. By creating the
                    // Pinger here we keep the only Client clones inside
                    // `self.v4`/`self.v6`, which live for the whole process.
                    let prober =
                        self.handle.block_on(setup_prober(probe, &self.v4, &self.v6));
                    let task = self.handle.spawn(target_loop(
                        prober,
                        result.clone(),
                        samples.clone(),
                        interval_rx,
                    ));
                    new_slots.push(Slot {
                        host: probe.host.clone(),
                        kind: probe.kind,
                        result,
                        samples,
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

    pub fn snapshot(&self) -> Vec<HostSnapshot> {
        let state = self.state.lock().unwrap();
        state
            .slots
            .iter()
            .map(|s| HostSnapshot {
                host: s.host.clone(),
                current: s.result.lock().unwrap().clone(),
                samples: s.samples.lock().unwrap().iter().cloned().collect(),
            })
            .collect()
    }
}

async fn target_loop(
    mut prober: Result<Prober, String>,
    result: Arc<Mutex<PingResult>>,
    samples: Arc<Mutex<VecDeque<Sample>>>,
    mut interval_rx: watch::Receiver<Duration>,
) {
    // The Prober is built by the caller (PingService::set_targets) so that
    // surge-ping Client clones never live inside a task body; see the comment
    // in set_targets for the full rationale.
    let mut seq: u16 = 0;
    loop {
        let start = Instant::now();
        let r = match &mut prober {
            Ok(Prober::Icmp(pinger)) => {
                match pinger.ping(PingSequence(seq), &[0u8; 32]).await {
                    Ok((_pkt, dur)) => PingResult::Ok(dur.as_secs_f64() * 1000.0),
                    Err(SurgeError::Timeout { .. }) => PingResult::Timeout,
                    Err(SurgeError::IOError(e)) => PingResult::Error(clean_io_error(&e)),
                    Err(e) => PingResult::Error(e.to_string()),
                }
            }
            Ok(Prober::Tcp(addr)) => tcp_probe(*addr).await,
            Err(e) => PingResult::Error(e.clone()),
        };
        seq = seq.wrapping_add(1);
        let finished_at = Instant::now();
        *result.lock().unwrap() = r.clone();
        {
            let mut s = samples.lock().unwrap();
            s.push_back(Sample { at: finished_at, result: r });
            let cutoff = finished_at.checked_sub(HISTORY_RETENTION);
            if let Some(cutoff) = cutoff {
                while let Some(front) = s.front() {
                    if front.at < cutoff {
                        s.pop_front();
                    } else {
                        break;
                    }
                }
            }
        }

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

async fn setup_prober(
    probe: &Probe,
    v4: &Result<Client, String>,
    v6: &Result<Client, String>,
) -> Result<Prober, String> {
    // IPv6 link-local addresses carry a zone id: "fe80::...%en0". The address
    // itself doesn't parse with a zone, so split it off and pass it to the
    // Pinger separately via scope_id (otherwise the kernel has no interface
    // to route through and we get "No route to host").
    let (host_clean, zone) = match probe.host.split_once('%') {
        Some((h, z)) => (h, Some(z)),
        None => (probe.host.as_str(), None),
    };
    let addr = resolve(host_clean)?;
    match probe.kind {
        ProbeKind::Icmp => {
            let client = match addr {
                IpAddr::V4(_) => v4.as_ref(),
                IpAddr::V6(_) => v6.as_ref(),
            };
            let client = client.map_err(Clone::clone)?;
            let ident = PingIdentifier(rand::random());
            let mut pinger = client.pinger(addr, ident).await;
            pinger.timeout(PING_TIMEOUT);
            if let Some(zone) = zone {
                if let Some(idx) = zone_to_index(zone) {
                    pinger.scope_id(idx);
                }
            }
            Ok(Prober::Icmp(pinger))
        }
        ProbeKind::Tcp => Ok(Prober::Tcp(addr)),
    }
}

/// TCP-based reachability probe used for the local router, which often
/// drops ICMP echo addressed to its own interface even while happily
/// forwarding everything else. Races a SYN to each port in
/// `TCP_PROBE_PORTS`; the first one to either accept the connection
/// (open) or reply RST (closed) wins — both prove the host is reachable.
/// If every port silently times out within `TCP_PROBE_TIMEOUT`, returns
/// `Timeout`.
async fn tcp_probe(addr: IpAddr) -> PingResult {
    let mut probes: FuturesUnordered<_> = TCP_PROBE_PORTS
        .iter()
        .map(|&port| {
            let sock = SocketAddr::new(addr, port);
            async move {
                let start = Instant::now();
                let res = tokio::time::timeout(
                    TCP_PROBE_TIMEOUT,
                    tokio::net::TcpStream::connect(sock),
                )
                .await;
                match res {
                    Ok(Ok(_)) => Some(start.elapsed()),
                    Ok(Err(e)) if e.kind() == std::io::ErrorKind::ConnectionRefused => {
                        Some(start.elapsed())
                    }
                    _ => None,
                }
            }
        })
        .collect();

    while let Some(opt) = probes.next().await {
        if let Some(dur) = opt {
            return PingResult::Ok(dur.as_secs_f64() * 1000.0);
        }
    }
    PingResult::Timeout
}

fn zone_to_index(name: &str) -> Option<u32> {
    let c = std::ffi::CString::new(name).ok()?;
    // SAFETY: `if_nametoindex` reads a NUL-terminated C string; CString
    // guarantees that. Returns 0 if the name is not a valid interface.
    let idx = unsafe { libc::if_nametoindex(c.as_ptr()) };
    if idx == 0 { None } else { Some(idx) }
}

fn clean_io_error(e: &std::io::Error) -> String {
    // io::Error::to_string() looks like "No route to host (os error 65)".
    // Strip the trailing OS-error noise for menu display.
    let msg = e.to_string();
    match msg.rfind(" (os error") {
        Some(idx) => msg[..idx].to_string(),
        None => msg,
    }
}

fn resolve(host: &str) -> Result<IpAddr, String> {
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(ip);
    }
    let addrs: Vec<IpAddr> = (host, 0u16)
        .to_socket_addrs()
        .map_err(|e| e.to_string())?
        .map(|s| s.ip())
        .collect();
    // Prefer IPv4: on dual-stack networks with broken v6 routing to a
    // particular destination (a very common condition), preferring the v6
    // address from getaddrinfo would silently make every ping time out.
    // Explicitly-typed IPv6 hosts still work via the parse() short-circuit
    // above.
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .copied()
        .ok_or_else(|| "no address".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pings_loopback() {
        let svc = PingService::new(Duration::from_millis(100));
        svc.set_targets(&[Probe { host: "127.0.0.1".to_string(), kind: ProbeKind::Icmp }]);
        // Give the loop time to complete at least one ping.
        std::thread::sleep(Duration::from_secs(1));
        let snap = svc.snapshot();
        assert_eq!(snap.len(), 1);
        match &snap[0].current {
            PingResult::Ok(ms) => assert!(*ms >= 0.0 && *ms < 1000.0, "got {ms}ms"),
            other => panic!("expected Ok, got {other:?}"),
        }
        assert!(!snap[0].samples.is_empty(), "expected at least one sample");
    }

    #[test]
    fn set_targets_reuses_unchanged_slots() {
        let svc = PingService::new(Duration::from_millis(100));
        svc.set_targets(&[Probe { host: "127.0.0.1".to_string(), kind: ProbeKind::Icmp }]);
        std::thread::sleep(Duration::from_millis(500));
        let first = svc.snapshot();
        assert!(matches!(first[0].current, PingResult::Ok(_)));
        let first_sample_count = first[0].samples.len();
        assert!(first_sample_count > 0);

        svc.set_targets(&[
            Probe { host: "127.0.0.1".to_string(), kind: ProbeKind::Icmp },
            Probe { host: "1.1.1.1".to_string(), kind: ProbeKind::Icmp },
        ]);
        let immediate = svc.snapshot();
        assert!(
            matches!(immediate[0].current, PingResult::Ok(_)),
            "reused slot lost its result: {:?}",
            immediate[0].current
        );
        assert_eq!(
            immediate[0].samples.len(),
            first_sample_count,
            "reused slot lost its history"
        );
        assert!(matches!(immediate[1].current, PingResult::Pending));
        assert!(immediate[1].samples.is_empty());
    }
}
