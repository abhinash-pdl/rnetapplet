use gtk4::prelude::*;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use super::theme::StrengthSetter;
use crate::state::{BackendCmd, Model};
use crate::ui::list::refresh_list;

#[derive(Clone)]
pub(crate) struct UiHandles {
    pub list: gtk4::ListBox,
    pub root: gtk4::Window,
    pub scroll: gtk4::ScrolledWindow,
    pub model: Rc<RefCell<Arc<Model>>>,
    pub query: Rc<RefCell<String>>,
    pub expanded: Rc<RefCell<Option<String>>>,
    pub errors: Rc<RefCell<HashMap<String, String>>>,
    pub err_token: Rc<RefCell<HashMap<String, Instant>>>,
    pub focus_ssid: Rc<RefCell<Option<String>>>,
    pub pw_drafts: Rc<RefCell<HashMap<String, String>>>,
    pub connecting: Rc<RefCell<HashMap<String, Instant>>>,
    pub pw_attempt: Rc<RefCell<HashSet<String>>>,
    pub unsaved_attempt: Rc<RefCell<HashSet<String>>>,
    pub pending_secret_paths: Rc<RefCell<HashMap<String, String>>>,
    pub hidden_expanded: Rc<Cell<bool>>,
    pub hidden_closing: Rc<Cell<bool>>,
    pub hidden_card: Rc<RefCell<Option<(gtk4::Revealer, gtk4::ListBoxRow)>>>,
    pub hidden_error: Rc<RefCell<Option<String>>>,
    pub hotspot_error: Rc<RefCell<Option<String>>>,
    pub hotspot_expanded: Rc<Cell<bool>>,
    pub hotspot_was: Rc<Cell<bool>>,
    pub hotspot_ssid: Rc<RefCell<String>>,
    pub hotspot_psk: Rc<RefCell<String>>,
    pub speeds: Rc<RefCell<(Option<u64>, Option<u64>)>>,
    pub speed_labels: Rc<RefCell<Option<(gtk4::Label, gtk4::Label)>>>,
    pub revealers: Rc<RefCell<HashMap<String, gtk4::Revealer>>>,
    pub chevrons: Rc<RefCell<HashMap<String, gtk4::Image>>>,
    pub connect_btns: Rc<RefCell<HashMap<String, gtk4::Button>>>,
    pub submits: Rc<RefCell<HashMap<String, gtk4::Button>>>,
    pub flips: Rc<RefCell<HashMap<String, std::rc::Rc<crate::ui::motion::Flip>>>>,
    pub ssid_labels: Rc<RefCell<HashMap<String, gtk4::Label>>>,
    pub pw_entries: Rc<RefCell<HashMap<String, gtk4::PasswordEntry>>>,
    pub strength_setters: Rc<RefCell<HashMap<String, StrengthSetter>>>,
    pub visible: Rc<Cell<bool>>,
    pub search_entry: Rc<RefCell<Option<gtk4::Widget>>>,
    pub hidden_entries: Rc<RefCell<Vec<gtk4::Widget>>>,
    pub hotspot_entries: Rc<RefCell<Vec<gtk4::Widget>>>,
    pub was_editing: Rc<Cell<bool>>,
    pub in_refresh: Rc<Cell<bool>>,
    pub pw_hovered: Rc<Cell<bool>>,
    pub scan_frozen: Arc<AtomicBool>,
    pub pending_model: Rc<RefCell<Option<Arc<crate::state::Model>>>>,
    pub pending_rebuild: Rc<Cell<bool>>,
    pub last_strengths: Rc<RefCell<HashMap<String, u8>>>,
    pub ap_misses: Rc<RefCell<HashMap<String, u8>>>,
    pub cmd_tx: async_channel::Sender<BackendCmd>,
}

impl UiHandles {
    pub fn hotspot_active(m: &Model) -> bool {
        m.hotspot.as_ref().is_some_and(|h| h.active)
    }

    pub fn active_ssid(m: &Model) -> Option<&str> {
        if Self::hotspot_active(m) {
            None
        } else {
            m.active_ssid.as_deref()
        }
    }

    pub(crate) fn track_entry(slot: &Rc<RefCell<Vec<gtk4::Widget>>>, w: &gtk4::Widget) {
        if !slot.borrow().iter().any(|e| e == w) {
            slot.borrow_mut().push(w.clone());
        }
    }

    pub fn focus_widget(&self) -> Option<gtk4::Widget> {
        gtk4::prelude::GtkWindowExt::focus(&self.root)
    }

    fn focused_within(&self, roots: &[gtk4::Widget]) -> bool {
        let Some(fw) = self.focus_widget() else {
            return false;
        };
        roots.iter().any(|r| &fw == r || fw.is_ancestor(r))
    }

    fn pw_widgets(&self) -> Vec<gtk4::Widget> {
        self.pw_entries
            .borrow()
            .values()
            .map(|e| e.clone().upcast())
            .collect()
    }

    pub fn any_entry_focused(&self) -> bool {
        let mut roots = self.pw_widgets();
        roots.extend(self.hidden_entries.borrow().iter().cloned());
        roots.extend(self.hotspot_entries.borrow().iter().cloned());
        if let Some(s) = self.search_entry.borrow().clone() {
            roots.push(s);
        }
        self.focused_within(&roots)
    }

    pub fn search_is_focused(&self) -> bool {
        let Some(s) = self.search_entry.borrow().clone() else {
            return false;
        };
        self.focused_within(std::slice::from_ref(&s))
    }

    pub fn hidden_is_focused(&self) -> bool {
        let roots = self.hidden_entries.borrow().clone();
        self.focused_within(&roots)
    }

    pub fn hotspot_is_focused(&self) -> bool {
        let roots = self.hotspot_entries.borrow().clone();
        self.focused_within(&roots)
    }

    pub fn typing_in_password(&self) -> bool {
        let expanded = self.expanded.borrow();
        let Some(target) = expanded.as_deref() else {
            return false;
        };
        let Some(entry) = self.pw_entries.borrow().get(target).cloned() else {
            return false;
        };
        let entry: gtk4::Widget = entry.upcast();
        self.focused_within(std::slice::from_ref(&entry))
    }

    pub fn editing_protected(&self) -> bool {
        self.typing_in_password()
            || self.hidden_is_focused()
            || self.hotspot_is_focused()
            || self.pw_hovered.get()
    }

    pub fn refresh_blocked(&self) -> bool {
        self.editing_protected()
    }

    pub fn sync_scan_lock(&self) {
        let frozen = self.any_entry_focused() || self.pw_hovered.get();
        tracing::debug!(frozen, expanded = ?self.expanded.borrow(), "scan lock sync");
        self.scan_frozen.store(frozen, Ordering::Relaxed);
    }

    pub fn sync_focus(&self) {
        self.sync_scan_lock();
        let editing = self.editing_protected();
        let was = self.was_editing.replace(editing);
        if was && !editing && self.visible.get() {
            if self.pending_rebuild.replace(false) {
                refresh_list(self);
            } else {
                self.maybe_restore_pw();
            }
        }
    }

    fn maybe_restore_pw(&self) {
        let Some(ssid) = self.focus_ssid.borrow().clone() else {
            return;
        };
        if self.expanded.borrow().as_deref() != Some(ssid.as_str()) {
            return;
        }
        if self.any_entry_focused() {
            return;
        }
        if let Some(fw) = self.focus_widget()
            && fw.downcast_ref::<gtk4::Button>().is_some()
        {
            return;
        }
        let h2 = self.clone();
        gtk4::glib::idle_add_local_once(move || {
            if !h2.visible.get() {
                return;
            }
            if h2.any_entry_focused() {
                return;
            }
            if let Some(fw) = h2.focus_widget()
                && fw.downcast_ref::<gtk4::Button>().is_some()
            {
                return;
            }
            let Some(entry) = h2.pw_entries.borrow().get(&ssid).cloned() else {
                return;
            };
            if entry.is_mapped() {
                entry.grab_focus();
            } else {
                let done = std::cell::Cell::new(false);
                entry.connect_map(move |w| {
                    if done.get() || !w.is_mapped() {
                        return;
                    }
                    done.set(true);
                    w.grab_focus();
                });
            }
        });
    }

    pub fn sync_hover(&self, active: bool) {
        self.pw_hovered.set(active);
        self.sync_scan_lock();
        if !active && self.pending_rebuild.replace(false) {
            refresh_list(self);
        }
    }

    pub fn prune_drafts(&self) {
        let live: HashSet<String> = self
            .model
            .borrow()
            .aps
            .iter()
            .map(|a| a.ssid.clone())
            .chain(self.model.borrow().active_ssid.clone())
            .collect();
        self.pw_drafts.borrow_mut().retain(|k, _| live.contains(k));
        self.connecting.borrow_mut().retain(|k, _| live.contains(k));
        self.err_token.borrow_mut().retain(|k, _| live.contains(k));
        self.pending_secret_paths
            .borrow_mut()
            .retain(|k, _| live.contains(k));
        for set in [&self.pw_attempt, &self.unsaved_attempt] {
            set.borrow_mut().retain(|k| live.contains(k));
        }
    }
}

pub(crate) fn shell_equivalent(a: &Model, b: &Model) -> bool {
    a.active_ssid == b.active_ssid
        && a.active_iface == b.active_iface
        && a.active_ipv4 == b.active_ipv4
        && a.nm_online == b.nm_online
        && a.wifi_enabled == b.wifi_enabled
        && a.networking_enabled == b.networking_enabled
        && UiHandles::hotspot_active(a) == UiHandles::hotspot_active(b)
        && a.hotspot.as_ref().map(|h| (&h.ssid, &h.psk))
            == b.hotspot.as_ref().map(|h| (&h.ssid, &h.psk))
}

pub(crate) fn ap_layout_key(ap: &crate::state::Ap) -> (&str, bool, bool, i32) {
    (ap.ssid.as_str(), ap.secured, ap.saved, ap.priority)
}

pub(crate) fn list_equivalent(a: &Model, b: &Model) -> bool {
    if !shell_equivalent(a, b) {
        return false;
    }
    if a.aps.len() != b.aps.len() {
        return false;
    }
    a.aps
        .iter()
        .zip(b.aps.iter())
        .all(|(x, y)| ap_layout_key(x) == ap_layout_key(y))
}

pub(crate) fn structural_change(old: &Model, new: &Model) -> bool {
    !list_equivalent(old, new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Ap;

    fn ap(ssid: &str, strength: u8) -> Ap {
        Ap {
            ssid: ssid.into(),
            strength,
            secured: true,
            saved: false,
            priority: 0,
        }
    }

    fn model(aps: Vec<Ap>) -> Model {
        Model {
            aps: Arc::from(aps),
            ..Model::empty()
        }
    }

    #[test]
    fn strength_jitter_alone_is_not_structural() {
        let a = model(vec![ap("A", 30), ap("B", 70)]);
        let b = model(vec![ap("A", 55), ap("B", 71)]);
        assert!(!structural_change(&a, &b));
    }

    #[test]
    fn reordered_aps_is_structural() {
        let a = model(vec![ap("A", 30), ap("B", 70)]);
        let b = model(vec![ap("B", 70), ap("A", 30)]);
        assert!(structural_change(&a, &b));
    }

    #[test]
    fn a_new_network_is_structural() {
        let a = model(vec![ap("A", 30)]);
        let b = model(vec![ap("A", 30), ap("B", 40)]);
        assert!(structural_change(&a, &b));
    }

    #[test]
    fn a_lost_network_is_structural() {
        let a = model(vec![ap("A", 30), ap("B", 40)]);
        let b = model(vec![ap("A", 30)]);
        assert!(structural_change(&a, &b));
    }

    #[test]
    fn becoming_secured_is_structural() {
        let a = model(vec![Ap {
            secured: false,
            ..ap("A", 30)
        }]);
        let b = model(vec![Ap {
            secured: true,
            ..ap("A", 30)
        }]);
        assert!(structural_change(&a, &b));
    }

    #[test]
    fn becoming_saved_is_structural() {
        let a = model(vec![ap("A", 30)]);
        let b = model(vec![Ap {
            saved: true,
            ..ap("A", 30)
        }]);
        assert!(structural_change(&a, &b));
    }

    #[test]
    fn priority_change_is_structural() {
        let a = model(vec![ap("A", 30)]);
        let b = model(vec![Ap {
            priority: 7,
            ..ap("A", 30)
        }]);
        assert!(structural_change(&a, &b));
    }

    #[test]
    fn losing_active_is_structural() {
        let mut a = model(vec![ap("A", 30)]);
        a.active_ssid = Some("A".into());
        let b = model(vec![ap("A", 30)]);
        assert!(structural_change(&a, &b));
    }

    #[test]
    fn nm_offline_is_structural() {
        let a = model(vec![ap("A", 30)]);
        let b = Model {
            nm_online: false,
            ..model(vec![ap("A", 30)])
        };
        assert!(structural_change(&a, &b));
    }

    #[test]
    fn identical_models_are_equivalent() {
        let a = model(vec![ap("A", 30), ap("B", 40)]);
        let b = model(vec![ap("A", 30), ap("B", 40)]);
        assert!(!structural_change(&a, &b));
    }

    #[test]
    fn a_hotspot_psk_change_is_structural() {
        use crate::state::HotspotInfo;
        let a = Model {
            hotspot: Some(HotspotInfo {
                ssid: "H".into(),
                psk: Some("oldpass".into()),
                active: true,
            }),
            ..model(vec![])
        };
        let b = Model {
            hotspot: Some(HotspotInfo {
                ssid: "H".into(),
                psk: Some("newpass".into()),
                active: true,
            }),
            ..model(vec![])
        };
        assert!(structural_change(&a, &b));
    }
}
