pub fn decode_ssid(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.iter().all(|&b| b == 0) {
        return None;
    }

    let s = String::from_utf8_lossy(bytes).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

pub const AP_FLAGS_PRIVACY: u32 = 0x1;
pub const AP_SEC_KEY_MGMT_802_1X: u32 = 0x200;

pub fn is_secured(wpa_flags: u32, rsn_flags: u32) -> bool {
    wpa_flags != 0 || rsn_flags != 0
}

pub fn is_wep_only(flags: u32, wpa_flags: u32, rsn_flags: u32) -> bool {
    wpa_flags == 0 && rsn_flags == 0 && flags & AP_FLAGS_PRIVACY != 0
}

pub const BAND_24: u8 = 1;
pub const BAND_5: u8 = 2;
pub const BAND_6: u8 = 4;

pub fn band_bit(freq_mhz: Option<u32>) -> u8 {
    match freq_mhz {
        Some(f) if f >= 5925 => BAND_6,
        Some(f) if f >= 4900 => BAND_5,
        Some(f) if (2400..4900).contains(&f) => BAND_24,
        _ => 0,
    }
}

pub fn band_label(bands: u8) -> String {
    let mut parts: Vec<&str> = Vec::new();
    if bands & BAND_24 != 0 {
        parts.push("2.4G");
    }
    if bands & BAND_5 != 0 {
        parts.push("5G");
    }
    if bands & BAND_6 != 0 {
        parts.push("6G");
    }
    if parts.is_empty() || (bands & !BAND_24 == 0 && parts.len() == 1) {
        return String::new();
    }
    parts.join(" | ")
}

pub fn is_enterprise(wpa_flags: u32, rsn_flags: u32) -> bool {
    wpa_flags & AP_SEC_KEY_MGMT_802_1X != 0 || rsn_flags & AP_SEC_KEY_MGMT_802_1X != 0
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

    #[test]
    fn wep_only_needs_privacy_flag() {
        assert!(is_wep_only(0x1, 0, 0));
        assert!(!is_wep_only(0x0, 0, 0));
        assert!(!is_wep_only(0x1, 0x100, 0));
    }

    #[test]
    fn band_bits_from_frequency() {
        assert_eq!(band_bit(Some(2412)), BAND_24);
        assert_eq!(band_bit(Some(5180)), BAND_5);
        assert_eq!(band_bit(Some(6175)), BAND_6);
        assert_eq!(band_bit(None), 0);
        assert_eq!(band_bit(Some(1800)), 0);
    }

    #[test]
    fn band_label_skips_plain_24ghz() {
        assert_eq!(band_label(0), "");
        assert_eq!(band_label(BAND_24), "");
        assert_eq!(band_label(BAND_5), "5G");
        assert_eq!(band_label(BAND_6), "6G");
    }

    #[test]
    fn band_label_merges_dual_band() {
        assert_eq!(band_label(BAND_24 | BAND_5), "2.4G | 5G");
        assert_eq!(band_label(BAND_24 | BAND_5 | BAND_6), "2.4G | 5G | 6G");
    }

    #[test]
    fn enterprise_needs_802_1x_key_mgmt() {
        assert!(is_enterprise(0x200, 0));
        assert!(is_enterprise(0, 0x200));
        assert!(!is_enterprise(0x100, 0x100));
        assert!(!is_enterprise(0, 0));
    }
}
