# regex-scanner

`prompt-scanner` scanner-backend plugin for deterministic detection: contact details,
financial identifiers and credential/secret shapes, matched by pattern and — where the
format allows it — confirmed by a checksum.

It runs before and alongside the ML backends rather than instead of them. A Luhn-valid
card number or an IBAN with a correct check digit is a fact, not a probability, and is
scored accordingly.

## Configuration

```yaml
regex-scanner:
  config:
    vendor: cf.builtin.regex_scanner   # the name a tenant policy enables
    priority: 500
    entropy_threshold: 3.0             # bits/char for the `entropy` validator; finite, 0..=8
    max_matches: 1024                  # non-zero; matches reported per scan
    # patterns: [...]                  # absent = built-in set; [] = no detection
```

Every pattern runs on every payload; each match becomes its own detection. Past
`max_matches` the scan stops and the backend adds one span-less detection,
`regex_scanner.scan_incomplete`, carrying the most severe action of the patterns that were cut short,
so the remainder is redacted or blocked as a whole rather than passed through. A
bad pattern or an out-of-range setting fails gear init. To run two pattern sets side by
side, boot two instances with different `vendor` values.

### Patterns

```yaml
    patterns:
      - category: "financial.credit_card"   # becomes the finding's category; keep it dotted
        regex: '\b(?:\d[ -]?){12,18}\d\b'    # Rust `regex` syntax; YAML single quotes
        validator: "luhn"                   # optional post-match check
        tier: "validated"                   # confidence: pattern 0.5, structural 0.75, validated 0.95
        action: "block"                     # suggested action, overridable per tenant
      - category: "secret.generic"
        regex: '(?i)\b(?:password|secret|api[_-]?key|token)\s*[:=]\s*["'']?([A-Za-z0-9+/_.-]{8,})'
        capture: 1                          # group the validator inspects; the span is always group 0
        validator: "entropy"
        tier: "pattern"
        action: "warn"
```

| Field | Required | Meaning |
|---|---|---|
| `category` | yes | Reported verbatim. Dotted (`family.thing`) so a tenant's `category_action_overrides` can target a whole family. |
| `regex` | yes | Rust `regex` crate syntax: no backreferences, no lookaround. |
| `tier` | no (`pattern`) | Confidence on the finding: `pattern` 0.5, `structural` 0.75, `validated` 0.95. |
| `action` | yes | `allow`, `audit`, `warn`, `redact` or `block`. |
| `validator` | no | `luhn` (13-19 digits), `iban` (mod-97, spaces stripped), `ipv4`, `ipv6`, `jwt` (header shape, not signature), `entropy` (at least `entropy_threshold`). A match that fails it is dropped. |
| `capture` | no (`0`) | Capture group the validator inspects. |

With `patterns` absent the built-in set (`default_patterns` in `src/config.rs`) runs:
e-mail, URL, IPv4 and IPv6 (including `::`-prefixed and IPv4-embedded forms), phone,
card number (Luhn), IBAN (also the printed form with spaces), AWS, GitHub, OpenAI,
Anthropic, Slack and Stripe credentials, JWT, PEM private keys, connection strings with
credentials, and an entropy-gated `key=value` secret. An explicit `patterns: []` disables
detection and is logged at `warn`. All patterns are compiled into one `RegexSet`, so a
payload with nothing to find costs one pass.

## Provenance

The confidence tiering in `src/domain/recognizer.rs` ("pattern < structural <
validated"), the validator split in `src/domain/validators.rs`, and the block-not-redact
posture for card numbers in `src/domain/patterns/financial.rs` follow **dlp-bench**, a
public Rust benchmark for local PII-detection models:

> <https://github.com/MikeFalcon77/dlp-bench> — MIT, Copyright (c) 2026 MikeY

Per-module doc comments name the specific upstream files. This notice is the attribution
MIT asks for; see that repository's `LICENSE` for the full terms.
