pub fn decode_ssid(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.iter().all(|&b| b == 0) {
        return None;
    }

    let s = String::from_utf8_lossy(bytes).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

pub fn is_secured(wpa_flags: u32, rsn_flags: u32) -> bool {
    wpa_flags != 0 || rsn_flags != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_utf8() {
        assert_eq!(decode_ssid(b"Home"), Some("Home".into()));
    }

    #[test]
    fn hidden_is_none() {
        assert_eq!(decode_ssid(b""), None);
        assert_eq!(decode_ssid(&[0, 0, 0]), None);
    }

    #[test]
    fn secured_flags() {
        assert!(!is_secured(0, 0));
        assert!(is_secured(0x1, 0));
        assert!(is_secured(0, 0x200));
    }
}
