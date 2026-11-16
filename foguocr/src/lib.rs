//! foguocr — OCR (imagem -> texto) para o Fogu via `fogu:ffi`, em Rust puro:
//! [ocrs](https://github.com/robertknight/ocrs) (detecção + reconhecimento de
//! texto) sobre o runtime [rten](https://github.com/robertknight/rten). Os
//! dois modelos (ONNX, ~10 MB) são baixados uma vez para `models/`.
//!
//! Convenções (Fogu 0.7+: o fogu:ffi aceita qualquer número de argumentos e
//! callbacks; ver `cb.rs` e `foc_set_sink`):
//! - Motores (`foc_load`) e jobs (`foc_download`, `foc_recognize*`) são
//!   handles `i64` (> 0), liberados com `foc_close`.
//! - Jobs rodam em thread própria; `foc_dispatch` entrega os eventos (JSON)
//!   pelo callback e o último é `done` (também em `foc_result`).
//! - Funções de status retornam `0` em sucesso e `-1` em erro; a mensagem
//!   fica em `foc_last_error`.
//! - Saídas (JSON/texto) vão para o buffer do chamador (`ffi.alloc`),
//!   terminadas em NUL; retorno = tamanho escrito, `-(tamanho necessário)` se
//!   o buffer for pequeno, ou -1 em erro. Com um sink registrado
//!   (`foc_set_sink`), `buf = NULL, cap = 0` entrega a saída inteira pelo callback.

mod cb;
mod engine;
mod job;
mod models;

use std::collections::HashMap;
use std::ffi::{CStr, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use engine::{Engine, LoadOpts, RecognizeOpts};
use job::Job;

enum Entry {
    Engine(Arc<Engine>),
    Job(Arc<Job>),
}

static HANDLES: LazyLock<Mutex<HashMap<i64, Entry>>> = LazyLock::new(Default::default);
static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

fn set_error(msg: impl Into<String>) {
    *LAST_ERROR.lock().unwrap() = msg.into();
}

/// Executa `f` sem deixar um panic atravessar a fronteira C.
fn guard<T>(on_error: T, f: impl FnOnce() -> Result<T, String>) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            set_error(e);
            on_error
        }
        Err(_) => {
            set_error("internal panic in foguocr");
            on_error
        }
    }
}

fn status(f: impl FnOnce() -> Result<(), String>) -> i32 {
    guard(-1, || f().map(|_| 0))
}

fn register(entry: Entry) -> i64 {
    let handle = NEXT_HANDLE.fetch_add(1, Ordering::Relaxed);
    HANDLES.lock().unwrap().insert(handle, entry);
    handle
}

fn engine_of(handle: i64) -> Result<Arc<Engine>, String> {
    match HANDLES.lock().unwrap().get(&handle) {
        Some(Entry::Engine(e)) => Ok(e.clone()),
        Some(Entry::Job(_)) => Err(format!("handle {handle} é um job, não um motor")),
        None => Err(format!("invalid handle {handle}")),
    }
}

fn job_of(handle: i64) -> Result<Arc<Job>, String> {
    match HANDLES.lock().unwrap().get(&handle) {
        Some(Entry::Job(j)) => Ok(j.clone()),
        Some(Entry::Engine(_)) => Err(format!("handle {handle} é um motor, não um job")),
        None => Err(format!("invalid handle {handle}")),
    }
}

unsafe fn text<'a>(ptr: *const c_char) -> Result<&'a str, String> {
    if ptr.is_null() {
        return Ok("");
    }
    unsafe { CStr::from_ptr(ptr) }.to_str().map_err(|_| "string is not valid UTF-8".to_string())
}

unsafe fn json_arg(ptr: *const c_char) -> Result<Value, String> {
    let s = unsafe { text(ptr) }?;
    if s.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(s).map_err(|e| format!("invalid JSON argument: {e}"))
}

unsafe fn bytes_arg(data: *const u8, len: i64) -> Result<Vec<u8>, String> {
    if len <= 0 || data.is_null() {
        return Err("invalid buffer (empty)".into());
    }
    Ok(unsafe { std::slice::from_raw_parts(data, len as usize) }.to_vec())
}

unsafe fn write_out(buf: *mut u8, cap: i64, s: &str) -> i64 {
    if let Some(n) = cb::try_sink(buf, cap, s.as_bytes()) {
        return n;
    }
    let need = s.len() + 1;
    if buf.is_null() || cap < need as i64 {
        return -(need as i64);
    }
    unsafe {
        std::ptr::copy_nonoverlapping(s.as_ptr(), buf, s.len());
        *buf.add(s.len()) = 0;
    }
    s.len() as i64
}

unsafe fn write_json(buf: *mut u8, cap: i64, value: Value) -> i64 {
    unsafe { write_out(buf, cap, &value.to_string()) }
}

/// Versão da biblioteca. Retorna o tamanho escrito (ver convenções).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_version(buf: *mut u8, cap: i64) -> i64 {
    unsafe { write_out(buf, cap, env!("CARGO_PKG_VERSION")) }
}

/// Última mensagem de erro. Retorna o tamanho escrito (0 se não há erro).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_last_error(buf: *mut u8, cap: i64) -> i64 {
    let msg = LAST_ERROR.lock().unwrap().clone();
    unsafe { write_out(buf, cap, &msg) }
}

/// Registra (ou remove, com NULL) o callback (Fogu 0.7+, `ffi.callback`) que
/// recebe as saídas das funções chamadas com `buf = NULL, cap = 0` (ver `cb.rs`).
#[unsafe(no_mangle)]
pub extern "C" fn foc_set_sink(sink: Option<cb::Sink>) -> i32 {
    cb::set_sink(sink);
    0
}

// ---- modelos ------------------------------------------------------------

/// `{"dir", "detection", "recognition", "ready", "urls"}` para a pasta `dir`
/// (vazia = `FOGU_OCR_MODELS` ou `models/`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_models(dir: *const c_char, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || {
        let dir = unsafe { text(dir) }?;
        let dir = if dir.is_empty() { models::default_dir() } else { PathBuf::from(dir) };
        Ok(unsafe { write_json(buf, cap, models::status(&dir)) })
    })
}

/// Baixa os modelos que faltam. `opts_json`: `{"dir", "base_url", "force", "timeout_ms"}`.
/// Eventos: `start` {file, url}, `progress` {file, bytes, total}, `done` {ok, dir, files}.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_download(opts_json: *const c_char) -> i64 {
    guard(-1, || {
        let opts = models::DownloadOpts::from_json(&unsafe { json_arg(opts_json) }?);
        Ok(register(Entry::Job(Arc::new(Job::spawn("download", move |ctx| {
            let emit = ctx.emit.clone();
            models::download(&opts, &|v| emit.emit(v), &ctx.cancel)
        })))))
    })
}

// ---- motor --------------------------------------------------------------

/// Carrega os modelos. `opts_json`: `{"dir", "detection", "recognition",
/// "beam": true | largura, "alphabet", "allowed_chars", "debug"}`.
/// Retorna o handle do motor (> 0) ou -1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_load(opts_json: *const c_char) -> i64 {
    guard(-1, || {
        let opts = LoadOpts::from_json(&unsafe { json_arg(opts_json) }?);
        Ok(register(Entry::Engine(Arc::new(Engine::load(&opts)?))))
    })
}

/// `{"detection", "recognition"}` do motor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_engine_info(engine: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || Ok(unsafe { write_json(buf, cap, engine_of(engine)?.info()) }))
}

fn spawn_recognize(engine: Arc<Engine>, load: impl FnOnce() -> Result<image::DynamicImage, String> + Send + 'static, opts: RecognizeOpts) -> i64 {
    register(Entry::Job(Arc::new(Job::spawn("recognize", move |_ctx| {
        let img = load()?;
        engine::recognize(&engine, &img, &opts)
    }))))
}

/// Reconhece o texto de um arquivo de imagem (PNG, JPEG, BMP, GIF, WebP, TIFF).
/// `opts_json`: `{"scale": 1.0, "max_width": 0, "words": false, "chars":
/// false, "corners": false}`. Retorna o handle do job; o evento `done` traz
/// `{ok, text, lines: [{text, box: [x, y, w, h], words?, chars?, corners?}],
/// width, height, scale, ms}`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_recognize_file(engine: i64, path: *const c_char, opts_json: *const c_char) -> i64 {
    guard(-1, || {
        let path = PathBuf::from(unsafe { text(path) }?);
        if !path.is_file() {
            return Err(format!("imagem não encontrada: {}", models::display(&path)));
        }
        let opts = RecognizeOpts::from_json(&unsafe { json_arg(opts_json) }?);
        Ok(spawn_recognize(engine_of(engine)?, move || engine::open(&path), opts))
    })
}

/// Como `foc_recognize_file`, com os bytes de uma imagem codificada (PNG, JPEG...).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_recognize_bytes(engine: i64, data: *const u8, len: i64, opts_json: *const c_char) -> i64 {
    guard(-1, || {
        let bytes = unsafe { bytes_arg(data, len) }?;
        let opts = RecognizeOpts::from_json(&unsafe { json_arg(opts_json) }?);
        Ok(spawn_recognize(engine_of(engine)?, move || engine::decode(&bytes), opts))
    })
}

/// Como `foc_recognize_file`, com pixels crus (`channels` = 1 cinza, 3 RGB, 4 RGBA).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_recognize_raw(
    engine: i64,
    data: *const u8,
    len: i64,
    width: i64,
    height: i64,
    channels: i64,
    opts_json: *const c_char,
) -> i64 {
    guard(-1, || {
        let bytes = unsafe { bytes_arg(data, len) }?;
        let opts = RecognizeOpts::from_json(&unsafe { json_arg(opts_json) }?);
        let (w, h, c) = (width.max(0) as u32, height.max(0) as u32, channels.max(0) as u32);
        Ok(spawn_recognize(engine_of(engine)?, move || engine::from_raw(&bytes, w, h, c), opts))
    })
}

// ---- jobs ---------------------------------------------------------------

/// Espera até `timeout_ms` pelo primeiro evento e entrega até `max` eventos
/// chamando `on_event(json, len, bin, len)` na thread de quem chamou.
/// Retorna quantos foram entregues ou -1.
#[unsafe(no_mangle)]
pub extern "C" fn foc_dispatch(handle: i64, timeout_ms: i64, max: i64, on_event: Option<cb::EventFn>) -> i64 {
    guard(-1, || {
        let f = on_event.ok_or("dispatch: on_event is NULL")?;
        let job = job_of(handle)?;
        let n = job.drain(Duration::from_millis(timeout_ms.max(0) as u64), max.max(1) as usize, |ev| cb::event(f, &ev.json, &[]));
        Ok(n as i64)
    })
}

/// O evento `done` do job (JSON), ou 0 se ainda não terminou.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn foc_result(handle: i64, buf: *mut u8, cap: i64) -> i64 {
    guard(-1, || match job_of(handle)?.result() {
        Some(v) => Ok(unsafe { write_json(buf, cap, v) }),
        None => Ok(0),
    })
}

/// Pede o cancelamento do job (um download para; um reconhecimento termina o que começou).
#[unsafe(no_mangle)]
pub extern "C" fn foc_cancel(handle: i64) -> i32 {
    status(|| {
        job_of(handle)?.cancel();
        Ok(())
    })
}

/// Libera um motor ou um job (jobs: cancela e espera a thread).
#[unsafe(no_mangle)]
pub extern "C" fn foc_close(handle: i64) -> i32 {
    status(|| {
        let entry = HANDLES.lock().unwrap().remove(&handle).ok_or_else(|| format!("invalid handle {handle}"))?;
        if let Entry::Job(job) = entry {
            job.close();
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;

    fn call(f: impl FnOnce(*mut u8, i64) -> i64) -> (i64, String) {
        let mut buf = vec![0u8; 1 << 16];
        let n = f(buf.as_mut_ptr(), buf.len() as i64);
        let s = if n >= 0 { String::from_utf8(buf[..n as usize].to_vec()).unwrap() } else { String::new() };
        (n, s)
    }

    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    fn last_error() -> String {
        call(|b, n| unsafe { foc_last_error(b, n) }).1
    }

    #[test]
    fn version_models_and_errors() {
        let (n, v) = call(|b, c| unsafe { foc_version(b, c) });
        assert_eq!(v, env!("CARGO_PKG_VERSION"));
        assert_eq!(n as usize, v.len());
        let dir = c("does-not-exist-models");
        let (_, j) = call(|b, n| unsafe { foc_models(dir.as_ptr(), b, n) });
        let s: Value = serde_json::from_str(&j).unwrap();
        assert_eq!(s["ready"], false);
        let opts = c(r#"{"dir": "does-not-exist-models"}"#);
        assert_eq!(unsafe { foc_load(opts.as_ptr()) }, -1);
        assert!(last_error().contains("não encontrado"), "{}", last_error());
        assert_eq!(foc_close(999), -1);
        let empty = c("");
        assert_eq!(unsafe { foc_recognize_file(999, empty.as_ptr(), empty.as_ptr()) }, -1);
    }

    /// Roda de verdade se os modelos estiverem em FOGU_OCR_MODELS ou ../models.
    #[test]
    fn recognize_if_models_present() {
        let dir = std::env::var("FOGU_OCR_MODELS").map(PathBuf::from).unwrap_or_else(|_| PathBuf::from("../models"));
        if !dir.join(models::DETECTION).is_file() {
            eprintln!("(modelos ausentes em {}; teste pulado)", dir.display());
            return;
        }
        let opts = c(&json!({ "dir": dir.to_string_lossy() }).to_string());
        let eng = unsafe { foc_load(opts.as_ptr()) };
        assert!(eng > 0, "{}", last_error());
        // Imagem sintética: fundo branco com texto preto desenhado em blocos
        // grandes (sem fonte); só verifica que o pipeline roda e devolve JSON.
        let img = image::RgbImage::from_pixel(320, 120, image::Rgb([255, 255, 255]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(img).write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png).unwrap();
        let o = c("{}");
        let job = unsafe { foc_recognize_bytes(eng, png.as_ptr(), png.len() as i64, o.as_ptr()) };
        assert!(job > 0, "{}", last_error());
        let deadline = std::time::Instant::now() + Duration::from_secs(60);
        let mut done = String::new();
        while std::time::Instant::now() < deadline {
            let (n, s) = call(|b, n| unsafe { foc_result(job, b, n) });
            if n > 0 {
                done = s;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let v: Value = serde_json::from_str(&done).expect("done");
        assert_eq!(v["ok"], true, "{v}");
        assert_eq!(v["width"], 320);
        assert!(v["lines"].is_array());
        assert_eq!(foc_close(job), 0);
        assert_eq!(foc_close(eng), 0);
    }
}
