use anyhow::{Context, Result};
use image::GrayImage;

use crate::state::{BackendCmd, Secret, Ssid};

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
    pub password: Option<Secret>,
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
        let Some((k, v)) = split_kv(&field) else {
            continue;
        };
        match k.to_ascii_uppercase().as_str() {
            "S" => ssid = Some(v),
            "P" => password = Some(Secret::from(v)),
            "H" => hidden = matches!(v.to_ascii_lowercase().as_str(), "true" | "1"),
            "T" => {
                let t = v.to_ascii_uppercase();
                if t.is_empty() {
                    continue;
                }
                saw_t = true;
                security = match t.as_str() {
                    "NOPASS" => WifiSecurity::Open,
                    "WEP" => WifiSecurity::Wep,
                    "WPA" | "WPA2" | "SAE" | "WPA3" => WifiSecurity::Wpa,
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

fn stretch_lut(gray: &GrayImage) -> [u8; 256] {
    let mut lo = 255u8;
    let mut hi = 0u8;
    for p in gray.pixels() {
        let v = p[0];
        if v < lo {
            lo = v;
        }
        if v > hi {
            hi = v;
        }
    }
    let mut lut = [0u8; 256];
    if hi <= lo {
        return lut;
    }
    for (i, slot) in lut.iter_mut().enumerate() {
        let v = i as u32;
        *slot = (((v.saturating_sub(lo as u32)) * 255) / (hi as u32 - lo as u32).max(1)) as u8;
    }
    lut
}

fn scan_with(gray: &GrayImage, map: impl Fn(u8) -> u8) -> Option<String> {
    let (w, h) = gray.dimensions();
    let mut prepared =
        rqrr::PreparedImage::prepare_from_greyscale(w as usize, h as usize, |x, y| {
            map(gray.get_pixel(x as u32, y as u32)[0])
        });
    for grid in prepared.detect_grids() {
        if let Ok((_, content)) = grid.decode()
            && content.starts_with("WIFI:")
        {
            return Some(content);
        }
    }
    None
}

fn try_variant(gray: &GrayImage, lut: &[u8; 256], invert: bool) -> Option<WifiQr> {
    scan_with(gray, |p| {
        let v = lut[p as usize];
        if invert { 255 - v } else { v }
    })
    .and_then(|c| parse_wifi_qr(&c))
}

pub(crate) fn decode_gray_variants(gray: &GrayImage) -> Option<WifiQr> {
    let lut = stretch_lut(gray);
    try_variant(gray, &lut, false)
        .or_else(|| try_variant(gray, &lut, true))
        .or_else(|| scan_with(gray, |p| p).and_then(|c| parse_wifi_qr(&c)))
}

pub fn decode_wifi_qr(bytes: &[u8]) -> Result<WifiQr> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format()?;
    let mut limits = image::Limits::default();
    limits.max_alloc = Some(128 * 1024 * 1024);
    limits.max_image_width = Some(8192);
    limits.max_image_height = Some(8192);
    reader.limits(limits);
    let mut original = reader.decode().context("not a readable image")?.to_luma8();
    const MAX_DIM: u32 = 1280;
    let (w, h) = original.dimensions();
    if w.max(h) > MAX_DIM {
        let s = MAX_DIM as f32 / w.max(h) as f32;
        original = image::imageops::resize(
            &original,
            ((w as f32 * s) as u32).max(1),
            ((h as f32 * s) as u32).max(1),
            image::imageops::FilterType::Triangle,
        );
    }
    decode_gray_variants(&original).ok_or_else(|| anyhow::anyhow!("no Wi-Fi QR payload found"))
}

pub fn route(qr: &WifiQr) -> Option<BackendCmd> {
    let ssid = Ssid::from(qr.ssid.clone());
    let psk = qr.password.as_deref().filter(|p| !p.is_empty());
    match &qr.security {
        WifiSecurity::Open if !qr.hidden => Some(BackendCmd::ConnectOpen(ssid)),
        WifiSecurity::Open => Some(BackendCmd::ConnectHidden {
            ssid,
            psk: Secret::from(""),
        }),
        WifiSecurity::Wpa if qr.hidden => match psk {
            Some(p) => Some(BackendCmd::ConnectHidden {
                ssid,
                psk: Secret::from(p),
            }),
            None => Some(BackendCmd::ExpandRow(ssid)),
        },
        WifiSecurity::Wpa => match psk {
            Some(p) => Some(BackendCmd::ConnectSecure {
                ssid,
                psk: Secret::from(p),
            }),
            None => Some(BackendCmd::ExpandRow(ssid)),
        },
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

    #[test]
    fn route_expands_when_the_password_is_missing() {
        let bare = WifiQr {
            ssid: "C".into(),
            password: None,
            hidden: false,
            security: WifiSecurity::Wpa,
        };
        assert!(matches!(route(&bare), Some(BackendCmd::ExpandRow(_))));
        let bare_hidden = WifiQr {
            hidden: true,
            ..bare.clone()
        };
        assert!(matches!(
            route(&bare_hidden),
            Some(BackendCmd::ExpandRow(_))
        ));
    }

    #[test]
    fn enterprise_qr_never_routes() {
        let eap = parse_wifi_qr("WIFI:S:Corp;T:WPA2-EAP;P:secret;;").unwrap();
        assert!(matches!(eap.security, WifiSecurity::Unknown(_)));
        assert!(route(&eap).is_none());
    }

    #[test]
    fn empty_type_falls_back_to_missing() {
        let qr = parse_wifi_qr("WIFI:S:Home;T:;P:hunter2;;").unwrap();
        assert_eq!(qr.security, WifiSecurity::Wpa);
    }

    #[test]
    fn hidden_wpa_with_password_routes_hidden() {
        let qr = WifiQr {
            ssid: "H".into(),
            password: Some(Secret::from("hunter2")),
            hidden: true,
            security: WifiSecurity::Wpa,
        };
        assert!(matches!(route(&qr), Some(BackendCmd::ConnectHidden { .. })));
    }

    #[test]
    fn password_is_redacted_in_debug() {
        let qr = WifiQr {
            ssid: "H".into(),
            password: Some(Secret::from("hunter2")),
            hidden: false,
            security: WifiSecurity::Wpa,
        };
        let shown = format!("{qr:?}");
        assert!(!shown.contains("hunter2"), "password leaked: {shown}");
    }

    fn qr_png(payload: &str, invert: bool) -> Vec<u8> {
        let code = qrcode::QrCode::new(payload.as_bytes()).expect("qr content");
        let mut img = code.render::<image::Luma<u8>>().quiet_zone(false).build();
        if invert {
            image::imageops::invert(&mut img);
        }
        let mut bytes = Vec::new();
        image::DynamicImage::ImageLuma8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .expect("png encode");
        bytes
    }

    #[test]
    fn round_trip_qr_from_generated_image() {
        let qr = decode_wifi_qr(&qr_png("WIFI:S:Cafe;T:WPA;P:hunter2;;", false)).expect("decode");
        assert_eq!(qr.ssid, "Cafe");
        assert_eq!(qr.password.as_deref(), Some("hunter2"));
        assert_eq!(qr.security, WifiSecurity::Wpa);
    }

    #[test]
    fn round_trip_inverted_qr_from_generated_image() {
        let qr = decode_wifi_qr(&qr_png("WIFI:S:Cafe;T:nopass;;", true)).expect("decode");
        assert_eq!(qr.ssid, "Cafe");
        assert_eq!(qr.security, WifiSecurity::Open);
    }
}
