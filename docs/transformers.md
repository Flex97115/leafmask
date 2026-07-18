# Transformers

Every transformer is **deterministic**: the same input value and
[`salt`](configuration/index.md#common) always produce the same output, across
runs and machines. That's what keeps references consistent after anonymization.

List them from your own build (custom transformers included) with:

```sh
leafmask list-transformers
leafmask show-transformer random_int
```

## At a glance

| Transformer | Operates on | Parameters |
| --- | --- | --- |
| [`hash`](#hash) | any | `length` |
| [`random_int`](#random_int) | int | `min`, `max` |
| [`random_float`](#random_float) | float | `min`, `max` |
| [`noise_int`](#noise_int) | int | `ratio` |
| [`noise_float`](#noise_float) | float | `ratio` |
| [`noise_date`](#noise_date) | datetime | `max_days` |
| [`masking`](#masking) | string | `char` |
| [`regexp_replace`](#regexp_replace) | string | `regexp`, `replace` |
| [`replace`](#replace) | any | `value` |
| [`set_null`](#set_null) | any | — |
| [`random_email`](#random_email) | string | `domain` |
| [`random_person`](#random_person) | document | — |
| [`random_company`](#random_company) | string | — |
| [`random_object_id`](#random_object_id) | objectId | — |
| [`random_bytes`](#random_bytes) | binary | `length` |
| [`template`](#template) | string | `template` |

Parameters marked *(required)* must be supplied; *(optional)* ones fall back to a
default.

## Hashing

### `hash`

Replace the value with a deterministic hex hash of the original.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `length` | int | optional | number of hex characters to keep (default 16) |

```yaml
- field: token
  name: hash
  params:
    length: 32
```

## Random generators

### `random_int`

Deterministically generate an integer within `[min, max]`.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `min` | int | required | lower bound |
| `max` | int | required | upper bound |

```yaml
- field: age
  name: random_int
  params: { min: 18, max: 95 }
```

### `random_float`

Deterministically generate a float within `[min, max]`.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `min` | float | required | lower bound |
| `max` | float | required | upper bound |

```yaml
- field: score
  name: random_float
  params: { min: 0.0, max: 1.0 }
```

### `random_email`

Deterministically generate an email address. Distinct per input, so it's safe
under a unique index.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `domain` | string | optional | email domain (default `example.com`) |

```yaml
- field: email
  name: random_email
  params: { domain: mail.test }
```

### `random_person`

Deterministically generate consistent person fields (`first_name`, `last_name`,
`email`) into an **embedded document** — the three stay internally consistent.

```yaml
- field: profile
  name: random_person
```

### `random_company`

Deterministically generate a company name.

```yaml
- field: employer
  name: random_company
```

### `random_object_id`

Deterministically transform an ObjectId while keeping it a **valid ObjectId** —
ideal for `_id` fields you propagate to references.

```yaml
- field: _id
  name: random_object_id
  apply_for_references: true
```

### `random_bytes`

Deterministically generate binary data of a given length.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `length` | int | optional | number of bytes (default 16) |

```yaml
- field: signature
  name: random_bytes
  params: { length: 32 }
```

## Noise (perturb, keep type)

Noise transformers nudge a value within a range while preserving its BSON type —
useful when you want realistic-but-altered numbers and dates.

### `noise_int`

Perturb an integer by up to ±`ratio` of its magnitude.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `ratio` | float | optional | maximum fractional perturbation (default 0.1) |

```yaml
- field: quantity
  name: noise_int
  params: { ratio: 0.25 }
```

### `noise_float`

Perturb a float by up to ±`ratio` of its magnitude.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `ratio` | float | optional | maximum fractional perturbation (default 0.1) |

### `noise_date`

Shift a date by up to ±`max_days`.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `max_days` | int | optional | maximum absolute shift in days (default 30) |

```yaml
- field: signup_date
  name: noise_date
  params: { max_days: 14 }
```

## Masking & replacement

### `masking`

Replace every character of a string with a mask character, keeping the length.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `char` | string | optional | mask character (default `*`) |

```yaml
- field: ssn
  name: masking
  params: { char: "•" }
```

!!! warning
    `masking` collapses values to equal-length strings — **not** safe under a
    unique index. Use `hash` or `random_email` there.

### `regexp_replace`

Apply a regular-expression replacement to a string value.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `regexp` | string | required | pattern |
| `replace` | string | required | replacement |

```yaml
- field: phone
  name: regexp_replace
  params:
    regexp: "[0-9]"
    replace: "X"
```

### `replace`

Replace the value with a fixed configured value (any type).

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `value` | any | required | replacement value |

```yaml
- field: internal_note
  name: replace
  params: { value: "[redacted]" }
```

### `set_null`

Set the value to `null`. No parameters.

```yaml
- field: legacy_field
  name: set_null
```

## Templating

### `template`

Render a template with `{{ field }}` placeholders substituted from the document.

| Parameter | Type | | Description |
| --- | --- | --- | --- |
| `template` | string | required | template text |

```yaml
- field: display_name
  name: template
  params:
    template: "{{ first_name }} {{ last_name }}"
```

!!! tip "Custom transformers"
    Need something bespoke? Write a [custom
    transformer](configuration/transformations.md#custom-transformers) as an
    external command or a template — it appears here alongside the built-ins.
