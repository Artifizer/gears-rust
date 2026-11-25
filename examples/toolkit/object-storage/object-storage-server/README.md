# object-storage-server

A standalone server that runs the object-storage example gear under Operation
Governance. The policies are Rego documents in
[`config/object-storage.yaml`](config/object-storage.yaml), loaded by the
`og-rego-config-plugin`.

Commands below run from the repository root.

## Build

```bash
cargo build -p object-storage-server
```

## Run

```bash
cargo run -p object-storage-server -- --config examples/toolkit/object-storage/object-storage-server/config/object-storage.yaml
```

The API listens on `http://127.0.0.1:8090`; OpenAPI docs are at
`http://127.0.0.1:8090/docs`. Authentication is disabled (static, accept all),
so every request is the same user in the default tenant. Data is kept in
SQLite under `~/.cf-gears-object-storage/`; delete that folder to start fresh.

## Try it

```bash
curl -s -X POST localhost:8090/object-storage/v1/objects -H 'content-type: application/json' -d '{"name":"report","content_type":"text/plain","body":"hello"}'
```

```bash
curl -s -X POST localhost:8090/object-storage/v1/objects -H 'content-type: application/json' -d '{"name":"tool","content_type":"application/x-msdownload","body":"MZ"}'
```

The second request is refused with `403` and
`"reason":"GOVERNANCE_BLOCKED: no-executables: executables cannot be stored"`.
Deleting an object younger than one hour is refused the same way by `retention`.

| Policy | Operation | Kind | Facts read |
|---|---|---|---|
| `no-executables` | create | guardrail | `content_type` (request) |
| `large-objects` | create | approval | `size_bytes` (computed) |
| `confidential-owner-only` | get | guardrail | `labels`, `is_owner` (computed) |
| `retention` | delete | guardrail | `age_seconds` (costly: the object is read only for it) |

`large-objects` cannot fire with this config: `max_body_bytes` (1 MB) rejects
larger bodies first. Raise `max_body_bytes` above 10 MB, or lower the policy's
threshold, to see `APPROVAL_REQUIRED`. The plugin only reports that approval
is required; it does not open approval requests.

## Test

Rust unit tests (no server needed; real SQLite and real Rego policies):

```bash
cargo test -p object-storage -p object-storage-sdk -p cf-gears-operation-governance-sdk -p cf-gears-og-rego-config-plugin
```

API and policy tests with pytest, against a running server:

```bash
python3 -m venv .venv
```

```bash
.venv/bin/pip install -r examples/toolkit/object-storage/object-storage-server/tests/requirements.txt
```

```bash
.venv/bin/pytest examples/toolkit/object-storage/object-storage-server/tests
```

Set `OBJECT_STORAGE_URL` to test a server elsewhere. Tests skip when the server
is not reachable. They create objects and, because of `retention`, cannot
delete them; delete `~/.cf-gears-object-storage/` to clean up.

To add a policy test: add the policy to `config/object-storage.yaml`, restart
the server, and assert on the status code and on `context.reason`, which
carries the policy name and its message, as
[`tests/test_object_storage.py`](tests/test_object_storage.py) does.
