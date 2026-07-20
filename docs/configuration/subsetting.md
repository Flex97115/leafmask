# Subsetting

Dumping a whole production database is often overkill — you want a **small,
consistent slice**. Subsetting lets you filter selected collections and have
Leafmask automatically pull in the related documents needed to keep the slice
referentially intact.

Because MongoDB doesn't enforce foreign keys, you declare the relationships
yourself with **virtual references**. Leafmask then treats them exactly as a
relational engine treats foreign keys.

!!! info "Status"
    The subsetting engine — reference graph traversal, cycle handling,
    polymorphic resolution — is implemented and covered by tests, but wiring
    the full multi-collection reference-following into the `dump` command is
    still on the roadmap; this page documents the model and config shape it
    uses. What *is* wired into `dump` today: `subset_conds` and a collection's
    [`query`](transformations.md#restricting-documents) are both pushed down
    to MongoDB as a real server-side filter, so only matching documents are
    fetched — declaring one of them narrows what a collection dumps, it just
    doesn't (yet) pull in documents from other collections it references.

## Virtual references

A virtual reference declares that a field on one collection points at a document
in another:

```yaml
virtual_references:
  - collection: orders
    references:
      - field: user_id            # orders.user_id ...
        references_collection: users   # ... points at users._id
        references_field: _id     # defaults to _id
        not_null: true            # the reference must resolve
```

| Field | Required | Description |
| --- | --- | --- |
| `field` | yes | the referencing field on this collection |
| `references_collection` | yes* | the target collection (*or use `polymorphic_exprs`) |
| `references_field` | no | the referenced field (default `_id`) |
| `not_null` | no | whether the reference must be satisfied |
| `polymorphic_exprs` | no | [polymorphic targets](#polymorphic-references) |

## Filtering with `subset_conds`

Attach a condition to a collection to dump only the documents you want. It goes
under `dump:`, alongside `transformation`:

```yaml
dump:
  subset_conds:
    orders: "region == 'EU'"
```

The condition is translated into a native MongoDB filter and passed to `find()`
— it narrows what's fetched from `orders` itself. Once multi-collection
reference-following is wired in, dumping with this condition will also pull in
**exactly the users those EU orders reference**, even though `users` has no
filter of its own; today only the `orders` side of that is live.

The condition uses the same [expression
language](transformations.md#conditional-transformation) as `when` — field
comparisons (`== != > < >= <=`) and `and`/`or`. A quoted value that parses as
an RFC3339 timestamp (e.g. `"2026-07-17T00:00:00Z"`) is compared as a real
datetime, so date-range conditions work against a `datetime` field:

```yaml
dump:
  subset_conds:
    residence_temperature: "created_at >= '2026-07-17T00:00:00Z' and created_at < '2026-07-18T00:00:00Z'"
```

### Dynamic date windows

The condition language has no relative-date function (no `now()`, no date
arithmetic) — it only compares against literal values. For a rolling window
like "the last 30 days" or "last month", compute the boundary in the shell and
inject it with [environment
interpolation](index.md#environment-interpolation), the same mechanism used to
keep credentials out of the config file:

```yaml
dump:
  subset_conds:
    residence_temperature: "created_at >= '${SUBSET_SINCE}'"
```

```sh
export SUBSET_SINCE=$(date -u -v-30d +%Y-%m-%dT%H:%M:%SZ)             # macOS/BSD date: last 30 days
export SUBSET_SINCE=$(date -u -d '30 days ago' +%Y-%m-%dT%H:%M:%SZ)   # GNU/Linux date: last 30 days
export SUBSET_SINCE=$(date -u -v1d -v-1m +%Y-%m-%dT00:00:00Z)         # macOS/BSD date: start of last month
export SUBSET_SINCE=$(date -u -d "$(date +%Y-%m-01) -1 month" +%Y-%m-%dT%H:%M:%SZ) # GNU/Linux: start of last month

leafmask --config leafmask.yaml dump
```

!!! warning "An unset variable filters nothing, silently"
    An undefined `${SUBSET_SINCE}` interpolates to an empty string, not an
    error. Comparing a `datetime` field against `''` with `>=` matches **every**
    document — BSON's type-ordering ranks `Date` above `String`, so the
    condition is always true instead of always false. Echo the variable (or run
    the script with `set -u`) before dumping to make sure it's actually set.

## Cyclic references

Self-references and cycles (A → B → A) are handled safely — a visited-set bounds
the traversal, so a cyclic graph never loops forever or fails the dump:

```yaml
virtual_references:
  - collection: employees
    references:
      - field: manager_id
        references_collection: employees   # self-reference
```

## Polymorphic references

A field can point at **different collections depending on a discriminator**.
Declare the cases with `polymorphic_exprs`:

```yaml
virtual_references:
  - collection: comments
    references:
      - field: parent_id
        polymorphic_exprs:
          - field: parent_type      # when comments.parent_type == "post"
            value: post
            references_collection: posts
          - field: parent_type      # when comments.parent_type == "photo"
            value: photo
            references_collection: photos
```

When subsetting, each comment resolves to the correct target collection based on
its `parent_type`, and only the referenced posts/photos are pulled in.

## Putting it together

```yaml
subset_conds:
  orders: "total > 100 and region == 'EU'"

virtual_references:
  - collection: orders
    references:
      - field: user_id
        references_collection: users
      - field: coupon_id
        references_collection: coupons
        not_null: false
  - collection: order_items
    references:
      - field: order_id
        references_collection: orders
      - field: product_id
        references_collection: products
```

This keeps orders, their users, their (optional) coupons, their line items, and
the referenced products all consistent in the resulting subset.
