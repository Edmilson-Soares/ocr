//! Jobs em thread própria (baixar modelos, reconhecer) com fila de eventos.
//!
//! O callback do fogu:ffi só pode rodar na thread do interpretador, durante
//! uma chamada FFI, então a thread do job nunca chama o Fogu: ela enfileira
//! eventos (JSON) e `foc_dispatch` os entrega na thread de quem chamou.
//! O último evento é sempre `done` (com `ok`), guardado também em `result`.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};

pub struct Event {
    pub json: String,
}

#[derive(Clone)]
pub struct Emitter {
    tx: mpsc::Sender<Event>,
    result: Arc<Mutex<Option<Value>>>,
}

impl Emitter {
    pub fn emit(&self, v: Value) {
        let _ = self.tx.send(Event { json: v.to_string() });
    }

    pub fn done(&self, mut v: Value) {
        v["type"] = "done".into();
        *self.result.lock().unwrap_or_else(|e| e.into_inner()) = Some(v.clone());
        self.emit(v);
    }
}

pub struct Ctx {
    pub emit: Emitter,
    pub cancel: Arc<AtomicBool>,
}

pub struct Job {
    events: Mutex<mpsc::Receiver<Event>>,
    result: Arc<Mutex<Option<Value>>>,
    cancel: Arc<AtomicBool>,
    thread: Mutex<Option<thread::JoinHandle<()>>>,
}

impl Job {
    /// Cria o job e roda `f` numa thread; `f` termina com `ctx.emit.done(..)`
    /// (ou devolve o erro, que vira um `done` com `ok: false`).
    pub fn spawn(name: &str, f: impl FnOnce(&Ctx) -> Result<Value, String> + Send + 'static) -> Job {
        let (tx, rx) = mpsc::channel();
        let result = Arc::new(Mutex::new(None));
        let cancel = Arc::new(AtomicBool::new(false));
        let ctx = Ctx { emit: Emitter { tx, result: result.clone() }, cancel: cancel.clone() };
        let handle = thread::Builder::new()
            .name(format!("foguocr-{name}"))
            .spawn(move || {
                let emit = ctx.emit.clone();
                match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&ctx))) {
                    Ok(Ok(mut v)) => {
                        v["ok"] = true.into();
                        emit.done(v);
                    }
                    Ok(Err(e)) => emit.done(json!({ "ok": false, "error": e })),
                    Err(_) => emit.done(json!({ "ok": false, "error": "internal panic in foguocr job" })),
                }
            })
            .expect("spawn job thread");
        Job { events: Mutex::new(rx), result, cancel, thread: Mutex::new(Some(handle)) }
    }

    /// Espera até `timeout` pelo primeiro evento e entrega a `deliver` até `max`
    /// eventos (os seguintes sem esperar). Retorna quantos foram entregues.
    pub fn drain(&self, timeout: Duration, max: usize, mut deliver: impl FnMut(&Event) -> bool) -> usize {
        let rx = self.events.lock().unwrap_or_else(|e| e.into_inner());
        let mut n = 0;
        let mut wait = timeout;
        while n < max.max(1) {
            let ev = if wait.is_zero() { rx.try_recv().ok() } else { rx.recv_timeout(wait).ok() };
            let Some(ev) = ev else { break };
            n += 1;
            wait = Duration::ZERO;
            if !deliver(&ev) {
                break;
            }
        }
        n
    }

    pub fn result(&self) -> Option<Value> {
        self.result.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn cancel(&self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Cancela (se ainda roda) e espera a thread terminar.
    pub fn close(&self) {
        self.cancel();
        if let Some(h) = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
    }
}
