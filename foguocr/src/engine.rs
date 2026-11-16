//! Motor de OCR (ocrs): carrega os modelos, decodifica a imagem e reconhece o
//! texto, devolvendo linhas e palavras com caixas.

use std::path::{Path, PathBuf};
use std::time::Instant;

use image::{DynamicImage, GenericImageView, imageops::FilterType};
use ocrs::{DecodeMethod, ImageSource, OcrEngine, OcrEngineParams, TextItem};
use serde_json::{Value, json};

use crate::models::{self, display};

pub struct LoadOpts {
    pub detection: PathBuf,
    pub recognition: PathBuf,
    pub beam: Option<u32>,
    pub alphabet: Option<String>,
    pub allowed_chars: Option<String>,
    pub debug: bool,
}

impl LoadOpts {
    pub fn from_json(v: &Value) -> LoadOpts {
        let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
        let dir = s("dir").map(PathBuf::from).unwrap_or_else(models::default_dir);
        let beam = match v.get("beam") {
            Some(Value::Bool(true)) => Some(100),
            Some(Value::Number(n)) => n.as_u64().filter(|n| *n > 1).map(|n| n as u32),
            _ => None,
        };
        LoadOpts {
            detection: s("detection").map(PathBuf::from).unwrap_or_else(|| dir.join(models::DETECTION)),
            recognition: s("recognition").map(PathBuf::from).unwrap_or_else(|| dir.join(models::RECOGNITION)),
            beam,
            alphabet: s("alphabet"),
            allowed_chars: s("allowed_chars").or_else(|| s("allowedChars")),
            debug: v.get("debug").and_then(Value::as_bool).unwrap_or(false),
        }
    }
}

pub struct Engine {
    pub inner: OcrEngine,
    pub detection: PathBuf,
    pub recognition: PathBuf,
}

fn load_model(path: &Path, what: &str) -> Result<rten::Model, String> {
    if !path.is_file() {
        return Err(format!(
            "modelo de {what} não encontrado: {} (rode ocr.download() para baixar os modelos, ou passe dir/detection/recognition)",
            display(path)
        ));
    }
    rten::Model::load_file(path).map_err(|e| format!("não foi possível carregar {}: {e}", display(path)))
}

impl Engine {
    pub fn load(opts: &LoadOpts) -> Result<Engine, String> {
        let detection_model = load_model(&opts.detection, "detecção")?;
        let recognition_model = load_model(&opts.recognition, "reconhecimento")?;
        let params = OcrEngineParams {
            detection_model: Some(detection_model),
            recognition_model: Some(recognition_model),
            debug: opts.debug,
            decode_method: match opts.beam {
                Some(width) => DecodeMethod::BeamSearch { width },
                None => DecodeMethod::Greedy,
            },
            alphabet: opts.alphabet.clone(),
            allowed_chars: opts.allowed_chars.clone(),
        };
        let inner = OcrEngine::new(params).map_err(|e| format!("não foi possível iniciar o OCR: {e}"))?;
        Ok(Engine { inner, detection: opts.detection.clone(), recognition: opts.recognition.clone() })
    }

    pub fn info(&self) -> Value {
        json!({ "detection": display(&self.detection), "recognition": display(&self.recognition) })
    }
}

pub struct RecognizeOpts {
    pub scale: f64,
    pub max_width: u32,
    pub words: bool,
    pub chars: bool,
    pub corners: bool,
}

impl RecognizeOpts {
    pub fn from_json(v: &Value) -> RecognizeOpts {
        let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
        RecognizeOpts {
            scale: v.get("scale").and_then(Value::as_f64).filter(|s| *s > 0.0).unwrap_or(1.0),
            max_width: v.get("max_width").or_else(|| v.get("maxWidth")).and_then(Value::as_u64).unwrap_or(0) as u32,
            words: b("words"),
            chars: b("chars"),
            corners: b("corners"),
        }
    }
}

/// Decodifica PNG/JPEG/BMP/GIF/WebP/TIFF dos bytes.
pub fn decode(bytes: &[u8]) -> Result<DynamicImage, String> {
    image::load_from_memory(bytes).map_err(|e| format!("imagem inválida: {e}"))
}

pub fn open(path: &Path) -> Result<DynamicImage, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", display(path)))?;
    decode(&bytes)
}

/// Imagem crua (RGB, RGBA ou cinza, linha a linha).
pub fn from_raw(data: &[u8], width: u32, height: u32, channels: u32) -> Result<DynamicImage, String> {
    let expected = (width as usize) * (height as usize) * (channels as usize);
    if data.len() != expected {
        return Err(format!("imagem crua: esperados {expected} bytes ({width}x{height}x{channels}), recebidos {}", data.len()));
    }
    let img = match channels {
        1 => image::GrayImage::from_raw(width, height, data.to_vec()).map(DynamicImage::ImageLuma8),
        3 => image::RgbImage::from_raw(width, height, data.to_vec()).map(DynamicImage::ImageRgb8),
        4 => image::RgbaImage::from_raw(width, height, data.to_vec()).map(DynamicImage::ImageRgba8),
        _ => return Err("imagem crua: channels deve ser 1, 3 ou 4".into()),
    };
    img.ok_or_else(|| "imagem crua inválida".into())
}

/// Reconhece o texto da imagem. `{ text, lines: [{ text, box: [x, y, w, h],
/// corners?, words?: [{ text, box }], chars?: [{ char, box }] }], width,
/// height, scale, ms }`.
pub fn recognize(engine: &Engine, img: &DynamicImage, opts: &RecognizeOpts) -> Result<Value, String> {
    let t0 = Instant::now();
    let (orig_w, orig_h) = img.dimensions();
    let mut scale = opts.scale;
    if opts.max_width > 0 && orig_w > opts.max_width {
        scale = scale.min(opts.max_width as f64 / orig_w as f64);
    }
    let img = if (scale - 1.0).abs() > 1e-6 {
        let w = ((orig_w as f64) * scale).round().max(1.0) as u32;
        let h = ((orig_h as f64) * scale).round().max(1.0) as u32;
        img.resize_exact(w, h, FilterType::CatmullRom)
    } else {
        img.clone()
    };
    let rgb = img.to_rgb8();
    let (w, h) = rgb.dimensions();
    let source = ImageSource::from_bytes(rgb.as_raw(), (w, h)).map_err(|e| format!("imagem: {e}"))?;
    let input = engine.inner.prepare_input(source).map_err(|e| format!("preparar imagem: {e}"))?;
    let word_rects = engine.inner.detect_words(&input).map_err(|e| format!("detectar texto: {e}"))?;
    let line_rects = engine.inner.find_text_lines(&input, &word_rects);
    let lines = engine.inner.recognize_text(&input, &line_rects).map_err(|e| format!("reconhecer texto: {e}"))?;

    // Caixas de volta na escala da imagem original.
    let inv = 1.0 / scale;
    let fix = |v: i32| -> i64 { ((v as f64) * inv).round() as i64 };
    let box_of = |item: &dyn TextItem| -> Value {
        let r = item.bounding_rect();
        json!([fix(r.left()), fix(r.top()), fix(r.width()), fix(r.height())])
    };

    let mut out_lines = Vec::new();
    let mut texts = Vec::new();
    for line in lines.iter().flatten() {
        let text = line.to_string();
        if text.trim().is_empty() {
            continue;
        }
        let mut v = json!({ "text": text, "box": box_of(line) });
        if opts.corners {
            let corners: Vec<Value> = line.rotated_rect().corners().iter().map(|p| json!([(p.x as f64 * inv).round(), (p.y as f64 * inv).round()])).collect();
            v["corners"] = Value::Array(corners);
        }
        if opts.words {
            let words: Vec<Value> = line.words().map(|wd| json!({ "text": wd.to_string(), "box": box_of(&wd) })).collect();
            v["words"] = Value::Array(words);
        }
        if opts.chars {
            let chars: Vec<Value> = line
                .chars()
                .iter()
                .map(|c| json!({ "char": c.char.to_string(), "box": [fix(c.rect.left()), fix(c.rect.top()), fix(c.rect.width()), fix(c.rect.height())] }))
                .collect();
            v["chars"] = Value::Array(chars);
        }
        texts.push(text);
        out_lines.push(v);
    }
    Ok(json!({
        "text": texts.join("\n"),
        "lines": out_lines,
        "width": orig_w,
        "height": orig_h,
        "scale": scale,
        "ms": t0.elapsed().as_millis() as u64,
    }))
}
