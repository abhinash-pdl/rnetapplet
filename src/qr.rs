use anyhow::{Context, Result};
use image::GrayImage;

use crate::state::BackendCmd;

#[derive(Debug, Clone, PartialEq)]
pub enum WifiSecurity {
    Open,
    Wpa,
    Wep,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WifiQr {
    pub ssid: String,
    pub password: Option<String>,
    pub hidden: bool,
    pub security: WifiSecurity,
}

fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn split_fields(s: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            cur.push(c);
            if let Some(n) = chars.next() {
                cur.push(n);
            }
        } else if c == ';' {
            fields.push(std::mem::take(&mut cur));
        } else {
            cur.push(c);
        }
    }
    fields.push(cur);
    fields
}

fn split_kv(field: &str) -> Option<(String, String)> {
    let mut key = String::new();
    let mut chars = field.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            key.push(c);
            if let Some(n) = chars.next() {
                key.push(n);
            }
        } else if c == ':' {
            let value: String = chars.collect();
            return Some((unescape(&key), unescape(&value)));
        } else {
            key.push(c);
        }
    }
    None
}

pub fn parse_wifi_qr(text: &str) -> Option<WifiQr> {
    let body = text.trim().strip_prefix("WIFI:")?;
    let mut ssid = None;
    let mut password = None;
    let mut hidden = false;
    let mut security = WifiSecurity::Open;
    let mut saw_t = false;
    for field in split_fields(body) {
        if field.is_empty() {
            continue;
        }
        let (k, v) = split_kv(&field)?;
        match k.to_ascii_uppercase().as_str() {
            "S" => ssid = Some(v),
            "P" => password = Some(v),
            "H" => hidden = matches!(v.to_ascii_lowercase().as_str(), "true" | "1"),
            "T" => {
                saw_t = true;
                security = match v.to_ascii_uppercase().as_str() {
                    "NOPASS" => WifiSecurity::Open,
                    "WEP" => WifiSecurity::Wep,
                    "WPA" | "WPA2" | "SAE" | "WPA3" | "WPA2-EAP" | "WPA-EAP" => WifiSecurity::Wpa,
                    other => WifiSecurity::Unknown(other.to_string()),
                };
            }
            _ => {}
        }
    }
    let ssid = ssid.filter(|s| !s.is_empty())?;
    if !saw_t && password.is_some() {
        security = WifiSecurity::Wpa;
    }
    Some(WifiQr {
        ssid,
        password,
        hidden,
        security,
    })
}

fn decode_gray(gray: GrayImage) -> Option<String> {
    let mut prepared = rqrr::PreparedImage::prepare(gray);
    for grid in prepared.detect_grids() {
        if let Ok((_, content)) = grid.decode()
            && content.starts_with("WIFI:")
        {
            return Some(content);
        }
    }
    None
}

pub fn decode_wifi_qr(bytes: &[u8]) -> Result<WifiQr> {
    let img = image::load_from_memory(bytes).context("not a readable image")?;
    let original = img.to_luma8();

    let mut thresholded = original.clone();
    for pixel in thresholded.pixels_mut() {
        pixel[0] = if pixel[0] < 160 { 0 } else { 255 };
    }
    let mut inverted = thresholded.clone();
    image::imageops::invert(&mut inverted);
    for candidate in [original, thresholded, inverted] {
        if let Some(content) = decode_gray(candidate)
            && let Some(qr) = parse_wifi_qr(&content)
        {
            return Ok(qr);
        }
    }
    anyhow::bail!("no Wi-Fi QR payload found");
}

pub fn route(qr: &WifiQr) -> Option<BackendCmd> {
    match &qr.security {
        WifiSecurity::Open => Some(if qr.hidden {
            BackendCmd::ConnectHidden {
                ssid: qr.ssid.clone(),
                psk: String::new(),
            }
        } else {
            BackendCmd::ConnectOpen(qr.ssid.clone())
        }),
        WifiSecurity::Wpa => {
            let psk = qr.password.clone().filter(|p| !p.is_empty())?;
            Some(if qr.hidden {
                BackendCmd::ConnectHidden {
                    ssid: qr.ssid.clone(),
                    psk,
                }
            } else {
                BackendCmd::ConnectSecure {
                    ssid: qr.ssid.clone(),
                    psk,
                }
            })
        }
        WifiSecurity::Wep | WifiSecurity::Unknown(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_open() {
        let qr = parse_wifi_qr("WIFI:S:Coffee;T:nopass;;").unwrap();
        assert_eq!(qr.ssid, "Coffee");
        assert_eq!(qr.security, WifiSecurity::Open);
        assert!(!qr.hidden);
    }

    #[test]
    fn parse_wpa_hidden() {
        let qr = parse_wifi_qr("WIFI:S:Home;T:WPA;P:hunter2;H:true;;").unwrap();
        assert_eq!(qr.ssid, "Home");
        assert_eq!(qr.password.as_deref(), Some("hunter2"));
        assert!(qr.hidden);
        assert_eq!(qr.security, WifiSecurity::Wpa);
    }

    #[test]
    fn parse_escapes() {
        let qr = parse_wifi_qr(r"WIFI:S:My\;Net;T:WPA;P:a\:b\\c;;").unwrap();
        assert_eq!(qr.ssid, "My;Net");
        assert_eq!(qr.password.as_deref(), Some("a:b\\c"));
    }

    #[test]
    fn parse_rejects_garbage() {
        assert!(parse_wifi_qr("hello").is_none());
        assert!(parse_wifi_qr("WIFI:T:WPA;P:x;;").is_none());
        assert!(parse_wifi_qr("WIFI:S:;;").is_none());
    }

    #[test]
    fn route_maps_flows() {
        let open = WifiQr {
            ssid: "C".into(),
            password: None,
            hidden: false,
            security: WifiSecurity::Open,
        };
        assert!(matches!(route(&open), Some(BackendCmd::ConnectOpen(_))));
        let wep = WifiQr {
            security: WifiSecurity::Wep,
            ..open.clone()
        };
        assert!(route(&wep).is_none());
    }
}
