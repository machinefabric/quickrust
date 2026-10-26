# Fast mode

Le impostazioni di build che QuickRust applica nella sua modalità predefinita `mode = "fast"`, quando si applica ciascuna e la chiave che la disattiva.

Nessuna di esse cambia ciò che fa il programma compilato. `mode = "compat"` (o `QRUST_MODE=compat`) non ne applica nessuna: la build è esattamente quella del Cargo della toolchain, e viene solo osservata. Perché queste impostazioni e non altre: [Perché QuickRust è veloce per impostazione predefinita](../explanation/fast-by-default.md).

## Impostazioni {#settings}

| Impostazione | Che cosa cambia | Si applica a | Disattivazione |
|---|---|---|---|
| `profile.dev.debug = "line-tables-only"` | le build dev conservano le informazioni su file e righe (backtrace, esecuzione passo passo, breakpoint) ma non quelle sulle variabili locali e sui tipi | comandi di build integrati | imposta tu `profile.dev.debug` |
| `profile.dev.package."*".debug = false` | nessuna informazione di debug per le dipendenze nelle build dev | comandi di build integrati | imposta tu `profile.dev.debug` o `profile.dev.package."*".debug` |
| il frontend parallelo di rustc (`-Zthreads=N`) | la compilazione da zero usa più thread | unità compilate da zero | `fast.frontend-threads = "off"` |
| il linker wild | le build per l'host degli output linkati usano `wild` per il link invece dell'LLD incluso in rustc | host Linux con glibc su cui è installato `wild` | `fast.linker = "off"` |
| la cache degli artefatti | un'unità già compilata in precedenza viene ripristinata invece che compilata | unità pianificate da Cargo | `cache.artifacts = "off"` |
| early cut-off | dopo una modifica che i dipendenti non possono osservare, i dipendenti vengono ripristinati invece che compilati, e dei binari si ripete solo il link | unità pianificate da Cargo, con la cache degli artefatti attiva | `fast.early-cutoff = "off"` |

L'output di QuickRust nel terminale è indipendente dalla fast mode: in fast mode `output` vale per impostazione predefinita `qrust`, in modalità compat `cargo` ([Output](output.md)).

## Impostazioni del profilo {#profile-settings}

Le due impostazioni del profilo arrivano a Cargo come argomenti `--config`, collocati insieme agli argomenti `--config` del comando stesso. Vengono passate ai comandi di build integrati (`build`, `check`, `test`, `bench`, `run`, `doc`, `rustdoc`, `rustc`, `clippy`, `fix`) e mai ad altri comandi o a sottocomandi esterni (`cargo nextest`, ...), ai quali Cargo non passa `--config`.

Un'impostazione definita dall'utente ha la precedenza, e QuickRust lascia allora intatta quell'impostazione e quelle che ne dipendono:

| Impostazione di QuickRust | Lasciata intatta quando l'utente ha impostato |
|---|---|
| `profile.dev.debug` | `profile.dev.debug` |
| `profile.dev.package."*".debug` | `profile.dev.debug` o `profile.dev.package."*".debug` |

Un'impostazione è considerata dell'utente quando è definita nel `Cargo.toml` alla radice del workspace, in un file di configurazione di Cargo che si applica al workspace, in un argomento `--config` o in una variabile d'ambiente `CARGO_PROFILE_*`.

## Impostazioni applicate dal wrapper di rustc {#settings-applied-by-the-rustc-wrapper}

Il frontend parallelo, il linker, la cache degli artefatti e l'early cut-off vengono applicati da `qrust-rustc-wrapper` alle compilazioni delle unità pianificate da Cargo. Nessuno di essi si applica:

- quando l'utente ha configurato un wrapper di rustc (`RUSTC_WRAPPER`, `build.rustc-wrapper` o sccache concatenato tramite `cache.provider`);
- con `measurement.enabled = false`;
- agli script di build e alle sonde del compilatore che eseguono, che vedono il compilatore esattamente com'è.

Il frontend parallelo, il linker e la ripetizione del link richiedono flag `-Z`. Con un compilatore stable o beta il wrapper imposta `RUSTC_BOOTSTRAP=1` per il processo del compilatore insieme a `-Zallow-features=`, così un crate che abilita `#![feature]` viene rifiutato come su stable (con l'errore di rustc "not in the list of allowed features"). Quando l'utente imposta `RUSTC_BOOTSTRAP`, nessuno di questi tre si applica.

### Frontend parallelo {#parallel-frontend}

| | |
|---|---|
| Chiave | `fast.frontend-threads`: `auto` (il numero di core, al massimo 8, sulle macchine con almeno 4 core; disattivato sotto i 4), `off`, oppure un numero non inferiore a 2 |
| Si applica a | unità compilate da zero: dipendenze, build release e altre build non incrementali, e la prima compilazione di un'unità del workspace in una directory target |
| Non si applica a | ricompilazioni incrementali; unità che la cache degli artefatti ha visto compilare rapidamente; unità i cui argomenti impostano già `-Zthreads` |

### Linker {#linker}

| | |
|---|---|
| Chiave | `fast.linker`: `auto`, `off` |
| Si applica quando | l'host è Linux con glibc e `wild` si trova in `~/.qrust/bin` o nel `PATH` |
| Si applica a | unità compilate per l'host che eseguono il link: binari, test, proc-macro, librerie dinamiche; in una build per un altro target (`--target`, `build.target`), i suoi build script e proc-macro |
| Non si applica a | unità per cui l'utente ha scelto un linker: `-C linker`, argomenti di link che selezionano un linker o feature del linker, dalla configurazione di Cargo o da `RUSTFLAGS` |
| Collegamento | `~/.qrust/linker/ld` punta al `wild` in uso |

### Cache degli artefatti {#artifact-cache}

| | |
|---|---|
| Chiave | `cache.artifacts`: `on`, `off` |
| Posizione | `~/.qrust/cache/artifacts/`, gestita insieme al resto della cache di QuickRust ([File](files.md#quickrusts-cache)) |
| Condivisa | le dipendenze tra tutti i checkout della macchina; le unità del workspace all'interno del checkout che le ha compilate |
| Non salvato | output linkati del workspace compilati in meno di `cache.artifacts-min-link-ms` (predefinito 300); una compilazione i cui input sono cambiati durante l'esecuzione |
| Differenza visibile | una dipendenza che include codice generato dal suo script di build, compilata la prima volta in un'altra directory target, nomina quella directory nei messaggi di panic e nelle informazioni di debug del codice generato; QuickRust lo segnala una volta (con una `NOTE`) e conta queste unità nel report (`cache.artifacts.restored_with_foreign_out_dir`) |

Come funziona: [La cache degli artefatti](../explanation/artifact-cache.md).

### Early cut-off {#early-cut-off}

| | |
|---|---|
| Chiave | `fast.early-cutoff`: `auto`, `off` (`QRUST_EARLY_CUTOFF`) |
| Richiede | la cache degli artefatti e il driver del compilatore di QuickRust per la toolchain |
| Driver | il rustc della toolchain stessa, compilato con il suo componente `rustc-dev`, una volta per toolchain: da `qrust setup` e quando un canale passa a una nuova release; altrimenti in background dopo la prima build che lo trova mancante (QuickRust lo segnala con una `NOTE`). `qrust driver status` indica se è compilato, `qrust driver build` lo compila |
| Effetto | l'hash del crate di una libreria copre ciò che i suoi dipendenti possono osservare, quindi una modifica che i dipendenti non possono osservare lascia invariate le loro chiavi di cache e vengono ripristinati; un binario o un test il cui codice non è cambiato viene di nuovo linkato (`-Zlink-only`) invece che compilato |
| Non applicato | a un'unità su cui il driver ha già fallito in precedenza (da quel momento la compila rustc; il report viene conservato in `~/.qrust/cache/drivers/<key>/failed/`); quando Cargo esegue un altro programma al posto di rustc (il clippy-driver di `cargo clippy`) |

Come funziona: [L'early cut-off](../explanation/early-cutoff.md).

## Nel report dell'esecuzione {#in-the-run-report}

Ogni esecuzione registra che cosa ha fatto la fast mode in `policy.fast` ([Schema del report di esecuzione](report-schema.md#policy)):

| Campo | Contenuto |
|---|---|
| `mode` | `fast` o `compat` |
| `applied[]` | ogni impostazione applicata: `key`, `value`, `why` (le impostazioni del profilo, `rustc -Zthreads`, `linker`) |
| `yielded[]` | ogni impostazione lasciata intatta: `key`, e `because` che nomina l'impostazione dell'utente e dove è stata definita |
| `note` | perché non è stato applicato nulla (modalità compat, non è un comando di build) |

`policy.cache.artifacts` indica se la cache degli artefatti era attiva e, in caso contrario, perché.
