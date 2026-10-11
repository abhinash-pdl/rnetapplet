use crate::qr::{self, WifiQr};
use crate::state::BackendCmd;
use gtk4::prelude::*;
use gtk4_layer_shell::{KeyboardMode, Layer, LayerShell};
use std::rc::Rc;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;
fn is_virtual_camera(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("virtual")
        || n.contains("obs virtual")
        || n.contains("v4l2loopback")
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
pub async fn file_scan_flow(on_code: Rc<dyn Fn(&WifiQr) -> bool>, window: gtk4::ApplicationWindow) {
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
        Ok((b, _)) => b.to_vec(),
        Err(e) => {
            tracing::warn!("QR image load failed: {e}");
            return;
        }
    };
    match gtk4::gio::spawn_blocking(move || qr::decode_wifi_qr(&bytes)).await {
        Ok(Ok(code)) => {
            tracing::info!(ssid = %code.ssid, "QR decoded from file");
            if on_code(&code) {
                window.close();
            }
        }
        Ok(Err(e)) => tracing::warn!("QR decode failed: {e:#}"),
        Err(_) => tracing::warn!("QR decode task failed"),
    }
}
fn try_decode_rgb(rgb: &[u8], w: u32, h: u32) -> Option<WifiQr> {
    crate::qr::decode_gray_variants(&work_gray_from_rgb(rgb, w, h)?)
}
const DECODE_HZ: u32 = 10;
const PREVIEW_TICK: Duration = Duration::from_millis(50);
const WORK_MAX_W: u32 = 960;
const WORK_MIN_W: u32 = 420;
const WORK_MAX_H: u32 = 960;
fn work_dims(w: u32, h: u32) -> (u32, u32) {
    if w == 0 || h == 0 {
        return (0, 0);
    }
    let (mut tw, mut th) = (w, h);
    if tw > WORK_MAX_W {
        th = (th * WORK_MAX_W / tw).max(1);
        tw = WORK_MAX_W;
    }
    if th > WORK_MAX_H {
        tw = (tw * WORK_MAX_H / th).max(1);
        th = WORK_MAX_H;
    }
    if tw < WORK_MIN_W {
        let s = (WORK_MAX_H / th.max(1)).min(640 / tw.max(1)).clamp(1, 3);
        tw *= s;
        th *= s;
    }
    (tw, th)
}
fn work_gray_from_rgb(rgb: &[u8], w: u32, h: u32) -> Option<image::GrayImage> {
    if w == 0 || h == 0 || rgb.len() < w as usize * h as usize * 3 {
        return None;
    }
    let (tw, th) = work_dims(w, h);
    if tw == 0 || th == 0 {
        return None;
    }
    let mut out = vec![0u8; tw as usize * th as usize];
    let src_w = w as usize;
    let shrinking = tw < w || th < h;
    for ty in 0..th as usize {
        let sy0 = ty * h as usize / th as usize;
        let sy1 = (((ty + 1) * h as usize) / th as usize)
            .max(sy0 + 1)
            .min(h as usize);
        let row = &mut out[ty * tw as usize..(ty + 1) * tw as usize];
        for (tx, px) in row.iter_mut().enumerate() {
            let sx0 = tx * src_w / tw as usize;
            let sx1 = (((tx + 1) * src_w) / tw as usize).max(sx0 + 1).min(src_w);
            let luma = if shrinking {
                let mut sum = 0u32;
                let mut n = 0u32;
                for sy in sy0..sy1 {
                    for sx in sx0..sx1 {
                        let i = (sy * src_w + sx) * 3;
                        sum +=
                            (rgb[i] as u32 * 77 + rgb[i + 1] as u32 * 150 + rgb[i + 2] as u32 * 29)
                                >> 8;
                        n += 1;
                    }
                }
                (sum / n.max(1)) as u8
            } else {
                let i = (sy0 * src_w + sx0) * 3;
                ((rgb[i] as u32 * 77 + rgb[i + 1] as u32 * 150 + rgb[i + 2] as u32 * 29) >> 8) as u8
            };
            *px = luma;
        }
    }
    image::GrayImage::from_raw(tw, th, out)
}
fn mirror_rgb(rgb: &mut [u8], w: u32) {
    let stride = w as usize * 3;
    if stride == 0 {
        return;
    }
    if !rgb.len().is_multiple_of(stride) {
        return;
    }
    for row in rgb.chunks_exact_mut(stride) {
        let mut lo = 0usize;
        let mut hi = w as usize - 1;
        while lo < hi {
            row.swap(lo * 3, hi * 3);
            row.swap(lo * 3 + 1, hi * 3 + 1);
            row.swap(lo * 3 + 2, hi * 3 + 2);
            lo += 1;
            hi -= 1;
        }
    }
}
fn open_camera(
    index: nokhwa::utils::CameraIndex,
    camera_name: &str,
) -> Result<nokhwa::Camera, String> {
    use nokhwa::{
        Camera,
        pixel_format::RgbFormat,
        utils::{CameraFormat, FrameFormat, RequestedFormat, RequestedFormatType, Resolution},
    };
    let attempts: Vec<(&str, RequestedFormatType)> = vec![
        (
            "480p-yuyv",
            RequestedFormatType::Closest(CameraFormat::new(
                Resolution::new(640, 480),
                FrameFormat::YUYV,
                30,
            )),
        ),
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
                tracing::warn!(
                    camera = camera_name,
                    format = label,
                    "camera format failed: {e}"
                );
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
    work: Arc<Mutex<Option<image::GrayImage>>>,
    status_tx: mpsc::Sender<String>,
) {
    use nokhwa::pixel_format::RgbFormat;
    let say = |s: String| {
        let _ = status_tx.send(s);
    };
    let (mut cam, _) = match open_any_camera() {
        Ok(c) => c,
        Err(e) => {
            say(e);
            return;
        }
    };
    let mut decode_errors = 0u32;
    let mut last_work = std::time::Instant::now() - Duration::from_secs(1);
    let work_every = Duration::from_millis(1000 / DECODE_HZ as u64);
    let mut frame_errors = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let raw_frame = match cam.frame() {
            Ok(f) => {
                frame_errors = 0;
                f
            }
            Err(_) => {
                frame_errors += 1;
                if frame_errors > 50 {
                    say("Camera stopped responding".to_string());
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let want_preview = frames.lock().unwrap_or_else(|e| e.into_inner()).is_none();
        let want_work = last_work.elapsed() >= work_every;
        if !want_preview && !want_work {
            continue;
        }
        let img = match raw_frame.decode_image::<RgbFormat>() {
            Ok(i) => i,
            Err(e) => {
                let fallback = image::load_from_memory(raw_frame.buffer())
                    .ok()
                    .map(|i| i.to_rgb8());
                match fallback {
                    Some(decoded) => decoded,
                    None => {
                        decode_errors += 1;
                        if decode_errors == 5 {
                            say(format!("Camera format unsupported: {e}"));
                        }
                        std::thread::sleep(Duration::from_millis(100));
                        continue;
                    }
                }
            }
        };
        let (w, h) = (img.width(), img.height());
        let rgb = img.into_raw();
        if w == 0 || h == 0 || rgb.len() != w as usize * h as usize * 3 {
            continue;
        }

        if want_work {
            last_work = std::time::Instant::now();
            if let Some(g) = work_gray_from_rgb(&rgb, w, h) {
                *work.lock().unwrap_or_else(|e| e.into_inner()) = Some(g);
            }
        }

        if want_preview {
            let mut preview = rgb;
            mirror_rgb(&mut preview, w);
            *frames.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(SharedFrame { w, h, rgb: preview });
        }
    }
}

fn decode_loop(
    stop: Arc<AtomicBool>,
    work: Arc<Mutex<Option<image::GrayImage>>>,
    res_tx: mpsc::Sender<WifiQr>,
) {
    let started = std::time::Instant::now();
    let mut misses = 0u32;
    while !stop.load(Ordering::Relaxed) {
        let gray = work.lock().unwrap_or_else(|e| e.into_inner()).take();
        let Some(g) = gray else {
            std::thread::sleep(Duration::from_millis(30));
            continue;
        };
        if let Some(qr) = crate::qr::decode_gray_variants(&g) {
            let done = qr::route(&qr).is_some();
            if res_tx.send(qr).is_err() {
                break;
            }
            if done {
                stop.store(true, Ordering::Relaxed);
                break;
            }
            continue;
        }
        misses += 1;
        let calm = started.elapsed() > Duration::from_secs(5) || misses > 50;
        std::thread::sleep(if calm {
            Duration::from_millis(300)
        } else {
            Duration::from_millis(20)
        });
    }
}
pub fn camera_self_test() -> anyhow::Result<()> {
    use nokhwa::pixel_format::RgbFormat;
    let (mut cam, camera_name) = open_any_camera().map_err(anyhow::Error::msg)?;
    println!("using camera: {camera_name}");
    for i in 0..5 {
        let frame = cam.frame()?;
        let img = match frame.decode_image::<RgbFormat>() {
            Ok(i) => i,
            Err(e) => match image::load_from_memory(frame.buffer())
                .ok()
                .map(|i| i.to_rgb8())
            {
                Some(decoded) => decoded,
                None => return Err(e.into()),
            },
        };
        let (w, h) = (img.width(), img.height());
        let raw = img.into_raw();
        println!(
            "frame {i}: {w}x{h} bytes={} ok={}",
            raw.len(),
            raw.len() == w as usize * h as usize * 3
        );
        if try_decode_rgb(&raw, w, h).is_some() {
            println!("decoded a QR payload");
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    Ok(())
}
pub fn open_scanner(
    app: &gtk4::Application,
    on_code: Rc<dyn Fn(&WifiQr) -> bool>,
) -> gtk4::ApplicationWindow {
    crate::ui::load_css();
    let window = gtk4::ApplicationWindow::new(app);
    window.set_title(Some("Scan QR code"));
    window.add_css_class("rnet-window");
    window.set_default_size(480, 560);
    window.init_layer_shell();
    window.set_layer(Layer::Top);
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
    let work: Arc<Mutex<Option<image::GrayImage>>> = Arc::new(Mutex::new(None));
    let tick_id: Rc<std::cell::RefCell<Option<gtk4::glib::SourceId>>> =
        Rc::new(std::cell::RefCell::new(None));
    let shutdown = {
        let stop = stop.clone();
        let frames = frames.clone();
        let work = work.clone();
        let tick_id = tick_id.clone();
        let picture = picture.clone();
        Rc::new(move || {
            stop.store(true, Ordering::Relaxed);
            if let Some(id) = tick_id.borrow_mut().take() {
                id.remove();
            }
            picture.set_paintable(Option::<&gtk4::gdk::Texture>::None);
            frames.lock().unwrap_or_else(|e| e.into_inner()).take();
            work.lock().unwrap_or_else(|e| e.into_inner()).take();
        })
    };
    let (res_tx, res_rx) = mpsc::channel::<WifiQr>();
    let (status_tx, status_rx) = mpsc::channel::<String>();
    {
        let stop = stop.clone();
        let frames = frames.clone();
        let work = work.clone();
        std::thread::spawn(move || camera_loop(stop, frames, work, status_tx));
    }
    {
        let stop = stop.clone();
        let work = work.clone();
        std::thread::spawn(move || decode_loop(stop, work, res_tx));
    }
    {
        let shutdown = shutdown.clone();
        window.connect_close_request(move |_| {
            shutdown();
            gtk4::glib::Propagation::Proceed
        });
    }
    {
        let window = window.clone();
        let shutdown = shutdown.clone();
        close_btn.connect_clicked(move |_| {
            shutdown();
            window.close();
        });
    }
    {
        let on_code = on_code.clone();
        let window = window.clone();
        file_btn.connect_clicked(move |_| {
            let on_code = on_code.clone();
            let window = window.clone();
            gtk4::glib::MainContext::default().spawn_local(async move {
                file_scan_flow(on_code, window).await;
            });
        });
    }
    let on_code_tick = on_code.clone();
    let window_tick = window.clone();
    let tick_id_sink = tick_id.clone();
    *tick_id_sink.borrow_mut() = Some(gtk4::glib::timeout_add_local(PREVIEW_TICK, move || {
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
            if on_code_tick(&code) {
                let window_tick = window_tick.clone();
                gtk4::glib::timeout_add_local_once(Duration::from_secs(1), move || {
                    window_tick.close();
                });
                return gtk4::glib::ControlFlow::Break;
            }
            status.set_text("Unsupported QR code");
            return gtk4::glib::ControlFlow::Continue;
        }
        gtk4::glib::ControlFlow::Continue
    }));
    window
}
fn qr_json(qr: &WifiQr) -> serde_json::Value {
    serde_json::json!({
        "ssid": qr.ssid,
        "psk": qr.password.as_deref().unwrap_or(""),
        "hidden": qr.hidden,
        "security": match qr.security {
            qr::WifiSecurity::Open => "Open",
            qr::WifiSecurity::Wpa => "Wpa",
            _ => "Other",
        },
    })
}

fn child_output_to_cmd(out: &[u8]) -> Option<BackendCmd> {
    let line = out
        .split(|b| *b == b'\n')
        .filter_map(|l| {
            let s = std::str::from_utf8(l).ok()?;
            let s = s.trim();
            (!s.is_empty()).then_some(s)
        })
        .next_back()?;
    let v: serde_json::Value = serde_json::from_str(line).ok()?;
    let ssid = v.get("ssid")?.as_str()?;
    if ssid.is_empty() {
        return None;
    }
    let password = match v.get("psk")?.as_str()? {
        "" => None,
        p => Some(crate::state::Secret::from(p)),
    };
    let hidden = v.get("hidden")?.as_bool()?;
    let security = match v.get("security")?.as_str()? {
        "Open" => qr::WifiSecurity::Open,
        "Wpa" => qr::WifiSecurity::Wpa,
        _ => return None,
    };
    qr::route(&WifiQr {
        ssid: ssid.to_string(),
        password,
        hidden,
        security,
    })
}

pub async fn run_scanner_child() -> Option<BackendCmd> {
    let exe = std::env::current_exe().ok()?;
    let out = tokio::time::timeout(
        Duration::from_secs(180),
        tokio::process::Command::new(exe)
            .arg("--scan-qr")
            .arg("--parent-pid")
            .arg(std::process::id().to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .ok()?
    .ok()?;
    if !out.status.success() {
        return None;
    }
    child_output_to_cmd(&out.stdout)
}

pub fn scanner_child(parent_pid: Option<u32>) -> anyhow::Result<()> {
    if let Some(ppid) = parent_pid {
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(2));
                if std::os::unix::process::parent_id() != ppid {
                    std::process::exit(1);
                }
            }
        });
    }
    let app = gtk4::Application::new(
        Some("dev.abhinash-pdl.rnetapplet.Scanner"),
        gtk4::gio::ApplicationFlags::NON_UNIQUE,
    );
    app.connect_activate(move |app| {
        let quit = app.clone();
        let on_code = Rc::new(move |code: &WifiQr| {
            if qr::route(code).is_none() {
                return false;
            }
            println!("{}", qr_json(code));
            let _ = std::io::Write::flush(&mut std::io::stdout());
            quit.quit();
            true
        });
        let window = open_scanner(app, on_code);
        let quit = app.clone();
        window.connect_hide(move |_| quit.quit());
    });
    app.run_with_args::<&str>(&[]);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn work_gray_from_rgb_matches_reference_luma() {
        let (w, h) = (512u32, 256u32);
        let mut rgb = vec![0u8; w as usize * h as usize * 3];
        for y in 0..h as usize {
            for x in 0..w as usize {
                let i = (y * w as usize + x) * 3;
                let v = ((x * 4 + y * 2) % 256) as u8;
                rgb[i] = v;
                rgb[i + 1] = v;
                rgb[i + 2] = v;
            }
        }
        let got = work_gray_from_rgb(&rgb, w, h).unwrap();
        assert_eq!(got.dimensions(), (w, h));
        for y in 0..h as usize {
            for x in 0..w as usize {
                let v = ((x * 4 + y * 2) % 256) as u8;
                assert_eq!(got.get_pixel(x as u32, y as u32)[0], v, "at {x},{y}");
            }
        }
    }
    #[test]
    fn work_gray_from_rgb_downsamples_large_frames() {
        let (w, h) = (1920u32, 1080u32);
        let rgb = vec![128u8; w as usize * h as usize * 3];
        let got = work_gray_from_rgb(&rgb, w, h).unwrap();
        assert_eq!(got.dimensions(), (WORK_MAX_W, 540));
        assert!(got.pixels().all(|p| p[0] == 128));
    }
    #[test]
    fn work_gray_from_rgb_upscales_tiny_frames() {
        let (w, h) = (160u32, 120u32);
        let rgb = vec![200u8; w as usize * h as usize * 3];
        let got = work_gray_from_rgb(&rgb, w, h).unwrap();
        assert!(got.width() >= WORK_MIN_W, "got {}", got.width());
        assert!(got.pixels().all(|p| p[0] == 200));
    }
    #[test]
    fn work_gray_from_rgb_rejects_malformed_input() {
        assert!(work_gray_from_rgb(&[0; 10], 4, 4).is_none(), "short buffer");
        assert!(work_gray_from_rgb(&[], 0, 4).is_none(), "zero width");
    }
    #[test]
    fn mirror_rgb_reverses_each_row() {
        let mut rgb = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        mirror_rgb(&mut rgb, 2);
        assert_eq!(rgb, vec![4u8, 5, 6, 1, 2, 3, 10, 11, 12, 7, 8, 9]);
        let mut odd = vec![1u8, 2, 3, 4, 5, 6, 7, 8, 9];
        mirror_rgb(&mut odd, 3);
        assert_eq!(odd, vec![7u8, 8, 9, 4, 5, 6, 1, 2, 3]);
        let mut empty: Vec<u8> = Vec::new();
        mirror_rgb(&mut empty, 0);
        assert!(empty.is_empty());
    }

    #[test]
    fn child_output_parses_to_connect() {
        let out = b"tracing noise\n{\"ssid\":\"Cafe\",\"psk\":\"hunter2\",\"hidden\":false,\"security\":\"Wpa\"}\n";
        let cmd = child_output_to_cmd(out).expect("parse");
        assert!(matches!(cmd, BackendCmd::ConnectSecure { .. }));
        assert!(child_output_to_cmd(b"").is_none());
        assert!(child_output_to_cmd(b"not json\n").is_none());
        assert!(
            child_output_to_cmd(
                b"{\"ssid\":\"\",\"psk\":\"\",\"hidden\":false,\"security\":\"Wpa\"}\n"
            )
            .is_none()
        );
        assert!(
            child_output_to_cmd(
                b"{\"ssid\":\"C\",\"psk\":\"\",\"hidden\":false,\"security\":\"Other\"}\n"
            )
            .is_none()
        );
    }

    #[test]
    fn child_output_hidden_open_routes() {
        let out = b"{\"ssid\":\"C\",\"psk\":\"\",\"hidden\":true,\"security\":\"Open\"}\n";
        assert!(matches!(
            child_output_to_cmd(out),
            Some(BackendCmd::ConnectHidden { .. })
        ));
    }

    #[test]
    fn child_output_hidden_wpa_round_trip() {
        let qr = WifiQr {
            ssid: "H".into(),
            password: Some(crate::state::Secret::from("hunter2")),
            hidden: true,
            security: crate::qr::WifiSecurity::Wpa,
        };
        let rendered = qr_json(&qr).to_string();
        let cmd = child_output_to_cmd(rendered.as_bytes()).expect("parse");
        assert!(matches!(cmd, BackendCmd::ConnectHidden { .. }));
    }

    #[test]
    fn work_dims_is_bounded_in_both_directions() {
        assert_eq!(work_dims(1920, 1080), (960, 540));
        assert_eq!(work_dims(800, 600), (800, 600), "already in range");
        assert_eq!(work_dims(200, 3000), (64, 960));
        assert_eq!(work_dims(0, 100), (0, 0));
    }
}
