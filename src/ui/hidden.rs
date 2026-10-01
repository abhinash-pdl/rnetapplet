use std::rc::Rc;

use gtk4::prelude::*;

use super::list::{refresh_list, request_rebuild};
use super::row::{mark_connecting, section_label};
use super::state::UiHandles;
use crate::state::BackendCmd;

pub(crate) fn hidden_form_row(h: &UiHandles) -> gtk4::ListBoxRow {
    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 6);
    vbox.set_margin_start(40);
    vbox.set_margin_end(12);
    vbox.set_margin_bottom(10);

    if let Some(e) = h.hidden_error.borrow().clone() {
        let el = gtk4::Label::new(Some(&e));
        el.set_halign(gtk4::Align::Start);
        el.add_css_class("error");
        el.set_wrap(true);
        vbox.append(&el);
    }

    let ssid_entry = gtk4::Entry::new();
    ssid_entry.set_placeholder_text(Some("Hidden SSID"));
    UiHandles::track_entry(&h.hidden_entries, &ssid_entry.clone().upcast());
    let motion_ssid = gtk4::EventControllerMotion::new();
    motion_ssid.connect_enter({
        let h = h.clone();
        move |_, _, _| h.sync_hover(true)
    });
    motion_ssid.connect_leave({
        let h = h.clone();
        move |_| h.sync_hover(false)
    });
    ssid_entry.add_controller(motion_ssid);
    vbox.append(&ssid_entry);

    let pass_entry = gtk4::PasswordEntry::new();
    pass_entry.set_width_chars(12);
    pass_entry.set_show_peek_icon(true);
    pass_entry.set_placeholder_text(Some("Password (optional)"));
    UiHandles::track_entry(&h.hidden_entries, &pass_entry.clone().upcast());
    let motion_pass = gtk4::EventControllerMotion::new();
    motion_pass.connect_enter({
        let h = h.clone();
        move |_, _, _| h.sync_hover(true)
    });
    motion_pass.connect_leave({
        let h = h.clone();
        move |_| h.sync_hover(false)
    });
    pass_entry.add_controller(motion_pass);
    vbox.append(&pass_entry);

    let btn_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk4::Align::End);
    let cancel = gtk4::Button::with_label("Cancel");
    cancel.add_css_class("flat");
    let connect = gtk4::Button::with_label("Connect");
    connect.add_css_class("suggested-action");
    btn_row.append(&cancel);
    btn_row.append(&connect);
    vbox.append(&btn_row);

    {
        let h = h.clone();
        cancel.connect_clicked(move |_| collapse_hidden(&h));
    }
    let submit = Rc::new({
        let h = h.clone();
        let ssid_w = ssid_entry.downgrade();
        let pass_w = pass_entry.downgrade();
        move || {
            let Some(ssid_entry) = ssid_w.upgrade() else {
                return;
            };
            let Some(pass_entry) = pass_w.upgrade() else {
                return;
            };
            let ssid = ssid_entry.text().trim().to_string();
            if ssid.is_empty() {
                *h.hidden_error.borrow_mut() = Some("Enter the network name".into());
                request_rebuild(&h);
                return;
            }
            let psk = pass_entry.text().to_string();
            if !psk.is_empty() && psk.len() < 8 {
                *h.hidden_error.borrow_mut() = Some("Password needs at least 8 characters".into());
                request_rebuild(&h);
                return;
            }
            h.hidden_error.borrow_mut().take();
            collapse_hidden(&h);
            mark_connecting(&h, &ssid, !psk.is_empty(), false);
            let _ = h.cmd_tx.try_send(BackendCmd::ConnectHidden { ssid, psk });
        }
    });
    {
        let submit = submit.clone();
        connect.connect_clicked(move |_| submit());
    }
    {
        let submit = submit.clone();
        pass_entry.connect_activate(move |_| submit());
    }

    let row = gtk4::ListBoxRow::new();
    row.set_child(Some(&vbox));
    row.set_activatable(false);
    row
}

pub(crate) fn ensure_hidden_card(h: &UiHandles) -> (gtk4::Revealer, gtk4::ListBoxRow) {
    if let Some(c) = h.hidden_card.borrow().as_ref() {
        return c.clone();
    }
    let holder = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    holder.append(&section_label("Hidden network"));
    holder.append(&hidden_form_row(h));
    let rev = gtk4::Revealer::new();
    rev.set_transition_type(gtk4::RevealerTransitionType::SlideDown);
    rev.set_transition_duration(160);
    rev.set_reveal_child(false);
    rev.set_child(Some(&holder));
    let wrap = gtk4::ListBoxRow::new();
    wrap.set_activatable(false);
    wrap.set_selectable(false);
    wrap.set_child(Some(&rev));
    let pair = (rev.clone(), wrap.clone());
    *h.hidden_card.borrow_mut() = Some(pair.clone());
    pair
}

pub(crate) fn expand_hidden(h: &UiHandles) {
    h.hidden_closing.set(false);
    h.hidden_expanded.set(true);
    h.hidden_error.borrow_mut().take();
    let (rev, _) = ensure_hidden_card(h);
    gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(8), move || {
        rev.set_reveal_child(true);
    });
    refresh_list(h);
}

pub(crate) fn collapse_hidden(h: &UiHandles) {
    h.hidden_expanded.set(false);
    h.hidden_error.borrow_mut().take();
    if h.hidden_card.borrow().is_some() {
        h.hidden_closing.set(true);
        if let Some((rev, _)) = h.hidden_card.borrow().as_ref() {
            rev.set_reveal_child(false);
        }
        let h2 = h.clone();
        let closing = h.hidden_closing.clone();
        gtk4::glib::timeout_add_local_once(std::time::Duration::from_millis(160), move || {
            closing.set(false);
            refresh_list(&h2);
        });
    } else {
        refresh_list(h);
    }
}
