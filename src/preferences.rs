#![allow(clippy::too_many_lines)]

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSButtonType, NSColor, NSFont, NSLayoutAttribute,
    NSLayoutRelation, NSStackView, NSStackViewDistribution, NSTextField, NSTextFieldBezelStyle,
    NSUserInterfaceLayoutOrientation, NSView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    NSWorkspace,
};
use objc2_app_kit::{NSControlStateValueOff, NSControlStateValueOn};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{ns_string, NSInteger, NSNotification, NSObject, NSObjectProtocol, NSString, NSURL};

use crate::ping::PingTarget;
use crate::settings;

const IDX_HOST: usize = 0;
const ROW_SUBVIEW_COUNT: usize = 2;
const MAX_ENTRY_ROWS: usize = 15;
const PREF_WINDOW_WIDTH: f64 = 480.0;
const CONTENT_PADDING: f64 = 16.0;
const SECTION_GAP: f64 = 10.0;
const GITHUB_URL: &str = "https://github.com/pior/netto";

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = PrefsControllerIvars]
    pub struct PrefsController;

    unsafe impl NSObjectProtocol for PrefsController {}

    unsafe impl NSWindowDelegate for PrefsController {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _notification: &NSNotification) {
            let mtm = MainThreadMarker::from(self);
            let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
            app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Accessory);
        }
    }

    impl PrefsController {
        #[unsafe(method(controlTextDidChange:))]
        fn control_text_did_change(&self, _notification: &NSNotification) {
            self.do_save();
        }

        #[unsafe(method(controlTextDidEndEditing:))]
        fn control_text_did_end_editing(&self, _notification: &NSNotification) {
            self.do_save();
        }

        #[unsafe(method(addEntry:))]
        unsafe fn add_entry(&self, _sender: &AnyObject) {
            let mtm = MainThreadMarker::from(self);
            self.do_add_entry(mtm);
        }

        #[unsafe(method(removeEntry:))]
        unsafe fn remove_entry(&self, sender: &NSButton) {
            let mtm = MainThreadMarker::from(self);
            self.do_remove_entry(mtm, sender);
        }

        #[unsafe(method(toggleLaunchAtLogin:))]
        unsafe fn toggle_launch_at_login(&self, sender: &NSButton) {
            self.do_toggle_launch_at_login(sender);
        }

        #[unsafe(method(openGitHub:))]
        unsafe fn open_github(&self, _sender: &AnyObject) {
            if let Some(url) = NSURL::URLWithString(&NSString::from_str(GITHUB_URL)) {
                NSWorkspace::sharedWorkspace().openURL(&url);
            }
        }

        #[unsafe(method(showWindow))]
        unsafe fn show_window_objc(&self) {
            self.show();
        }
    }
);

pub struct PrefsControllerIvars {
    window: Retained<NSWindow>,
    rows_stack: Retained<NSStackView>,
    on_save: Box<dyn Fn()>,
}

impl PrefsController {
    pub fn new(
        mtm: MainThreadMarker,
        targets: &[PingTarget],
        on_save: Box<dyn Fn()>,
    ) -> Retained<Self> {
        let frame = CGRect::new(
            CGPoint::new(0.0, 0.0),
            CGSize::new(PREF_WINDOW_WIDTH, 300.0),
        );
        let style = NSWindowStyleMask::Titled | NSWindowStyleMask::Closable;

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                style,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setTitle(ns_string!("Netto Settings"));
        window.center();
        unsafe { window.setReleasedWhenClosed(false) };

        let rows_stack = NSStackView::new(mtm);
        rows_stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
        rows_stack.setSpacing(8.0);
        rows_stack.setAlignment(NSLayoutAttribute::Width);
        rows_stack.setTranslatesAutoresizingMaskIntoConstraints(false);

        let this = Self::alloc(mtm).set_ivars(PrefsControllerIvars {
            window: window.clone(),
            rows_stack: rows_stack.clone(),
            on_save,
        });
        let controller: Retained<Self> = unsafe { msg_send![super(this), init] };

        let delegate: &ProtocolObject<dyn NSWindowDelegate> = ProtocolObject::from_ref(&*controller);
        window.setDelegate(Some(delegate));

        let add_row = create_add_button_row(mtm, &controller);
        rows_stack.addArrangedSubview(&add_row);

        for target in targets {
            controller.do_add_entry_with(mtm, &target.host);
        }

        let launch_checkbox = create_launch_at_login_checkbox(mtm, &controller);
        launch_checkbox.setTranslatesAutoresizingMaskIntoConstraints(false);
        let footer = create_footer(mtm, &controller);
        footer.setTranslatesAutoresizingMaskIntoConstraints(false);

        let content_view = NSView::new(mtm);
        content_view.setTranslatesAutoresizingMaskIntoConstraints(false);
        window.setContentView(Some(&content_view));

        content_view.addSubview(&rows_stack);
        content_view.addSubview(&launch_checkbox);
        content_view.addSubview(&footer);

        unsafe {
            let leading = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*rows_stack, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
                Some(&*content_view), NSLayoutAttribute::Leading, 1.0, CONTENT_PADDING,
            );
            let trailing = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*rows_stack, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
                Some(&*content_view), NSLayoutAttribute::Trailing, 1.0, -CONTENT_PADDING,
            );
            let top = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*rows_stack, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
                Some(&*content_view), NSLayoutAttribute::Top, 1.0, CONTENT_PADDING,
            );
            let cb_center = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*launch_checkbox, NSLayoutAttribute::CenterX, NSLayoutRelation::Equal,
                Some(&*content_view), NSLayoutAttribute::CenterX, 1.0, 0.0,
            );
            let cb_top = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*launch_checkbox, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
                Some(&*rows_stack), NSLayoutAttribute::Bottom, 1.0, SECTION_GAP,
            );
            let footer_top = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*footer, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
                Some(&*launch_checkbox), NSLayoutAttribute::Bottom, 1.0, SECTION_GAP,
            );
            let footer_bottom = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*footer, NSLayoutAttribute::Bottom, NSLayoutRelation::Equal,
                Some(&*content_view), NSLayoutAttribute::Bottom, 1.0, -CONTENT_PADDING,
            );
            let footer_leading = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*footer, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
                Some(&*content_view), NSLayoutAttribute::Leading, 1.0, CONTENT_PADDING,
            );
            let footer_trailing = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*footer, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
                Some(&*content_view), NSLayoutAttribute::Trailing, 1.0, -CONTENT_PADDING,
            );
            leading.setActive(true);
            trailing.setActive(true);
            top.setActive(true);
            cb_center.setActive(true);
            cb_top.setActive(true);
            footer_top.setActive(true);
            footer_bottom.setActive(true);
            footer_leading.setActive(true);
            footer_trailing.setActive(true);
        }

        controller.resize_to_fit_entries();
        controller.update_add_button_state();

        controller
    }

    fn do_toggle_launch_at_login(&self, sender: &NSButton) {
        use objc2_service_management::{SMAppService, SMAppServiceStatus};

        let service = unsafe { SMAppService::mainAppService() };
        let enabled = sender.state() == NSControlStateValueOn;

        if enabled {
            if let Err(err) = unsafe { service.registerAndReturnError() } {
                eprintln!("Failed to enable launch at login: {err}");
                sender.setState(NSControlStateValueOff);
            }
        } else {
            if let Err(err) = unsafe { service.unregisterAndReturnError() } {
                eprintln!("Failed to disable launch at login: {err}");
                let status = unsafe { service.status() };
                if status == SMAppServiceStatus::Enabled {
                    sender.setState(NSControlStateValueOn);
                }
            }
        }
    }

    pub fn show(&self) {
        let mtm = MainThreadMarker::from(self);
        let app = objc2_app_kit::NSApplication::sharedApplication(mtm);
        app.setActivationPolicy(objc2_app_kit::NSApplicationActivationPolicy::Regular);
        app.activate();

        let window = &self.ivars().window;
        window.deminiaturize(None);
        window.makeKeyAndOrderFront(None);
        window.orderFrontRegardless();
    }

    fn do_add_entry(&self, mtm: MainThreadMarker) {
        if self.entry_count() >= MAX_ENTRY_ROWS {
            return;
        }
        self.do_add_entry_with(mtm, "");
        self.do_save();
    }

    fn do_add_entry_with(&self, mtm: MainThreadMarker, host: &str) {
        if self.entry_count() >= MAX_ENTRY_ROWS {
            return;
        }

        let ivars = self.ivars();
        let delegate: &AnyObject = self;

        let row_stack = NSStackView::new(mtm);
        row_stack.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
        row_stack.setSpacing(8.0);
        row_stack.setDistribution(NSStackViewDistribution::Fill);

        let host_field = create_text_field(mtm, "Host / IP", host);
        host_field.setTranslatesAutoresizingMaskIntoConstraints(false);
        unsafe {
            let _: () = msg_send![&host_field, setDelegate: delegate];
        }

        let delete_btn = unsafe {
            NSButton::buttonWithTitle_target_action(
                ns_string!("\u{2715}"),
                Some(delegate),
                Some(sel!(removeEntry:)),
                mtm,
            )
        };
        delete_btn.setBezelStyle(objc2_app_kit::NSBezelStyle::SmallSquare);
        delete_btn.setBordered(false);
        delete_btn.setTranslatesAutoresizingMaskIntoConstraints(false);
        add_width_constraint(&delete_btn, 24.0);

        row_stack.addArrangedSubview(&host_field);
        row_stack.addArrangedSubview(&delete_btn);

        let count = ivars.rows_stack.arrangedSubviews().count();
        if count > 0 {
            ivars
                .rows_stack
                .insertArrangedSubview_atIndex(&row_stack, (count - 1) as NSInteger);
        } else {
            ivars.rows_stack.addArrangedSubview(&row_stack);
        }

        self.resize_to_fit_entries();
        self.update_add_button_state();
    }

    fn do_remove_entry(&self, _mtm: MainThreadMarker, sender: &NSButton) {
        let ivars = self.ivars();
        let subviews = ivars.rows_stack.arrangedSubviews();

        for i in 0..subviews.count() {
            let row_view: Retained<NSView> = subviews.objectAtIndex(i).downcast().unwrap();
            if unsafe { row_view.isDescendantOf(&sender.superview().unwrap()) } {
                ivars.rows_stack.removeArrangedSubview(&row_view);
                row_view.removeFromSuperview();
                break;
            }
        }

        self.resize_to_fit_entries();
        self.update_add_button_state();
        self.do_save();
    }

    fn entry_count(&self) -> usize {
        self.ivars()
            .rows_stack
            .arrangedSubviews()
            .count()
            .saturating_sub(1)
    }

    fn resize_to_fit_entries(&self) {
        let window = &self.ivars().window;
        if let Some(content_view) = window.contentView() {
            content_view.layoutSubtreeIfNeeded();
            let fitting = content_view.fittingSize();
            window.setContentSize(CGSize::new(PREF_WINDOW_WIDTH, fitting.height));
        }
    }

    fn update_add_button_state(&self) {
        let subviews = self.ivars().rows_stack.arrangedSubviews();
        if subviews.count() == 0 {
            return;
        }

        let Ok(add_row) = subviews
            .objectAtIndex(subviews.count() - 1)
            .downcast::<NSStackView>()
        else {
            return;
        };
        let add_row_subviews = add_row.arrangedSubviews();
        if add_row_subviews.count() < 2 {
            return;
        }
        let Ok(add_button) = add_row_subviews.objectAtIndex(1).downcast::<NSButton>() else {
            return;
        };
        add_button.setEnabled(self.entry_count() < MAX_ENTRY_ROWS);
    }

    fn do_save(&self) {
        let ivars = self.ivars();
        let mut targets = Vec::new();

        let subviews = ivars.rows_stack.arrangedSubviews();
        for i in 0..subviews.count() {
            let Ok(row_view) = subviews.objectAtIndex(i).downcast::<NSStackView>() else {
                continue;
            };
            let row_subviews = row_view.arrangedSubviews();
            if row_subviews.count() < ROW_SUBVIEW_COUNT {
                continue;
            }

            let host_field: Retained<NSTextField> =
                row_subviews.objectAtIndex(IDX_HOST).downcast().unwrap();

            let host = host_field.stringValue().to_string();
            if host.is_empty() {
                continue;
            }

            targets.push(PingTarget { host });
        }

        settings::save_targets(&targets);
        (ivars.on_save)();
    }
}

fn add_width_constraint(view: &NSView, width: f64) {
    unsafe {
        let constraint =
            objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                view,
                NSLayoutAttribute::Width,
                NSLayoutRelation::Equal,
                None,
                NSLayoutAttribute::NotAnAttribute,
                1.0,
                width,
            );
        view.addConstraint(&*constraint);
    }
}

fn create_footer(mtm: MainThreadMarker, target: &PrefsController) -> Retained<NSStackView> {
    let footer = NSStackView::new(mtm);
    footer.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    footer.setSpacing(8.0);
    footer.setDistribution(NSStackViewDistribution::Fill);

    let font = NSFont::systemFontOfSize(NSFont::smallSystemFontSize());
    let secondary = NSColor::secondaryLabelColor();

    let version_label = format!("Netto v{}", env!("CARGO_PKG_VERSION"));
    let name = NSTextField::labelWithString(&NSString::from_str(&version_label), mtm);
    name.setFont(Some(&font));
    name.setTextColor(Some(&secondary));

    let link = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("github.com/pior/netto"),
            Some(target as &AnyObject),
            Some(sel!(openGitHub:)),
            mtm,
        )
    };
    link.setBordered(false);
    link.setFont(Some(&font));
    link.setContentTintColor(Some(&NSColor::linkColor()));
    link.setToolTip(Some(&NSString::from_str(GITHUB_URL)));

    let spacer = NSView::new(mtm);
    spacer.setTranslatesAutoresizingMaskIntoConstraints(false);

    footer.addArrangedSubview(&name);
    footer.addArrangedSubview(&link);
    footer.addArrangedSubview(&spacer);
    footer
}

fn create_text_field(
    mtm: MainThreadMarker,
    placeholder: &str,
    value: &str,
) -> Retained<NSTextField> {
    let field = NSTextField::new(mtm);
    field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    field.setStringValue(&NSString::from_str(value));
    field.setEditable(true);
    field.setBezeled(true);
    field.setBezelStyle(NSTextFieldBezelStyle::RoundedBezel);
    field
}

fn create_launch_at_login_checkbox(
    mtm: MainThreadMarker,
    target: &PrefsController,
) -> Retained<NSButton> {
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    let target_obj: &AnyObject = target;
    let btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("Launch at Login"),
            Some(target_obj),
            Some(sel!(toggleLaunchAtLogin:)),
            mtm,
        )
    };
    btn.setButtonType(NSButtonType::Switch);

    let service = unsafe { SMAppService::mainAppService() };
    let status = unsafe { service.status() };
    btn.setState(if status == SMAppServiceStatus::Enabled {
        NSControlStateValueOn
    } else {
        NSControlStateValueOff
    });

    btn
}

fn create_add_button_row(mtm: MainThreadMarker, target: &PrefsController) -> Retained<NSStackView> {
    let target_obj: &AnyObject = target;
    let btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            ns_string!("\u{ff0b}"),
            Some(target_obj),
            Some(sel!(addEntry:)),
            mtm,
        )
    };
    btn.setBezelStyle(objc2_app_kit::NSBezelStyle::SmallSquare);
    btn.setBordered(false);
    btn.setTranslatesAutoresizingMaskIntoConstraints(false);
    add_width_constraint(&btn, 24.0);

    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);

    let spacer = NSView::new(mtm);
    spacer.setTranslatesAutoresizingMaskIntoConstraints(false);

    row.addArrangedSubview(&spacer);
    row.addArrangedSubview(&btn);

    row
}
