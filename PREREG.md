# Pre-registro / Pre-registration

**ES.** Este archivo y `prereg.toml` se commitean y se publican **antes** de escribir el código de medición y antes de medir. Cualquier cambio posterior es un desvío del protocolo y se reporta en el README.

**EN.** This file and `prereg.toml` are committed and published **before** the measurement code is written and before measuring. Any later change is a protocol deviation and is reported in the README.

## Question / Pregunta

How many WAL bytes per transaction do full page images (FPI, Postgres `full_page_writes`) cost in a new B+tree with 16 KiB pages, compared with a mode that relies on atomic (untearable) page writes such as Linux `RWF_ATOMIC`? And does a crash harness confirm that each protection really prevents torn pages, while turning both off does not?

¿Cuántos bytes de WAL por transacción cuestan las imágenes de página completas (FPI, `full_page_writes` de Postgres) en un B+tree nuevo con páginas de 16 KiB, contra un modo que confía en escrituras de página atómicas (sin desgarro) como `RWF_ATOMIC` de Linux? ¿Y el arnés de crashes confirma que cada protección evita páginas rotas, y que sin ninguna de las dos aparecen?

## Design / Diseño

- **New B+tree.** Not `bptree-wal`. Slotted 16 KiB pages with a page LSN and a CRC32C, ARIES-style redo with physiological records ("set value of key k in page p"), and page-LSN idempotence.
- **WAL.**
  - FPI mode: the first change to a page after a checkpoint's redo pointer logs the page image, without the free-space hole.
  - Atomic mode: never logs FPIs and assumes page writes are atomic.
  - Splits log the resulting pages in full in both modes.
- **Page writes in the harness.**
  - Non-atomic: 4096-byte units, each persisted with p = 0.5 when the crash hits that page.
  - Atomic (emulated): the whole page or nothing.
- **Crash point.** Uniform over the page writes of the checkpoint flushes in the trial. WAL records of committed transactions are durable.
- **Box limits, declared now.** Kernel 6.12 and no device with atomic writes. `RWF_ATOMIC` is only **detected** (`statx` `STATX_WRITE_ATOMIC`), never measured. Throughput and commit p99 with real atomic writes are **not** measured. Atomic mode in the harness is an emulation that proves the recovery logic, not the device.

## Metrics and thresholds / Métricas y umbrales

| Metric | Threshold |
|---|---|
| WAL bytes/tx, atomic vs FPI, primary checkpoint interval (10k tx), 1M keys, 300k random updates, 5 runs, bootstrap 95% CI | reduction >= 40% (CI lower bound); also reported at 1k and 100k |
| Torn pages after recovery, FPI on, non-atomic writes, 10k crashes | 0 |
| Torn pages after recovery, FPI off, emulated atomic writes, 10k crashes | 0 |
| Positive control: FPI off, non-atomic writes, 10k crashes | >= 1 torn page (expected many) |
| Committed values after recovery match the model | all trials, every mode except the control |
| `RWF_ATOMIC` on the box | detected and reported only |

Seeds: bytes runs 0..4; crashes 0..9999 per mode. If a result is 0 out of N, the README reports the rule-of-three 95% bound (3/N).
