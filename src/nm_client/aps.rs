pub fn decode_ssid(bytes: &[u8]) -> Option<String> {
    if bytes.is_empty() || bytes.iter().all(|&b| b == 0) {
        return None;
    }

    let s = String::from_utf8_lossy(bytes).trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}

pub const AP_FLAGS_PRIVACY: u32 = 0x1;
pub const AP_SEC_KEY_MGMT_802_1X: u32 = 0x200;

pub const FREQ_2_4_MIN: u32 = 2400;
pub const FREQ_2_4_MAX_EXCL: u32 = 4900;
pub const FREQ_5_MIN: u32 = 4900;
pub const FREQ_6_MIN: u32 = 5925;

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub struct Bands(u8);

impl Bands {
    pub const NONE: Self = Self(0);
    pub const B2G: Self = Self(1);
    pub const B5G: Self = Self(2);
    pub const B6G: Self = Self(4);

    pub fn from_freq(freq_mhz: Option<u32>) -> Self {
        match freq_mhz {
            Some(f) if f >= FREQ_6_MIN => Self::B6G,
            Some(f) if f >= FREQ_5_MIN => Self::B5G,
            Some(f) if (FREQ_2_4_MIN..FREQ_2_4_MAX_EXCL).contains(&f) => Self::B2G,
            _ => Self::NONE,
        }
    }

    pub fn has_5_or_6(self) -> bool {
        self.0 & (Self::B5G.0 | Self::B6G.0) != 0
    }

    pub fn label(self) -> String {
        let mut parts: Vec<&str> = Vec::with_capacity(3);
        if self.0 & Self::B2G.0 != 0 {
            parts.push("2.4G");
        }
        if self.0 & Self::B5G.0 != 0 {
            parts.push("5G");
        }
        if self.0 & Self::B6G.0 != 0 {
            parts.push("6G");
        }
        if !self.has_5_or_6() {
            return String::new();
        }
        parts.join(" | ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Security {
    Open,
    Wep,
    Wpa,
    Enterprise,
}

impl Security {
    pub fn is_secured(self) -> bool {
        !matches!(self, Self::Open)
    }

    pub fn is_wep(self) -> bool {
        matches!(self, Self::Wep)
    }

    pub fn is_enterprise(self) -> bool {
        matches!(self, Self::Enterprise)
    }

    fn rank(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Wep => 1,
            Self::Wpa => 2,
            Self::Enterprise => 3,
        }
    }

    pub fn stricter(self, other: Self) -> Self {
        if other.rank() > self.rank() {
            other
        } else {
            self
        }
    }
}

pub fn classify_security(wpa_flags: u32, rsn_flags: u32, flags: u32) -> Security {
    if is_enterprise(wpa_flags, rsn_flags) {
        Security::Enterprise
    } else if is_wep_only(flags, wpa_flags, rsn_flags) {
        Security::Wep
    } else if is_secured(wpa_flags, rsn_flags) {
        Security::Wpa
    } else {
        Security::Open
    }
}

pub fn is_secured(wpa_flags: u32, rsn_flags: u32) -> bool {
    wpa_flags != 0 || rsn_flags != 0
}

pub fn is_wep_only(flags: u32, wpa_flags: u32, rsn_flags: u32) -> bool {
    wpa_flags == 0 && rsn_flags == 0 && flags & AP_FLAGS_PRIVACY != 0
}

pub fn is_enterprise(wpa_flags: u32, rsn_flags: u32) -> bool {
    wpa_flags & AP_SEC_KEY_MGMT_802_1X != 0 || rsn_flags & AP_SEC_KEY_MGMT_802_1X != 0
}

impl std::ops::BitOr for Bands {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitOrAssign for Bands {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
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
        assert_eq!(Bands::from_freq(Some(2412)), Bands::B2G);
        assert_eq!(Bands::from_freq(Some(5180)), Bands::B5G);
        assert_eq!(Bands::from_freq(Some(6175)), Bands::B6G);
        assert_eq!(Bands::from_freq(None), Bands::NONE);
        assert_eq!(Bands::from_freq(Some(1800)), Bands::NONE);
    }

    #[test]
    fn band_label_skips_plain_24ghz() {
        assert_eq!(Bands::NONE.label(), "");
        assert_eq!(Bands::B2G.label(), "");
        assert_eq!(Bands::B5G.label(), "5G");
        assert_eq!(Bands::B6G.label(), "6G");
    }

    #[test]
    fn band_label_merges_dual_band() {
        assert_eq!(Bands::B2G | Bands::B5G, Bands(Bands::B2G.0 | Bands::B5G.0));
        let dual = Bands::from_freq(Some(2412)) | Bands::from_freq(Some(5180));
        assert_eq!(dual.label(), "2.4G | 5G");
        let tri = dual | Bands::from_freq(Some(6175));
        assert_eq!(tri.label(), "2.4G | 5G | 6G");
    }

    #[test]
    fn enterprise_needs_802_1x_key_mgmt() {
        assert!(is_enterprise(0x200, 0));
        assert!(is_enterprise(0, 0x200));
        assert!(!is_enterprise(0x100, 0x100));
        assert!(!is_enterprise(0, 0));
    }
}
