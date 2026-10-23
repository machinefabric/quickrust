# Benchmark

Gli scenari di `qrust benchmark`, le opzioni e i verdetti di `qrust experiment`, come le esecuzioni vengono effettuate e scartate, e dove vengono memorizzati i risultati.

Guida: [Come misurare l'effetto di un'impostazione di build](../how-to/measure-a-change.md).

## Scenari {#scenarios}

`qrust benchmark --list` li stampa:

```text
clean       cold  cargo build            full build from an empty build directory
no-change   warm  cargo build            rebuild with nothing changed (build-system overhead)
small-edit  warm  cargo build            rebuild after a one-line edit to a workspace source file
test-build  cold  cargo test --no-run    build all test targets from an empty build directory
release     cold  cargo build --release  optimized build from an empty build directory
```

| Stato della cache | Ogni ripetizione parte da |
|---|---|
| `cold` | una directory di build vuota e una cache degli artefatti vuota |
| `warm` | l'albero lasciato dall'esecuzione precedente; prima viene eseguita una build di preparazione (mai un campione) |

Gli argomenti di Cargo dopo `--` sostituiscono il comando di default dello scenario (`-- check --target wasm32-unknown-unknown`). Il comando deve essere un comando di build (`build`, `check`, `test`, `bench`, `run`, `doc`, `rustc`, `clippy`); `--target-dir` viene rifiutato.

## `qrust benchmark` {#qrust-benchmark}

| Opzione | Default | Significato |
|---|---|---|
| `--scenario NAME` | `no-change` | lo scenario |
| `--reps N` | `3` | ripetizioni misurate (almeno 1) |
| `--warmups N` | `1` | ripetizioni non misurate che le precedono |
| `--edit FILE` | la radice della libreria del primo membro del workspace, altrimenti la radice del suo binario | il file che `small-edit` modifica; deve essere un file `.rs` all'interno del workspace |
| `--allow-mutation` | disattivato (`benchmark.allow-mutation`) | permette a `small-edit` di modificare il file senza chiedere |
| `--timeout DURATION` | nessuno | ferma un'esecuzione dopo `DURATION` (`90s`, `5m`); solo Unix |
| `--format text\|json` | `text` | il risultato come tabella o come JSON |

L'avanzamento va su stderr, una riga indentata per esecuzione: `     Benchmark clean: measured 2/3 [41.2s]`. L'exit status è 0 quando il benchmark è stato completato e 1 altrimenti.

### Directory di build {#build-directories}

Ogni benchmark compila in `<target>/qrust-bench/<scenario>-<label>/` all'interno della directory target del workspace (`<label>` è `default`, oppure `baseline` e `candidate-<name>` in un esperimento), con una cache degli artefatti propria in `.qrust-artifacts/` lì dentro. Gli scenari cold svuotano entrambe prima di ogni ripetizione, così una build da zero paga il costo della memorizzazione nella cache e non ripristina mai nulla da essa, e la baseline e il candidato di un esperimento non condividono mai risultati. QuickRust elimina una directory del genere solo quando contiene il suo marcatore `.qrust-owned`.

### Modifiche ai sorgenti {#source-edits}

`small-edit` aggiunge una riga di commento, `// qrust-benchmark-edit <n>`, in fondo al file prima di ogni ripetizione successiva alla build di preparazione.

- Senza `--allow-mutation` o `benchmark.allow-mutation = true`, QuickRust chiede conferma su un terminale (`NOTE the small-edit scenario will temporarily append a comment to <file> and restore it afterwards. Proceed? [y/N]`) e rifiuta se non c'è un terminale.
- Prima della prima modifica, i byte originali e un journal vengono scritti in `~/.qrust/workspaces/<id>/restore/`.
- Il file viene ripristinato byte per byte all'uscita normale, in caso di fallimento e in caso di interruzione; un ripristino che fallisce stampa `FAIL SOURCE FILE NOT RESTORED: ...` con il percorso dell'originale salvato.
- Se QuickRust è stato terminato forzatamente, il `qrust benchmark` successivo ripristina il file, ma solo quando è esattamente l'originale seguito dalle righe marcatore di QuickRust; altrimenti si ferma e indica l'originale. `qrust doctor` segnala un journal rimasto.

### Esecuzioni che vengono scartate {#runs-that-are-discarded}

| Esecuzione | Effetto |
|---|---|
| la build è fallita, è stata terminata, è andata in timeout o è stata interrotta | l'esecuzione non è valida |
| un file sorgente è cambiato durante l'esecuzione (a parte `Cargo.lock`) | l'esecuzione non è valida |
| non è stato possibile analizzare l'esecuzione | l'esecuzione non è valida |
| un'esecuzione di preparazione o di warm-up non è valida | il benchmark si ferma con stato `invalid` |
| un'esecuzione misurata ha usato una toolchain o una classe di macchina diversa dalle altre | esclusa dal gruppo di campioni, con un `WARN` |

Il risultato conta `samples_valid` e `samples_discarded`. Un benchmark senza alcuna esecuzione misurata valida è `invalid`.

### Risultato {#result}

| Campo | Contenuto |
|---|---|
| `id` | l'id del benchmark (`--benchmark ID`, `benchmark:ID`) |
| `state` | `complete`, `invalid` o `interrupted` |
| `failure` | perché non è completo |
| `scenario` | nome, stato della cache, etichetta, overlay `--config` e nomi delle variabili d'ambiente |
| `command` | il comando di Cargo, con i dati sensibili oscurati |
| `build_directory` | dove ha compilato |
| `requested` | ripetizioni, warm-up, timeout |
| `runs[]` | ogni esecuzione: id dell'esecuzione, fase (`prime`, `warmup`, `measured`), iterazione, tempo reale, exit, se è valida e, se non lo è, perché |
| `samples_valid`, `samples_discarded`, `excluded_heterogeneous` | il gruppo di campioni |
| `mutation` | il file modificato e se è stato ripristinato |
| `summary` | le metriche del gruppo, con le distribuzioni |

I risultati vengono memorizzati in `~/.qrust/workspaces/<id>/benchmarks/<id>.json`.

## `qrust experiment` {#qrust-experiment}

Un esperimento esegue il benchmark dello stesso scenario due volte, come baseline e come candidato con le impostazioni candidate, e facoltativamente controlla che il candidato funzioni ancora.

| Opzione | Significato |
|---|---|
| `codegen`, `linker` | dà il nome all'esperimento (altrimenti `candidate`); nessun altro effetto |
| `--name NAME` | il nome dell'esperimento: lettere, cifre, `-`, `_` |
| `--set KEY=VALUE` | un'assegnazione `--config` di Cargo per il candidato (TOML `key = value`); ripetibile |
| `--env KEY=VALUE` | una variabile d'ambiente per il candidato; ripetibile |
| `--codegen-units N` | `profile.<p>.codegen-units = N` |
| `--debug VALUE` | `profile.<p>.debug = VALUE` |
| `--opt-level VALUE` | `profile.<p>.opt-level = VALUE` |
| `--lto VALUE` | `profile.<p>.lto = VALUE` |
| `--linker NAME` | `target.<host>.rustflags = ["-C", "link-arg=-fuse-ld=NAME"]` |
| `--verify '<cargo args>'` | un comando di Cargo eseguito con le impostazioni candidate dopo il benchmark del candidato (`--verify 'test'`) |
| `--scenario NAME` | default `clean` |
| `--reps N`, `--warmups N`, `--edit FILE`, `--allow-mutation`, `--timeout DURATION`, `-- <cargo args>` | come per `qrust benchmark` |

`<p>` è `release` quando gli argomenti di Cargo dopo `--` contengono `--release` o `-r`, e `dev` altrimenti. I numeri e `true`/`false` vengono scritti senza virgolette, gli altri valori come stringhe TOML. È richiesta almeno un'impostazione candidata. Le impostazioni vengono applicate attraverso `--config` e l'ambiente solo per le esecuzioni dell'esperimento; nessun file viene modificato.

### Verdetti {#verdicts}

| Stato | Quando |
|---|---|
| `invalid` | il candidato non è stato compilato, oppure il comando di verifica è fallito (exit status 1) |
| `inconclusive` | baseline e candidato non sono confrontabili, oppure la differenza di tempo reale rientra nel rumore tra un'esecuzione e l'altra (la maggiore delle due deviazioni standard relative, almeno 1%) |
| `verified` | misurato al di fuori del rumore, e il comando di verifica è passato |
| `measured-unverified` | misurato al di fuori del rumore, senza alcun comando di verifica |

Il risultato elenca anche l'effetto noto di ogni impostazione sul programma compilato (`dimensions.runtime_and_debugging`), la variazione della dimensione dell'output e, per `verified` e `measured-unverified`, le impostazioni sotto forma di proposta da valutare. Le modifiche a profili, linker e flag non vengono mai applicate automaticamente. I risultati vengono memorizzati insieme ai benchmark.

## qrust-bench {#qrust-bench}

`qrust-bench` (in `bench/`) è un harness separato che misura QuickRust rispetto al Cargo upstream su progetti reali fissati a una versione e calcola lo speed index. I suoi carichi di lavoro, scenari, livelli e record sono descritti in [benchmarks/README.md](../../benchmarks/README.md); i risultati sono in [benchmarks/PROGRESS.md](../../benchmarks/PROGRESS.md).
