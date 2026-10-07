mod appearance;
mod hidden;
mod hotspot;
mod list;
mod notify;
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

use crate::nm_client::connect::MSG_WRONG_PASSWORD;
use gtk4::gdk::Key;
use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use list::{apply_model, refresh_list, request_rebuild};
use placement::{hide_popup, open_editor, output_scale_for_x, place_near};
use row::flat_icon_button;
use state::UiHandles;
use theme::{header_icon_px, themed_icon};

use crate::state::{BackendCmd, Model, UiEvent};

const POPUP_W: i32 = 425;

const POPUP_MAX_H: i32 = 530;

const POPUP_MIN_LIST_H: i32 = 96;

mod motion;
use motion::{MIN_HEIGHT_JITTER, SizeAnim};

const CSS: &str = "
window.rnet-window, window.rnet-window > .background, window.rnet-window:backdrop, window.rnet-window:backdrop > .background, window.rnet-window.background, window.rnet-window.background:backdrop { background-color: transparent; background-image: none; box-shadow: none; border: none; border-radius: 0; }
.rnet-card { background-color: @theme_bg_color; border-radius: 12px; padding: 8px 6px; }
 .rnet-window list, .rnet-window listview { background-color: transparent; }
 .rnet-window scrolledwindow > overshoot { background: transparent; box-shadow: none; }
 .rnet-window image { opacity: 1; }
 .rnet-title { margin: 0; }
 .rnet-window spinner { color: @theme_fg_color; }
 .rnet-hbtn { min-height: 20px; min-width: 24px; padding: 2px 4px; margin: 0; }
 .rnet-hbtn > image { -gtk-icon-size: 16px; }
 .rnet-section-row { background-color: transparent; }
 .rnet-section-row:hover, .rnet-section-row:active, .rnet-section-row:selected { background-color: transparent; }
 .rnet-row { min-height: 48px; margin: 1px 0; border-radius: 6px; background-color: @theme_bg_color; background-image: none; }
 .rnet-row:hover { background-color: alpha(@theme_fg_color, 0.08); background-image: none; }
 .rnet-row:active { background-color: alpha(@theme_fg_color, 0.13); background-image: none; }
 .flat { transition: background-color 120ms ease-in-out; }
 .rnet-chip {
     padding: 1px 7px;
     margin-left: 2px;
     border-radius: 10px;
     background-image: none;
     background-color: alpha(@theme_fg_color, 0.14);
     color: @theme_fg_color;
     font-size: 0.8em;
     font-weight: bold;
 }
 .rnet-chip-saved {
     padding: 1px 7px;
     margin-left: 2px;
     border-radius: 10px;
     background-image: none;
     background-color: alpha(@theme_fg_color, 0.09);
     color: alpha(@theme_fg_color, 0.75);
     font-size: 0.8em;
 }
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

fn measure_chrome(
    card: &gtk4::Box,
    list_scroll: &gtk4::ScrolledWindow,
    w: i32,
    min_list: i32,
) -> i32 {
    card.set_size_request(w, -1);
    list_scroll.set_size_request(-1, min_list);
    let (_, at_floor, _, _) = card.measure(gtk4::Orientation::Vertical, w);
    (at_floor - min_list).max(0)
}

fn scale_limits(scale: i32) -> (i32, i32, i32) {
    let s = scale.max(1);
    (
        (POPUP_W as f64 / s as f64).ceil() as i32,
        (POPUP_MAX_H as f64 / s as f64).ceil() as i32,
        (POPUP_MIN_LIST_H as f64 / s as f64).ceil() as i32,
    )
}

pub(crate) fn measure_popup(
    card: &gtk4::Box,
    list_scroll: &gtk4::ScrolledWindow,
    scale: i32,
    chrome: i32,
) -> (i32, i32, i32) {
    let (w, max_h, min_list) = scale_limits(scale);
    let (_, natural_w, _, _) = card.measure(gtk4::Orientation::Horizontal, -1);
    if natural_w > w {
        tracing::warn!(
            natural = natural_w,
            requested = w,
            "popup content wants more width than requested; it would show wider"
        );
    }
    let rows_natural = list_scroll
        .child()
        .map(|c| c.measure(gtk4::Orientation::Vertical, w).1)
        .unwrap_or(0);
    let list_room = (max_h - chrome).max(min_list);
    let list_h = rows_natural.clamp(min_list, list_room);
    (w, (chrome + list_h).min(max_h), list_h)
}

pub(crate) fn apply_popup_height(
    revealer: &gtk4::Revealer,
    card: &gtk4::Box,
    list_scroll: &gtk4::ScrolledWindow,
    w: i32,
    h: i32,
    list_h: i32,
    max_h: i32,
) {
    card.set_size_request(w, h);
    revealer.set_size_request(w, -1);
    list_scroll.set_size_request(-1, list_h);
    tracing::debug!(
        w,
        h,
        list_h,
        max_h,
        card_ah = card.height(),
        "popup height pinned"
    );
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
        let errors: Rc<RefCell<HashMap<String, String>>> = Rc::new(RefCell::new(HashMap::new()));
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
        let hotspot_closing: Rc<Cell<bool>> = Rc::new(Cell::new(false));
        let hotspot_card: Rc<RefCell<Option<(gtk4::Revealer, gtk4::ListBoxRow)>>> =
            Rc::new(RefCell::new(None));
        let hotspot_ssid: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let hotspot_psk: Rc<RefCell<String>> = Rc::new(RefCell::new(String::new()));
        let speeds: Rc<RefCell<(Option<u64>, Option<u64>)>> = Rc::new(RefCell::new((None, None)));
        let speed_labels: Rc<RefCell<Option<(gtk4::Label, gtk4::Label)>>> =
            Rc::new(RefCell::new(None));
        let revealers: Rc<RefCell<HashMap<String, gtk4::Revealer>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let chevrons: Rc<RefCell<HashMap<String, gtk4::Image>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let rows: Rc<RefCell<HashMap<String, gtk4::ListBoxRow>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let action_btns: Rc<RefCell<HashMap<String, state::ActionBtn>>> = Default::default();
        let card_actions: Rc<RefCell<HashMap<String, gtk4::Box>>> = Default::default();
        let status: Rc<RefCell<HashMap<String, gtk4::Box>>> = Rc::new(RefCell::new(HashMap::new()));
        let error_labels: Rc<RefCell<HashMap<String, gtk4::Label>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let ssid_labels: Rc<RefCell<HashMap<String, gtk4::Label>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let pw_entries: Rc<RefCell<HashMap<String, gtk4::PasswordEntry>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let strength_setters: Rc<RefCell<HashMap<String, theme::StrengthSetter>>> =
            Rc::new(RefCell::new(HashMap::new()));
        let hide_gen: Rc<Cell<u64>> = Rc::new(Cell::new(0));
        let pending_height: Rc<Cell<i32>> = Rc::new(Cell::new(0));
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

        search.set_max_width_chars(11);
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

        scroll.set_propagate_natural_width(false);
        scroll.set_overlay_scrolling(true);

        scroll.set_policy(gtk4::PolicyType::Never, gtk4::PolicyType::Automatic);

        scroll.vscrollbar().set_visible(false);
        scroll.hscrollbar().set_visible(false);
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
        popup_revealer.set_transition_duration(motion::REVEAL_MS);
        popup_revealer.set_reveal_child(false);
        popup_revealer.set_child(Some(&vbox));
        popup_revealer.set_halign(gtk4::Align::End);
        popup_revealer.set_valign(gtk4::Align::Start);
        overlay.add_overlay(&popup_revealer);

        let expanded = Rc::new(std::cell::RefCell::new(None::<String>));
        let extra_heights = Rc::new(std::cell::RefCell::new(std::collections::HashMap::<
            String,
            i32,
        >::new()));
        let popup_scale = Rc::new(std::cell::Cell::new(output_scale_for_x(None)));
        let applied_w = Rc::new(std::cell::Cell::new(0));
        let applied_h = Rc::new(std::cell::Cell::new(0));
        let base_list_h = Rc::new(std::cell::Cell::new(0));
        let anim_until = Rc::new(std::cell::Cell::new(0i64));

        let chrome = Rc::new(std::cell::Cell::new(0));
        {
            let (w, max_h, min_list) = scale_limits(popup_scale.get());
            chrome.set(measure_chrome(&vbox, &scroll, w, min_list));
            let (w, h, list_h) = measure_popup(&vbox, &scroll, popup_scale.get(), chrome.get());
            applied_w.set(w);
            applied_h.set(h);
            apply_popup_height(&popup_revealer, &vbox, &scroll, w, h, list_h, max_h);
        }

        let size = {
            let rev = popup_revealer.clone();
            let card = vbox.clone();
            let sc = scroll.clone();
            let scale = popup_scale.clone();
            let applied = applied_h.clone();
            let chrome = chrome.clone();
            let apply: Rc<dyn Fn(i32)> = Rc::new(move |h| {
                let (w, max_h, min_list) = scale_limits(scale.get());
                let list_h = (h - chrome.get()).max(min_list);
                apply_popup_height(&rev, &card, &sc, w, h, list_h, max_h);
                applied.set(h);
            });
            SizeAnim::new(&vbox, apply)
        };

        let grow: Rc<dyn Fn(i32)> = {
            let scale = popup_scale.clone();
            let chrome = chrome.clone();
            let base = base_list_h.clone();
            let size = size.clone();
            Rc::new(move |reserved| {
                let (_, max_h, min_list) = scale_limits(scale.get());
                let ch = chrome.get();
                let want = (base.get().max(min_list) + ch + reserved).clamp(min_list + ch, max_h);
                tracing::debug!(want, base = base.get(), reserved, "grow popup");
                if want != size.to.get() {
                    size.run(want);
                }
            })
        };

        let grow_by: Rc<dyn Fn(i32)> = {
            let scale = popup_scale.clone();
            let chrome = chrome.clone();
            let size = size.clone();
            Rc::new(move |delta: i32| {
                let (_, max_h, min_list) = scale_limits(scale.get());
                let want =
                    (size.to.get().saturating_add(delta)).clamp(min_list + chrome.get(), max_h);
                if want != size.to.get() {
                    tracing::debug!(want, delta, "grow_by popup");
                    size.run(want);
                }
            })
        };

        let fit: Rc<dyn Fn()> = {
            let card = vbox.clone();
            let expanded2 = expanded.clone();
            let base2 = base_list_h.clone();
            let hidden_open = hidden_expanded.clone();
            let hotspot_open = hotspot_expanded.clone();
            let sc = scroll.clone();
            let scale = popup_scale.clone();
            let busy = anim_until.clone();
            let chrome = chrome.clone();
            let size = size.clone();
            let shown = visible.clone();
            let dirty = Rc::new(Cell::new(false));
            let queued = Rc::new(Cell::new(false));
            let last = Rc::new(Cell::new(0i64));
            Rc::new(move || {
                if !shown.get() {
                    return;
                }
                dirty.set(true);
                if queued.get() {
                    return;
                }
                queued.set(true);
                let (card2, sc2) = (card.clone(), sc.clone());
                let (dirty2, queued2, last2) = (dirty.clone(), queued.clone(), last.clone());
                let (expanded3, base3) = (expanded2.clone(), base2.clone());
                let (hidden_open2, hotspot_open2) = (hidden_open.clone(), hotspot_open.clone());
                let (scale2, busy2, chrome2) = (scale.get(), busy.clone(), chrome.get());
                let size2 = size.clone();
                gtk4::glib::idle_add_local_once(move || {
                    let now = motion::now_ms();
                    let wait = (motion::FIT_GAP_MS - (now - last2.get())).max(0);
                    gtk4::glib::timeout_add_local_once(
                        std::time::Duration::from_millis(wait as u64),
                        move || {
                            queued2.set(false);
                            if !dirty2.get() {
                                return;
                            }

                            if motion::now_ms() < busy2.get() {
                                return;
                            }
                            dirty2.set(false);
                            let (_, measured, list_h) =
                                measure_popup(&card2, &sc2, scale2, chrome2);
                            if expanded3.borrow().is_none()
                                && !hidden_open2.get()
                                && !hotspot_open2.get()
                            {
                                base3.set(list_h);
                            }
                            let (_, max_h, min_list) = scale_limits(scale2);
                            let want = measured.clamp(min_list + chrome2, max_h);
                            let diff = want - size2.to.get();
                            if diff.abs() < MIN_HEIGHT_JITTER {
                                return;
                            }
                            size2.run(want);
                            last2.set(motion::now_ms());
                            tracing::debug!(diff, want, base = list_h, "popup refitted to rows");
                        },
                    );
                });
            })
        };
        let h = UiHandles {
            fit,
            grow,
            grow_by,
            extra_heights: extra_heights.clone(),
            anim_until,
            scroll: scroll.clone(),
            list: list.clone(),
            root: window.clone().upcast(),
            model: model.clone(),
            query: query.clone(),
            expanded: expanded.clone(),
            errors: errors.clone(),
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
            hotspot_closing: hotspot_closing.clone(),
            hotspot_card: hotspot_card.clone(),
            hotspot_ssid: hotspot_ssid.clone(),
            hotspot_psk: hotspot_psk.clone(),
            speeds: speeds.clone(),
            speed_labels: speed_labels.clone(),
            revealers: revealers.clone(),
            chevrons: chevrons.clone(),
            rows: rows.clone(),
            action_btns: action_btns.clone(),
            card_actions: card_actions.clone(),
            status: status.clone(),
            error_labels: error_labels.clone(),
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
            rebuild_queued: Rc::new(Cell::new(false)),
            last_strengths: Rc::new(RefCell::new(HashMap::new())),
            ap_misses: Rc::new(RefCell::new(HashMap::new())),
            cmd_tx: cmd_tx.clone(),
        };
        *h.search_entry.borrow_mut() = Some(search.clone().upcast());

        {
            let visible = visible.clone();
            let pending = pending_height.clone();
            let size_m = size.clone();
            popup_revealer.connect_map(move |rev| {
                if visible.get() {
                    rev.set_reveal_child(true);
                }
                let target = pending.take();
                if target > 0 {
                    size_m.run(target);
                }
            });
        }

        {
            let visible = visible.clone();
            let flag = popup_visible.clone();
            let window_c = window.clone();
            let revealer_c = popup_revealer.clone();
            let size_c = size.clone();
            let hide_gen_c = hide_gen.clone();
            let h_c = h.clone();
            window.connect_close_request(move |_| {
                if visible.get() {
                    flag.store(false, Ordering::Relaxed);
                    hide_popup(&window_c, &revealer_c, &size_c, &visible, &hide_gen_c);
                    (h_c.grow_by)(i32::MIN);
                    list::schedule_teardown(&h_c);
                }
                gtk4::glib::Propagation::Stop
            });
        }

        {
            let window_b = window.clone();
            let revealer_b = popup_revealer.clone();
            let size_b = size.clone();
            let visible_b = visible.clone();
            let flag_b = popup_visible.clone();
            let hide_gen_b = hide_gen.clone();
            let h_b = h.clone();
            let gesture = gtk4::GestureClick::new();
            gesture.connect_pressed(move |_, _, _, _| {
                if visible_b.get() {
                    flag_b.store(false, Ordering::Relaxed);
                    hide_popup(&window_b, &revealer_b, &size_b, &visible_b, &hide_gen_b);
                    (h_b.grow_by)(i32::MIN);
                    list::schedule_teardown(&h_b);
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
                if next {
                    hotspot::expand_hotspot(&h);
                } else {
                    hotspot::collapse_hotspot(&h);
                }
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

        window.set_visible(false);

        {
            let h = h.clone();
            search.connect_search_changed(move |entry| {
                *h.query.borrow_mut() = entry.text().to_string();
                list::request_rebuild_coalesced(&h);
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
            let scroll_s = scroll.clone();
            let scale_s = popup_scale.clone();
            let chrome_s = chrome.clone();
            let applied_s = applied_h.clone();
            window.connect_scale_factor_notify(move |w| {
                let scale = w.scale_factor();
                scale_s.set(scale);
                let (width, h, list_h) = measure_popup(&vbox_s, &scroll_s, scale, chrome_s.get());
                let (_, max_h, _) = scale_limits(scale);
                applied_s.set(h);
                apply_popup_height(&rev_s, &vbox_s, &scroll_s, width, h, list_h, max_h);
            });
        }
        {
            let window_e = window.clone();
            let revealer_e = popup_revealer.clone();
            let size_e = size.clone();
            let visible_e = visible.clone();
            let flag_e = popup_visible.clone();
            let hide_gen_e = hide_gen.clone();
            let h_e = h.clone();
            let keys = gtk4::EventControllerKey::new();
            keys.connect_key_pressed(move |_, key, _, _| {
                if key == Key::Escape {
                    flag_e.store(false, Ordering::Relaxed);
                    hide_popup(&window_e, &revealer_e, &size_e, &visible_e, &hide_gen_e);
                    (h_e.grow_by)(i32::MIN);
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

                            {
                                let prev = h.model.borrow().active_ssid.clone();
                                if prev.as_deref() != m.active_ssid.as_deref() {
                                    match m.active_ssid.as_deref() {
                                        Some(ssid) => notify::send("Connected", ssid),
                                        None => {
                                            if let Some(was) = prev.as_deref() {
                                                notify::send("Disconnected", was);
                                            }
                                        }
                                    }
                                }
                            }

                            if let Some(active) = m.active_ssid.as_deref() {
                                h.focus_ssid.borrow_mut().take();
                                h.errors.borrow_mut().remove(active);
                                h.connecting.borrow_mut().remove(active);
                                h.pw_attempt.borrow_mut().remove(active);
                                h.unsaved_attempt.borrow_mut().remove(active);
                                h.pw_drafts.borrow_mut().remove(active);
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

                                if let Some(entry) = h.pw_entries.borrow().get(&ssid).cloned() {
                                    let typed = entry.text().to_string();
                                    h.pw_drafts.borrow_mut().insert(ssid.clone(), typed);
                                }
                                row::flash_error(&h, &ssid, MSG_WRONG_PASSWORD);
                                row::set_expanded(&h, Some(&ssid));
                            } else {
                                h.errors.borrow_mut().remove(&ssid);
                                row::show_error_label(&h, &ssid, None);
                                h.pending_secret_paths
                                    .borrow_mut()
                                    .insert(ssid.clone(), path);
                                row::set_expanded(&h, Some(&ssid));
                            }
                        }
                        UiEvent::SsidError { ssid, message } => {
                            h.connecting.borrow_mut().remove(&ssid);
                            h.pw_attempt.borrow_mut().remove(&ssid);
                            h.unsaved_attempt.borrow_mut().remove(&ssid);
                            if h.ssid_labels.borrow().contains_key(&ssid) {
                                row::flash_error(&h, &ssid, &message);
                                row::set_expanded(&h, Some(&ssid));
                            } else {
                                tracing::info!(%ssid, "connect failed: {message}");
                                *h.hidden_error.borrow_mut() =
                                    Some(format!("Failed to connect to \u{201c}{ssid}\u{201d}"));
                                request_rebuild(&h);
                            }
                        }
                        UiEvent::BackendError(message) => {
                            *h.hotspot_error.borrow_mut() = Some(message);
                            request_rebuild(&h);
                        }
                        UiEvent::ExpandRow(ssid) => {
                            row::set_expanded(&h, Some(&ssid));
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
                        hide_popup(&window, &popup_revealer, &size, &visible, &hide_gen);
                        (h_open.grow_by)(i32::MIN);
                        list::schedule_teardown(&h_open);
                        continue;
                    }
                    *h_open.expanded.borrow_mut() = None;
                    h_open.focus_ssid.borrow_mut().take();
                    h_open.connecting.borrow_mut().clear();
                    h_open.errors.borrow_mut().clear();
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
                    {
                        let tx = h_open.cmd_tx.clone();
                        gtk4::glib::idle_add_local_once(move || {
                            let _ = tx.try_send(crate::state::BackendCmd::Rescan);
                        });
                    }
                    let entry = if pos.is_some() { "tray" } else { "ipc" };
                    let scale = output_scale_for_x(pos.map(|(x, _)| x));
                    tracing::debug!(entry, scale, "opening popup");
                    popup_scale.set(scale);
                    refresh_list(&h_open);
                    let (w, final_h, list_h) =
                        measure_popup(&vbox_open, &scroll, scale, chrome.get());
                    applied_w.set(w);
                    applied_h.set(final_h);
                    base_list_h.set(list_h);
                    pending_height.set(final_h);
                    if let Some((x, y)) = pos {
                        place_near(&popup_revealer, x, y, w, final_h);
                    }
                    *expanded.borrow_mut() = None;
                    extra_heights.borrow_mut().clear();
                    h_open.anim_until.set(0);
                    (h_open.fit)();

                    hide_gen.set(hide_gen.get() + 1);
                    popup_revealer.set_reveal_child(false);
                    window.set_visible(true);
                    window.present();
                    if popup_revealer.is_mapped() {
                        let target = pending_height.take();
                        if target > 0 {
                            size.run(target);
                        }
                        popup_revealer.set_reveal_child(true);
                    }

                    let _ = tx_open.try_send(BackendCmd::Rescan);
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
