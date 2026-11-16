# ocr — imagem para texto (OCR) para Fogu

Uma lib Rust (`foguocr`) com API C para o `fogu:ffi`, mais um módulo Fogu (`ocr.fg`), que
reconhece texto em imagens (fotos, capturas de tela, documentos digitalizados) e devolve as
linhas em ordem de leitura, com caixas, palavras e caracteres.

É Rust puro: [ocrs](https://github.com/robertknight/ocrs) (detecção e reconhecimento de texto)
sobre o runtime [rten](https://github.com/robertknight/rten). Não precisa de Tesseract nem de
bibliotecas do sistema. Os dois modelos (ONNX, ~10 MB no total) são baixados uma vez para
`models/` com `ocr.download()`. Reconhecem texto impresso no alfabeto latino **sem acentos**:
o alfabeto do modelo é ASCII (letras, dígitos e pontuação), então `ê`, `ç`, `ã` saem como a
letra mais parecida ou `?`. Não reconhecem manuscrito nem outros alfabetos.

```
foguocr/        lib Rust (cdylib) com a API C
native/         foguocr.dll / libfoguocr.so / libfoguocr.dylib (gerado pelo build)
models/         text-detection.onnx + text-recognition.onnx (baixados por ocr.download())
ocr.fg          wrapper Fogu: download(), load_engine(), Engine, text(), recognize()
main.fg         exemplo: gera uma imagem com texto, reconhece e mostra as caixas
```

<!-- fogu:plataformas -->
## Plataformas, binários e pré-requisitos

Ao importar `ocr.fg`, a lib reconhece o sistema e a arquitetura
(`system.info.os`/`arch`) e já carrega o binário certo — `.dll` no Windows,
`.so` no Linux, `.dylib` no macOS. Se ele faltar, o programa não para no `import`:
o erro (com a lista de caminhos tentados) volta na primeira chamada que precisar da lib.
`nativeInfo()` mostra o que foi detectado: `{ platform, os, file, path, loaded, error }`.

| Plataforma | Arquivo | Neste repositório |
|---|---|---|
| `windows-x64` | `native/windows-x64/foguocr.dll` | incluído |
| `windows-arm64` | `native/windows-arm64/foguocr.dll` | — |
| `linux-x64` | `native/linux-x64/libfoguocr.so` | incluído |
| `linux-arm64` | `native/linux-arm64/libfoguocr.so` | incluído |
| `macos-x64` | `native/macos-x64/libfoguocr.dylib` | incluído |
| `macos-arm64` | `native/macos-arm64/libfoguocr.dylib` | incluído |

**Instalação** (Fogu 0.8.0+): `fogu add Edmilson-Soares/ocr@^0.2.0`. Os binários de `native/` vão
no repositório git, então a lib já funciona depois de instalada — sem Rust/cargo na máquina.

Ordem de busca: o caminho passado a `load(path)`, a variável `FOGU_OCR_LIB`,
`native/<plataforma>/`, `native/` e o build do cargo no projeto em execução, uma
cópia em `fogu_modules/` e os pacotes instalados com `fogu add` (`$FOGU_HOME/fogu_modules`).

**Pré-requisitos**

- Nenhum em nenhum sistema (Rust puro). Os modelos ficam em `models/` ou na pasta de
  `FOGU_OCR_MODELS`; `ocr.download()` os baixa (precisa de internet uma vez). Para baixar à
  mão, veja [Modelos](#modelos-download).
- Rust ≥ 1.85 (edition 2024) para gerar os binários.

**Gerar os binários**

```sh
fogu run build:native        # Windows: native/windows-<arch>/ (build.ps1)
sh build.sh                  # Linux/macOS/WSL: o sistema atual
sh build.sh all              # Linux x64/arm64 e macOS x64/arm64 (cargo-zigbuild + zig)
```

## Callbacks do fogu:ffi (requer Fogu 0.8.0+)

A camada nativa usa `ffi.callback` (veja `_native.fg`, igual em todas as libs deste repositório):

- **Sink de saída**: a lib recebe um callback uma vez (`foc_set_sink`) e, chamada com
  `buf = nil, cap = 0`, entrega a resposta inteira (JSON) por ele.
- **Regras do fogu:ffi respeitadas**: um callback só roda na thread do interpretador, durante
  uma chamada FFI — a thread do reconhecimento nunca chama o Fogu; ela enfileira eventos e
  `foc_dispatch` os entrega na thread de quem chamou.
- **Esperas cooperativas**: `recognize()`/`download()` esperam em fatias de 10 ms com
  `time.delay(1ms)` entre elas, então as outras tarefas `fg` continuam rodando enquanto os
  modelos trabalham.
- **Fogu 0.7.4+** (e 0.8.0): o código compilado chama direto as funções FFI de assinatura numérica (sem
  alocação por chamada), então sondar status/eventos num laço é barato; `fogu .` roda o projeto
  (o script `start` do `fogu.json`); os tipos `int32`, `uint8`, `float64`... valem nas assinaturas.
<!-- /fogu:plataformas -->

## Modelos (download)

Os dois modelos são os do projeto [ocrs](https://github.com/robertknight/ocrs) (licença
Apache-2.0 / MIT), publicados em
<https://ocrs-models.s3-accelerate.amazonaws.com/>:

| arquivo | link | tamanho |
|---|---|---|
| detecção | <https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx> | ~2,5 MB |
| reconhecimento | <https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx> | ~9,7 MB |

`ocr.download()` (ou `ocr.load_engine({ download: true })`) baixa os dois para `models/`.
Para baixar à mão (máquina sem internet, proxy, CI), salve os arquivos com esses nomes em
`models/` ou na pasta de `FOGU_OCR_MODELS`:

```sh
mkdir -p models
curl -L -o models/text-detection.onnx   https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx
curl -L -o models/text-recognition.onnx https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx
```

```powershell
New-Item -ItemType Directory -Force models | Out-Null
Invoke-WebRequest https://ocrs-models.s3-accelerate.amazonaws.com/text-detection.onnx   -OutFile models/text-detection.onnx
Invoke-WebRequest https://ocrs-models.s3-accelerate.amazonaws.com/text-recognition.onnx -OutFile models/text-recognition.onnx
```

`ocr.models()` devolve `{ dir, detection, recognition, ready, urls }` com os links em uso;
`ocr.download({ baseUrl: "https://meu-servidor/ocrs/" })` baixa de um espelho próprio (os
nomes dos arquivos são os mesmos). Outros modelos ocrs (ou um de reconhecimento treinado com
outro alfabeto) entram por `ocr.load_engine({ detection, recognition, alphabet })`.

## Build e execução

```sh
fogu run build:native       # ou ./build.ps1 (Windows) / ./build.sh (Linux/macOS)
fogu run main.fg            # baixa os modelos na primeira vez, gera uma imagem e reconhece
fogu run main.fg foto.jpg   # reconhece a sua imagem
fogu test                   # os testes de reconhecimento são pulados sem modelos
```

## Como funciona

```fogu
import ocr from "./ocr.fg"

fn main() {
    if !ocr.ready()? { ocr.download()? }               // models/ (uma vez)
    print(ocr.text("nota.jpg")?)                        // só o texto, linhas com "\n"

    const r = ocr.recognize("nota.jpg", { words: true })?
    for line in r.lines { print(line.text, line.box) }  // box = [x, y, largura, altura]

    const digits = ocr.load_engine({ allowedChars: "0123456789" })?   // só números
    print(digits.text("medidor.png")?)
    return true, nil
}
main()
```

A imagem pode ser um caminho (PNG, JPEG, BMP, GIF, WebP, TIFF), bytes de uma imagem
codificada, ou uma imagem do `fogu:image` (capturas, recortes, pré-processamento: `img.crop`,
`img.grayscale`, `img.contrast`...). O motor detecta as palavras, agrupa em linhas, reconhece
cada linha e devolve tudo em ordem de leitura. Os modelos são carregados uma vez por
`Engine` (ou pelo motor padrão de `ocr.text`/`ocr.recognize`).

### `ocr.recognize(image, options)` / `engine.recognize(...)`

| opção | padrão | |
|---|---|---|
| `scale` | `1.0` | redimensiona antes de reconhecer: `2.0` ajuda em texto pequeno, `0.5` em imagens enormes (as caixas voltam na escala original) |
| `maxWidth` | `0` | reduz imagens mais largas que isso |
| `words` | `false` | cada linha traz `words: [{ text, box }]` |
| `chars` | `false` | cada linha traz `chars: [{ char, box }]` |
| `corners` | `false` | os 4 cantos da caixa rotacionada de cada linha |
| `timeout` | sem limite | |

Resultado: `{ text, lines: [{ text, box: [x, y, w, h], words?, chars?, corners? }], width,
height, scale, ms }`. `engine.text(image)` devolve só o texto. `engine.start(image)` devolve o
`Job` sem esperar (`job.wait()`, `job.events()`, `job.cancel()`). `engine.recognizeRaw(bytes,
width, height, channels)` aceita pixels crus (1 = cinza, 3 = RGB, 4 = RGBA).

### `ocr.load_engine(options)`

| opção | |
|---|---|
| `dir`, `detection`, `recognition` | pasta ou caminhos dos modelos (padrão `models/`) |
| `beam` | `true` ou largura: decodificação por busca em feixe (mais precisa, mais lenta) |
| `allowedChars` | só esses caracteres na saída (`"0123456789"` para números, `"ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"` para placas) |
| `alphabet` | alfabeto de um modelo de reconhecimento próprio |
| `download` | `true`: baixa os modelos se faltarem |

`ocr.models(dir)` diz o que está baixado; `ocr.download({ dir, force, onProgress })` baixa.

### Dicas de qualidade

- Texto com menos de ~20 px de altura: use `scale: 2`.
- Fotos tortas ou com sombra: recorte e corrija antes com o `fogu:image` (`crop`, `rotate`,
  `grayscale`, `contrast`).
- Para campos numéricos use `allowedChars`, que elimina confusões como `O`/`0` e `l`/`1`.
- Acentos: o modelo não os produz. Para textos em português, corrija depois com um dicionário
  ou compare sem acentos (`"lê"` chega como `"l?"` ou `"le"`).

## API C (`foguocr`)

Prefixo `foc_`. Handles `i64` (> 0) para motores e jobs; status `0`/`-1` (mensagem em
`foc_last_error`); saídas no buffer do chamador ou pelo sink.

| função | |
|---|---|
| `foc_version`, `foc_last_error`, `foc_set_sink` | |
| `foc_models(dir, buf, cap)` | `{ dir, detection, recognition, ready, urls }` |
| `foc_download(opts_json)` | job: `start`, `progress` {file, bytes, total}, `done` {ok, dir, files} |
| `foc_load(opts_json)` | handle do motor; `foc_engine_info(h, buf, cap)` |
| `foc_recognize_file(h, path, opts)`, `foc_recognize_bytes(h, data, len, opts)`, `foc_recognize_raw(h, data, len, w, h, channels, opts)` | job; `done` {ok, text, lines, width, height, scale, ms} |
| `foc_dispatch(job, timeout_ms, max, cb)`, `foc_result(job, buf, cap)`, `foc_cancel`, `foc_close` | |

Testes da lib Rust: `cargo test --manifest-path foguocr/Cargo.toml` (o de reconhecimento roda
se `../models` ou `FOGU_OCR_MODELS` tiver os modelos).
