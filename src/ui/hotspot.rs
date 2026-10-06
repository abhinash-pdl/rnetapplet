use gtk4::prelude::*;

use super::list::refresh_list;
use super::placement::open_editor;
use super::row::action_button;
use super::state::UiHandles;
use super::theme::themed_icon;
use crate::state::BackendCmd;

pub(crate) fn hotspot_section_row(h: &UiHandles) -> gtk4::ListBoxRow {
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    vbox.set_margin_start(8);
    vbox.set_margin_end(8);
    vbox.set_margin_top(8);
    vbox.set_margin_bottom(8);

    if let Some(e) = h.hotspot_error.borrow().clone() {
        let el = gtk4::Label::new(Some(&e));
        el.set_halign(gtk4::Align::Start);
        el.add_css_class("error");
        el.set_wrap(true);
        vbox.append(&el);
    }

    let saved = h.model.borrow().hotspot.clone();
    if saved.as_ref().is_some_and(|x| x.active) {
        let Some(hs) = saved else {
            let row = gtk4::ListBoxRow::new();
            row.set_activatable(false);
            return row;
        };
        let hbox = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        let icon = gtk4::Image::from_icon_name(&themed_icon(&[
            "network-wireless-hotspot-symbolic",
            "network-wireless-symbolic",
        ]));
        icon.set_pixel_size(24);
        hbox.append(&icon);
        let name = gtk4::Label::new(Some(&hs.ssid));
        name.set_halign(gtk4::Align::Start);
        name.set_hexpand(true);
        hbox.append(&name);
        let edit = gtk4::Button::with_label("Edit");
        edit.add_css_class("flat");
        edit.set_tooltip_text(Some("Edit"));
        edit.connect_clicked(move |_| open_editor());
        hbox.append(&edit);
        hbox.append(&action_button("Stop", true, BackendCmd::StopHotspot, h));
        vbox.append(&hbox);
        if let Some(psk) = hs.psk.as_deref() {
            let key = gtk4::Label::new(Some(&format!("Password: {psk}")));
            key.set_halign(gtk4::Align::Start);
            key.add_css_class("dim-label");
            key.add_css_class("rnet-hotspot-key");
            vbox.append(&key);
        }
    } else {
        if h.hotspot_ssid.borrow().is_empty() {
            *h.hotspot_ssid.borrow_mut() = String::from("RnetHotspot");
        }
        let ssid_entry = gtk4::Entry::new();
        ssid_entry.set_placeholder_text(Some("Name"));
        ssid_entry.set_text(&h.hotspot_ssid.borrow().clone());
        UiHandles::track_entry(&h.hotspot_entries, &ssid_entry.clone().upcast());
        vbox.append(&ssid_entry);

        let pass_entry = gtk4::PasswordEntry::new();
        pass_entry.set_width_chars(12);
        pass_entry.set_show_peek_icon(false);
        pass_entry.set_placeholder_text(Some("Password"));
        pass_entry.set_text(&h.hotspot_psk.borrow().clone());
        UiHandles::track_entry(&h.hotspot_entries, &pass_entry.clone().upcast());
        vbox.append(&pass_entry);

        let btn_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
        btn_row.set_halign(gtk4::Align::End);
        let cancel = gtk4::Button::with_label("Close");
        cancel.add_css_class("flat");
        let start = gtk4::Button::with_label("Start");
        start.set_sensitive(
            !ssid_entry.text().trim().is_empty() && (8..=63).contains(&pass_entry.text().len()),
        );
        btn_row.append(&cancel);
        btn_row.append(&start);
        vbox.append(&btn_row);

        {
            let h = h.clone();
            cancel.connect_clicked(move |_| {
                h.hotspot_expanded.set(false);
                h.hotspot_error.borrow_mut().take();
                refresh_list(&h);
            });
        }
        {
            let start_w = start.downgrade();
            let pass_w = pass_entry.downgrade();
            let draft_ssid = h.hotspot_ssid.clone();
            let draft_psk = h.hotspot_psk.clone();
            ssid_entry.connect_changed(move |e| {
                *draft_ssid.borrow_mut() = e.text().to_string();
                let Some(pass) = pass_w.upgrade() else {
                    return;
                };
                *draft_psk.borrow_mut() = pass.text().to_string();
                let Some(start) = start_w.upgrade() else {
                    return;
                };
                start.set_sensitive(
                    !e.text().trim().is_empty() && (8..=63).contains(&pass.text().len()),
                );
            });
        }
        {
            let start_w = start.downgrade();
            let ssid_w = ssid_entry.downgrade();
            let draft_psk = h.hotspot_psk.clone();
            pass_entry.connect_changed(move |e| {
                *draft_psk.borrow_mut() = e.text().to_string();
                let Some(ssid) = ssid_w.upgrade() else {
                    return;
                };
                let Some(start) = start_w.upgrade() else {
                    return;
                };
                start.set_sensitive(
                    !ssid.text().trim().is_empty() && (8..=63).contains(&e.text().len()),
                );
            });
        }
        {
            let h = h.clone();
            let ssid_w = ssid_entry.downgrade();
            let pass_w = pass_entry.downgrade();
            start.connect_clicked(move |btn| {
                let Some(ssid_entry) = ssid_w.upgrade() else {
                    return;
                };
                let Some(pass_entry) = pass_w.upgrade() else {
                    return;
                };
                let ssid = ssid_entry.text().trim().to_string();
                let psk = pass_entry.text().to_string();
                if ssid.is_empty() {
                    *h.hotspot_error.borrow_mut() = Some("Enter a name".into());
                    refresh_list(&h);
                    return;
                }
                if !(8..=63).contains(&psk.len()) {
                    *h.hotspot_error.borrow_mut() = Some("Password must be 8-63 characters".into());
                    refresh_list(&h);
                    return;
                }
                h.hotspot_error.borrow_mut().take();
                h.hotspot_expanded.set(false);
                *h.hotspot_ssid.borrow_mut() = ssid.clone();
                *h.hotspot_psk.borrow_mut() = psk.clone();
                btn.grab_focus();
                let _ = h.cmd_tx.try_send(BackendCmd::CreateHotspot { ssid, psk });
            });
        }
    }

    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(&vbox));
    row.set_activatable(false);
    row
}

pub(crate) fn hotspot_shown(h: &UiHandles) -> bool {
    h.hotspot_expanded.get()
        || UiHandles::hotspot_active(&h.model.borrow())
        || h.hotspot_error.borrow().is_some()
}
