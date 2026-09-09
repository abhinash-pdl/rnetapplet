use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc, Mutex,
};
use std::time::Duration;

use gtk4::prelude::*;
use gtk4_layer_shell::{Edge, KeyboardMode, Layer, LayerShell};

use crate::qr::{self, WifiQr};
use crate::state::BackendCmd;

fn is_virtual_camera(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("virtual")
        || n.contains("obs")
        || n.contains("loopback")
        || n.contains("droidcam")
        || n.contains("manycam")
}

fn candidate_cameras() -> Result<Vec<(nokhwa::utils::CameraIndex, String)>, String> {
    use nokhwa::utils::ApiBackend;
    let cameras = nokhwa::query(ApiBackend::Video4Linux)
        .map_err(|e| format!("could not list cameras: {e}"))?;
    if cameras.is_empty() {
        return Err("no camera found; check /dev/video* permissions".to_string());
    }
    let mut ordered: Vec<(nokhwa::utils::CameraIndex, String)> = Vec::new();
    for c in &cameras {
        if !is_virtual_camera(&c.human_name().to_string()) {
            ordered.push((c.index().clone(), c.human_name().to_string()));
        }
    }
    for c in &cameras {
        if is_virtual_camera(&c.human_name().to_string()) {
            ordered.push((c.index().clone(), c.human_name().to_string()));
        }
    }
    Ok(ordered)
}

struct SharedFrame {
    w: u32,
    h: u32,
    rgb: Vec<u8>,
}

pub async fn file_scan_flow(cmd_tx: async_channel::Sender<BackendCmd>) {
    let dlg = gtk4::FileDialog::new();
    dlg.set_title("Open Wi-Fi QR code image");
    let filter = gtk4::FileFilter::new();
    filter.set_name(Some("Images"));
    filter.add_mime_type("image/*");
    dlg.set_default_filter(Some(&filter));
    let file = match dlg.open_future(None::<&gtk4::Window>).await {
        Ok(f) => f,
        Err(_) => return,
    };
    let bytes = match file.load_bytes(gtk4::gio::Cancellable::NONE) {
        Ok((b, _)) => b,
        Err(e) => {
            tracing::warn!("QR image load failed: {e}");
            return;
        }
    };
    match qr::decode_wifi_qr(&bytes) {
        Ok(code) => {
            tracing::info!(ssid = %code.ssid, "QR decoded from file");
            match qr::route(&code) {
                Some(cmd) => {
                    let _ = cmd_tx.send(cmd).await;
                }
                None => tracing::warn!(ssid = %code.ssid, "unsupported QR security"),
            }
        }
        Err(e) => tracing::warn!("QR decode failed: {e:#}"),
    }
}

fn scan_gray(gray: &image::GrayImage) -> Option<WifiQr> {
    let mut prepared = rqrr::PreparedImage::prepare(gray.clone());
    for grid in prepared.detect_grids() {
        if let Ok((_, content)) = grid.decode() {
            if content.starts_with("WIFI:") {
                if let Some(qr) = qr::parse_wifi_qr(&content) {
                    return Some(qr);
                }
            }
        }
    }
    None
}

fn stretch_lut(gray: &image::GrayImage) -> [u8; 256] {
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
        let lo = lo as u32;
        let hi = hi as u32;
        *slot = (((v.saturating_sub(lo)) * 255) / (hi - lo).max(1)) as u8;
    }
    lut
}

fn apply_lut(gray: &image::GrayImage, lut: &[u8; 256]) -> image::GrayImage {
    let mut out = gray.clone();
    for p in out.pixels_mut() {
        p[0] = lut[p[0] as usize];
    }
    out
}

fn decode_gray_variants(gray: &image::GrayImage) -> Option<WifiQr> {
    let lut = stretch_lut(gray);
    let stretched = apply_lut(gray, &lut);
    if let Some(qr) = scan_gray(&stretched) {
        return Some(qr);
    }
    let mut inv = stretched.clone();
    image::imageops::invert(&mut inv);
    if let Some(qr) = scan_gray(&inv) {
        return Some(qr);
    }
    let mut thresh = stretched.clone();
    for p in thresh.pixels_mut() {
        p[0] = if p[0] < 128 { 0 } else { 255 };
    }
    if let Some(qr) = scan_gray(&thresh) {
        return Some(qr);
    }
    scan_gray(gray)
}

fn work_gray(full: &image::GrayImage) -> image::GrayImage {
    const MAX_W: u32 = 960;
    let (w, h) = full.dimensions();
    if w > MAX_W {
        let nw = MAX_W;
        let nh = (h * nw / w).max(1);
        return image::imageops::resize(full, nw, nh, image::imageops::FilterType::Triangle);
    }
    if w < 420 {
        let s = (640 / w.max(1)).clamp(2, 3);
        return image::imageops::resize(
            full,
            w * s,
            h * s,
            image::imageops::FilterType::Triangle,
        );
    }
    full.clone()
}

fn try_decode_rgb(rgb: &[u8], w: u32, h: u32) -> Option<WifiQr> {
    let img = image::RgbImage::from_raw(w, h, rgb.to_vec())?;
    let gray = image::imageops::grayscale(&img);
    decode_gray_variants(&work_gray(&gray))
}

fn open_camera(
    index: nokhwa::utils::CameraIndex,
    camera_name: &str,
) -> Result<nokhwa::Camera, String> {
    use nokhwa::{
        pixel_format::RgbFormat,
        utils::{CameraFormat, FrameFormat, RequestedFormat, RequestedFormatType, Resolution},
        Camera,
    };

    let attempts: Vec<(&str, RequestedFormatType)> = vec![
        (
            "720p-mjpeg",
            RequestedFormatType::Closest(CameraFormat::new(
                Resolution::new(1280, 720),
                FrameFormat::MJPEG,
                30,
            )),
        ),
        (
            "480p-mjpeg",
            RequestedFormatType::Closest(CameraFormat::new(
                Resolution::new(640, 480),
                FrameFormat::MJPEG,
                30,
            )),
        ),
        ("default", RequestedFormatType::None),
        ("high-res", RequestedFormatType::AbsoluteHighestResolution),
        (
            "480p-yuyv",
            RequestedFormatType::Closest(CameraFormat::new(
                Resolution::new(640, 480),
                FrameFormat::YUYV,
                30,
            )),
        ),
        ("high-fps", RequestedFormatType::AbsoluteHighestFrameRate),
    ];
    let mut last_err = String::from("no formats attempted");
    for (label, kind) in attempts {
        let format = RequestedFormat::new::<RgbFormat>(kind);
        match Camera::new(index.clone(), format).and_then(|mut c| {
            c.open_stream().map(|_| c).map_err(|e| {
                nokhwa::NokhwaError::OpenStreamError(format!("{label} open_stream: {e}"))
            })
        }) {
            Ok(cam) => {
                tracing::info!(camera = camera_name, format = label, "camera opened");
                return Ok(cam);
            }
            Err(e) => {
                tracing::warn!(camera = camera_name, format = label, "camera format failed: {e}");
                last_err = format!("{label}: {e}");
            }
        }
    }
    Err(last_err)
}

fn open_any_camera() -> Result<(nokhwa::Camera, String), String> {
    let candidates = candidate_cameras()?;
    let mut errors = Vec::new();
    for (index, name) in candidates {
        match open_camera(index, &name) {
            Ok(cam) => return Ok((cam, name)),
            Err(e) => errors.push(format!("{name}: {e}")),
        }
    }

    let nodes = std::fs::read_dir("/dev")
        .map(|rd| {
            let mut v: Vec<String> = rd
                .filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| n.starts_with("video"))
                .collect();
            v.sort();
            v.join(", ")
        })
        .unwrap_or_default();
    Err(format!(
        "Could not open camera. Tried [{}]. Devices: [{nodes}]. Close other apps using the camera and check /dev/video* permission (video group).",
        errors.join(" | ")
    ))
}

fn camera_loop(
    stop: Arc<AtomicBool>,
    frames: Arc<Mutex<Option<SharedFrame>>>,
    res_tx: mpsc::Sender<WifiQr>,
    status_tx: mpsc::Sender<String>,
) {
    use nokhwa::pixel_format::RgbFormat;
    let say = |s: String| {
        let _ = status_tx.send(s);
    };
    let (mut cam, _) = match open_any_camera() {
        Ok(c) => c,
        Err(e) => { say(e); return; }
    };
    let mut decode_errors = 0u32;
    let mut last_preview = std::time::Instant::now() - Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {

        let frame = match cam.frame() {
            Ok(f) => f,
            Err(_) => {
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let img = match frame.decode_image::<RgbFormat>() {
            Ok(i) => i,
            Err(e) => {
                decode_errors += 1;
                if decode_errors == 5 {
                    say(format!("Camera format unsupported: {e}"));
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let (w, h) = (img.width(), img.height());
        let rgb = img.into_raw();
        if w == 0 || h == 0 || rgb.len() != w as usize * h as usize * 3 {
            continue;
        }
        if try_decode_rgb(&rgb, w, h).is_some_and(|qr| res_tx.send(qr).is_ok()) {
            break;
        }
        if last_preview.elapsed() > Duration::from_millis(200) {
            last_preview = std::time::Instant::now();
            *frames.lock().unwrap_or_else(|e| e.into_inner()) = Some(SharedFrame {
                w,
                h,
                rgb,
            });
        }
    }
}

pub fn camera_self_test() -> anyhow::Result<()> {
    use nokhwa::pixel_format::RgbFormat;
    let (mut cam, camera_name) = open_any_camera().map_err(anyhow::Error::msg)?;
    println!("using camera: {camera_name}");
    for i in 0..5 {
        let frame = cam.frame()?;
        let img = frame.decode_image::<RgbFormat>()?;
        let (w, h) = (img.width(), img.height());
        let raw = img.into_raw();
        println!("frame {i}: {w}x{h} bytes={} ok={}", raw.len(), raw.len() == w as usize * h as usize * 3);
        if try_decode_rgb(&raw, w, h).is_some() {
            println!("decoded a QR payload");
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Ok(())
}
pub fn open_scanner(app: &gtk4::Application, cmd_tx: async_channel::Sender<BackendCmd>) {
    crate::ui::load_css();
    let window = gtk4::ApplicationWindow::new(app);
    window.set_title(Some("Scan QR code"));
    window.add_css_class("rnet-window");
    let (mw, mh) = gtk4::gdk::Display::default()
        .and_then(|d| {
            let monitors = d.monitors();
            if monitors.n_items() == 0 {
                return None;
            }
            monitors
                .item(0)
                .and_then(|o| o.downcast::<gtk4::gdk::Monitor>().ok())
        })
        .map(|mon| {
            let g = mon.geometry();
            (g.width(), g.height())
        })
        .unwrap_or((1920, 1080));
    let w = 480;
    let h = 560;
    window.set_default_size(w, h);
    window.init_layer_shell();
    window.set_layer(Layer::Top);
    window.set_anchor(Edge::Top, true);
    window.set_anchor(Edge::Bottom, true);
    window.set_anchor(Edge::Left, true);
    window.set_anchor(Edge::Right, true);
    window.set_margin(Edge::Top, ((mh - h) / 2).max(0));
    window.set_margin(Edge::Bottom, ((mh - h) / 2).max(0));
    window.set_margin(Edge::Left, ((mw - w) / 2).max(0));
    window.set_margin(Edge::Right, ((mw - w) / 2).max(0));
    window.set_exclusive_zone(0);
    window.set_keyboard_mode(KeyboardMode::OnDemand);

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    vbox.add_css_class("rnet-card");
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);

    let picture = gtk4::Picture::new();
    picture.set_content_fit(gtk4::ContentFit::Cover);
    picture.set_size_request(456, 400);
    vbox.append(&picture);

    let status = gtk4::Label::new(None);
    status.add_css_class("dim-label");
    status.set_halign(gtk4::Align::Start);
    status.set_wrap(true);
    vbox.append(&status);

    let btn_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk4::Align::End);
    let file_btn = gtk4::Button::with_label("Open image file");
    let close_btn = gtk4::Button::with_label("Close");
    btn_row.append(&file_btn);
    btn_row.append(&close_btn);
    vbox.append(&btn_row);

    window.set_child(Some(&vbox));
    window.present();

    let stop = Arc::new(AtomicBool::new(false));
    let frames: Arc<Mutex<Option<SharedFrame>>> = Arc::new(Mutex::new(None));
    let (res_tx, res_rx) = mpsc::channel::<WifiQr>();
    let (status_tx, status_rx) = mpsc::channel::<String>();

    {
        let stop = stop.clone();
        let frames = frames.clone();
        std::thread::spawn(move || camera_loop(stop, frames, res_tx, status_tx));
    }
    {
        let stop = stop.clone();
        window.connect_close_request(move |_| {
            stop.store(true, Ordering::Relaxed);
            gtk4::glib::Propagation::Proceed
        });
    }
    {
        let window = window.clone();
        close_btn.connect_clicked(move |_| {
            stop.store(true, Ordering::Relaxed);
            window.close();
        });
    }
    {
        let cmd_tx = cmd_tx.clone();
        file_btn.connect_clicked(move |_| {
            let cmd_tx = cmd_tx.clone();
            gtk4::glib::MainContext::default().spawn_local(async move {
                file_scan_flow(cmd_tx).await;
            });
        });
    }

    let cmd_tx_tick = cmd_tx.clone();
    let window_tick = window.clone();
    gtk4::glib::timeout_add_local(Duration::from_millis(120), move || {
        if let Ok(s) = status_rx.try_recv() {
            status.set_text(&s);
        }
        if let Some(f) = frames.lock().unwrap_or_else(|e| e.into_inner()).take() {

            let need = f.w as u64 * f.h as u64 * 3;
            if f.w > 0 && f.h > 0 && f.rgb.len() as u64 == need {
                let bytes = gtk4::glib::Bytes::from_owned(f.rgb);
                let tex = gtk4::gdk::MemoryTexture::new(
                    f.w as i32,
                    f.h as i32,
                    gtk4::gdk::MemoryFormat::R8g8b8,
                    &bytes,
                    (f.w * 3) as usize,
                );
                picture.set_paintable(Some(&tex));
            }
        }
        if let Ok(code) = res_rx.try_recv() {
            tracing::info!(ssid = %code.ssid, "QR scanned from camera");
            status.set_text(&format!("Connecting to {}…", code.ssid));
            if let Some(cmd) = qr::route(&code) {
                let _ = cmd_tx_tick.try_send(cmd);
            } else {
                status.set_text("Unsupported QR code");
                return gtk4::glib::ControlFlow::Continue;
            }
            let window_tick = window_tick.clone();
            gtk4::glib::timeout_add_local_once(Duration::from_secs(1), move || {
                window_tick.close();
            });
            return gtk4::glib::ControlFlow::Break;
        }
        gtk4::glib::ControlFlow::Continue
    });
}
