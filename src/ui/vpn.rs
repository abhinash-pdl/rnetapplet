use gtk4::prelude::*;

use super::row::detail_line;
use super::state::UiHandles;
use super::theme::themed_icon;
use crate::state::{BackendCmd, VpnConnection};

pub(crate) fn vpn_row(vpn: &VpnConnection, h: &UiHandles) -> gtk4::ListBoxRow {
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);

    let icon = gtk4::Image::from_icon_name(&themed_icon(&["network-vpn-symbolic", "network-vpn"]));
    icon.set_pixel_size(24);
    icon.set_valign(gtk4::Align::Center);
    hbox.append(&icon);

    let name_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    name_box.set_hexpand(true);
    let label = gtk4::Label::new(Some(&vpn.id));
    label.set_halign(gtk4::Align::Start);
    label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    label.set_max_width_chars(24);
    name_box.append(&label);
    if vpn.active {
        let badge = gtk4::Label::new(Some("active"));
        badge.add_css_class("dim-label");
        name_box.append(&badge);
    }
    hbox.append(&name_box);

    let id = vpn.id.clone();
    let active = vpn.active;
    let tx = h.cmd_tx.clone();
    let btn = gtk4::Button::with_label(if active { "Disconnect" } else { "Connect" });
    btn.add_css_class("flat");
    btn.set_halign(gtk4::Align::End);
    btn.connect_clicked(move |_| {
        let cmd = if active {
            BackendCmd::DeactivateVpn(id.clone())
        } else {
            BackendCmd::ActivateVpn(id.clone())
        };
        let _ = tx.try_send(cmd);
    });
    hbox.append(&btn);
    vbox.append(&hbox);

    if let Some(err) = h.errors.borrow().get(&vpn.id) {
        let el = gtk4::Label::new(Some(err));
        el.set_halign(gtk4::Align::Start);
        el.add_css_class("error");
        el.set_wrap(true);
        el.set_margin_start(40);
        el.set_margin_end(12);
        el.set_margin_bottom(8);
        vbox.append(&el);
    } else if !vpn.active {
        let hint = detail_line("Saved VPN connection");
        hint.set_margin_start(40);
        hint.set_margin_end(12);
        hint.set_margin_bottom(8);
        vbox.append(&hint);
    }

    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(&vbox));
    row.set_activatable(false);
    row.add_css_class("rnet-row");
    row
}
