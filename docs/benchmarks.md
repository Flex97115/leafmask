# Benchmarks

Real dump/restore throughput measured with `leafmask bench` against a live
MongoDB deployment.

## Results

Machine: **Apple M3, 24 GiB RAM** — **MongoDB 7.0.37 in Docker, local**.
Storage: local directory (gzip). Figures **exclude any S3 (or other remote
storage) network cost** — a networked backend adds upload/download latency and
bandwidth limits on top of these numbers.

| Documents | Dump | Restore | Dump size | Docs/s (dump) |
|---|---|---|---|---|
| 100,000 | 0.6s | 0.9s | 2.4 MiB | 175,799 |
| 1,000,000 | 5.3s | 9.0s | 24.1 MiB | 189,319 |
| 10,000,000 *(estimated)* | 52.8s | 1m 30s | 241.4 MiB | 189,319 |

The 10,000,000-document row is not actually run: it is a **linear
extrapolation from the 1,000,000 run**, scaled 10x.

Reproduce with:

```sh
leafmask bench --uri mongodb://localhost:27017 --markdown
```

## What `bench` does

`bench` runs an end-to-end, self-contained measurement for each requested
size, in order:

1. **Seed** — insert N synthetic "client" documents into
   `leafmask_bench.clients` on the target deployment. Seeding time is **not**
   included in the reported dump/restore timings.
2. **Dump** — time a real gzip dump of the `leafmask_bench` database to local
   directory storage.
3. **Drop** — drop the `leafmask_bench` database.
4. **Restore** — time a real restore of that dump back into an empty
   database.
5. **Verify** — check the restored document count matches what was seeded.
6. **Clean up** — drop the database and remove the temporary dump, so nothing
   is left behind between sizes.

### Document shape

Each seeded "client" document has 10 fields:

- `_id`
- `first_name`, `last_name`, `email`, `phone`
- `address` (`street`, `city`, `zip`)
- `status`
- `created_at`
- `orders_count`, `balance`

## Flags

| Flag | Default | Description |
| --- | --- | --- |
| `--uri <uri>` | — | MongoDB URI to benchmark against (or the global `--uri` / `LEAFMASK_MONGO_URI`) |
| `--sizes <n,n,...>` | `100000,1000000` | document counts to actually seed, dump, and restore |
| `--estimate <n>` | `10000000` | an extra row computed by linear extrapolation from the largest measured `--sizes` entry, marked *(estimated)* — never actually seeded or run |
| `--markdown` | off | print results as a markdown table (used to produce the table above) |
| `--keep` | off | don't drop the `leafmask_bench` database for the last size after the run |

No config file is required — `--uri` or `LEAFMASK_MONGO_URI` is enough. The
command requires a binary built with the `mongo` feature; see
[Commands → bench](commands.md#bench).

## Safety guard

`bench` always operates on a database literally named `leafmask_bench`. To
avoid ever touching unrelated data with the same name, it writes a marker
collection when it creates that database and refuses to seed into, drop, or
otherwise touch a `leafmask_bench` database it did not create itself. With
`--keep`, only the database for the *last* size in `--sizes` survives the run;
every other size is cleaned up as usual.

## Caveats

- All figures use **local directory storage**; they say nothing about network
  throughput to S3, Azure Blob, or SFTP backends.
- The 10,000,000-document figures are a **linear extrapolation**, not a
  measured run — actual performance at that scale may differ (e.g. due to
  memory pressure, disk I/O contention, or MongoDB server-side effects that
  don't scale linearly).
- Results depend heavily on hardware, disk speed, and MongoDB deployment
  topology; treat the table as a reference point; re-run `leafmask bench` on
  your own infrastructure for numbers that matter to you.
