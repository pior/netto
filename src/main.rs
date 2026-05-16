#![deny(unsafe_op_in_unsafe_fn)]

mod netinfo;
mod ping;
mod preferences;
mod settings;

use std::cell::{Cell, RefCell};
use std::time::Duration;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::sel;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenu, NSMenuDelegate,
    NSMenuItem, NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{
    ns_string, NSNotification, NSObject, NSObjectProtocol, NSRunLoop, NSRunLoopCommonModes,
    NSString, NSTimer,
};

use netinfo::NetInfo;
use ping::{PingResult, PingService, PingTarget};

const DEFAULT_TARGETS: &[(&str, &str)] = &[
    ("google.com", "google.com"),
    ("cloudflare.com", "1.1.1.1"),
    ("apple.com", "apple.com"),
];

const SLOW_INTERVAL: Duration = Duration::from_secs(10);
const FAST_INTERVAL: Duration = Duration::from_secs(1);

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
            self.reschedule_timer(mtm, SLOW_INTERVAL);
        }
    }

    unsafe impl NSMenuDelegate for AppDelegate {
        #[unsafe(method(menuWillOpen:))]
        fn menu_will_open(&self, _menu: &NSMenu) {
            let mtm = MainThreadMarker::from(self);
            self.ivars().menu_open.set(true);
            self.ivars().ping_service.set_interval(FAST_INTERVAL);
            self.reschedule_timer(mtm, FAST_INTERVAL);
            self.tick(mtm);
        }

        #[unsafe(method(menuDidClose:))]
        fn menu_did_close(&self, _menu: &NSMenu) {
            let mtm = MainThreadMarker::from(self);
            self.ivars().menu_open.set(false);
            self.ivars().ping_service.set_interval(SLOW_INTERVAL);
            self.reschedule_timer(mtm, SLOW_INTERVAL);
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

        #[unsafe(method(reloadTargets:))]
        unsafe fn reload_targets(&self, _sender: &AnyObject) {
            let mtm = MainThreadMarker::from(self);
            if let Some(targets) = settings::load_targets() {
                *self.ivars().targets.borrow_mut() = targets;
            }
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
    target_items: RefCell<Vec<Retained<NSMenuItem>>>,
    local_item: Retained<NSMenuItem>,
    router_item: Retained<NSMenuItem>,
    dns_items: RefCell<Vec<Retained<NSMenuItem>>>,
    settings_item: Retained<NSMenuItem>,
    quit_item: Retained<NSMenuItem>,
    menu_open: Cell<bool>,
    refresh_timer: RefCell<Option<Retained<NSTimer>>>,
    prefs_controller: RefCell<Option<Retained<NSObject>>>,
}

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        let targets = settings::load_targets().unwrap_or_else(|| {
            let defaults: Vec<PingTarget> = DEFAULT_TARGETS
                .iter()
                .map(|(name, host)| PingTarget {
                    name: name.to_string(),
                    host: host.to_string(),
                })
                .collect();
            settings::save_targets(&defaults);
            defaults
        });

        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);

        let local_item = info_item(mtm);
        let router_item = info_item(mtm);

        let settings_item =
            create_menu_item(mtm, ns_string!("Settings\u{2026}"), Some(sel!(showPreferences:)));
        settings_item.setEnabled(true);

        let quit_item = create_menu_item(mtm, ns_string!("Quit"), Some(sel!(terminate:)));
        quit_item.setKeyEquivalent(ns_string!("q"));

        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars {
            status_item,
            targets: RefCell::new(targets),
            ping_service: PingService::new(SLOW_INTERVAL),
            net_info: RefCell::new(NetInfo::default()),
            menu,
            target_items: RefCell::new(Vec::new()),
            local_item,
            router_item,
            dns_items: RefCell::new(Vec::new()),
            settings_item,
            quit_item,
            menu_open: Cell::new(false),
            refresh_timer: RefCell::new(None),
            prefs_controller: RefCell::new(None),
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
        let structure_changed = prev_info.router_ip.is_some() != new_info.router_ip.is_some()
            || prev_info.dns_ips.len() != new_info.dns_ips.len()
            || ivars.target_items.borrow().len() != ivars.targets.borrow().len();

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

    fn host_list(&self, info: &NetInfo) -> Vec<String> {
        let targets = self.ivars().targets.borrow();
        let mut hosts: Vec<String> = targets.iter().map(|t| t.host.clone()).collect();
        if let Some(ip) = &info.router_ip {
            hosts.push(ip.clone());
        }
        hosts.extend(info.dns_ips.iter().cloned());
        hosts
    }

    fn rebuild_menu_structure(&self, mtm: MainThreadMarker, info: &NetInfo) {
        let ivars = self.ivars();
        let menu = &ivars.menu;
        menu.removeAllItems();

        // Resize target_items to match current targets.
        let target_count = ivars.targets.borrow().len();
        {
            let mut items = ivars.target_items.borrow_mut();
            items.resize_with(target_count, || info_item(mtm));
            for item in items.iter() {
                menu.addItem(item);
            }
        }

        menu.addItem(&NSMenuItem::separatorItem(mtm));
        menu.addItem(&ivars.local_item);
        menu.addItem(&ivars.router_item);

        // Resize dns_items to match current DNS count (or one placeholder if empty).
        {
            let mut items = ivars.dns_items.borrow_mut();
            let target_count = info.dns_ips.len().max(1);
            items.resize_with(target_count, || info_item(mtm));
            for item in items.iter() {
                menu.addItem(item);
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

        // Snapshot layout: [targets..., router?, dns...]
        let target_count = targets.len();
        let target_results = &snapshot.get(..target_count).unwrap_or(&[]);
        let mut idx = target_count;
        let router_result = if info.router_ip.is_some() {
            let r = snapshot.get(idx).cloned().unwrap_or(PingResult::Pending);
            idx += 1;
            Some(r)
        } else {
            None
        };
        let dns_results: Vec<PingResult> = snapshot.get(idx..).unwrap_or(&[]).to_vec();

        // Status bar.
        if let Some(button) = ivars.status_item.button(mtm) {
            let title = if targets.is_empty() {
                "\u{1F310} --".to_string()
            } else {
                match target_results.first().cloned().unwrap_or(PingResult::Pending) {
                    PingResult::Ok(ms) => format!("\u{1F310} {ms:.0}ms"),
                    PingResult::Timeout => "\u{1F310} ---".to_string(),
                    PingResult::Error(_) => "\u{1F310} err".to_string(),
                    PingResult::Pending => "\u{1F310} ...".to_string(),
                }
            };
            button.setTitle(&NSString::from_str(&title));
        }

        // Target items.
        let target_items = ivars.target_items.borrow();
        for (i, target) in targets.iter().enumerate() {
            if let Some(item) = target_items.get(i) {
                let result = target_results
                    .get(i)
                    .cloned()
                    .unwrap_or(PingResult::Pending);
                let text = format!("{}: {}", target.name, format_result(&result));
                item.setTitle(&NSString::from_str(&text));
            }
        }

        // Local IP.
        let local_text = format!(
            "Local IP: {}",
            info.local_ip.as_deref().unwrap_or("unknown")
        );
        ivars.local_item.setTitle(&NSString::from_str(&local_text));

        // Router.
        let router_text = match (info.router_ip.as_deref(), router_result) {
            (Some(ip), Some(r)) => format!("Router {ip}: {}", format_result(&r)),
            _ => "Router: unknown".to_string(),
        };
        ivars.router_item.setTitle(&NSString::from_str(&router_text));

        // DNS items.
        let dns_items = ivars.dns_items.borrow();
        if info.dns_ips.is_empty() {
            if let Some(item) = dns_items.first() {
                item.setTitle(ns_string!("DNS: unknown"));
            }
        } else {
            for (i, ip) in info.dns_ips.iter().enumerate() {
                if let Some(item) = dns_items.get(i) {
                    let result = dns_results
                        .get(i)
                        .cloned()
                        .unwrap_or(PingResult::Pending);
                    let text = format!("DNS {ip}: {}", format_result(&result));
                    item.setTitle(&NSString::from_str(&text));
                }
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
                let _: () = msg_send![obj, reloadTargets: std::ptr::null::<AnyObject>()];
            }
        });

        let controller = {
            let targets = ivars.targets.borrow();
            preferences::PrefsController::new(mtm, &targets, on_save)
        };

        controller.show();
        *ivars.prefs_controller.borrow_mut() = Some(Retained::into_super(controller));
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

fn format_result(result: &PingResult) -> String {
    match result {
        PingResult::Ok(ms) => format!("{ms:.1}ms"),
        PingResult::Timeout => "timeout".to_string(),
        PingResult::Error(e) => e.clone(),
        PingResult::Pending => "...".to_string(),
    }
}

fn info_item(mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let item = NSMenuItem::new(mtm);
    item.setEnabled(true);
    item
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

fn main() {
    let mtm = MainThreadMarker::new().unwrap();

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let delegate = AppDelegate::new(mtm);
    let object = ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(object));

    app.run();
}
