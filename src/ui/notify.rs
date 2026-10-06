pub(crate) fn send(summary: &str, body: &str) {
    let _ = std::process::Command::new("notify-send")
        .arg("rnetapplet")
        .arg(format!("{summary}: {body}"))
        .spawn();
}
