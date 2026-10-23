# App performance benchmarks

Release-mode timings for import, list, search, and export. Use these numbers for
capacity notes and curriculum metrics. AI / cascade benchmarks are separate
(`bun run test:cascade`) and are not part of this suite.

## How to run

```sh
bun run test:performance
```

Requirements:

- Bun and a Rust toolchain
- the private compatibility corpus cloned into `data/email/` (includes the
  25 MB and 4.3 MB Comuni fixtures; maintainers only, it holds real contacts)
- compare runs on the same machine and the same `--release` build

The suite is ignored by default unit tests. It prints `[performance]` lines to
stdout.

## Workloads

| Metric                          | Workload                                                                                               |
| ------------------------------- | ------------------------------------------------------------------------------------------------------ |
| TXT parse                       | `Comuni/Comuni 7.txt` (~25 MB, 107 519 rows, 177 087 candidates)                                       |
| XLSX parse                      | `Comuni/Comuni 4.xlsx` (~4.3 MB, 107 519 rows, 177 086 candidates)                                     |
| Persist                         | first 20 000 candidates from the TXT parse, SQLite in-memory, FTS suspended during insert then rebuilt |
| List / search / facets / export | 100 000 synthetic `utenteN@example.test` contacts, status `valid`                                      |

Corpus scale (compatibility data, not a timed run): about 258 files, ~205 MB on
disk, ~3.0M extracted addresses in `src-tauri/fixtures/corpus-baseline.tsv`.

## Reference measurement

| Host                          | Date       | Build                                                 |
| ----------------------------- | ---------- | ----------------------------------------------------- |
| Apple M5, macOS 27.2, aarch64 | 2026-09-25 | `cargo test --release` via `bun run test:performance` |

Schema v54 materializes the exportable contact projection (`contacts.is_exportable`,
partial indexes) and loads all facets in one scan; the row parser matches each
header against the field keys once instead of once per row. The v53 column is
the previous reference run on the same machine.

| Metric                               | v54 (2026-09-25)                            | v53 (2026-09-21)              |
| ------------------------------------ | ------------------------------------------- | ----------------------------- |
| TXT parse                            | 1 630 ms · ~108.6k candidates/s · 14.6 MB/s | 4 477 ms · 5.3 MB/s           |
| XLSX parse                           | 996 ms · ~177.8k candidates/s · 4.4 MB/s    | 2 468 ms · 1.8 MB/s           |
| Persist (20k)                        | 736 ms · ~27.2k occurrences/s               | 591 ms · ~33.8k occurrences/s |
| List first page (100k, with total)   | 0.8 ms                                      | 418 ms                        |
| List mid page (offset 50k, no total) | 4.5 ms                                      | 299 ms                        |
| FTS search `torino` (100k)           | 6.6 ms (10 000 hits)                        | 54 ms                         |
| Facet load (100k)                    | 53 ms                                       | 108 ms                        |
| CSV export (100k)                    | 87 ms · ~1.15M contacts/s · 129 MB/s        | 291 ms · 35 MB/s              |

Persist varies between runs on the same build (591–768 ms for v53); the v54
import suspends the per-row contact triggers like the FTS triggers, so it pays
only a set-based flag refresh at the end.

## Curriculum one-liners

Concrete claims from the reference run above (always keep the hardware tag):

- Parsed a 25 MB delimited contact file (~177k email candidates) at ~109k candidates/s on Apple M5
- Parsed the matching ~4.3 MB XLSX workbook at ~178k candidates/s on Apple M5
- Persisted 20k import occurrences into SQLite at ~27k occurrences/s (deduped to ~15k contacts)
- Full-text search over 100k contacts returned a 100-row page in ~7 ms on Apple M5
- Listed the first 100 of 100k contacts with an exact total in under 1 ms on Apple M5
- Streamed a 100k-row CSV export at ~1.15M contacts/s (~129 MB/s) on Apple M5
- Compatibility corpus: 258 files / ~205 MB / ~3.0M extracted addresses
