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
    polymorphic resolution — is implemented and covered by tests. Per-collection
    document filtering is available today in `dump` via a collection
    [`query`](transformations.md#restricting-documents). Wiring the full
    multi-collection reference-following into the `dump` command is on the
    near-term roadmap; this page documents the model and config shape it uses.

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

Attach a condition to a collection to seed the subset with only the documents you
want. Everything reachable from them through declared references is included
automatically — even collections you didn't filter.

```yaml
subset_conds:
  orders: "region == 'EU'"
```

Given the `orders → users` reference above, dumping with this condition includes
**only EU orders** plus **exactly the users those orders reference**, even though
`users` has no filter of its own.

The condition uses the same [expression
language](transformations.md#conditional-transformation) as `when`.

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
