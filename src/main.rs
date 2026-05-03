#![deny(unsafe_op_in_unsafe_fn)]

mod ping;

use std::cell::RefCell;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject, Sel};
use objc2::sel;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate, NSMenu, NSMenuItem,
    NSStatusBar, NSStatusItem, NSVariableStatusItemLength,
};
use objc2_foundation::{ns_string, NSNotification, NSObject, NSObjectProtocol, NSString, NSTimer};

use ping::{PingResult, PingTarget};

const TARGETS: &[(&str, &str)] = &[
    ("google.com", "google.com"),
    ("cloudflare.com", "1.1.1.1"),
    ("apple.com", "apple.com"),
];

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
            self.refresh_pings();
            self.update_display(mtm);
            self.schedule_timer(mtm);
        }
    }

    impl AppDelegate {
        #[unsafe(method(timerFired:))]
        unsafe fn timer_fired(&self, _timer: &NSTimer) {
            let mtm = MainThreadMarker::from(self);
            self.refresh_pings();
            self.update_display(mtm);
            self.schedule_timer(mtm);
        }
    }
);

struct AppDelegateIvars {
    status_item: Retained<NSStatusItem>,
    targets: RefCell<Vec<PingTarget>>,
    results: RefCell<Vec<PingResult>>,
}

impl AppDelegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let status_bar = NSStatusBar::systemStatusBar();
        let status_item = status_bar.statusItemWithLength(NSVariableStatusItemLength);

        let targets: Vec<PingTarget> = TARGETS
            .iter()
            .map(|(name, host)| PingTarget {
                name: name.to_string(),
                host: host.to_string(),
            })
            .collect();

        let results = vec![PingResult::Pending; targets.len()];

        let this = Self::alloc(mtm).set_ivars(AppDelegateIvars {
            status_item,
            targets: RefCell::new(targets),
            results: RefCell::new(results),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn refresh_pings(&self) {
        let targets = self.ivars().targets.borrow();
        let mut results = self.ivars().results.borrow_mut();
        for (i, target) in targets.iter().enumerate() {
            results[i] = ping::ping_host(&target.host);
        }
    }

    fn update_display(&self, mtm: MainThreadMarker) {
        let ivars = self.ivars();
        let targets = ivars.targets.borrow();
        let results = ivars.results.borrow();

        // Status bar title: show primary target ping
        if let Some(button) = ivars.status_item.button(mtm) {
            let title = match &results[0] {
                PingResult::Ok(ms) => format!("\u{1F310} {ms:.0}ms"),
                PingResult::Timeout => "\u{1F310} ---".to_string(),
                PingResult::Error(_) => "\u{1F310} err".to_string(),
                PingResult::Pending => "\u{1F310} ...".to_string(),
            };
            button.setTitle(&NSString::from_str(&title));
        }

        // Build menu
        let menu = NSMenu::new(mtm);
        menu.setAutoenablesItems(false);

        for (target, result) in targets.iter().zip(results.iter()) {
            let text = match result {
                PingResult::Ok(ms) => format!("{}: {ms:.1}ms", target.name),
                PingResult::Timeout => format!("{}: timeout", target.name),
                PingResult::Error(e) => format!("{}: {e}", target.name),
                PingResult::Pending => format!("{}: ...", target.name),
            };
            let item = NSMenuItem::new(mtm);
            item.setTitle(&NSString::from_str(&text));
            item.setEnabled(false);
            menu.addItem(&item);
        }

        menu.addItem(&NSMenuItem::separatorItem(mtm));

        let quit_item = create_menu_item(mtm, ns_string!("Quit"), Some(sel!(terminate:)));
        quit_item.setKeyEquivalent(ns_string!("q"));
        menu.addItem(&quit_item);

        ivars.status_item.setMenu(Some(&menu));
    }

    fn schedule_timer(&self, _mtm: MainThreadMarker) {
        unsafe {
            NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                10.0,
                self as &AnyObject,
                sel!(timerFired:),
                None,
                false,
            );
        }
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

fn main() {
    let mtm = MainThreadMarker::new().unwrap();

    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let delegate = AppDelegate::new(mtm);
    let object = ProtocolObject::from_ref(&*delegate);
    app.setDelegate(Some(object));

    app.run();
}
