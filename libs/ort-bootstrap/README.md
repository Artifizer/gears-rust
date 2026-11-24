# ort-bootstrap

Bounded, abandonable initialization for the gears that load ONNX Runtime.

`ort` 2.0.0-rc.12 **hangs instead of erroring** when the ONNX Runtime library it `dlopen`s
via `ORT_DYLIB_PATH` cannot be loaded. The hang cannot be interrupted from inside, so the
blocked thread has to be abandoned rather than joined. This crate holds that one pattern,
so `file-parser` and the ONNX gears do not each carry their own copy of a subtle
correctness argument.

```rust,ignore
let detector = Bootstrap::new("Magika content-type detector", Duration::from_secs(30))
    .run(move || MagikaDetector::with_config(extensions, threads), ctx.cancellation_token())
    .await?;
```

See the crate docs for why a raw `std::thread` rather than `spawn_blocking`, and what
`BootstrapError::leaked_init_thread` obliges the caller to do. Only `Timeout` and
`Cancelled` (fired while the loader was running) leave a thread behind; the other
failures — `CancelledBeforeStart` (shutdown began before the loader started),
`SpawnFailed` (the OS refused the thread), `LoaderPanicked`, `InitThreadDied` and
`Load` — leak nothing.

Provision a compatible runtime with `scripts/fetch-onnxruntime.sh`.
