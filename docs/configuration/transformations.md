# Transformations

Transformations are the heart of Leafmask: they rewrite field values while a
dump streams out, so the result is anonymized instead of a verbatim copy of
production.

They live under `dump.transformation` as a list — one entry per collection:

```yaml
dump:
  transformation:
    - collection: users
      transformers:
        - field: email
          name: random_email
        - field: ssn
          name: masking
```

Collections **without** an entry are dumped unchanged.

## Anatomy of a rule

Each item under `transformers` applies one transformer to one field:

```yaml
- field: created_at        # (1)!
  name: noise_date         # (2)!
  params:                  # (3)!
    max_days: 7
  when: "status == 'active'"   # (4)!
  type_override: datetime      # (5)!
```

1.  The target field. Dotted paths reach into embedded documents
    (`address.zip`). `column:` is accepted as an alias.
2.  A transformer name — built-in (see the [reference](../transformers.md)) or a
    [custom one](#custom-transformers).
3.  Parameters for the transformer, validated against their declared types at
    load time.
4.  Optional [condition](#conditional-transformation) — the transformer only runs
    when it's true.
5.  Optional [type override](#type-overrides).

| Field | Required | Description |
| --- | --- | --- |
| `field` (or `column`) | yes | target field path |
| `name` | yes | transformer name |
| `params` | no | transformer parameters |
| `when` | no | only apply when this expression is truthy |
| `dynamic_params` | no | derive a parameter from another field |
| `apply_for_references` | no | propagate to referencing fields |
| `type_override` | no | operate on a different logical type |

## Parameters

Parameters are typed and checked when the config loads — a value that can't be
cast to the expected type is rejected up front, not at dump time.

```yaml
- field: age
  name: random_int
  params:
    min: 18
    max: 95
```

Parameter keys are case-insensitive and preserve their original casing.

## Conditional transformation

A `when` expression gates a rule: the transformer only runs for documents where
it evaluates truthy; others pass that field through unchanged.

```yaml
- field: email
  name: set_null
  when: "opted_out == true"
```

A collection-level `when` skips the whole document's transformation:

```yaml
- collection: users
  when: "region == 'EU'"     # only anonymize EU users
  transformers:
    - field: email
      name: random_email
```

The expression language supports:

- field references (dotted paths): `age`, `address.country`
- comparisons: `==` `!=` `>` `<` `>=` `<=`
- boolean composition: `and`, `or`
- bare-field truthiness: `flags.enabled`

```yaml
when: "age >= 18 and country == 'US'"
```

## Dynamic parameters

A parameter can be derived from **another field on the same document** at
transform time, so generated data stays internally consistent (e.g. keep
`updated_at` after `created_at`):

```yaml
- field: updated_at
  name: noise_date
  params:
    max_days: 30
  dynamic_params:
    max_days:
      field: retention_days   # read the bound from the document
```

The resolved value is cast to the parameter's type just like a static value. A
`dynamic_params` entry that references a field the document doesn't have is a
validation error, not a crash.

## Type overrides

Force a transformer to operate on a different logical type than the field's
native BSON type. Handy when a value is stored as one type but you want to mask
it as another:

```yaml
- field: pin
  name: masking          # masking works on strings
  type_override: string  # ...but `pin` is an int
```

Valid types: `string`, `int`, `float`, `bool`, `objectId`, `decimal128`,
`datetime`, `binary`, `bytes`, `any`.

## Propagation to references

`apply_for_references: true` makes a transformer automatically apply to every
field that holds a [declared reference](subsetting.md#virtual-references) to the
transformed field — keeping foreign-key-like values consistent without repeating
the config. A referencing field that has its own explicit transformer is never
overridden.

```yaml
transformation:
  - collection: users
    transformers:
      - field: _id
        name: random_object_id
        apply_for_references: true   # every *.user_id gets the same mapping
```

## Restricting documents

A collection entry can carry a `query` to dump only matching documents:

```yaml
- collection: users
  query:
    active: true       # only dump active users
  transformers:
    - field: email
      name: random_email
```

For following relationships to keep a subset referentially intact, see
[Subsetting](subsetting.md).

## Custom transformers

Extend the engine with your own transformers under `custom_transformers`. They
show up in the [catalog](../commands.md#show-transformer) next to the built-ins
and can declare their own typed parameters.

=== "Command driver"

    Spawns an executable that exchanges one JSON value per line over
    stdin/stdout. Write it in any language.

    ```yaml
    custom_transformers:
      - name: my_masker
        driver: command
        command: ["python3", "masker.py"]
        parameters:
          - name: locale
            type: string
            required: true
    ```

    ```yaml
    dump:
      transformation:
        - collection: users
          transformers:
            - field: phone
              name: my_masker
              params:
                locale: fr
    ```

=== "Template driver"

    A `{{ field }}` template rendered from the document — no external process.

    ```yaml
    custom_transformers:
      - name: display_name
        driver: template
        template: "{{ first_name }} {{ last_name }}"
    ```

!!! danger "Uniqueness under indexes"
    If a field has a **unique index**, anonymize it with a transformer that
    preserves distinctness (`random_email`, `hash`, `random_object_id`).
    `masking` collapses values to equal-length stars and will collide on restore.
