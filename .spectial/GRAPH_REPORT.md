# Graph report — Leafmask

_33 features · 11 domain(s) · generated 2026-07-26_

## God nodes

A **god node** is a feature many others depend on — specifically, one whose number of dependents is in the top quartile and is at least 2. Change one and many features may break, so these carry the widest blast radius.

| Feature | Depended on by | Domain |
|---|---|---|
| `dump.storage-format` — On-storage dump format | 6 | dump |
| `mongo.access-layer` — MongoDB access abstraction | 5 | mongo |
| `config.loading` — Configuration file loading | 4 | config |
| `transform.parameter-framework` — Transformer parameter declaration and validation toolkit | 4 | transform |
| `transform.builtin-transformers` — Built-in transformer library | 3 | transform |
| `transform.custom-transformers` — Custom/external transformers | 3 | transform |
| `storage.streaming-multipart-upload` — Streaming multipart upload to cloud storage | 2 | storage |
| `subset.condition-filtering` — Dump only the documents matching a per-collection condition | 2 | subset |
| `subset.virtual-references` — Virtual (non-enforced) reference declaration | 2 | subset |
| `transform.apply-transformations` — Apply configured transformations while dumping | 2 | transform |
| `transform.transformation-condition` — Conditional transformation | 2 | transform |

## Leaf features

Nothing depends on these; safe to change in isolation.

- `bench.throughput` — Measure dump and restore throughput
- `catalog.list-transformers` — List available transformers
- `catalog.show-transformer` — Show one transformer's documentation
- `dumpmgmt.delete-dump` — Delete or prune dumps
- `dumpmgmt.list-dumps` — List dumps in storage
- `dumpmgmt.show-dump` — Show a dump's metadata and table of contents
- `transform.transformation-inheritance` — Transformation propagation to referencing fields
- `validate.schema-diff` — Schema drift detection
- `validate.transformation-preview` — Preview transformations against live data without dumping

## By domain

| Domain | Features |
|---|---|
| bench | 1 |
| catalog | 2 |
| config | 1 |
| dump | 2 |
| dumpmgmt | 3 |
| mongo | 1 |
| restore | 3 |
| storage | 6 |
| subset | 3 |
| transform | 8 |
| validate | 3 |

## Cycles

_None — a cycle is a validation error, so a generated report never has one._

## Suggested queries

- What breaks if I change `dump.storage-format`?
- What depends on `dump.storage-format`?
- Show the `transform` domain subgraph.
- Which features are leaves I can safely remove?
