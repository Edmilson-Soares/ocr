//! Os modelos do ocrs (detecção e reconhecimento de texto, ONNX) e o download
//! deles para `models/` (ou `FOGU_OCR_MODELS`), com eventos de progresso.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};

pub const DETECTION: &str = "text-detection.onnx";
pub const RECOGNITION: &str = "text-recognition.onnx";
pub const BASE_URL: &str = "https://ocrs-models.s3-accelerate.amazonaws.com/";

/// Pasta padrão dos modelos: `FOGU_OCR_MODELS` ou `models/` (relativa ao cwd).
pub fn default_dir() -> PathBuf {
    match std::env::var("FOGU_OCR_MODELS") {
        Ok(d) if !d.is_empty() => PathBuf::from(d),
        _ => PathBuf::from("models"),
    }
}

fn abs(p: &Path) -> PathBuf {
    if p.is_absolute() {
        return p.to_path_buf();
    }
    std::env::current_dir().map(|d| d.join(p)).unwrap_or_else(|_| p.to_path_buf())
}

pub fn display(p: &Path) -> String {
    let s = abs(p).to_string_lossy().into_owned();
    s.strip_prefix(r"\\?\").unwrap_or(&s).to_string()
}

/// `{ dir, detection, recognition, ready }` — os caminhos existem?
pub fn status(dir: &Path) -> Value {
    let det = dir.join(DETECTION);
    let rec = dir.join(RECOGNITION);
    json!({
        "dir": display(dir),
        "detection": if det.is_file() { Value::from(display(&det)) } else { Value::Null },
        "recognition": if rec.is_file() { Value::from(display(&rec)) } else { Value::Null },
        "ready": det.is_file() && rec.is_file(),
        "urls": [format!("{BASE_URL}{DETECTION}"), format!("{BASE_URL}{RECOGNITION}")],
    })
}

pub struct DownloadOpts {
    pub dir: PathBuf,
    pub base_url: String,
    pub force: bool,
    pub timeout: Duration,
}

impl DownloadOpts {
    pub fn from_json(v: &Value) -> DownloadOpts {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let mut base_url = s("base_url").unwrap_or_else(|| BASE_URL.into());
        if !base_url.ends_with('/') {
            base_url.push('/');
        }
        DownloadOpts {
            dir: s("dir").map(PathBuf::from).unwrap_or_else(default_dir),
            base_url,
            force: v.get("force").and_then(Value::as_bool).unwrap_or(false),
            timeout: Duration::from_millis(v.get("timeout_ms").and_then(Value::as_u64).unwrap_or(600_000)),
        }
    }
}

fn fetch(url: &str, dest: &Path, timeout: Duration, emit: &dyn Fn(Value), cancel: &AtomicBool) -> Result<(), String> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder().timeout_global(Some(timeout)).user_agent("foguocr").max_redirects(10).build(),
    );
    let mut resp = agent.get(url).call().map_err(|e| format!("download de {url}: {e}"))?;
    let total = resp.body_mut().content_length().unwrap_or(0);
    let mut reader = resp.body_mut().with_config().limit(u64::MAX).reader();
    let name = dest.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let tmp = dest.with_extension("part");
    let result = (|| -> Result<(), String> {
        let mut file = File::create(&tmp).map_err(|e| format!("{}: {e}", display(&tmp)))?;
        let mut buf = vec![0u8; 64 * 1024];
        let (mut done, mut last) = (0u64, 0u64);
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err("cancelado".into());
            }
            let n = reader.read(&mut buf).map_err(|e| format!("download: {e}"))?;
            if n == 0 {
                break;
            }
            file.write_all(&buf[..n]).map_err(|e| format!("{}: {e}", display(&tmp)))?;
            done += n as u64;
            if done - last >= 256 * 1024 {
                last = done;
                emit(json!({ "type": "progress", "file": name, "bytes": done, "total": total }));
            }
        }
        file.flush().map_err(|e| e.to_string())?;
        emit(json!({ "type": "progress", "file": name, "bytes": done, "total": total }));
        Ok(())
    })();
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
    std::fs::rename(&tmp, dest).map_err(|e| format!("{}: {e}", display(dest)))?;
    Ok(())
}

/// Baixa os dois modelos (os que faltam, ou todos com `force`).
pub fn download(opts: &DownloadOpts, emit: &dyn Fn(Value), cancel: &AtomicBool) -> Result<Value, String> {
    std::fs::create_dir_all(&opts.dir).map_err(|e| format!("não foi possível criar {}: {e}", display(&opts.dir)))?;
    let mut files = Vec::new();
    for name in [DETECTION, RECOGNITION] {
        let dest = opts.dir.join(name);
        if dest.is_file() && !opts.force {
            files.push(display(&dest));
            continue;
        }
        let url = format!("{}{name}", opts.base_url);
        emit(json!({ "type": "start", "file": name, "url": url }));
        fetch(&url, &dest, opts.timeout, emit, cancel)?;
        files.push(display(&dest));
    }
    Ok(json!({ "dir": display(&opts.dir), "files": files }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_and_opts() {
        let dir = std::env::temp_dir().join(format!("foguocr-none-{}", std::process::id()));
        let s = status(&dir);
        assert_eq!(s["ready"], false);
        assert!(s["detection"].is_null());
        let o = DownloadOpts::from_json(&json!({ "base_url": "http://x/y", "force": true }));
        assert_eq!(o.base_url, "http://x/y/");
        assert!(o.force);
        assert_eq!(DownloadOpts::from_json(&json!({})).dir, default_dir());
    }
}
