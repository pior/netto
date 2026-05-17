#![allow(clippy::too_many_lines)]

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::sel;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2::runtime::ProtocolObject;
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSButtonType, NSColor, NSFont, NSLayoutAttribute,
    NSLayoutRelation, NSSlider, NSStackView, NSStackViewDistribution, NSTextAlignment,
    NSTextField, NSTextFieldBezelStyle, NSUserInterfaceLayoutOrientation, NSView, NSWindow,
    NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
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
const SECTION_GAP: f64 = 22.0;
const SLIDER_ROW_SPACING: f64 = 14.0;
const SLIDER_WIDTH: f64 = 260.0;
const TICKS_TOP_GAP: f64 = 2.0;
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

        #[unsafe(method(sliderChanged:))]
        fn slider_changed(&self, _sender: &NSSlider) {
            self.do_save_prefs();
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
    slow_slider: Retained<NSSlider>,
    fast_slider: Retained<NSSlider>,
    tolerance_slider: Retained<NSSlider>,
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

        let prefs = settings::load_prefs();
        let slow_slider = make_stepped_slider(mtm, settings::SLOW_STEPS, prefs.slow_secs);
        let fast_slider = make_stepped_slider(mtm, settings::FAST_STEPS, prefs.fast_secs);
        let tolerance_slider =
            make_stepped_slider(mtm, settings::TOLERANCE_STEPS, prefs.tolerance_ms);

        let this = Self::alloc(mtm).set_ivars(PrefsControllerIvars {
            window: window.clone(),
            rows_stack: rows_stack.clone(),
            slow_slider: slow_slider.clone(),
            fast_slider: fast_slider.clone(),
            tolerance_slider: tolerance_slider.clone(),
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

        // Wire slider actions now that the controller exists.
        let controller_obj: &AnyObject = &controller;
        for s in [&slow_slider, &fast_slider, &tolerance_slider] {
            unsafe {
                s.setTarget(Some(controller_obj));
                s.setAction(Some(sel!(sliderChanged:)));
            }
        }

        let prefs_section = create_prefs_section(
            mtm,
            &slow_slider,
            &fast_slider,
            &tolerance_slider,
        );
        prefs_section.setTranslatesAutoresizingMaskIntoConstraints(false);

        let launch_checkbox = create_launch_at_login_checkbox(mtm, &controller);
        launch_checkbox.setTranslatesAutoresizingMaskIntoConstraints(false);
        let footer = create_footer(mtm, &controller);
        footer.setTranslatesAutoresizingMaskIntoConstraints(false);

        let content_view = NSView::new(mtm);
        content_view.setTranslatesAutoresizingMaskIntoConstraints(false);
        window.setContentView(Some(&content_view));

        content_view.addSubview(&rows_stack);
        content_view.addSubview(&prefs_section);
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
            let prefs_leading = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*prefs_section, NSLayoutAttribute::Leading, NSLayoutRelation::Equal,
                Some(&*content_view), NSLayoutAttribute::Leading, 1.0, CONTENT_PADDING,
            );
            let prefs_trailing = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*prefs_section, NSLayoutAttribute::Trailing, NSLayoutRelation::Equal,
                Some(&*content_view), NSLayoutAttribute::Trailing, 1.0, -CONTENT_PADDING,
            );
            let prefs_top = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*prefs_section, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
                Some(&*rows_stack), NSLayoutAttribute::Bottom, 1.0, SECTION_GAP,
            );
            let cb_top = objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
                &*launch_checkbox, NSLayoutAttribute::Top, NSLayoutRelation::Equal,
                Some(&*prefs_section), NSLayoutAttribute::Bottom, 1.0, SECTION_GAP,
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
            prefs_leading.setActive(true);
            prefs_trailing.setActive(true);
            prefs_top.setActive(true);
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

    fn do_save_prefs(&self) {
        let ivars = self.ivars();
        // All three sliders are 0..N-1 indices into the step arrays.
        let slow = step_value(&ivars.slow_slider, settings::SLOW_STEPS, settings::SLOW_DEFAULT);
        let fast = step_value(&ivars.fast_slider, settings::FAST_STEPS, settings::FAST_DEFAULT);
        let tolerance_ms = step_value(
            &ivars.tolerance_slider,
            settings::TOLERANCE_STEPS,
            settings::TOLERANCE_DEFAULT,
        );
        settings::save_prefs(&settings::AppPrefs {
            slow_secs: slow,
            fast_secs: fast,
            tolerance_ms,
        });
        (ivars.on_save)();
    }

    fn do_save(&self) {
        let ivars = self.ivars();
        let mut targets = Vec::new();

        let subviews = ivars.rows_stack.arrangedSubviews();
        // The last arranged subview is the add-button row (spacer + button),
        // not an entry row — skip it.
        let entry_count = subviews.count().saturating_sub(1);
        for i in 0..entry_count {
            let Ok(row_view) = subviews.objectAtIndex(i).downcast::<NSStackView>() else {
                continue;
            };
            let row_subviews = row_view.arrangedSubviews();
            if row_subviews.count() < ROW_SUBVIEW_COUNT {
                continue;
            }

            let Ok(host_field) = row_subviews
                .objectAtIndex(IDX_HOST)
                .downcast::<NSTextField>()
            else {
                continue;
            };

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

/// Stepped slider: integer index 0..steps.len()-1 with one tick per step,
/// snapping to ticks only. Initial position is the index of the step
/// closest to `value` (which is already snapped on `load_prefs`).
fn make_stepped_slider(
    mtm: MainThreadMarker,
    steps: &[f64],
    value: f64,
) -> Retained<NSSlider> {
    let n = steps.len().max(1);
    let slider = NSSlider::new(mtm);
    slider.setMinValue(0.0);
    slider.setMaxValue((n - 1) as f64);
    slider.setNumberOfTickMarks(n as isize);
    slider.setAllowsTickMarkValuesOnly(true);
    let idx = steps
        .iter()
        .enumerate()
        .min_by(|(_, a), (_, b)| {
            (*a - value)
                .abs()
                .partial_cmp(&(*b - value).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0);
    slider.setDoubleValue(idx as f64);
    slider.setContinuous(true);
    slider.setTranslatesAutoresizingMaskIntoConstraints(false);
    slider
}

/// Read the current step value from a stepped slider, falling back to
/// `default` if the index is somehow out of range.
fn step_value(slider: &NSSlider, steps: &[f64], default: f64) -> f64 {
    let idx = slider.doubleValue().round() as isize;
    if idx < 0 {
        return *steps.first().unwrap_or(&default);
    }
    steps.get(idx as usize).copied().unwrap_or(default)
}

fn create_prefs_section(
    mtm: MainThreadMarker,
    slow_slider: &NSSlider,
    fast_slider: &NSSlider,
    tolerance_slider: &NSSlider,
) -> Retained<NSStackView> {
    let stack = NSStackView::new(mtm);
    stack.setOrientation(NSUserInterfaceLayoutOrientation::Vertical);
    stack.setSpacing(SLIDER_ROW_SPACING);
    stack.setAlignment(NSLayoutAttribute::Width);

    let slow_ticks: Vec<String> = settings::SLOW_STEPS.iter().map(|v| format_secs(*v)).collect();
    let fast_ticks: Vec<String> = settings::FAST_STEPS.iter().map(|v| format_secs(*v)).collect();
    let tolerance_ticks: Vec<String> = settings::TOLERANCE_STEPS
        .iter()
        .map(|v| format!("{:.0}", v))
        .collect();

    stack.addArrangedSubview(&slider_row(
        mtm,
        "Background refresh",
        slow_slider,
        &slow_ticks,
    ));
    stack.addArrangedSubview(&slider_row(mtm, "Open refresh", fast_slider, &fast_ticks));
    stack.addArrangedSubview(&slider_row(
        mtm,
        "Latency tolerance (ms)",
        tolerance_slider,
        &tolerance_ticks,
    ));
    stack
}

fn format_secs(v: f64) -> String {
    if (v - v.round()).abs() < 1e-6 {
        format!("{:.0}s", v.round())
    } else {
        format!("{v}s")
    }
}

/// One slider row: the title sits at the left edge of the section, the
/// slider is pinned to the right edge with a fixed width (so all sliders
/// line up), and the tick-value row sits underneath the slider, sharing
/// its width and right edge.
fn slider_row(
    mtm: MainThreadMarker,
    title: &str,
    slider: &NSSlider,
    tick_labels: &[String],
) -> Retained<NSView> {
    let container = NSView::new(mtm);
    container.setTranslatesAutoresizingMaskIntoConstraints(false);

    let title_label = NSTextField::labelWithString(&NSString::from_str(title), mtm);
    title_label.setFont(Some(&NSFont::systemFontOfSize(NSFont::smallSystemFontSize())));
    title_label.setAlignment(NSTextAlignment::Left);
    title_label.setTranslatesAutoresizingMaskIntoConstraints(false);

    let ticks_row = make_tick_labels_row(mtm, tick_labels);
    ticks_row.setTranslatesAutoresizingMaskIntoConstraints(false);

    container.addSubview(&title_label);
    container.addSubview(slider);
    container.addSubview(&ticks_row);

    let title_obj: &AnyObject = &title_label;
    let slider_obj: &AnyObject = slider;
    let ticks_obj: &AnyObject = &ticks_row;
    let container_obj: &AnyObject = &container;
    unsafe {
        for c in [
            // Title: left edge of container, vertically centered with slider.
            pin_two(title_obj, NSLayoutAttribute::Leading, container_obj, NSLayoutAttribute::Leading, 0.0),
            pin_two(title_obj, NSLayoutAttribute::CenterY, slider_obj, NSLayoutAttribute::CenterY, 0.0),
            // Title may not overrun the slider column.
            pin_two_rel(title_obj, NSLayoutAttribute::Trailing, NSLayoutRelation::LessThanOrEqual,
                slider_obj, NSLayoutAttribute::Leading, -8.0),
            // Slider: right edge, fixed width, at the top of the container.
            pin_two(slider_obj, NSLayoutAttribute::Trailing, container_obj, NSLayoutAttribute::Trailing, 0.0),
            pin_const(slider_obj, NSLayoutAttribute::Width, SLIDER_WIDTH),
            pin_two(slider_obj, NSLayoutAttribute::Top, container_obj, NSLayoutAttribute::Top, 0.0),
            // Ticks: same right edge and width as the slider, just below it.
            pin_two(ticks_obj, NSLayoutAttribute::Trailing, container_obj, NSLayoutAttribute::Trailing, 0.0),
            pin_const(ticks_obj, NSLayoutAttribute::Width, SLIDER_WIDTH),
            pin_two(ticks_obj, NSLayoutAttribute::Top, slider_obj, NSLayoutAttribute::Bottom, TICKS_TOP_GAP),
            // Container hugs the ticks row's bottom edge.
            pin_two(container_obj, NSLayoutAttribute::Bottom, ticks_obj, NSLayoutAttribute::Bottom, 0.0),
        ] {
            c.setActive(true);
        }
    }

    container
}

unsafe fn pin_two(
    a: &AnyObject,
    aa: NSLayoutAttribute,
    b: &AnyObject,
    ba: NSLayoutAttribute,
    constant: f64,
) -> Retained<objc2_app_kit::NSLayoutConstraint> {
    unsafe { pin_two_rel(a, aa, NSLayoutRelation::Equal, b, ba, constant) }
}

unsafe fn pin_two_rel(
    a: &AnyObject,
    aa: NSLayoutAttribute,
    rel: NSLayoutRelation,
    b: &AnyObject,
    ba: NSLayoutAttribute,
    constant: f64,
) -> Retained<objc2_app_kit::NSLayoutConstraint> {
    unsafe {
        objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            a, aa, rel, Some(b), ba, 1.0, constant,
        )
    }
}

unsafe fn pin_const(
    a: &AnyObject,
    attr: NSLayoutAttribute,
    constant: f64,
) -> Retained<objc2_app_kit::NSLayoutConstraint> {
    unsafe {
        objc2_app_kit::NSLayoutConstraint::constraintWithItem_attribute_relatedBy_toItem_attribute_multiplier_constant(
            a,
            attr,
            NSLayoutRelation::Equal,
            None,
            NSLayoutAttribute::NotAnAttribute,
            1.0,
            constant,
        )
    }
}

/// Horizontal stack of small tick-value labels. Uses `EqualCentering` so
/// labels' centers are evenly distributed across the row's width — close
/// to (though not pixel-perfect with) NSSlider's tick mark positions.
fn make_tick_labels_row(mtm: MainThreadMarker, labels: &[String]) -> Retained<NSStackView> {
    let row = NSStackView::new(mtm);
    row.setOrientation(NSUserInterfaceLayoutOrientation::Horizontal);
    row.setDistribution(NSStackViewDistribution::EqualCentering);
    row.setAlignment(NSLayoutAttribute::CenterY);
    let font = NSFont::systemFontOfSize(NSFont::smallSystemFontSize() - 1.0);
    let color = NSColor::tertiaryLabelColor();
    for s in labels {
        let lbl = NSTextField::labelWithString(&NSString::from_str(s), mtm);
        lbl.setFont(Some(&font));
        lbl.setTextColor(Some(&color));
        lbl.setAlignment(NSTextAlignment::Center);
        row.addArrangedSubview(&lbl);
    }
    row
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
