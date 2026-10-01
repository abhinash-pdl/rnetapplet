mod appearance;
mod hidden;
mod hotspot;
mod list;
mod motion;
mod placement;
mod row;
mod state;
mod theme;
mod vpn;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gtk4::gdk::Key;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use list::{apply_model, refresh_list, request_rebuild};
use placement::{hide_popup, open_editor, output_scale_for_x, place_near};
use row::flat_icon_button;
use state::UiHandles;
use theme::{header_icon_px, themed_icon};

use crate::state::{BackendCmd, Model, UiEvent};

const POPUP_W: i32 = 415;
const POPUP_H: i32 = 550;
const POPUP_MS: u32 = 190;

const CSS: &str = "
window.rnet-window, window.rnet-window > .background, window.rnet-window:backdrop, window.rnet-window:backdrop > .background, window.rnet-window.background, window.rnet-window.background:backdrop { background-color: transparent; background-image: none; box-shadow: none; border: none; border-radius: 0; }
.rnet-card { background-color: @theme_bg_color; border-radius: 12px; padding: 8px 6px; }
 .rnet-window list, .rnet-window listview { background-color: transparent; }
 .rnet-window scrolledwindow > overshoot { background: transparent; box-shadow: none; }
 .rnet-window image { opacity: 1; }
 .rnet-title { margin: 0; }
 .rnet-section-row { background-color: transparent; }
 .rnet-section-row:hover, .rnet-section-row:active, .rnet-section-row:selected { background-color: transparent; }
 .rnet-row { min-height: 48px; margin: 1px 0; border-radius: 6px; background-color: @theme_bg_color; background-image: none; }
 .rnet-row:hover { background-color: alpha(@theme_fg_color, 0.08); background-image: none; }
 .rnet-row:active { background-color: alpha(@theme_fg_color, 0.13); background-image: none; }
 .flat { transition: background-color 120ms ease-in-out; }
 .rnet-popover { padding: 14px; }
 .rnet-hotspot-key { font-family: monospace; }
 .rnet-status { padding-top: 6px; }
 .error { color: @error_color; }
 .disconnect-label { color: @error_color; }
 .rnet-social-row { background-image: none; }
.rnet-qr { padding-left: 10px; padding-right: 10px; }
.rnet-toolbar entry { min-height: 8px; padding-top: 0; padding-bottom: 0; }
.rnet-window entry { caret-color: @theme_fg_color; }
.rnet-window entry > text > selection { background-color: alpha(@theme_fg_color, 0.25); color: @theme_fg_color; }
.rnet-window entry:focus-within { border-image: radial-gradient(circle closest-corner at center calc(100% - 1px), alpha(@theme_fg_color, 0.6) 100%, transparent 100%) 2/0 0 2px; outline-color: alpha(@theme_fg_color, 0.4); box-shadow: none; }
.rnet-lock-badge { background-color: @theme_base_color; border-radius: 999px; padding: 0.5px; }
";

pub(crate) fn apply_popup_size(revealer: &gtk4::Revealer, card: &gtk4::Box, scale: i32) {
    let s = scale.max(1);
    let w = (POPUP_W as f64 / s as f64).ceil() as i32;
    let h = (POPUP_H as f64 / s as f64).ceil() as i32;
    tracing::debug!(scale = s, w, h, "popup size applied");
    card.set_size_request(w, h);
    revealer.set_size_request(w, -1);
}

pub(crate) fn load_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(CSS);
    let Some(display) = gtk4::gdk::Display::default() else {
        tracing::warn!("no display; skipping applet CSS");
        return;
    };
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_USER,
    );
}

pub fn run(
    initial: Model,
    ui_rx: async_channel::Receiver<UiEvent>,
    toggle_rx: async_channel::Receiver<Option<(i32, i32)>>,
    quit_rx: async_channel::Receiver<()>,
    cmd_tx: async_channel::Sender<BackendCmd>,
    popup_visible: Arc<AtomicBool>,
    scan_frozen: Arc<AtomicBool>,
) {
    gtk4::glib::log_set_default_handler(|_d, level, msg| {
        if matches!(level, gtk4::glib::LogLevel::Error) {
            eprintln!("{msg}");
        }
    });

    appearance::sync_color_scheme();

    let app = gtk4::Application::new(Some("dev.abhinash-pdl.rnetapplet"), Default::default());

    app.connect_activate(move |app| {
        load_css();
        let ui_rx = ui_rx.clone();
        let toggle_rx = toggle_rx.clone();
        let quit_rx = quit_rx.clone();
        let cmd_tx = cmd_tx.clone();
        let popup_visible = popup_visible.clone();
        let scan_frozen = scan_frozen.clone();

        let model = Rc::new(RefCell::new(Arc::new(initial.clone())));
        let query = Rc::new(RefCell::new(String::new()));
        let visible = Rc::new(Cell::new(false));
        let prog_guard = Rc::new(Cell::new(false));
        let expanded: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let errors: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
        let err_token: Rc<RefCell<HashMap<String, std::time::Instant>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pw_drafts: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
        let focus_ssid: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let pending_secret_paths: Rc<RefCell<HashMap<String, String>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let connecting: Rc<RefCell<HashMap<String, std::time::Instant>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pw_attempt: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
        let unsaved_attempt: Rc<RefCell<HashSet<String>>> = Rc::new(RefCell::new(HashSet::new()));
        let hidden_expanded: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let hidden_closing: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let hidden_card: Rc<RefCell<Option<(gtk4::Revealer, gtk4::ListBoxRow)>>> =
            Rc::new(RefCell::new(None));
        let hidden_error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let hotspot_error: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));
        let hotspot_expanded: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let hotspot_was: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let hotspot_ssid: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let hotspot_psk: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let speeds: Rc<RefCell<(Option<u64>, Option<u64>)>> = Rc::new(RefCell::new((None, None)));
        let speed_labels: Rc<RefCell<Option<(gtk4::Label, gtk4::Label)>>> =
            Rc::new(RefCell::new(None));
        let revealers: Rc<RefCell<HashMap<String, gtk4::Revealer>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let chevrons: Rc<RefCell<HashMap<String, gtk4::Image>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let connect_btns: Rc<RefCell<HashMap<String, gtk4::Button>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let submits: Rc<RefCell<HashMap<String, gtk4::Button>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let flips: Rc<RefCell<HashMap<String, std::rc::Rc<motion::Flip>>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let ssid_labels: Rc<RefCell<HashMap<String, gtk4::Label>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pw_entries: Rc<RefCell<HashMap<String, gtk4::PasswordEntry>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let strength_setters: Rc<RefCell<HashMap<String, theme::StrengthSetter>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let hide_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));
        let pending_rebuild = Rc::new(Cell::new(false));

        let window = gtk4::ApplicationWindow::new(app);
        window.set_title(Some("Networks"));
        window.add_css_class("rnet-window");
        window.init_layer_shell();
        window.set_layer(Layer::Overlay);
        for edge in [Edge::Top, Edge::Right, Edge::Bottom, Edge::Left] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(0);
        window.set_keyboard_mode(KeyboardMode::OnDemand);

        {
            #![allow(deprecated)]
            let sc = window.style_context();
            let shade = |name: &str| {
                sc.lookup_color(name)
                    .map(|rgba| {
                        format!(
                            "#{:02x}{:02x}{:02x}",
                            (rgba.red() * 255.) as u8,
                            (rgba.green() * 255.) as u8,
                            (rgba.blue() * 255.) as u8
                        )
                    })
                    .unwrap_or_else(|| "<unresolved>".to_string())
            };
            let theme = gtk4::Settings::default()
                .and_then(|s| s.gtk_theme_name())
                .map(|g| g.to_string())
                .unwrap_or_else(|| "<default>".to_string());
            tracing::info!(
                theme,
                bg = %shade("theme_bg_color"),
                base = %shade("theme_base_color"),
                fg = %shade("theme_fg_color"),
                sel = %shade("theme_selected_bg_color"),
                "theme bootstrap"
            );
            if let Some(display) = gtk4::gdk::Display::default() {
                let monitors = display.monitors();
                for i in 0..monitors.n_items() {
                    if let Some(obj) = monitors.item(i)
                        && let Ok(m) = obj.downcast::<gtk4::gdk::Monitor>()
                    {
                        let g = m.geometry();
                        tracing::info!(
                            x = g.x(),
                            w = g.width(),
                            h = g.height(),
                            scale = m.scale_factor(),
                            "output"
                        );
                    }
                }
            }
        }

        {
            let visible = visible.clone();
            let flag = popup_visible.clone();
            let hide_window = window.clone();
            window.connect_close_request(move |_| {
                visible.set(false);
                flag.store(false, Ordering::Relaxed);
                hide_window.set_visible(false);
                gtk4::glib::Propagation::Stop
            });
        }

        let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        vbox.add_css_class("rnet-card");

        let header = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        header.add_css_class("rnet-header");
        header.set_margin_start(8);
        header.set_margin_end(8);
        let title = gtk4::Label::new(Some("Networks"));
        title.set_halign(gtk4::Align::Start);
        title.set_hexpand(true);
        title.add_css_class("rnet-title");
        header.append(&title);
        let px = header_icon_px();
        let rescan = flat_icon_button("view-refresh-symbolic", px, "Rescan");
        header.append(&rescan);
        let hidden = flat_icon_button("list-add-symbolic", px, "Hidden network");
        header.append(&hidden);
        let settings = flat_icon_button("preferences-system-symbolic", px, "Network settings");
        header.append(&settings);
        vbox.append(&header);
        settings.connect_clicked(move |_| open_editor());

        let toolbar = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
        toolbar.add_css_class("rnet-toolbar");
        toolbar.set_margin_start(4);
        toolbar.set_margin_end(4);

        let wifi_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
        let wifi_icon = gtk4::Image::from_icon_name(&themed_icon(&[
            "network-wireless-symbolic",
            "network-wireless-disabled-symbolic",
        ]));
        wifi_icon.set_pixel_size(18);
        wifi_icon.set_valign(gtk4::Align::Center);
        wifi_icon.set_tooltip_text(Some("Wi-Fi"));
        wifi_row.append(&wifi_icon);
        let wifi_switch = gtk4::Switch::new();
        wifi_switch.set_valign(gtk4::Align::Center);
        wifi_switch.set_active(model.borrow().wifi_enabled);
        wifi_switch.set_tooltip_text(Some("Wi-Fi"));
        wifi_row.append(&wifi_switch);
        toolbar.append(&wifi_row);

        let air_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
        let air_icon = gtk4::Image::from_icon_name(&themed_icon(&[
            "airplane-mode-symbolic",
            "network-wireless-disabled-symbolic",
        ]));
        air_icon.set_pixel_size(18);
        air_icon.set_valign(gtk4::Align::Center);
        air_icon.set_tooltip_text(Some("Airplane mode"));
        air_row.append(&air_icon);
        let air_switch = gtk4::Switch::new();
        air_switch.set_valign(gtk4::Align::Center);
        air_switch.set_active(model.borrow().airplane_mode());
        air_switch.set_tooltip_text(Some("Airplane mode"));
        air_row.append(&air_switch);
        toolbar.append(&air_row);

        {
            let tx = cmd_tx.clone();
            let guard = prog_guard.clone();
            wifi_switch.connect_state_set(move |_, state| {
                if !guard.get() {
                    let _ = tx.try_send(BackendCmd::SetWifi(state));
                }
                gtk4::glib::Propagation::Proceed
            });
        }
        {
            let tx = cmd_tx.clone();
            let guard = prog_guard.clone();
            air_switch.connect_state_set(move |_, state| {
                if !guard.get() {
                    let _ = tx.try_send(BackendCmd::SetAirplane(state));
                }
                gtk4::glib::Propagation::Proceed
            });
        }

        let hotspot_icon = gtk4::Image::from_icon_name(&themed_icon(&[
            "network-wireless-hotspot-symbolic",
            "network-wireless-hotspot",
            "hotspot-symbolic",
            "network-wireless-symbolic",
        ]));
        hotspot_icon.set_pixel_size(18);
        let hotspot_btn = gtk4::Button::new();
        hotspot_btn.set_child(Some(&hotspot_icon));
        hotspot_btn.set_focus_on_click(false);
        hotspot_btn.set_focusable(false);
        hotspot_btn.set_tooltip_text(Some("Hotspot"));
        toolbar.append(&hotspot_btn);

        let search = gtk4::SearchEntry::new();
        search.set_placeholder_text(Some("Search..."));
        search.set_width_chars(2);
        search.set_hexpand(true);
        search.set_margin_end(10);
        toolbar.append(&search);

        let qr_icon = gtk4::Image::from_icon_name(&themed_icon(&[
            "scanner-symbolic",
            "camera-photo-symbolic",
        ]));
        qr_icon.set_pixel_size(22);
        let qr = gtk4::Button::new();
        qr.set_child(Some(&qr_icon));
        qr.set_tooltip_text(Some("Scan QR"));
        qr.add_css_class("rnet-qr");
        qr.set_focusable(false);
        qr.set_focus_on_click(false);
        toolbar.append(&qr);
        vbox.append(&toolbar);

        let scroll = gtk4::ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);
        scroll.set_overlay_scrolling(true);
        scroll.vscrollbar().set_visible(false);
        let list = gtk4::ListBox::new();
        list.set_selection_mode(gtk4::SelectionMode::None);
        scroll.set_child(Some(&list));
        vbox.append(&scroll);

        let backdrop = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
        backdrop.set_hexpand(true);
        backdrop.set_vexpand(true);
        let overlay = gtk4::Overlay::new();
        overlay.set_child(Some(&backdrop));
        window.set_child(Some(&overlay));

        let popup_revealer = gtk4::Revealer::new();
        popup_revealer.set_transition_type(gtk4::RevealerTransitionType::Crossfade);
        popup_revealer.set_transition_duration(POPUP_MS);
        popup_revealer.set_child(Some(&vbox));
        popup_revealer.set_halign(gtk4::Align::End);
        popup_revealer.set_valign(gtk4::Align::Start);
        apply_popup_size(&popup_revealer, &vbox, output_scale_for_x(None));
        overlay.add_overlay(&popup_revealer);

        let h = UiHandles {
            list: list.clone(),
            root: window.clone().upcast(),
            scroll: scroll.clone(),
            model: model.clone(),
            query: query.clone(),
            expanded: expanded.clone(),
            errors: errors.clone(),
            err_token: err_token.clone(),
            focus_ssid: focus_ssid.clone(),
            pw_drafts: pw_drafts.clone(),
            connecting: connecting.clone(),
            pw_attempt: pw_attempt.clone(),
            unsaved_attempt: unsaved_attempt.clone(),
            pending_secret_paths: pending_secret_paths.clone(),
            hidden_expanded: hidden_expanded.clone(),
            hidden_closing: hidden_closing.clone(),
            hidden_card: hidden_card.clone(),
            hidden_error: hidden_error.clone(),
            hotspot_error: hotspot_error.clone(),
            hotspot_expanded: hotspot_expanded.clone(),
            hotspot_was: hotspot_was.clone(),
            hotspot_ssid: hotspot_ssid.clone(),
            hotspot_psk: hotspot_psk.clone(),
            speeds: speeds.clone(),
            speed_labels: speed_labels.clone(),
            revealers: revealers.clone(),
            chevrons: chevrons.clone(),
            connect_btns: connect_btns.clone(),
            submits: submits.clone(),
            flips: flips.clone(),
            ssid_labels: ssid_labels.clone(),
            pw_entries: pw_entries.clone(),
            strength_setters: strength_setters.clone(),
            visible: visible.clone(),
            search_entry: Rc::new(RefCell::new(None)),
            hidden_entries: Rc::new(RefCell::new(Vec::new())),
            hotspot_entries: Rc::new(RefCell::new(Vec::new())),
            was_editing: Rc::new(Cell::new(false)),
            in_refresh: Rc::new(Cell::new(false)),
            pw_hovered: Rc::new(Cell::new(false)),
            scan_frozen: scan_frozen.clone(),
            pending_model: Rc::new(RefCell::new(None)),
            pending_rebuild: pending_rebuild.clone(),
            last_strengths: Rc::new(RefCell::new(HashMap::new())),
            ap_misses: Rc::new(RefCell::new(HashMap::new())),
            cmd_tx: cmd_tx.clone(),
        };
        *h.search_entry.borrow_mut() = Some(search.clone().upcast());

        {
            let window_c = window.clone();
            let revealer_c = popup_revealer.clone();
            let visible_c = visible.clone();
            let flag_c = popup_visible.clone();
            let hide_gen_c = hide_gen.clone();
            let h_c = h.clone();
            let gesture = gtk4::GestureClick::new();
            gesture.connect_pressed(move |_, _, _, _| {
                if visible_c.get() {
                    flag_c.store(false, Ordering::Relaxed);
                    hide_popup(&window_c, &revealer_c, &visible_c, &hide_gen_c);
                    list::schedule_teardown(&h_c);
                }
            });
            backdrop.add_controller(gesture);
        }

        {
            let h = h.clone();
            hidden.connect_clicked(move |_| {
                if h.hidden_expanded.get() {
                    hidden::collapse_hidden(&h);
                } else {
                    hidden::expand_hidden(&h);
                }
            });
        }

        {
            let h = h.clone();
            hotspot_btn.connect_clicked(move |_| {
                let hs = h.model.borrow().hotspot.clone();
                if hs.as_ref().is_some_and(|x| x.active) {
                    h.hotspot_error.borrow_mut().take();
                    let _ = h.cmd_tx.try_send(BackendCmd::StopHotspot);
                    return;
                }
                if let Some(cfg) = hs {
                    let draft_psk = h.hotspot_psk.borrow().clone();
                    let psk = if draft_psk.is_empty() {
                        cfg.psk.clone().unwrap_or_default()
                    } else {
                        draft_psk
                    };
                    let draft_ssid = h.hotspot_ssid.borrow().clone();
                    let ssid = if draft_ssid.is_empty() {
                        cfg.ssid.clone()
                    } else {
                        draft_ssid
                    };
                    if !ssid.trim().is_empty() && (8..=63).contains(&psk.len()) {
                        h.hotspot_error.borrow_mut().take();
                        let _ = h.cmd_tx.try_send(BackendCmd::CreateHotspot { ssid, psk });
                        return;
                    }
                }
                let next = !h.hotspot_expanded.get();
                h.hotspot_expanded.set(next);
                if !next {
                    h.hotspot_error.borrow_mut().take();
                }
                refresh_list(&h);
            });
        }

        {
            let h = h.clone();
            let rescan_w = rescan.downgrade();
            rescan.connect_clicked(move |_| {
                let _ = h.cmd_tx.try_send(BackendCmd::Refresh);
                let _ = h.cmd_tx.try_send(BackendCmd::Rescan);
                if let Some(btn) = rescan_w.upgrade() {
                    btn.set_sensitive(false);
                    gtk4::glib::timeout_add_local_once(
                        std::time::Duration::from_secs(2),
                        move || btn.set_sensitive(true),
                    );
                }
            });
        }

        {
            let tx = cmd_tx.clone();
            if let Some(settings) = gtk4::Settings::default() {
                let h2 = h.clone();
                let window2 = window.clone();
                let revealer2 = popup_revealer.clone();
                let refresh = Rc::new(move || {
                    theme::clear_caches();
                    request_rebuild(&h2);
                    window2.queue_resize();
                    window2.queue_draw();
                    revealer2.queue_resize();
                    revealer2.queue_draw();
                });
                let t1 = tx.clone();
                let r1 = refresh.clone();
                settings.connect_gtk_icon_theme_name_notify(move |_| {
                    r1();
                    let _ = t1.try_send(BackendCmd::RefreshTray);
                });
                let t2 = tx.clone();
                let r2 = refresh.clone();
                settings.connect_gtk_theme_name_notify(move |_| {
                    r2();
                    let _ = t2.try_send(BackendCmd::RefreshTray);
                });
                let t3 = tx.clone();
                let r3 = refresh.clone();
                settings.connect_gtk_xft_dpi_notify(move |_| {
                    r3();
                    let _ = t3.try_send(BackendCmd::RefreshTray);
                });
            }
        }

        {
            let app = app.clone();
            let cmd_tx = cmd_tx.clone();
            qr.connect_clicked(move |_| {
                crate::qr_scan::open_scanner(&app, cmd_tx.clone());
            });
        }

        refresh_list(&h);
        window.set_visible(false);

        {
            let h = h.clone();
            search.connect_search_changed(move |entry| {
                *h.query.borrow_mut() = entry.text().to_string();
                refresh_list(&h);
            });
        }

        {
            let hf = h.clone();
            window.connect_focus_widget_notify(move |w| {
                if let Some(f) = gtk4::prelude::GtkWindowExt::focus(w) {
                    tracing::debug!(focused = %f.css_name(), "focus changed");
                }
                hf.sync_focus();
            });
        }

        {
            let rev_s = popup_revealer.clone();
            let vbox_s = vbox.clone();
            window.connect_scale_factor_notify(move |w| {
                apply_popup_size(&rev_s, &vbox_s, w.scale_factor());
            });
        }
        {
            let window_e = window.clone();
            let revealer_e = popup_revealer.clone();
            let visible_e = visible.clone();
            let flag_e = popup_visible.clone();
            let hide_gen_e = hide_gen.clone();
            let h_e = h.clone();
            let keys = gtk4::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == Key::Escape {
                    flag_e.store(false, Ordering::Relaxed);
                    hide_popup(&window_e, &revealer_e, &visible_e, &hide_gen_e);
                    list::schedule_teardown(&h_e);
                    gtk4::glib::Propagation::Stop
                } else {
                    gtk4::glib::Propagation::Proceed
                }
            });
            window.add_controller(keys);
        }

        {
            let h = h.clone();
            let wifi_switch = wifi_switch.clone();
            let air_switch = air_switch.clone();
            let wifi_icon = wifi_icon.clone();
            let air_icon = air_icon.clone();
            let prog_guard = prog_guard.clone();
            gtk4::glib::MainContext::default().spawn_local(async move {
                while let Ok(ev) = ui_rx.recv().await {
                    match ev {
                        UiEvent::Model(m) => {
                            if wifi_switch.state() != m.wifi_enabled && !m.airplane_mode() {
                                prog_guard.set(true);
                                wifi_switch.set_active(m.wifi_enabled);
                                prog_guard.set(false);
                            }
                            if air_switch.state() != m.airplane_mode() {
                                prog_guard.set(true);
                                air_switch.set_active(m.airplane_mode());
                                prog_guard.set(false);
                            }
                            wifi_switch.set_sensitive(!m.airplane_mode());
                            if m.airplane_mode() && wifi_switch.state() {
                                prog_guard.set(true);
                                wifi_switch.set_active(false);
                                prog_guard.set(false);
                            }
                            let wifi_on = m.wifi_enabled && !m.airplane_mode();
                            wifi_icon.set_icon_name(Some(&themed_icon(if wifi_on {
                                &[
                                    "network-wireless-symbolic",
                                    "network-wireless-disabled-symbolic",
                                ]
                            } else {
                                &[
                                    "network-wireless-disabled-symbolic",
                                    "network-wireless-symbolic",
                                ]
                            })));
                            wifi_icon.set_opacity(if wifi_on { 1.0 } else { 0.5 });
                            air_icon.set_opacity(if m.airplane_mode() { 1.0 } else { 0.5 });
                            let hs_active = UiHandles::hotspot_active(&m);

                            if let Some(active) = m.active_ssid.as_deref() {
                                h.focus_ssid.borrow_mut().take();
                                h.errors.borrow_mut().remove(active);
                                h.connecting.borrow_mut().remove(active);
                                h.pw_attempt.borrow_mut().remove(active);
                                h.unsaved_attempt.borrow_mut().remove(active);
                                h.pw_drafts.borrow_mut().remove(active);
                                h.err_token.borrow_mut().remove(active);
                            }

                            let was_active = UiHandles::hotspot_active(&h.model.borrow());
                            if hs_active && !was_active {
                                h.hotspot_expanded.set(true);
                            } else if was_active && !hs_active {
                                h.hotspot_expanded.set(false);
                                h.hotspot_error.borrow_mut().take();
                            }

                            if let Some(hs) = m.hotspot.as_ref() {
                                if h.hotspot_ssid.borrow().is_empty() {
                                    *h.hotspot_ssid.borrow_mut() = hs.ssid.clone();
                                }
                                if h.hotspot_psk.borrow().is_empty()
                                    && let Some(psk) = hs.psk.clone()
                                {
                                    *h.hotspot_psk.borrow_mut() = psk;
                                }
                            }
                            apply_model(&h, m);
                        }
                        UiEvent::SecretsNeeded {
                            ssid,
                            path,
                            request_new,
                        } => {
                            h.connecting.borrow_mut().remove(&ssid);
                            if request_new {
                                if h.unsaved_attempt.borrow_mut().remove(&ssid) {
                                    h.pending_secret_paths.borrow_mut().remove(&ssid);
                                    let _ = h.cmd_tx.try_send(BackendCmd::Forget(ssid.clone()));
                                } else {
                                    h.pending_secret_paths
                                        .borrow_mut()
                                        .insert(ssid.clone(), path);
                                }
                                h.pw_drafts.borrow_mut().remove(&ssid);
                                *h.expanded.borrow_mut() = Some(ssid.clone());
                                *h.focus_ssid.borrow_mut() = Some(ssid);
                                request_rebuild(&h);
                            } else {
                                h.errors.borrow_mut().remove(&ssid);
                                h.err_token.borrow_mut().remove(&ssid);
                                h.pending_secret_paths
                                    .borrow_mut()
                                    .insert(ssid.clone(), path);
                                *h.expanded.borrow_mut() = Some(ssid.clone());
                                *h.focus_ssid.borrow_mut() = Some(ssid);
                                request_rebuild(&h);
                            }
                        }
                        UiEvent::SsidError { ssid, message } => {
                            h.connecting.borrow_mut().remove(&ssid);
                            h.pw_attempt.borrow_mut().remove(&ssid);
                            h.unsaved_attempt.borrow_mut().remove(&ssid);
                            *h.expanded.borrow_mut() = Some(ssid.clone());
                            *h.focus_ssid.borrow_mut() = Some(ssid.clone());
                            row::flash_error(&h, &ssid, &message);
                        }
                        UiEvent::BackendError(message) => {
                            *h.hotspot_error.borrow_mut() = Some(message);
                            request_rebuild(&h);
                        }
                        UiEvent::Speeds { up_bps, down_bps } => {
                            *h.speeds.borrow_mut() = (up_bps, down_bps);
                            if let Some((down_label, up_label)) = h.speed_labels.borrow().clone() {
                                down_label.set_text(&format!(
                                    "↓ {}",
                                    crate::state::format_rate(down_bps)
                                ));
                                up_label
                                    .set_text(&format!("↑ {}", crate::state::format_rate(up_bps)));
                            }
                        }
                    }
                }
            });
        }

        {
            let window = window.clone();
            let visible = visible.clone();
            let flag = popup_visible.clone();
            let hide_gen = hide_gen.clone();
            let h_open = h.clone();
            let search_open = search.clone();
            let tx_open = cmd_tx.clone();
            let vbox_open = vbox.clone();
            gtk4::glib::MainContext::default().spawn_local(async move {
                while let Ok(pos) = toggle_rx.recv().await {
                    if visible.get() {
                        flag.store(false, Ordering::Relaxed);
                        hide_popup(&window, &popup_revealer, &visible, &hide_gen);
                        list::schedule_teardown(&h_open);
                        continue;
                    }
                    if let Some((x, _y)) = pos {
                        place_near(&popup_revealer, x);
                    }
                    *h_open.expanded.borrow_mut() = None;
                    h_open.focus_ssid.borrow_mut().take();
                    h_open.connecting.borrow_mut().clear();
                    h_open.errors.borrow_mut().clear();
                    h_open.err_token.borrow_mut().clear();
                    h_open.pw_drafts.borrow_mut().clear();
                    h_open.pending_secret_paths.borrow_mut().clear();
                    h_open.pw_attempt.borrow_mut().clear();
                    h_open.unsaved_attempt.borrow_mut().clear();
                    h_open.hidden_expanded.set(false);
                    h_open.hidden_closing.set(false);
                    h_open.hidden_error.borrow_mut().take();
                    h_open.hotspot_error.borrow_mut().take();
                    h_open.hotspot_expanded.set(false);
                    *h_open.speeds.borrow_mut() = (None, None);
                    *h_open.query.borrow_mut() = String::new();
                    search_open.set_text("");

                    visible.set(true);
                    flag.store(true, Ordering::Relaxed);
                    window.set_visible(true);
                    window.present();
                    apply_popup_size(
                        &popup_revealer,
                        &vbox_open,
                        output_scale_for_x(pos.map(|(x, _)| x)),
                    );
                    {
                        let rev_a = popup_revealer.clone();
                        gtk4::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(500),
                            move || {
                                tracing::debug!(
                                    alloc_w = rev_a.width(),
                                    alloc_h = rev_a.height(),
                                    "popup allocated"
                                );
                            },
                        );
                    }
                    hide_gen.set(hide_gen.get() + 1);

                    refresh_list(&h_open);
                    let _ = tx_open.try_send(BackendCmd::Rescan);

                    popup_revealer.set_reveal_child(false);
                    {
                        let v = visible.clone();
                        let pop = popup_revealer.clone();
                        gtk4::glib::timeout_add_local_once(
                            std::time::Duration::from_millis(8),
                            move || {
                                if v.get() {
                                    pop.set_reveal_child(true);
                                }
                            },
                        );
                    }
                }
            });
        }

        {
            let app = app.clone();
            gtk4::glib::MainContext::default().spawn_local(async move {
                if quit_rx.recv().await.is_ok() {
                    app.quit();
                }
            });
        }
    });

    app.run();
}
