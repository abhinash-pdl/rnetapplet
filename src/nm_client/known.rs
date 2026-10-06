use std::collections::BTreeSet;
use std::path::PathBuf;

fn store_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(base.join("rnetapplet").join("connected"))
}

pub fn hex_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 2);
    for b in s.as_bytes() {
        out.push_str(&format!("{b:02x}"));
    }
    out
}

pub fn hex_decode(s: &str) -> Option<String> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(s.len() / 2);
    let raw = s.as_bytes();
    for pair in raw.chunks(2) {
        let hi = (pair[0] as char).to_digit(16)?;
        let lo = (pair[1] as char).to_digit(16)?;
        bytes.push((hi * 16 + lo) as u8);
    }
    String::from_utf8(bytes).ok()
}

pub fn load() -> BTreeSet<String> {
    let Some(path) = store_path() else {
        return BTreeSet::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return BTreeSet::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .filter_map(hex_decode)
        .collect()
}

pub fn save(set: &BTreeSet<String>) {
    let Some(path) = store_path() else {
        return;
    };
    if let Some(dir) = path.parent()
        && std::fs::create_dir_all(dir).is_err()
    {
        tracing::warn!(
            "could not create {} for connected-network list",
            dir.display()
        );
        return;
    }
    let mut text = String::new();
    for ssid in set {
        text.push_str(&hex_encode(ssid));
        text.push('\n');
    }
    if let Err(e) = std::fs::write(&path, text) {
        tracing::warn!("could not write {}: {e}", path.display());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_roundtrip_handles_odd_ssid_bytes() {
        for ssid in ["Poudel", "caf\u{e9} \u{2603}", "\u{0}a b", ""] {
            assert_eq!(hex_decode(&hex_encode(ssid)).as_deref(), Some(ssid));
        }
    }

    #[test]
    fn hex_decode_rejects_garbage() {
        assert!(hex_decode("zz").is_none());
        assert!(hex_decode("abc").is_none());
    }
}
