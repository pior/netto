#![deny(unsafe_op_in_unsafe_fn)]

mod netinfo;
mod ping;
mod preferences;
mod settings;
mod sparkline;

use std::cell::{Cell, RefCell};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::sel;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2::AnyThread;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSBezierPath, NSButton,
    NSColor, NSFont, NSFontAttributeName, NSFontWeightRegular, NSForegroundColorAttributeName,
    NSAttributedStringNSStringDrawing, NSImage, NSLayoutAttribute, NSLayoutConstraint, NSLayoutRelation, NSLineBreakMode, NSMenu,
    NSMenuDelegate, NSMenuItem, NSPasteboard, NSPasteboardTypeString, NSStatusBar, NSStatusItem,
    NSTextAlignment, NSTextField, NSToolTipTag, NSVariableStatusItemLength, NSView,
};
use objc2_foundation::{NSAttributedString, NSDictionary};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use std::ffi::c_void;
use objc2_foundation::{
    ns_string, NSNotification, NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes,
    NSString, NSTimer,
};

use netinfo::NetInfo;
use ping::{HostSnapshot, PingResult, PingService, PingTarget, Probe, ProbeKind};

const DEFAULT_TARGETS: &[&str] = &["google.com", "cloudflare.com", "apple.com"];

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = AppDelegateIvars]
    struct AppDelegate;

    unsafe impl NSObjectProtocol for AppDelegate {}

    unsafe impl NSApplicationDelegate for AppDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::from(self);
            self.install_menu_delegate();
            self.tick(mtm);
            let slow = self.ivars().slow_interval.get();
            self.reschedule_timer(mtm, slow);
        }
    }

    unsafe impl NSMenuDelegate for AppDelegate {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            let mtm = MainThreadMarker::from(self);
            // Run tick *before* marking the menu as open so any pending
            // structural change (e.g. public IP arrived async) gets folded
            // into the menu we're about to show — once `menu_open` is true,
            // rebuild_menu_structure is skipped.
            self.tick(mtm);
            self.ivars().menu_open.set(true);
            let fast = self.ivars().fast_interval.get();
            self.ivars().ping_service.set_interval(fast);
            self.reschedule_timer(mtm, fast);
        }

        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &NSMenu) {
            let mtm = MainThreadMarker::from(self);
            self.ivars().menu_open.set(false);
            let slow = self.ivars().slow_interval.get();
            self.ivars().ping_service.set_interval(slow);
            self.reschedule_timer(mtm, slow);
        }
    }

    impl AppDelegate {
        #[unsafe(method(timerFired:))]
        unsafe fn timer_fired(&self, _timer: &NSTimer) {
            let mtm = MainThreadMarker::from(self);
            self.tick(mtm);
        }

        #[unsafe(method(showPreferences:))]
        unsafe fn show_preferences(&self, _sender: &AnyObject) {
            unsafe {
                NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                    0.0,
                    self as &AnyObject,
                    sel!(openPreferencesDeferred:),
                    None,
                    false,
                );
            }
        }

        #[unsafe(method(openPreferencesDeferred:))]
        unsafe fn open_preferences_deferred(&self, _timer: &NSTimer) {
            let mtm = MainThreadMarker::from(self);
            self.open_preferences(mtm);
        }

        /// Action target for IP chips — copies the sender's plain title
        /// (the IP address) into the general pasteboard and triggers a
        /// brief visual flash on the chip.
        #[unsafe(method(copyIp:))]
        unsafe fn copy_ip(&self, sender: &ChipButton) {
            let title = sender.title();
            let pasteboard = NSPasteboard::generalPasteboard();
            pasteboard.clearContents();
            unsafe { pasteboard.setString_forType(&title, NSPasteboardTypeString) };
            sender.flash_copied();
        }

        #[unsafe(method(reloadSettings:))]
        unsafe fn reload_settings(&self, _sender: &AnyObject) {
            let mtm = MainThreadMarker::from(self);
            if let Some(targets) = settings::load_targets() {
                *self.ivars().targets.borrow_mut() = targets;
            }
            self.apply_prefs();
            self.tick(mtm);
        }
    }
);

struct AppDelegateIvars {
    status_item: Retained<NSStatusItem>,
    targets: RefCell<Vec<PingTarget>>,
    ping_service: PingService,
    net_info: RefCell<NetInfo>,
    menu: Retained<NSMenu>,
    internet_row: InfoRow,
    router_row: InfoRow,
    target_rows: RefCell<Vec<InfoRow>>,
    dns_rows: RefCell<Vec<DnsRow>>,
    vpn_row: InfoRow,
    ip_rows: RefCell<Vec<NetInfoRow>>,
    settings_item: Retained<NSMenuItem>,
    quit_item: Retained<NSMenuItem>,
    menu_open: Cell<bool>,
    slow_interval: Cell<Duration>,
    fast_interval: Cell<Duration>,
    refresh_timer: RefCell<Option<Retained<NSTimer>>>,
    prefs_controller: RefCell<Option<Retained<NSObject>>>,
    public_v4: Arc<Mutex<Option<String>>>,
    public_v6: Arc<Mutex<Option<String>>>,
}

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        let targets = settings::load_targets().unwrap_or_else(|| {
            let defaults: Vec<PingTarget> = DEFAULT_TARGETS
                .iter()
                .map(|host| PingTarget {
                    host: (*host).to_string(),
                })
                .collect();
            settings::save_targets(&defaults);
            defaults
        });

        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);

        let internet_row = InfoRow::new(mtm);
        let router_row = InfoRow::new(mtm);
        let vpn_row = InfoRow::new(mtm);

        let settings_item =
            create_menu_item(mtm, ns_string!("Settings\u{2026}"), Some(sel!(showPreferences:)));
        settings_item.setEnabled(true);

        let quit_item = create_menu_item(mtm, ns_string!("Quit"), Some(sel!(terminate:)));
        quit_item.setKeyEquivalent(ns_string!("q"));

        let prefs = settings::load_prefs();
        sparkline::set_tolerance_ms(prefs.tolerance_ms);
        let slow = Duration::from_secs_f64(prefs.slow_secs);
        let fast = Duration::from_secs_f64(prefs.fast_secs);

        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars {
            status_item,
            targets: RefCell::new(targets),
            ping_service: PingService::new(slow),
            net_info: RefCell::new(NetInfo::default()),
            menu,
            internet_row,
            router_row,
            target_rows: RefCell::new(Vec::new()),
            dns_rows: RefCell::new(Vec::new()),
            vpn_row,
            ip_rows: RefCell::new(Vec::new()),
            settings_item,
            quit_item,
            menu_open: Cell::new(false),
            slow_interval: Cell::new(slow),
            fast_interval: Cell::new(fast),
            refresh_timer: RefCell::new(None),
            prefs_controller: RefCell::new(None),
            public_v4: Arc::new(Mutex::new(None)),
            public_v6: Arc::new(Mutex::new(None)),
        });
        let delegate: Retained<Self> = unsafe { msg_send![super(this), init] };
        unsafe { delegate.ivars().settings_item.setTarget(Some(&*delegate as &AnyObject)) };
        delegate
    }

    fn install_menu_delegate(&self) {
        let ivars = self.ivars();
        let proto = ProtocolObject::from_ref(self);
        ivars.menu.setDelegate(Some(proto));
        ivars.status_item.setMenu(Some(&ivars.menu));
    }

    /// One UI tick: refresh netinfo, push the host list to the ping service,
    /// rebuild menu structure if it changed (only when menu is closed), and
    /// update item titles + status bar from the current snapshot.
    fn tick(&self, mtm: MainThreadMarker) {
        let ivars = self.ivars();
        let new_info = netinfo::collect();

        let prev_info = ivars.net_info.borrow().clone();

        // Re-fetch the public IP whenever the local network state changes
        // (initial launch counts: prev is all-None, new is populated).
        let network_changed = prev_info.local_v4 != new_info.local_v4
            || prev_info.local_v6 != new_info.local_v6
            || prev_info.router_v4 != new_info.router_v4
            || prev_info.router_v6 != new_info.router_v6
            || prev_info.vpn_interface != new_info.vpn_interface;
        if network_changed {
            fetch_public_ips(ivars.public_v4.clone(), ivars.public_v6.clone());
        }

        let public = self.public_snapshot();
        let new_ip_row_count = ip_entries(&new_info, &public).len();
        let structure_changed = ivars.ip_rows.borrow().len() != new_ip_row_count
            || ivars.dns_rows.borrow().len() != new_info.dns_ips.len().max(1)
            || ivars.target_rows.borrow().len() != ivars.targets.borrow().len()
            || pick_router_probe(&prev_info).is_some() != pick_router_probe(&new_info).is_some()
            || prev_info.vpn_interface.is_some() != new_info.vpn_interface.is_some();

        *ivars.net_info.borrow_mut() = new_info.clone();

        // Push current host list to the service (it diffs internally; unchanged
        // hosts keep their results in-flight).
        let hosts = self.host_list(&new_info);
        ivars.ping_service.set_targets(&hosts);

        if structure_changed && !ivars.menu_open.get() {
            self.rebuild_menu_structure(mtm, &new_info);
        }

        self.update_titles(mtm, &new_info);
    }

    fn public_snapshot(&self) -> (Option<String>, Option<String>) {
        let ivars = self.ivars();
        (
            ivars.public_v4.lock().unwrap().clone(),
            ivars.public_v6.lock().unwrap().clone(),
        )
    }

    fn host_list(&self, info: &NetInfo) -> Vec<Probe> {
        let targets = self.ivars().targets.borrow();
        let mut probes: Vec<Probe> = targets
            .iter()
            .map(|t| Probe { host: t.host.clone(), kind: ProbeKind::Icmp })
            .collect();
        if let Some(p) = pick_router_probe(info) {
            probes.push(p);
        }
        probes.extend(
            info.dns_ips
                .iter()
                .map(|ip| Probe { host: ip.clone(), kind: ProbeKind::Icmp }),
        );
        probes
    }

    fn rebuild_menu_structure(&self, mtm: MainThreadMarker, info: &NetInfo) {
        let ivars = self.ivars();
        let menu = &ivars.menu;
        menu.removeAllItems();

        // Connectivity aggregate + router latency at top.
        menu.addItem(&ivars.internet_row.item);
        menu.addItem(&ivars.router_row.item);

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Per-target rows.
        let target_count = ivars.targets.borrow().len();
        {
            let mut rows = ivars.target_rows.borrow_mut();
            rows.resize_with(target_count, || InfoRow::new(mtm));
            for row in rows.iter() {
                menu.addItem(&row.item);
            }
        }

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // DNS rows (label + IP chip + sparkline + latency).
        {
            let mut rows = ivars.dns_rows.borrow_mut();
            let row_count = info.dns_ips.len().max(1);
            rows.resize_with(row_count, || DnsRow::new(mtm, self as &AnyObject));
            for row in rows.iter() {
                menu.addItem(&row.item);
            }
        }

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        // Network info block: VPN status (when active) then IP chip rows
        // (Local, Router, Public) with click-to-copy v4/v6 chips.
        if info.vpn_interface.is_some() {
            menu.addItem(&ivars.vpn_row.item);
        }
        {
            let public = self.public_snapshot();
            let entries = ip_entries(info, &public);
            let mut rows = ivars.ip_rows.borrow_mut();
            rows.resize_with(entries.len(), || NetInfoRow::new(mtm, self as &AnyObject));
            for row in rows.iter() {
                menu.addItem(&row.item);
            }
        }

        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&ivars.settings_item);
        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&ivars.quit_item);
    }

    fn update_titles(&self, mtm: MainThreadMarker, info: &NetInfo) {
        let ivars = self.ivars();
        let targets = ivars.targets.borrow();
        let snapshot = ivars.ping_service.snapshot();
        let now = Instant::now();

        // Snapshot layout: [targets..., router?, dns...]
        let target_count = targets.len();
        let target_snaps: &[HostSnapshot] = snapshot.get(..target_count).unwrap_or(&[]);
        let mut idx = target_count;
        let router_snap: Option<&HostSnapshot> = if pick_router_probe(info).is_some() {
            let r = snapshot.get(idx);
            idx += 1;
            r
        } else {
            None
        };
        let dns_snaps: &[HostSnapshot] = snapshot.get(idx..).unwrap_or(&[]);

        // Internet aggregate: best-of-N across user targets.
        let target_buckets: Vec<Vec<sparkline::BucketInfo>> = target_snaps
            .iter()
            .map(|s| sparkline::bucketize(&s.samples, now))
            .collect();
        let internet_buckets = sparkline::best_of(&target_buckets);
        let internet_current = aggregate_best(target_snaps);

        // Status bar tracks the Internet aggregate as a 4-cell mini sparkline.
        if let Some(button) = ivars.status_item.button(mtm) {
            let image = make_status_image(&internet_buckets);
            button.setImage(Some(&image));
            button.setTitle(ns_string!(""));
            // Silence the "err"/"ms" hint we'd otherwise lose by surfacing
            // the current state in the button's tooltip.
            let hint = match &internet_current {
                PingResult::Ok(ms) => format!("Internet: {ms:.1} ms"),
                PingResult::Timeout => "Internet: timeout".to_string(),
                PingResult::Error(e) => format!("Internet: {e}"),
                PingResult::Pending => "Internet: …".to_string(),
            };
            button.setToolTip(Some(&NSString::from_str(&hint)));
        }

        // Internet row.
        ivars
            .internet_row
            .set("Internet", internet_buckets, &format_result(&internet_current));

        // Router row (sparkline + latency).
        match router_snap {
            Some(s) => {
                let buckets = sparkline::bucketize(&s.samples, now);
                ivars.router_row.set("Router", buckets, &format_result(&s.current));
            }
            None => ivars.router_row.set("Router", vec![], "unknown"),
        }

        // Per-target rows.
        let target_rows = ivars.target_rows.borrow();
        for (i, target) in targets.iter().enumerate() {
            if let Some(row) = target_rows.get(i) {
                let (current, buckets) = match target_snaps.get(i) {
                    Some(s) => (s.current.clone(), sparkline::bucketize(&s.samples, now)),
                    None => (PingResult::Pending, vec![]),
                };
                row.set(&target.host, buckets, &format_result(&current));
            }
        }

        // DNS rows: label + chip + sparkline + latency. Strip IPv6 zone-id
        // suffix for display (e.g. "fe80::...%en0" → "fe80::...").
        let dns_rows = ivars.dns_rows.borrow();
        if info.dns_ips.is_empty() {
            if let Some(row) = dns_rows.first() {
                row.set(None, vec![], "");
            }
        } else {
            for (i, ip) in info.dns_ips.iter().enumerate() {
                if let Some(row) = dns_rows.get(i) {
                    let display_ip = ip.split('%').next().unwrap_or(ip);
                    let (current, buckets) = match dns_snaps.get(i) {
                        Some(s) => (s.current.clone(), sparkline::bucketize(&s.samples, now)),
                        None => (PingResult::Pending, vec![]),
                    };
                    row.set(Some(display_ip), buckets, &format_result(&current));
                }
            }
        }

        // VPN status (text-only).
        if let Some(iface) = &info.vpn_interface {
            ivars.vpn_row.set(&format!("VPN: active ({iface})"), vec![], "");
        }

        // IP chip rows.
        let public = self.public_snapshot();
        let entries = ip_entries(info, &public);
        let ip_rows = ivars.ip_rows.borrow();
        for (i, entry) in entries.iter().enumerate() {
            if let Some(row) = ip_rows.get(i) {
                row.set(
                    &format!("{}:", entry.label),
                    entry.v4.as_deref(),
                    entry.v6.as_deref(),
                );
            }
        }
    }

    fn open_preferences(&self, mtm: MainThreadMarker) {
        let ivars = self.ivars();

        {
            let controller_ref = ivars.prefs_controller.borrow();
            if let Some(obj) = controller_ref.as_ref() {
                let _: () = unsafe { msg_send![obj, showWindow] };
                return;
            }
        }

        let app_delegate_ptr = self as *const AppDelegate as usize;
        let on_save = Box::new(move || {
            let obj = app_delegate_ptr as *const AnyObject;
            unsafe {
                let _: () = msg_send![obj, reloadSettings: std::ptr::null::<AnyObject>()];
            }
        });

        let controller = {
            let targets = ivars.targets.borrow();
            preferences::PrefsController::new(mtm, &targets, on_save)
        };

        controller.show();
        *ivars.prefs_controller.borrow_mut() = Some(Retained::into_super(controller));
    }

    /// Reload tunable preferences (intervals + sparkline sensitivity) from
    /// NSUserDefaults and immediately apply them: update the live intervals,
    /// retarget the ping service, and reschedule the UI tick timer at the
    /// rate matching the menu's current state.
    fn apply_prefs(&self) {
        let prefs = settings::load_prefs();
        let ivars = self.ivars();
        let slow = Duration::from_secs_f64(prefs.slow_secs);
        let fast = Duration::from_secs_f64(prefs.fast_secs);
        ivars.slow_interval.set(slow);
        ivars.fast_interval.set(fast);
        sparkline::set_tolerance_ms(prefs.tolerance_ms);
        let mtm = MainThreadMarker::from(self);
        let current = if ivars.menu_open.get() { fast } else { slow };
        ivars.ping_service.set_interval(current);
        self.reschedule_timer(mtm, current);
    }

    fn reschedule_timer(&self, _mtm: MainThreadMarker, interval: Duration) {
        let ivars = self.ivars();
        if let Some(timer) = ivars.refresh_timer.borrow_mut().take() {
            timer.invalidate();
        }
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                interval.as_secs_f64(),
                self as &AnyObject,
                sel!(timerFired:),
                None,
                true,
            )
        };
        // Register in common modes so it keeps firing while the menu is up
        // (which switches the run loop into NSEventTrackingRunLoopMode).
        unsafe {
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
        *ivars.refresh_timer.borrow_mut() = Some(timer);
    }
}

struct IpEntry {
    label: &'static str,
    v4: Option<String>,
    v6: Option<String>,
}

/// The set of IP-chip rows to display, in order. Each entry shows a label
/// plus one or two clickable address chips; entries with neither v4 nor v6
/// are omitted.
/// Drop an IPv6 zone-id suffix ("%en0") for display purposes.
fn strip_zone(addr: &str) -> &str {
    addr.split('%').next().unwrap_or(addr)
}

/// 192.0.0.0/29 is reserved by RFC 7335 for DNS64/NAT64 use. On IPv6-only
/// mobile networks with 464XLAT, macOS sets the IPv4 default gateway to
/// 192.0.0.1 as a CLAT placeholder — no real host lives there, so probing
/// it (ICMP or TCP) always times out.
fn is_clat_placeholder(ip: &str) -> bool {
    match ip.parse::<std::net::Ipv4Addr>() {
        Ok(a) => {
            let o = a.octets();
            o[0] == 192 && o[1] == 0 && o[2] == 0 && o[3] < 8
        }
        Err(_) => false,
    }
}

/// Pick the best router address to probe:
/// 1. IPv6 router (ICMPv6 — link-local with zone id works directly, and
///    IPv6 routers generally answer echo since ND requires it).
/// 2. IPv4 router via TCP fallback — but skip the CLAT placeholder block,
///    which has no real host to talk to.
fn pick_router_probe(info: &NetInfo) -> Option<Probe> {
    if let Some(v6) = &info.router_v6 {
        return Some(Probe { host: v6.clone(), kind: ProbeKind::Icmp });
    }
    if let Some(v4) = &info.router_v4 {
        if !is_clat_placeholder(v4) {
            return Some(Probe { host: v4.clone(), kind: ProbeKind::Tcp });
        }
    }
    None
}

fn ip_entries(
    info: &NetInfo,
    public: &(Option<String>, Option<String>),
) -> Vec<IpEntry> {
    let mut rows = Vec::new();
    if info.local_v4.is_some() || info.local_v6.is_some() {
        rows.push(IpEntry {
            label: "Local",
            v4: info.local_v4.clone(),
            v6: info.local_v6.clone(),
        });
    }
    if info.router_v4.is_some() || info.router_v6.is_some() {
        rows.push(IpEntry {
            label: "Router",
            v4: info.router_v4.clone(),
            // router_v6 may carry a "%zone" suffix (needed for ICMPv6 to
            // link-local) — strip it for chip display.
            v6: info.router_v6.as_deref().map(strip_zone).map(str::to_string),
        });
    }
    if public.0.is_some() || public.1.is_some() {
        rows.push(IpEntry {
            label: "Public",
            v4: public.0.clone(),
            v6: public.1.clone(),
        });
    }
    rows
}

/// Kick off background fetches for the public IPv4 and IPv6 addresses via
/// `api.ipify.org`. The v4 and v6 hostnames have only A and only AAAA
/// records respectively, so the address family is forced by DNS — no need
/// to bind locally or pin a route. Each fetch runs in its own short-lived
/// thread, writes to the shared slot on success, or clears it on failure.
fn fetch_public_ips(v4_slot: Arc<Mutex<Option<String>>>, v6_slot: Arc<Mutex<Option<String>>>) {
    fetch_one("https://api.ipify.org", v4_slot);
    fetch_one("https://api6.ipify.org", v6_slot);
}

fn fetch_one(url: &'static str, slot: Arc<Mutex<Option<String>>>) {
    std::thread::spawn(move || {
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(5))
            .build();
        let new_value = agent
            .get(url)
            .call()
            .ok()
            .and_then(|resp| resp.into_string().ok())
            .and_then(|s| {
                let t = s.trim();
                if t.is_empty() || t.len() > 64 {
                    None
                } else {
                    Some(t.to_string())
                }
            });
        if let Ok(mut g) = slot.lock() {
            *g = new_value;
        }
    });
}

/// Best-of-N across snapshots' current results. If any target is Ok, return
/// the minimum-latency Ok. Otherwise return Pending if any is Pending, else
/// Timeout/Error.
fn aggregate_best(snaps: &[HostSnapshot]) -> PingResult {
    if snaps.is_empty() {
        return PingResult::Pending;
    }
    let best_ok: Option<f64> = snaps
        .iter()
        .filter_map(|s| match s.current {
            PingResult::Ok(ms) => Some(ms),
            _ => None,
        })
        .fold(None, |acc, ms| Some(acc.map_or(ms, |a: f64| a.min(ms))));
    if let Some(ms) = best_ok {
        return PingResult::Ok(ms);
    }
    if snaps.iter().any(|s| matches!(s.current, PingResult::Pending)) {
        return PingResult::Pending;
    }
    if snaps.iter().any(|s| matches!(s.current, PingResult::Timeout)) {
        return PingResult::Timeout;
    }
    PingResult::Error("all targets failed".to_string())
}

const SPARK_CELL_W: f64 = 5.0;
const SPARK_GAP: f64 = 1.0;
const SPARK_HEIGHT: f64 = 11.0;
const SPARK_RADIUS: f64 = 1.5;
const LATENCY_WIDTH: f64 = 52.0;

fn spark_total_width() -> f64 {
    let n = sparkline::BUCKET_COUNT as f64;
    n * SPARK_CELL_W + (n - 1.0) * SPARK_GAP
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[ivars = SparklineViewIvars]
    struct SparklineView;

    impl SparklineView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, _dirty: CGRect) {
            self.do_draw();
        }

        #[unsafe(method(intrinsicContentSize))]
        fn intrinsic_content_size(&self) -> CGSize {
            CGSize::new(spark_total_width(), SPARK_HEIGHT)
        }

        /// NSToolTipOwner informal protocol: AppKit invokes this when the
        /// pointer hovers over a registered tooltip rect. We encoded the
        /// bucket index in `data` when we registered each rect.
        #[unsafe(method_id(view:stringForToolTip:point:userData:))]
        unsafe fn provide_tooltip(
            &self,
            _view: &NSView,
            _tag: NSToolTipTag,
            _point: CGPoint,
            data: *mut c_void,
        ) -> Retained<NSString> {
            let idx = data as usize;
            let tooltips = self.ivars().tooltips.borrow();
            let text = tooltips.get(idx).map(String::as_str).unwrap_or("");
            NSString::from_str(text)
        }
    }
);

struct SparklineViewIvars {
    cells: RefCell<Vec<sparkline::BucketInfo>>,
    tooltips: RefCell<Vec<String>>,
}

impl SparklineView {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(SparklineViewIvars {
            cells: RefCell::new(Vec::new()),
            tooltips: RefCell::new(vec![String::new(); sparkline::BUCKET_COUNT]),
        });
        let view: Retained<Self> = unsafe { msg_send![super(this), init] };
        view.setTranslatesAutoresizingMaskIntoConstraints(false);

        // Register one tooltip rect per bucket; userData carries the index.
        let owner: &AnyObject = &view;
        for i in 0..sparkline::BUCKET_COUNT {
            let x = (i as f64) * (SPARK_CELL_W + SPARK_GAP);
            let rect = CGRect::new(
                CGPoint::new(x, 0.0),
                CGSize::new(SPARK_CELL_W, SPARK_HEIGHT),
            );
            unsafe {
                view.addToolTipRect_owner_userData(rect, owner, i as *mut c_void);
            }
        }

        view
    }

    fn set_buckets(&self, buckets: Vec<sparkline::BucketInfo>) {
        let tooltips: Vec<String> = buckets.iter().map(format_bucket_tooltip).collect();
        *self.ivars().cells.borrow_mut() = buckets;
        *self.ivars().tooltips.borrow_mut() = tooltips;
        self.setNeedsDisplay(true);
    }

    fn do_draw(&self) {
        let cells = self.ivars().cells.borrow();
        let bounds = self.bounds();
        let y = (bounds.size.height - SPARK_HEIGHT) / 2.0;
        for (i, bucket) in cells.iter().enumerate() {
            let x = (i as f64) * (SPARK_CELL_W + SPARK_GAP);
            let rect = CGRect::new(
                CGPoint::new(x, y),
                CGSize::new(SPARK_CELL_W, SPARK_HEIGHT),
            );
            cell_color(bucket).setFill();
            let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
                rect,
                SPARK_RADIUS,
                SPARK_RADIUS,
            );
            path.fill();
        }
    }
}

/// NSButton subclass for the IP chips. Draws a subtle rounded-rect background
/// sized to the attributed title (so the chip's frame can stay a fixed width
/// for column alignment while the visible "pill" hugs the text). When the
/// user clicks the chip, `flash_copied` briefly swaps the background to a
/// green tint as a "copied" cue.
const CHIP_RADIUS: f64 = 4.0;
const CHIP_BG_VPAD: f64 = 2.0;
const CHIP_FLASH_SECS: f64 = 0.7;

define_class!(
    #[unsafe(super(NSButton))]
    #[thread_kind = MainThreadOnly]
    #[ivars = ChipButtonIvars]
    struct ChipButton;

    impl ChipButton {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self, dirty: CGRect) {
            self.draw_bg();
            unsafe {
                let _: () = msg_send![super(self), drawRect: dirty];
            }
        }

        #[unsafe(method(flashCopied))]
        fn flash_copied_objc(&self) {
            self.flash_copied();
        }

        #[unsafe(method(endFlash:))]
        unsafe fn end_flash(&self, _timer: &NSTimer) {
            self.ivars().flashing.set(false);
            unsafe { let _: () = msg_send![self, setNeedsDisplay: true]; }
        }
    }
);

struct ChipButtonIvars {
    flashing: Cell<bool>,
}

impl ChipButton {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ChipButtonIvars {
            flashing: Cell::new(false),
        });
        let btn: Retained<Self> = unsafe { msg_send![super(this), init] };
        btn
    }

    fn flash_copied(&self) {
        self.ivars().flashing.set(true);
        unsafe { let _: () = msg_send![self, setNeedsDisplay: true]; }
        // Use timerWithTimeInterval + addTimer:forMode: so the revert fires
        // while the menu is open (NSEventTrackingRunLoopMode).
        unsafe {
            let timer = NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                CHIP_FLASH_SECS,
                self as &AnyObject,
                sel!(endFlash:),
                None,
                false,
            );
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
        }
    }

    fn draw_bg(&self) {
        let attr = self.attributedTitle();
        let text_size = attr.size();
        if text_size.width <= 0.0 {
            return;
        }
        let bounds = self.bounds();
        let bg_h = (text_size.height + 2.0 * CHIP_BG_VPAD).min(bounds.size.height);
        let bg_w = text_size.width.min(bounds.size.width);
        let y = (bounds.size.height - bg_h) / 2.0;
        let bg_rect = CGRect::new(CGPoint::new(0.0, y), CGSize::new(bg_w, bg_h));
        let (base, alpha) = if self.ivars().flashing.get() {
            (NSColor::systemGreenColor(), 0.30)
        } else {
            (NSColor::labelColor(), 0.08)
        };
        let bg = base.colorWithAlphaComponent(alpha);
        bg.setFill();
        NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(bg_rect, CHIP_RADIUS, CHIP_RADIUS)
            .fill();
    }
}

// Status-bar mini sparkline geometry.
const STATUS_CELLS: usize = 4;
const STATUS_CELL_W: f64 = 4.0;
const STATUS_GAP: f64 = 1.5;
const STATUS_HEIGHT: f64 = 11.0;
const STATUS_RADIUS: f64 = 1.0;

/// Build the status-bar image: the most recent `STATUS_CELLS` buckets of
/// the Internet aggregate, drawn as a tiny gradient sparkline.
fn make_status_image(buckets: &[sparkline::BucketInfo]) -> Retained<NSImage> {
    let width = STATUS_CELLS as f64 * STATUS_CELL_W + (STATUS_CELLS - 1) as f64 * STATUS_GAP;
    let size = CGSize::new(width, STATUS_HEIGHT);
    let image = NSImage::initWithSize(NSImage::alloc(), size);
    // `lockFocus` / `unlockFocus` are flagged deprecated in favor of
    // `imageWithSize:flipped:drawingHandler:`, but that needs a block (extra
    // dep). For a tiny status-bar icon redrawn every tick this is fine.
    #[allow(deprecated)]
    image.lockFocus();

    let start = buckets.len().saturating_sub(STATUS_CELLS);
    let tail = &buckets[start..];
    for (i, bucket) in tail.iter().enumerate() {
        let x = (i as f64) * (STATUS_CELL_W + STATUS_GAP);
        let rect = CGRect::new(
            CGPoint::new(x, 0.0),
            CGSize::new(STATUS_CELL_W, STATUS_HEIGHT),
        );
        cell_color(bucket).setFill();
        let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            rect,
            STATUS_RADIUS,
            STATUS_RADIUS,
        );
        path.fill();
    }

    // Pad on the left with empty cells if the history doesn't have enough
    // recent buckets yet (e.g. right after launch).
    for i in tail.len()..STATUS_CELLS {
        let x = (i as f64) * (STATUS_CELL_W + STATUS_GAP);
        let rect = CGRect::new(
            CGPoint::new(x, 0.0),
            CGSize::new(STATUS_CELL_W, STATUS_HEIGHT),
        );
        NSColor::tertiaryLabelColor().setFill();
        let path = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(
            rect,
            STATUS_RADIUS,
            STATUS_RADIUS,
        );
        path.fill();
    }

    #[allow(deprecated)]
    image.unlockFocus();
    // Don't tint with the system template color — we want our own gradient.
    image.setTemplate(false);
    image
}

/// Color for a single sparkline cell. Empty cells use the system's tertiary
/// label gray; otherwise the bucket's badness score maps onto a hue gradient
/// (green → yellow → red).
fn cell_color(b: &sparkline::BucketInfo) -> Retained<NSColor> {
    match sparkline::badness(b) {
        None => NSColor::tertiaryLabelColor(),
        Some(score) => gradient_color(score),
    }
}

fn gradient_color(score: f64) -> Retained<NSColor> {
    let t = score.clamp(0.0, 1.0) as f64;
    // Hue: green ≈ 120°/360 = 0.333 at t=0, red = 0 at t=1; yellow naturally
    // appears around t ≈ 0.5 (60°). Slightly desaturated for menu rendering.
    let hue = (1.0 - t) * 0.333;
    NSColor::colorWithHue_saturation_brightness_alpha(hue, 0.78, 0.85, 1.0)
}

fn format_bucket_tooltip(b: &sparkline::BucketInfo) -> String {
    if b.samples == 0 {
        return "no data".to_string();
    }
    let loss_pct = b.loss_rate() * 100.0;
    match (b.max_ok_ms, b.failed > 0) {
        (Some(ms), false) => format!("{ms:.1} ms"),
        (Some(ms), true) => format!("{ms:.1} ms · {loss_pct:.0}% loss"),
        (None, _) => "all timeouts".to_string(),
    }
}

/// A menu row with a custom NSView containing a left label, a sparkline
/// view, and a right (latency) label. The right label is pinned to the
/// view's trailing edge with a fixed width, and the sparkline is pinned
/// immediately to its left with a fixed width — so the sparkline column
/// has the same x position across every row.
struct InfoRow {
    item: Retained<NSMenuItem>,
    left: Retained<NSTextField>,
    sparkline: Retained<SparklineView>,
    right: Retained<NSTextField>,
}

impl InfoRow {
    fn new(mtm: MainThreadMarker) -> Self {
        let item = NSMenuItem::new(mtm);
        item.setEnabled(true);

        let view = NSView::new(mtm);
        view.setTranslatesAutoresizingMaskIntoConstraints(false);

        let left = NSTextField::labelWithString(ns_string!(""), mtm);
        let right = NSTextField::labelWithString(ns_string!(""), mtm);
        let font = NSFont::menuFontOfSize(0.0);
        left.setFont(Some(&font));
        right.setFont(Some(&font));
        right.setAlignment(NSTextAlignment::Right);
        // Single line + tail truncation. With attributed strings of mixed
        // font sizes (e.g. DNS row: "DNS " + smaller monospaced IPv6), this
        // ensures the field can shrink horizontally to fit the available
        // space instead of clipping the IP entirely.
        left.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        left.setMaximumNumberOfLines(1);
        left.setTranslatesAutoresizingMaskIntoConstraints(false);
        right.setTranslatesAutoresizingMaskIntoConstraints(false);

        let sparkline = SparklineView::new(mtm);

        view.addSubview(&left);
        view.addSubview(&sparkline);
        view.addSubview(&right);

        const H_MARGIN: f64 = 14.0;
        const V_MARGIN: f64 = 3.0;
        const SPACING: f64 = 6.0;

        let left_obj: &AnyObject = &left;
        let spark_obj: &AnyObject = &sparkline;
        let right_obj: &AnyObject = &right;
        let view_obj: &AnyObject = &view;
        unsafe {
            for c in [
                // Vertical alignment + view height.
                pin(left_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(spark_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(right_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(left_obj, NSLayoutAttribute::Top, view_obj, NSLayoutAttribute::Top, V_MARGIN),
                pin(view_obj, NSLayoutAttribute::Bottom, left_obj, NSLayoutAttribute::Bottom, V_MARGIN),

                // Right column: fixed width, pinned to view trailing.
                pin(right_obj, NSLayoutAttribute::Trailing, view_obj, NSLayoutAttribute::Trailing, -H_MARGIN),
                pin_const(right_obj, NSLayoutAttribute::Width, LATENCY_WIDTH),

                // Sparkline: fixed width, pinned immediately left of the right column.
                pin(spark_obj, NSLayoutAttribute::Trailing, right_obj, NSLayoutAttribute::Leading, -SPACING),
                pin_const(spark_obj, NSLayoutAttribute::Width, spark_total_width()),
                pin_const(spark_obj, NSLayoutAttribute::Height, SPARK_HEIGHT),

                // Left label: leading edge of view, may shrink up to the sparkline.
                pin(left_obj, NSLayoutAttribute::Leading, view_obj, NSLayoutAttribute::Leading, H_MARGIN),
                pin_rel(
                    left_obj,
                    NSLayoutAttribute::Trailing,
                    NSLayoutRelation::LessThanOrEqual,
                    spark_obj,
                    NSLayoutAttribute::Leading,
                    -SPACING,
                ),
            ] {
                c.setActive(true);
            }
        }

        item.setView(Some(&view));

        InfoRow { item, left, sparkline, right }
    }

    fn set(&self, left: &str, buckets: Vec<sparkline::BucketInfo>, right: &str) {
        self.left.setStringValue(&NSString::from_str(left));
        self.right.setStringValue(&NSString::from_str(right));
        self.sparkline.set_buckets(buckets);
    }
}

/// DNS row: "DNS" label + click-to-copy IP chip + sparkline + latency.
/// Same right-side column layout as `InfoRow` so sparklines and latencies
/// align across all rows.
struct DnsRow {
    item: Retained<NSMenuItem>,
    label: Retained<NSTextField>,
    chip: Retained<ChipButton>,
    sparkline: Retained<SparklineView>,
    right: Retained<NSTextField>,
}

impl DnsRow {
    fn new(mtm: MainThreadMarker, copy_target: &AnyObject) -> Self {
        let item = NSMenuItem::new(mtm);
        item.setEnabled(true);

        let view = NSView::new(mtm);
        view.setTranslatesAutoresizingMaskIntoConstraints(false);

        let label = NSTextField::labelWithString(ns_string!(""), mtm);
        let right = NSTextField::labelWithString(ns_string!(""), mtm);
        let font = NSFont::menuFontOfSize(0.0);
        label.setFont(Some(&font));
        right.setFont(Some(&font));
        right.setAlignment(NSTextAlignment::Right);
        label.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
        label.setMaximumNumberOfLines(1);
        label.setTranslatesAutoresizingMaskIntoConstraints(false);
        right.setTranslatesAutoresizingMaskIntoConstraints(false);

        let chip = make_chip(mtm, copy_target);
        let sparkline = SparklineView::new(mtm);

        view.addSubview(&label);
        view.addSubview(&chip);
        view.addSubview(&sparkline);
        view.addSubview(&right);

        const H_MARGIN: f64 = 14.0;
        const V_MARGIN: f64 = 3.0;
        const SPACING: f64 = 6.0;
        const LABEL_CHIP_GAP: f64 = 2.0;

        let label_obj: &AnyObject = &label;
        let chip_obj: &AnyObject = &chip;
        let spark_obj: &AnyObject = &sparkline;
        let right_obj: &AnyObject = &right;
        let view_obj: &AnyObject = &view;
        unsafe {
            for c in [
                pin(label_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(chip_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(spark_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(right_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(label_obj, NSLayoutAttribute::Top, view_obj, NSLayoutAttribute::Top, V_MARGIN),
                pin(view_obj, NSLayoutAttribute::Bottom, label_obj, NSLayoutAttribute::Bottom, V_MARGIN),

                // Right column: fixed-width latency pinned to view trailing.
                pin(right_obj, NSLayoutAttribute::Trailing, view_obj, NSLayoutAttribute::Trailing, -H_MARGIN),
                pin_const(right_obj, NSLayoutAttribute::Width, LATENCY_WIDTH),

                // Sparkline: fixed width, pinned immediately left of the right column.
                pin(spark_obj, NSLayoutAttribute::Trailing, right_obj, NSLayoutAttribute::Leading, -SPACING),
                pin_const(spark_obj, NSLayoutAttribute::Width, spark_total_width()),
                pin_const(spark_obj, NSLayoutAttribute::Height, SPARK_HEIGHT),

                // Label + chip on the left; chip may shrink up to the sparkline.
                pin(label_obj, NSLayoutAttribute::Leading, view_obj, NSLayoutAttribute::Leading, H_MARGIN),
                pin(chip_obj, NSLayoutAttribute::Leading, label_obj, NSLayoutAttribute::Trailing, LABEL_CHIP_GAP),
                pin_rel(
                    chip_obj,
                    NSLayoutAttribute::Trailing,
                    NSLayoutRelation::LessThanOrEqual,
                    spark_obj,
                    NSLayoutAttribute::Leading,
                    -SPACING,
                ),
            ] {
                c.setActive(true);
            }
        }

        item.setView(Some(&view));
        DnsRow { item, label, chip, sparkline, right }
    }

    /// `ip: None` renders "DNS: unknown" without a chip; `Some(ip)` shows
    /// "DNS" + a click-to-copy chip.
    fn set(&self, ip: Option<&str>, buckets: Vec<sparkline::BucketInfo>, right: &str) {
        match ip {
            Some(s) => {
                self.label.setStringValue(ns_string!("DNS"));
                set_chip(&self.chip, ip_kind(s), Some(s));
            }
            None => {
                self.label.setStringValue(ns_string!("DNS: unknown"));
                set_chip(&self.chip, ChipKind::V4, None);
            }
        }
        self.right.setStringValue(&NSString::from_str(right));
        self.sparkline.set_buckets(buckets);
    }
}

/// Network-info row that displays the label plus one or two clickable IP
/// "chips" (v4 + v6). Each chip is a click-to-copy button — title is the
/// plain IP, the visual styling (font + color) is set through an attributed
/// title so v4 and v6 are visually distinct and v6 is rendered in a more
/// compact font to fit.
struct NetInfoRow {
    item: Retained<NSMenuItem>,
    label: Retained<NSTextField>,
    v4: Retained<ChipButton>,
    v6: Retained<ChipButton>,
}

#[derive(Clone, Copy)]
enum ChipKind {
    V4,
    V6,
}

impl NetInfoRow {
    fn new(mtm: MainThreadMarker, copy_target: &AnyObject) -> Self {
        let item = NSMenuItem::new(mtm);
        item.setEnabled(true);

        let view = NSView::new(mtm);
        view.setTranslatesAutoresizingMaskIntoConstraints(false);

        let label = NSTextField::labelWithString(ns_string!(""), mtm);
        label.setFont(Some(&NSFont::menuFontOfSize(0.0)));
        label.setTranslatesAutoresizingMaskIntoConstraints(false);

        let v4 = make_chip(mtm, copy_target);
        let v6 = make_chip(mtm, copy_target);

        view.addSubview(&label);
        view.addSubview(&v4);
        view.addSubview(&v6);

        const H_MARGIN: f64 = 14.0;
        const V_MARGIN: f64 = 3.0;
        const SPACING: f64 = 8.0;
        // Fixed label column width so the v4 chip starts at the same x
        // across all IP rows (Local / Router / Public), independent of the
        // label text or the v6 chip width.
        const LABEL_WIDTH: f64 = 56.0;
        // Fixed v4 chip width so the v6 chip starts at the same x across
        // rows, independent of the v4 address length.
        // Wide enough to hold "255.255.255.255" rendered with the chip's
        // leading/trailing space padding at 12pt monospace.
        const V4_WIDTH: f64 = 132.0;

        let label_obj: &AnyObject = &label;
        let v4_obj: &AnyObject = &v4;
        let v6_obj: &AnyObject = &v6;
        let view_obj: &AnyObject = &view;
        unsafe {
            for c in [
                pin(label_obj, NSLayoutAttribute::Leading, view_obj, NSLayoutAttribute::Leading, H_MARGIN),
                pin_const(label_obj, NSLayoutAttribute::Width, LABEL_WIDTH),
                pin(label_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(label_obj, NSLayoutAttribute::Top, view_obj, NSLayoutAttribute::Top, V_MARGIN),
                pin(view_obj, NSLayoutAttribute::Bottom, label_obj, NSLayoutAttribute::Bottom, V_MARGIN),
                pin(v4_obj, NSLayoutAttribute::Leading, label_obj, NSLayoutAttribute::Trailing, SPACING),
                pin_const(v4_obj, NSLayoutAttribute::Width, V4_WIDTH),
                pin(v4_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(v6_obj, NSLayoutAttribute::Leading, v4_obj, NSLayoutAttribute::Trailing, SPACING),
                pin(v6_obj, NSLayoutAttribute::CenterY, view_obj, NSLayoutAttribute::CenterY, 0.0),
                pin(view_obj, NSLayoutAttribute::Trailing, v6_obj, NSLayoutAttribute::Trailing, H_MARGIN),
            ] {
                c.setActive(true);
            }
        }

        item.setView(Some(&view));
        NetInfoRow { item, label, v4, v6 }
    }

    fn set(&self, label: &str, v4: Option<&str>, v6: Option<&str>) {
        self.label.setStringValue(&NSString::from_str(label));
        set_chip(&self.v4, ChipKind::V4, v4);
        set_chip(&self.v6, ChipKind::V6, v6);
    }
}

fn make_chip(mtm: MainThreadMarker, target: &AnyObject) -> Retained<ChipButton> {
    let btn = ChipButton::new(mtm);
    btn.setBordered(false);
    btn.setTitle(ns_string!(""));
    // Left-align the title so the IP addresses line up at the leading edge
    // of the fixed-width chip column instead of being centered.
    btn.setAlignment(NSTextAlignment::Left);
    btn.setTranslatesAutoresizingMaskIntoConstraints(false);
    unsafe {
        btn.setTarget(Some(target));
        btn.setAction(Some(sel!(copyIp:)));
    }
    btn.setHidden(true);
    let tooltip = NSString::from_str("click to copy");
    btn.setToolTip(Some(&tooltip));
    btn
}

fn set_chip(btn: &ChipButton, kind: ChipKind, ip: Option<&str>) {
    match ip {
        Some(s) if !s.is_empty() => {
            btn.setHidden(false);
            // Plain title (used as the pasteboard payload on click).
            btn.setTitle(&NSString::from_str(s));
            // Attributed title is just for display (font + color); the
            // button still reports its plain `title` for copy. Pad the
            // display with spaces so the chip's rounded background has
            // natural horizontal padding around the IP text.
            let padded = format!(" {s} ");
            let attr = chip_attributed_title(kind, &padded);
            btn.setAttributedTitle(&attr);
        }
        _ => btn.setHidden(true),
    }
}

fn ip_kind(ip: &str) -> ChipKind {
    if ip.contains(':') {
        ChipKind::V6
    } else {
        ChipKind::V4
    }
}

fn ip_color(_kind: ChipKind) -> Retained<NSColor> {
    // Same darker blue for both v4 and v6.
    NSColor::colorWithSRGBRed_green_blue_alpha(0.10, 0.35, 0.75, 1.0)
}

fn chip_attributed_title(kind: ChipKind, ip: &str) -> Retained<NSAttributedString> {
    let (font, color) = match kind {
        ChipKind::V4 => (
            NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular }),
            ip_color(kind),
        ),
        ChipKind::V6 => (
            // Smaller monospaced font for v6 — IPv6 addresses are long and
            // we want them to take less horizontal space than v4.
            NSFont::monospacedSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular }),
            ip_color(kind),
        ),
    };
    let key_color: &NSString = unsafe { NSForegroundColorAttributeName };
    let key_font: &NSString = unsafe { NSFontAttributeName };
    let attrs = NSDictionary::from_slices::<NSString>(
        &[key_color, key_font],
        &[color.as_ref() as &AnyObject, font.as_ref() as &AnyObject],
    );
    let s = NSString::from_str(ip);
    unsafe {
        NSAttributedString::initWithString_attributes(
            NSAttributedString::alloc(),
            &s,
            Some(&attrs),
        )
    }
}

unsafe fn pin(
    a: &AnyObject,
    a_attr: NSLayoutAttribute,
    b: &AnyObject,
    b_attr: NSLayoutAttribute,
    constant: f64,
) -> Retained<NSLayoutConstraint> {
    unsafe { pin_rel(a, a_attr, NSLayoutRelation::Equal, b, b_attr, constant) }
}

unsafe fn pin_rel(
    a: &AnyObject,
    a_attr: NSLayoutAttribute,
    relation: NSLayoutRelation,
    b: &AnyObject,
    b_attr: NSLayoutAttribute,
    constant: f64,
) -> Retained<NSLayoutConstraint> {
    unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            a,
            a_attr,
            relation,
            Some(b),
            b_attr,
            1.0,
            constant,
        )
    }
}

/// Constant constraint: `a.attr == constant` (no other item).
unsafe fn pin_const(
    a: &AnyObject,
    a_attr: NSLayoutAttribute,
    constant: f64,
) -> Retained<NSLayoutConstraint> {
    unsafe {
        NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            a,
            a_attr,
            NSLayoutRelation::Equal,
            None,
            NSLayoutAttribute::NotAnAttribute,
            1.0,
            constant,
        )
    }
}

fn format_result(result: &PingResult) -> String {
    match result {
        PingResult::Ok(ms) => format!("{:.0}ms", ms.round()),
        PingResult::Timeout => "timeout".to_string(),
        PingResult::Error(e) => e.clone(),
        PingResult::Pending => "...".to_string(),
    }
}

fn create_menu_item(
    mtm: MainThreadMarker,
    title: &NSString,
    action: Option<Sel>,
) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setTitle(title);
    unsafe { item.setAction(action) };
    item
}

fn install_main_menu(mtm: MainThreadMarker, app: &NSApplication) {
    let main = NSMenu::new(mtm);

    // Edit submenu with nil-targeted standard selectors. AppKit dispatches
    // these through the first responder, so they reach the focused text field
    // even though this is an LSUIElement (accessory) app.
    let edit_item = NSMenuItem::new(mtm);
    edit_item.setTitle(ns_string!("Edit"));
    let edit_menu = NSMenu::new(mtm);
    edit_menu.setTitle(ns_string!("Edit"));

    let entries: &[(&NSString, Sel, &NSString)] = &[
        (ns_string!("Undo"), sel!(undo:), ns_string!("z")),
        (ns_string!("Redo"), sel!(redo:), ns_string!("Z")),
        (ns_string!("Cut"), sel!(cut:), ns_string!("x")),
        (ns_string!("Copy"), sel!(copy:), ns_string!("c")),
        (ns_string!("Paste"), sel!(paste:), ns_string!("v")),
        (ns_string!("Select All"), sel!(selectAll:), ns_string!("a")),
    ];
    for (title, action, key) in entries {
        let item = create_menu_item(mtm, title, Some(*action));
        item.setKeyEquivalent(key);
        edit_menu.addItem(&item);
    }

    edit_item.setSubmenu(Some(&edit_menu));
    main.addItem(&edit_item);

    app.setMainMenu(Some(&main));
}

fn main() {
    let mtm = MainThreadMarker::new().unwrap();

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    install_main_menu(mtm, &app);

    let delegate = AppDelegate::new(mtm);
    let object = ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(object));

    app.run();
}
