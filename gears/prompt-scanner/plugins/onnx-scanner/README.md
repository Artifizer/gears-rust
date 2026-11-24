# onnx-scanner

`prompt-scanner` scanner-backend plugin that runs PII detection on ONNX models it hosts
in-process.
Two decoder families behind one `ScannerBackendClient`: GLiNER2 span extraction
(`PiiBackendConfig::Gliner2`) and BIO token classification
(`PiiBackendConfig::BioTokenClassifier`), selected by config.

It owns the model-specific tensor work — prompt construction, span decoding, word
alignment, label mapping — in `src/domain/`, and, in `src/runtime/`, the model host those
backends run on: replica pool, SHA-256 and graph-contract verification, input bounds,
inference timeout and load shedding, session rebuild after a panic, tokenization, metrics
and readiness. The ONNX Runtime session itself (one pinned `ort`, an open under a deadline
on an abandonable thread, exclusive use of one session) is the toolkit's
(`toolkit-onnx-runtime`). The two layers meet at one internal trait, `ModelRuntime`, so a
backend is testable against a fake without a model on disk.

The host was the standalone `onnx-runner` gear until it was absorbed here; its design and
recorded decisions are in [`docs/runtime/`](./docs/runtime/DESIGN.md).

## Configuration

The models are listed under this plugin's own config, not under a separate gear:

```yaml
onnx-scanner:
  config:
    models:            # see docs/runtime/config-example.yaml for every field
      - model_id: gliner2-pii
        model_dir: ./models/gliner2-pii
        sha256: "..."
    backend: { kind: gliner2, model_id: gliner2-pii }
```

Models and the ONNX Runtime are provisioned out of band (`scripts/fetch-models.sh`,
`scripts/fetch-onnxruntime.sh`) and verified at start; nothing is fetched on the request
path.

`backend.model_id` must name an entry of `models`. A payload over `max_words` (default
2,000, at least 64; the old key `defer_threshold_words` is still accepted) or
`max_payload_bytes` (default 1 MiB) is not scanned: `detect` returns a `Validation` error
rather than deferring, so `prompt-scanner` fails the scan when this backend is required,
and otherwise lists it in the verdict's `unscored_backends` (`error`). An unloaded model
is reported the same way, as `unavailable`.

## Tests

The integration tests open a real ONNX session against the committed fixture and need an
ONNX Runtime on `ORT_DYLIB_PATH`. From the repository root:

```sh
export ORT_DYLIB_PATH="$(bash scripts/fetch-onnxruntime.sh --path)"
cargo test -p cf-gears-onnx-scanner --features fixture-gen,onnxruntime-tests
```

Plain `cargo test` skips them. Suites against the real models need `real-model-tests`.

## Provenance

The GLiNER2 decode path (`src/domain/gliner2/{mod,decode,prompt,tensor,words}.rs`) and the
`GLINER2_PII` label schema are derived from **dlp-bench**, a public Rust benchmark for
local PII-detection models:

> <https://github.com/MikeFalcon77/dlp-bench> — MIT, Copyright (c) 2026 MikeY

Per-module doc comments name the specific upstream files they came from. This notice is
the attribution MIT asks for; see that repository's `LICENSE` for the full terms.
