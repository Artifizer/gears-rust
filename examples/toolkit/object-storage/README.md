# Object Storage — an example of Operation Governance in a gear

A small object store (create, get, update, delete) with a REST API and a
SQLite or Postgres table, whose every operation is under Operation Governance.

| Crate | What to look at |
|---|---|
| `object-storage-sdk` | `src/gts.rs`: one Governed Operation type per operation. Its fields are the Facts policies read, with their Fact Tiers. It is separate from the REST DTOs. |
| `object-storage` | `src/domain/governance.rs`: one `FactProvider` per operation. `src/domain/service.rs`: Authorization → Governance Check → write. |
| `object-storage-server` | `config/object-storage.yaml`: Rego policies loaded by the `og-rego-config-plugin`. |

## The governance step, in one place

```rust
// 1. Authorization
let scope = self.authorize(ctx, actions::CREATE, None, tenant_id).await?;
// 2. Governance, before the first side effect
let facts = CreateOpFacts { repo, conn: &conn, scope: &scope, new: &new, tenant_id, owner_id };
self.govern(ctx, GovernedResource::new(OBJECT_RESOURCE_TYPE, tenant_id), &facts).await?;
// 3. The write
self.repo.insert(&conn, &scope, object).await
```

When no policy governs an operation, the check is an in-memory lookup: no Fact
is loaded. Otherwise the Enforcer asks the provider only for the Facts the
policies read, cheapest tier first. Costly Facts are on demand:
`owner_object_count` is a query only when a policy reads it, and delete reads
the object only when a policy reads its `age_seconds`, which comes from the
`created_at` the gear generates.

## Run it

```bash
cargo run -p object-storage-server -- --config examples/toolkit/object-storage/object-storage-server/config/object-storage.yaml
```

```bash
curl -s -X POST localhost:8090/object-storage/v1/objects -H 'content-type: application/json' -d '{"name":"a","content_type":"application/x-msdownload","body":"MZ"}'
```

That request is refused with `403` and the policy's message. Deleting an object
younger than one hour is refused the same way.
