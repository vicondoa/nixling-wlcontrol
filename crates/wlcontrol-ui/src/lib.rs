//! GTK4/libadwaita control center.
//!
//! The public entry point stays intentionally small (`open(config)`) so the CLI
//! can keep delegating display concerns to this crate while core state and
//! nixling protocol logic remain in their owning crates.

use std::cell::{Cell, RefCell};
use std::process::Command;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use wlcontrol_core::error::{WlError, WlResult};
use wlcontrol_core::model::{
    ActionKind, Connectivity, PlannedAction, RuntimeState, SocketIntent, WlState,
};
use wlcontrol_core::{plan, reduce, Config};
use wlcontrol_nixling::NixlingClient;

mod view_model;

use view_model::{
    action_label, action_vm_name, empty_group_message, needs_confirmation, role_indicator,
    state_badge, unavailable_tooltip, usb_claim_summary, visible_vm_groups, vm_subtitle,
};

const APP_ID: &str = "dev.vicondoa.NixlingWlControl";
const WINDOW_TITLE: &str = "nixling VMs";

const APP_CSS: &str = r#"
.state-pill {
  border-radius: 999px;
  font-weight: 700;
  padding: 3px 9px;
}
.state-running {
  background: alpha(@success_bg_color, 0.18);
  color: @success_color;
}
.state-stopped {
  background: alpha(@window_fg_color, 0.08);
  color: alpha(@window_fg_color, 0.72);
}
.state-progress {
  background: alpha(@accent_bg_color, 0.18);
  color: @accent_color;
}
.state-unknown {
  background: alpha(@warning_bg_color, 0.18);
  color: @warning_color;
}
.vm-details {
  margin-top: 6px;
  margin-bottom: 8px;
}
.action-flow {
  margin-top: 6px;
  margin-bottom: 8px;
}
"#;

#[derive(Clone)]
struct WindowState {
    current: Rc<RefCell<Option<WlState>>>,
    show_internal: Rc<Cell<bool>>,
}

/// Open (or focus) the control center window.
///
/// The GTK application id is stable so the compositor can apply a native
/// Wayland/niri window rule, and default `gio::ApplicationFlags` preserve
/// single-instance open/focus semantics.
pub fn open(config: &Config) -> WlResult<()> {
    adw::init().map_err(|err| WlError::Config(format!("failed to initialize GTK: {err}")))?;
    install_css();

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::empty())
        .build();
    let config = config.clone();

    app.connect_activate(move |app| {
        if let Some(window) = app.active_window() {
            window.present();
            return;
        }

        let window = build_window(app, config.clone());
        window.present();
    });

    let exit = app.run();
    if exit == glib::ExitCode::SUCCESS {
        Ok(())
    } else {
        Err(WlError::Config(format!(
            "GTK application exited with status {}",
            exit.value()
        )))
    }
}

fn build_window(app: &adw::Application, config: Config) -> adw::ApplicationWindow {
    let state = WindowState {
        current: Rc::new(RefCell::new(None)),
        show_internal: Rc::new(Cell::new(false)),
    };

    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(WINDOW_TITLE)
        .default_width(520)
        .default_height(640)
        .build();

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&adw::WindowTitle::new(WINDOW_TITLE, "")));

    let role_label = gtk::Label::new(Some("loading"));
    role_label.add_css_class("caption");
    role_label.add_css_class("dim-label");
    role_label.set_tooltip_text(Some("nixling public-socket connectivity and role"));
    header.pack_start(&role_label);

    let show_internal = gtk::ToggleButton::with_label("Internal");
    show_internal.set_tooltip_text(Some("Show hidden VMs and framework net VMs"));
    header.pack_end(&show_internal);

    let refresh_button = gtk::Button::from_icon_name("view-refresh-symbolic");
    refresh_button.set_tooltip_text(Some("Refresh"));
    header.pack_end(&refresh_button);
    window.set_titlebar(Some(&header));

    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.set_vexpand(true);
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&content));
    window.set_content(Some(&toast_overlay));

    refresh_button.connect_clicked(glib::clone!(
        #[weak]
        content,
        #[weak]
        role_label,
        #[weak]
        refresh_button,
        #[weak]
        toast_overlay,
        #[weak]
        window,
        #[strong]
        config,
        #[strong]
        state,
        move |_| {
            refresh_state_async(
                config.clone(),
                state.clone(),
                content.clone(),
                role_label.clone(),
                refresh_button.clone(),
                toast_overlay.clone(),
                window.clone(),
            );
        }
    ));

    show_internal.connect_toggled(glib::clone!(
        #[weak]
        content,
        #[weak]
        role_label,
        #[weak]
        refresh_button,
        #[weak]
        toast_overlay,
        #[weak]
        window,
        #[strong]
        config,
        #[strong]
        state,
        move |button| {
            state.show_internal.set(button.is_active());
            if let Some(current) = state.current.borrow().clone() {
                render_state(
                    &content,
                    &role_label,
                    &refresh_button,
                    &toast_overlay,
                    &window,
                    &config,
                    &state,
                    &current,
                );
            }
        }
    ));

    render_loading(&content, "Loading VM state…");
    refresh_state_async(
        config,
        state,
        content,
        role_label,
        refresh_button,
        toast_overlay,
        window.clone(),
    );

    window
}

fn install_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let provider = gtk::CssProvider::new();
    provider.load_from_data(APP_CSS);
    gtk::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

fn load_state(config: &Config) -> WlState {
    let client = NixlingClient::new(config);
    reduce::reduce_with_config(client.refresh(), config)
}

fn refresh_state_async(
    config: Config,
    state: WindowState,
    content: gtk::Box,
    role_label: gtk::Label,
    refresh_button: gtk::Button,
    toast_overlay: adw::ToastOverlay,
    window: adw::ApplicationWindow,
) {
    refresh_button.set_sensitive(false);
    if state.current.borrow().is_none() {
        render_loading(&content, "Loading VM state…");
    }

    let refresh = gio::spawn_blocking(move || {
        let loaded = load_state(&config);
        (config, loaded)
    });

    glib::MainContext::default().spawn_local(glib::clone!(
        #[weak]
        content,
        #[weak]
        role_label,
        #[weak]
        refresh_button,
        #[weak]
        toast_overlay,
        #[weak]
        window,
        #[strong]
        state,
        async move {
            match refresh.await {
                Ok((config, loaded)) => {
                    *state.current.borrow_mut() = Some(loaded.clone());
                    refresh_button.set_sensitive(true);
                    render_state(
                        &content,
                        &role_label,
                        &refresh_button,
                        &toast_overlay,
                        &window,
                        &config,
                        &state,
                        &loaded,
                    );
                }
                Err(_) => {
                    refresh_button.set_sensitive(true);
                    show_toast(&toast_overlay, "refresh worker stopped unexpectedly");
                }
            }
        }
    ));
}

#[allow(clippy::too_many_arguments)]
fn render_state(
    content: &gtk::Box,
    role_label: &gtk::Label,
    refresh_button: &gtk::Button,
    toast_overlay: &adw::ToastOverlay,
    window: &adw::ApplicationWindow,
    config: &Config,
    state: &WindowState,
    current: &WlState,
) {
    role_label.set_text(&role_indicator(current));
    match current.connectivity {
        Connectivity::DaemonDown => render_daemon_down(
            content,
            config,
            state,
            role_label,
            refresh_button,
            toast_overlay,
            window,
        ),
        Connectivity::AuthDenied => render_auth_denied(content),
        Connectivity::Connected => render_vm_groups(
            content,
            config,
            state,
            role_label,
            refresh_button,
            toast_overlay,
            window,
            current,
        ),
    }
}

fn clear_content(content: &gtk::Box) {
    while let Some(child) = content.first_child() {
        content.remove(&child);
    }
}

fn render_loading(content: &gtk::Box, message: &str) {
    clear_content(content);

    let box_ = gtk::Box::new(gtk::Orientation::Vertical, 12);
    box_.set_valign(gtk::Align::Center);
    box_.set_halign(gtk::Align::Center);
    box_.set_vexpand(true);
    box_.set_hexpand(true);

    let spinner = gtk::Spinner::new();
    spinner.start();
    box_.append(&spinner);

    let label = gtk::Label::new(Some(message));
    label.add_css_class("dim-label");
    box_.append(&label);

    content.append(&box_);
}

fn render_daemon_down(
    content: &gtk::Box,
    config: &Config,
    state: &WindowState,
    role_label: &gtk::Label,
    refresh_button: &gtk::Button,
    toast_overlay: &adw::ToastOverlay,
    window: &adw::ApplicationWindow,
) {
    clear_content(content);
    let page = adw::StatusPage::builder()
        .icon_name("network-error-symbolic")
        .title("nixlingd unreachable")
        .description("The nixling public socket could not be reached. Start or restart nixlingd, then retry.")
        .vexpand(true)
        .build();
    let retry = gtk::Button::with_label("Retry");
    retry.add_css_class("suggested-action");
    retry.set_halign(gtk::Align::Center);
    retry.connect_clicked(glib::clone!(
        #[weak]
        content,
        #[weak]
        role_label,
        #[weak]
        refresh_button,
        #[weak]
        toast_overlay,
        #[weak]
        window,
        #[strong]
        config,
        #[strong]
        state,
        move |_| {
            refresh_state_async(
                config.clone(),
                state.clone(),
                content.clone(),
                role_label.clone(),
                refresh_button.clone(),
                toast_overlay.clone(),
                window.clone(),
            );
        }
    ));
    page.set_child(Some(&retry));
    content.append(&page);
}

fn render_auth_denied(content: &gtk::Box) {
    clear_content(content);
    let page = adw::StatusPage::builder()
        .icon_name("dialog-password-symbolic")
        .title("Authorization required")
        .description("nixlingd is reachable, but this user has role none. Join the nixling lifecycle group or run from an authorized session.")
        .vexpand(true)
        .build();
    content.append(&page);
}

#[allow(clippy::too_many_arguments)]
fn render_vm_groups(
    content: &gtk::Box,
    config: &Config,
    state: &WindowState,
    role_label: &gtk::Label,
    refresh_button: &gtk::Button,
    toast_overlay: &adw::ToastOverlay,
    window: &adw::ApplicationWindow,
    current: &WlState,
) {
    clear_content(content);
    let groups = visible_vm_groups(current, state.show_internal.get());

    if groups.is_empty() {
        let page = adw::StatusPage::builder()
            .icon_name("computer-symbolic")
            .title("No VMs")
            .description(empty_group_message(state.show_internal.get()))
            .vexpand(true)
            .build();
        content.append(&page);
        return;
    }

    let page = adw::PreferencesPage::new();
    for group in groups {
        let pref_group = adw::PreferencesGroup::builder().title(&group.env).build();
        for vm in group.vms {
            pref_group.add(&build_vm_row(
                &vm,
                current,
                config,
                state,
                role_label,
                refresh_button,
                toast_overlay,
                window,
                content,
            ));
        }
        page.add(&pref_group);
    }

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&page)
        .build();
    content.append(&scrolled);
}

#[allow(clippy::too_many_arguments)]
fn build_vm_row(
    vm: &wlcontrol_core::Vm,
    current: &WlState,
    config: &Config,
    state: &WindowState,
    role_label: &gtk::Label,
    refresh_button: &gtk::Button,
    toast_overlay: &adw::ToastOverlay,
    window: &adw::ApplicationWindow,
    content: &gtk::Box,
) -> adw::ExpanderRow {
    let subtitle = vm_subtitle(vm, config.show_pending_restart);
    let row = adw::ExpanderRow::builder()
        .title(&vm.name)
        .subtitle(&subtitle)
        .build();

    let badge = build_state_badge(vm.state);
    row.add_prefix(&badge);

    let details = build_vm_details(vm);
    row.add_row(&details);

    let action_flow = gtk::FlowBox::builder()
        .selection_mode(gtk::SelectionMode::None)
        .max_children_per_line(3)
        .column_spacing(6)
        .row_spacing(6)
        .build();
    action_flow.add_css_class("action-flow");
    action_flow.set_valign(gtk::Align::Start);

    for availability in plan::vm_actions(current, config, &vm.name) {
        let button = build_action_button(
            availability,
            config,
            state,
            role_label,
            refresh_button,
            toast_overlay,
            window,
            content,
        );
        action_flow.insert(&button, -1);
    }
    row.add_row(&action_flow);

    row
}

fn build_state_badge(state: RuntimeState) -> gtk::Label {
    let spec = state_badge(state);
    let badge = gtk::Label::new(Some(spec.label));
    badge.add_css_class("state-pill");
    badge.add_css_class(spec.css_class);
    badge.set_valign(gtk::Align::Center);
    badge
}

fn build_vm_details(vm: &wlcontrol_core::Vm) -> gtk::Box {
    let details = gtk::Box::new(gtk::Orientation::Vertical, 4);
    details.add_css_class("vm-details");

    append_detail_line(
        &details,
        &format!(
            "Static IP: {}",
            vm.static_ip.as_deref().unwrap_or("not declared")
        ),
    );
    if vm.readiness.is_empty() {
        append_detail_line(&details, "Readiness: not reported");
    } else {
        append_detail_line(&details, &format!("Readiness: {}", vm.readiness.join(", ")));
    }
    if vm.usb.is_empty() {
        append_detail_line(&details, "USB claims: none");
    } else {
        for claim in &vm.usb {
            append_detail_line(&details, &format!("USB: {}", usb_claim_summary(claim)));
        }
    }
    if vm.pending_restart {
        append_detail_line(
            &details,
            "Pending restart: running closure differs from declared",
        );
    }

    details
}

fn append_detail_line(details: &gtk::Box, text: &str) {
    let label = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .build();
    label.add_css_class("caption");
    label.add_css_class("dim-label");
    details.append(&label);
}

#[allow(clippy::too_many_arguments)]
fn build_action_button(
    availability: wlcontrol_core::ActionAvailability,
    config: &Config,
    state: &WindowState,
    role_label: &gtk::Label,
    refresh_button: &gtk::Button,
    toast_overlay: &adw::ToastOverlay,
    window: &adw::ApplicationWindow,
    content: &gtk::Box,
) -> gtk::Button {
    let action = availability.action.clone();
    let button = gtk::Button::with_label(&action_label(&action));
    button.set_focus_on_click(true);

    if availability.is_available() {
        button.set_tooltip_text(Some("Run this action"));
        let config = config.clone();
        let state = state.clone();
        button.connect_clicked(glib::clone!(
            #[weak]
            button,
            #[weak]
            content,
            #[weak]
            role_label,
            #[weak]
            refresh_button,
            #[weak]
            toast_overlay,
            #[weak]
            window,
            #[strong]
            action,
            #[strong]
            config,
            #[strong]
            state,
            move |_| {
                handle_action_click(
                    action.clone(),
                    config.clone(),
                    state.clone(),
                    button.clone(),
                    content.clone(),
                    role_label.clone(),
                    refresh_button.clone(),
                    toast_overlay.clone(),
                    window.clone(),
                );
            }
        ));
    } else if let Some(reason) = availability.unavailable {
        button.set_sensitive(false);
        button.set_tooltip_text(Some(&unavailable_tooltip(&reason)));
    }

    button
}

#[allow(clippy::too_many_arguments)]
fn handle_action_click(
    action: ActionKind,
    config: Config,
    state: WindowState,
    button: gtk::Button,
    content: gtk::Box,
    role_label: gtk::Label,
    refresh_button: gtk::Button,
    toast_overlay: adw::ToastOverlay,
    window: adw::ApplicationWindow,
) {
    let Some(current) = state.current.borrow().clone() else {
        show_toast(&toast_overlay, "state is still loading");
        return;
    };

    let Some(vm) = action_vm_name(&action).and_then(|name| {
        current
            .vms
            .iter()
            .find(|candidate| candidate.name == name)
            .cloned()
    }) else {
        execute_action(
            action,
            config,
            state,
            button,
            content,
            role_label,
            refresh_button,
            toast_overlay,
            window,
            current,
        );
        return;
    };

    if needs_confirmation(&action, &vm) {
        confirm_destructive_action(
            action,
            config,
            state,
            button,
            content,
            role_label,
            refresh_button,
            toast_overlay,
            window,
            current,
        );
    } else {
        execute_action(
            action,
            config,
            state,
            button,
            content,
            role_label,
            refresh_button,
            toast_overlay,
            window,
            current,
        );
    }
}

#[allow(clippy::too_many_arguments, deprecated)]
fn confirm_destructive_action(
    action: ActionKind,
    config: Config,
    state: WindowState,
    button: gtk::Button,
    content: gtk::Box,
    role_label: gtk::Label,
    refresh_button: gtk::Button,
    toast_overlay: adw::ToastOverlay,
    window: adw::ApplicationWindow,
    current: WlState,
) {
    let action_name = action_label(&action);
    let vm = action_vm_name(&action).unwrap_or("VM");
    let dialog = adw::MessageDialog::builder()
        .transient_for(&window)
        .modal(true)
        .heading(format!("{action_name} {vm}?"))
        .body("This action changes a running VM. Confirm to continue.")
        .build();
    dialog.add_response("cancel", "Cancel");
    dialog.add_response("confirm", &action_name);
    dialog.set_default_response(Some("cancel"));
    dialog.set_close_response("cancel");
    dialog.set_response_appearance("confirm", adw::ResponseAppearance::Destructive);
    dialog.connect_response(
        Some("confirm"),
        glib::clone!(
            #[weak]
            dialog,
            #[strong]
            action,
            #[strong]
            config,
            #[strong]
            state,
            #[strong]
            button,
            #[strong]
            content,
            #[strong]
            role_label,
            #[strong]
            refresh_button,
            #[strong]
            toast_overlay,
            #[strong]
            window,
            #[strong]
            current,
            move |_, _| {
                dialog.close();
                execute_action(
                    action.clone(),
                    config.clone(),
                    state.clone(),
                    button.clone(),
                    content.clone(),
                    role_label.clone(),
                    refresh_button.clone(),
                    toast_overlay.clone(),
                    window.clone(),
                    current.clone(),
                );
            }
        ),
    );
    dialog.connect_response(Some("cancel"), |dialog, _| dialog.close());
    dialog.present();
}

#[allow(clippy::too_many_arguments)]
fn execute_action(
    action: ActionKind,
    config: Config,
    state: WindowState,
    button: gtk::Button,
    content: gtk::Box,
    role_label: gtk::Label,
    refresh_button: gtk::Button,
    toast_overlay: adw::ToastOverlay,
    window: adw::ApplicationWindow,
    current: WlState,
) {
    match plan::plan(&action, &current, &config) {
        Ok(PlannedAction::Process { argv }) => match spawn_process(argv) {
            Ok(()) => show_toast(&toast_overlay, "terminal launched"),
            Err(err) => show_toast(&toast_overlay, &err.to_string()),
        },
        Ok(PlannedAction::Socket { intent }) => dispatch_socket_async(
            intent,
            config,
            state,
            button,
            content,
            role_label,
            refresh_button,
            toast_overlay,
            window,
        ),
        Err(reason) => show_toast(&toast_overlay, &unavailable_tooltip(&reason)),
    }
}

fn spawn_process(argv: Vec<String>) -> WlResult<()> {
    let Some((program, args)) = argv.split_first() else {
        return Err(WlError::Config(
            "empty terminal argv; check [terminal] config".to_owned(),
        ));
    };
    Command::new(program).args(args).spawn()?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn dispatch_socket_async(
    intent: SocketIntent,
    config: Config,
    state: WindowState,
    button: gtk::Button,
    content: gtk::Box,
    role_label: gtk::Label,
    refresh_button: gtk::Button,
    toast_overlay: adw::ToastOverlay,
    window: adw::ApplicationWindow,
) {
    button.set_sensitive(false);
    let worker_config = config.clone();
    let dispatch = gio::spawn_blocking(move || {
        NixlingClient::new(&worker_config)
            .dispatch(&intent)
            .map(|outcome| outcome.summary)
            .map_err(|err| err.to_string())
    });

    glib::MainContext::default().spawn_local(glib::clone!(
        #[weak]
        button,
        #[weak]
        content,
        #[weak]
        role_label,
        #[weak]
        refresh_button,
        #[weak]
        toast_overlay,
        #[weak]
        window,
        #[strong]
        config,
        #[strong]
        state,
        async move {
            match dispatch.await {
                Ok(Ok(summary)) => {
                    button.set_sensitive(true);
                    show_toast(&toast_overlay, &summary);
                    refresh_state_async(
                        config.clone(),
                        state.clone(),
                        content.clone(),
                        role_label.clone(),
                        refresh_button.clone(),
                        toast_overlay.clone(),
                        window.clone(),
                    );
                }
                Ok(Err(err)) => {
                    button.set_sensitive(true);
                    show_toast(&toast_overlay, &err);
                }
                Err(_) => {
                    button.set_sensitive(true);
                    show_toast(&toast_overlay, "action worker stopped unexpectedly");
                }
            }
        }
    ));
}

fn show_toast(toast_overlay: &adw::ToastOverlay, message: &str) {
    let toast = adw::Toast::new(message);
    toast.set_timeout(5);
    toast_overlay.add_toast(toast);
}
