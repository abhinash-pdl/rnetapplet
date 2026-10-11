use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::run;
use crate::state::{BackendCmd, Model, UiEvent};

pub(crate) fn run_popup_child(pos: Option<(i32, i32)>) -> anyhow::Result<()> {
    let ppid = std::os::unix::process::parent_id();
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(std::time::Duration::from_secs(2));
            if std::os::unix::process::parent_id() != ppid {
                std::process::exit(1);
            }
        }
    });
    let mut first = String::new();
    {
        use std::io::BufRead as _;
        let _ = std::io::stdin().lock().read_line(&mut first);
    }
    let initial = serde_json::from_str::<UiEvent>(&first)
        .ok()
        .and_then(|ev| match ev {
            UiEvent::Model(m) => Some(m),
            _ => None,
        })
        .map(|m| m.as_ref().clone())
        .unwrap_or_else(Model::empty);
    let (ev_tx, ev_rx) = async_channel::bounded::<UiEvent>(64);
    std::thread::spawn(move || {
        use std::io::BufRead as _;
        loop {
            let mut line = String::new();
            let n = std::io::stdin().lock().read_line(&mut line);
            let Ok(_) = n else {
                break;
            };
            if line.is_empty() {
                break;
            }
            let Ok(ev) = serde_json::from_str::<UiEvent>(&line) else {
                continue;
            };
            if ev_tx.send_blocking(ev).is_err() {
                break;
            }
        }
    });
    let (cmd_tx, cmd_rx) = async_channel::bounded::<BackendCmd>(64);
    std::thread::spawn(move || {
        use std::io::Write as _;
        let stdout = std::io::stdout();
        while let Ok(cmd) = cmd_rx.recv_blocking() {
            let Ok(mut line) = serde_json::to_string(&cmd) else {
                continue;
            };
            line.push('\n');
            let mut guard = stdout.lock();
            if guard.write_all(line.as_bytes()).is_err() {
                break;
            }
            let _ = guard.flush();
        }
    });
    let (_toggle_keep, toggle_rx) = async_channel::bounded::<Option<(i32, i32)>>(4);
    let _ = _toggle_keep.try_send(pos);
    let (_quit_keep, quit_rx) = async_channel::bounded::<()>(2);
    let visible = Arc::new(AtomicBool::new(true));
    let frozen = Arc::new(AtomicBool::new(false));
    run(
        initial, ev_rx, toggle_rx, quit_rx, cmd_tx, visible, frozen, true,
    );
    Ok(())
}
