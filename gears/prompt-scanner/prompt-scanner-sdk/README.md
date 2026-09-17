# prompt-scanner SDK

Transport-agnostic public API for the `prompt-scanner` gear. Consumers depend on this
crate, never on the gear itself.

- `PromptScannerClientV1` — `scan`, `scan_async`, `get_job_status`, `get_verdict`,
  `find_by_correlation_id`, `delete_evidence`, policy read/write.
- `ScanRequest` (`ScanContent::Text` or `ScanContent::File`) / `ScanInput` / `ScanVerdict` /
  `Detection` / `Finding` / `VerdictAction` / `PromptScannerError` — the models, size
  limits (`MAX_PAYLOAD_BYTES`, `MAX_ATTACHMENT_BYTES`, ...), plus `redact_spans` for
  applying a `Redact` verdict to text. `Debug` on request types prints payloads as lengths.
- `ScanVerdict::unscored_backends` (`UnscoredBackend` / `UnscoredReason`: `timeout`,
  `unavailable`, `error`, `deferred`) — enabled backends that did not contribute to the
  verdict (degraded coverage). They never change `action`; check the list when you need
  full coverage. Set on the synchronous `scan` verdict only.
- `ScannerBackendClient` + `ScannerBackendSpecV1` — the extension point a detector
  implements to join the fan-out. A backend may return `BackendOutcome::Defer` on an
  inline run; the gateway retries it in the background with `ScanInput::phase` set to
  `ScanPhase::Deferred` (`is_deferred_run()`), where it must scan in full and must not
  defer again.
- `PolicyProvider` + `PolicyProviderSpecV1` — the extension point that supplies tenant
  policy; the gear stores none. It deliberately takes no
  database runner, so a provider backed by an external policy service fits.

Both extension points register the same way: build a `PluginV1<ScannerBackendSpecV1>` (or
`PluginV1<PolicyProviderSpecV1>`) registration, push it to `types-registry`, then register
the implementation into `ClientHub` with `register_scoped` under
`ClientScope::gts_id(&instance_id)`. A backend's `vendor` is the name a tenant policy puts
in `enabled_backends`. `regex-scanner` and `always-allow-plugin` are small working
examples; the traits' rustdoc (`src/plugin_api.rs`, `src/gts.rs`) states the contracts.

See [../docs/DESIGN.md](../docs/DESIGN.md) for the architecture.
