# cortex9-wal-atomic-bench

WAL without full page images: what atomic page writes save in a new B+tree, with a crash harness for torn pages.

> **ES** abajo · **EN** below

## ES

### Qué es

Un B+tree nuevo (páginas ranuradas de 16 KiB con LSN y CRC32C) con un WAL fisiológico y redo estilo ARIES. Compara dos formas de proteger páginas contra escrituras desgarradas:

- **FPI**: la primera modificación de cada página después de un checkpoint registra una imagen completa de la página (con eliminación de hueco), como `full_page_writes` de Postgres.
- **Atómico (emulado)**: el disco nunca desgarra una escritura de página, como prometería `RWF_ATOMIC`. Sin imágenes en el WAL.
- **Sin protección**: control positivo. Sin imágenes y con escrituras que se desgarran.

En los tres modos los splits se registran como imágenes de las páginas nuevas. El disco es simulado en memoria: cada escritura de página se parte en unidades de 4 KiB y cada unidad persiste con p = 0.5 si el crash cae en esa escritura.

### Pre-registro

Los parámetros, umbrales y semillas se publicaron **antes de medir** en el commit [`59fbd65`](https://github.com/federiconbaez/cortex9-wal-atomic-bench/commit/59fbd6574056b24abc224f9898f245273940c71f), empujado el 2026-10-29 a las 00:21:39 (ART). Ver [PREREG.md](PREREG.md) y [prereg.toml](prereg.toml).

| Archivo | sha256 |
|---|---|
| prereg.toml | `06d6ddfbf39c103d89e822f9f2532e1cf0f6cd6fb646235530b07c379c87bd2b` |
| PREREG.md | `9d0346566960c891db7ab73a9bd9a6d5729923ab1859b39c8ae2bbe2a4aa5ffa` |

El desarrollo usó solo semillas desde 9.000.000 y tamaños chicos (`dev`), fuera de los rangos registrados. Las páginas son ranuradas, como dice el pre-registro.

### Resultados de la corrida registrada

Binario compilado desde `c871c3d` (sha256 `c8caffb2358f8c2d4660dcae073bb0289471cc57db8d1ea4b456518926afe6ba`), 4 hilos.

**Bytes de WAL por transacción** (1.000.000 claves, 300.000 updates aleatorios, 5 corridas con semillas 0..4, IC 95% por bootstrap con 10.000 remuestreos):

| Checkpoint cada | FPI (B/tx) | Atómico (B/tx) | Reducción (media) | IC 95% |
|---|---|---|---|---|
| 1.000 tx | 13.761,4 | 129,0 | 99,0624% | [99,0622%, 99,0626%] |
| **10.000 tx (primario)** | **8.180,8** | **129,0** | **98,4231%** | **[98,4217%, 98,4245%]** |
| 100.000 tx | 1.223,6 | 129,0 | 89,4572% | [89,4572%, 89,4572%] |

Criterio: cota inferior del IC primario ≥ 40%. **PASS** (98,42%).

Con checkpoint cada 100.000 tx, cada intervalo toca prácticamente todas las hojas (unas 7.500), así que las 5 corridas dan las mismas 22.557 imágenes y el IC tiene ancho cero. No es un error, pero ese intervalo no informa sobre la varianza.

**Crashes emulados** (10.000 por modo, semillas 0..9999):

| Modo | Desgarros en disco antes de recovery | Páginas rotas tras recovery | Estado incorrecto | Criterio |
|---|---|---|---|---|
| FPI | 7.811 | **0** | 0 | 0 → **PASS** |
| Atómico (emulado) | 0 | **0** | 0 | 0 → **PASS** |
| Sin protección (control) | 7.811 | **7.254 ensayos** | 7.254 | ≥ 1 → **PASS** |

Cotas para los ceros (regla del tres, 95%): la tasa de páginas rotas tras recovery es **< 3/10.000 (0,03%) por ensayo** tanto en FPI como en el modo atómico. Esto no prueba que sea cero.

**Detección de `RWF_ATOMIC`** con `statx(STATX_WRITE_ATOMIC)`: en `/workspace` y `/tmp` de la máquina de medición el kernel no reporta escrituras atómicas (`statx_write_atomic_reported=false`). Por eso el modo atómico es una **emulación**: este repo nunca emite escrituras `RWF_ATOMIC`. El job `bytes` del CI imprime la detección del runner como dato informativo.

### Límites

- El disco y los desgarros son un modelo (unidades de 4 KiB, p = 0.5), no hardware real.
- La reducción de bytes depende de la carga: updates aleatorios uniformes sobre un árbol grande son el peor caso para FPI. Cargas con mucha localidad pagarían menos imágenes.
- Los bytes cuentan solo el WAL, no las escrituras de páginas ni el costo de garantizar atomicidad en el dispositivo.
- La medición corrió en una sola máquina; los tiempos de pared (unos 8 s por intervalo y unos 78 s por modo) son orientativos.

### Cómo reproducir

```sh
cargo build --release --locked
./target/release/wal-atomic-bench bytes --interval all
./target/release/wal-atomic-bench crash --mode fpi --threads 4
./target/release/wal-atomic-bench crash --mode atomic --threads 4
./target/release/wal-atomic-bench crash --mode unprotected --threads 4
./target/release/wal-atomic-bench detect /ruta/a/probar
```

Cada subcomando falla con código distinto de cero si no se cumple su umbral registrado. El CI corre los tres modos y el bench de bytes completos, además de tests en Linux y macOS, MSRV 1.80, Miri sobre los tests de la biblioteca y AddressSanitizer sobre los tests del motor.

### Licencia

Apache-2.0. Ver [LICENSE](LICENSE).

## EN

### What it is

A new B+tree (16 KiB slotted pages with page LSN and CRC32C) with a physiological WAL and ARIES-style redo. It compares two ways of protecting pages against torn writes:

- **FPI**: the first change to each page after a checkpoint logs a full page image (with hole elimination), like Postgres `full_page_writes`.
- **Atomic (emulated)**: the disk never tears a page write, as `RWF_ATOMIC` would promise. No images in the WAL.
- **Unprotected**: positive control. No images and tearable writes.

In all three modes, splits log images of the new pages. The disk is simulated in memory: each page write is split into 4 KiB units, and each unit persists with p = 0.5 if the crash lands on that write.

### Pre-registration

Parameters, thresholds and seeds were published **before measuring** in commit [`59fbd65`](https://github.com/federiconbaez/cortex9-wal-atomic-bench/commit/59fbd6574056b24abc224f9898f245273940c71f), pushed on 2026-10-29 at 00:21:39 (ART). See [PREREG.md](PREREG.md) and [prereg.toml](prereg.toml); sha256 values in the table above.

Development used only seeds from 9,000,000 up and small sizes (`dev`), outside the registered ranges. Pages are slotted, as the pre-registration says.

### Results of the registered run

Binary built from `c871c3d` (sha256 `c8caffb2358f8c2d4660dcae073bb0289471cc57db8d1ea4b456518926afe6ba`), 4 threads.

**WAL bytes per transaction** (1,000,000 keys, 300,000 random updates, 5 runs with seeds 0..4, 95% bootstrap CI with 10,000 resamples):

| Checkpoint every | FPI (B/tx) | Atomic (B/tx) | Reduction (mean) | 95% CI |
|---|---|---|---|---|
| 1,000 tx | 13,761.4 | 129.0 | 99.0624% | [99.0622%, 99.0626%] |
| **10,000 tx (primary)** | **8,180.8** | **129.0** | **98.4231%** | **[98.4217%, 98.4245%]** |
| 100,000 tx | 1,223.6 | 129.0 | 89.4572% | [89.4572%, 89.4572%] |

Criterion: primary CI lower bound ≥ 40%. **PASS** (98.42%).

With a checkpoint every 100,000 tx, each interval touches nearly every leaf (about 7,500), so all 5 runs log the same 22,557 images and the CI has zero width. That is not a bug, but this interval says nothing about variance.

**Emulated crashes** (10,000 per mode, seeds 0..9999):

| Mode | Tears on disk before recovery | Torn pages after recovery | Wrong state | Criterion |
|---|---|---|---|---|
| FPI | 7,811 | **0** | 0 | 0 → **PASS** |
| Atomic (emulated) | 0 | **0** | 0 | 0 → **PASS** |
| Unprotected (control) | 7,811 | **7,254 trials** | 7,254 | ≥ 1 → **PASS** |

Bounds for the zeros (rule of three, 95%): the rate of torn pages after recovery is **< 3/10,000 (0.03%) per trial** for both FPI and atomic mode. This does not prove it is zero.

**`RWF_ATOMIC` detection** with `statx(STATX_WRITE_ATOMIC)`: on `/workspace` and `/tmp` of the measuring machine the kernel reports no atomic writes (`statx_write_atomic_reported=false`). The atomic mode is therefore an **emulation**: this repo never issues `RWF_ATOMIC` writes. The CI `bytes` job prints the runner's detection for information only.

### Limits

- The disk and the tears are a model (4 KiB units, p = 0.5), not real hardware.
- The bytes reduction depends on the workload: uniform random updates over a large tree are the worst case for FPI. Workloads with more locality would log fewer images.
- Bytes count only the WAL, not page writes or the cost of making the device atomic.
- Measured on a single machine; wall times (about 8 s per interval and about 78 s per mode) are indicative.

### Reproduce

See the commands in the Spanish section. Each subcommand exits non-zero if its registered threshold is not met. CI runs all three modes and the full bytes bench, plus tests on Linux and macOS, MSRV 1.80, Miri on the library tests and AddressSanitizer on the engine tests.

### License

Apache-2.0. See [LICENSE](LICENSE).
