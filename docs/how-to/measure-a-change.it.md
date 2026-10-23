# Come misurare l'effetto di un'impostazione di build

Questa guida ti mostra come misurare quanto tempo impiega il tuo progetto a compilare in una data situazione, e se una modifica alle impostazioni di build (codegen unit, informazioni di debug, un linker, una variabile d'ambiente) lo rende più veloce senza romperlo.

Entrambi i comandi compilano in directory proprie e non modificano alcun file nel tuo repository, tranne lo scenario `small-edit`, che modifica un file sorgente e poi lo ripristina.

## Misura la build così com'è {#measure-the-build-as-it-is}

```bash
qrust benchmark --scenario clean --reps 5
```

```text
Benchmark 01M4SGKSDHS59VDFVQBMMSVXJC  scenario clean (cold): COMPLETE
  command: cargo build
  PHASE     #  WALL   SAMPLE  RUN
  warmup    1  211ms  valid   01M4SGKSDJ6GQVGSECXMFXEX1D
  measured  1  143ms  valid   01M4SGKSN537TSEAYC2Z4W6EZK
  measured  2  137ms  valid   01M4SGKST98MBZWGN1BVRY7A01
  measured  3  138ms  valid   01M4SGKSZ8SYQ2S5QYFRKNT4WJ
  measured  4  137ms  valid   01M4SGKT47E8GAD5QVT18MBVVC
  measured  5  147ms  valid   01M4SGKT9500QKMBDAPNFG6KN1
  wall time: median 138ms  (min 137ms, max 147ms, stddev 4ms, n=5)
  critical path (median): 101ms   units compiled (median): 2
  samples: 5 valid, 0 discarded
```

Scegli lo scenario che corrisponde alla situazione che ti interessa (`qrust benchmark --list` li mostra):

| Scenario | Misura |
|---|---|
| `clean` | una build completa a partire da una directory di build vuota |
| `no-change` | una ricompilazione senza alcuna modifica (il predefinito) |
| `small-edit` | una ricompilazione dopo la modifica di una riga in un file sorgente del workspace |
| `test-build` | la compilazione di tutti i target di test a partire da una directory di build vuota |
| `release` | una build ottimizzata a partire da una directory di build vuota |

- Per misurare un altro comando, metti i suoi argomenti di Cargo dopo `--`: `qrust benchmark --scenario clean -- check --target wasm32-unknown-unknown`.
- `--reps N` e `--warmups N` impostano il numero di ripetizioni misurate e di quelle scartate. Più ripetizioni riducono il rumore.
- `--timeout 5m` abbandona una ripetizione che impiega più tempo (solo su Unix).
- `--format json` stampa il risultato come JSON, con il suo `id`, per gli script.

`small-edit` aggiunge un commento in fondo a un file sorgente prima di ogni ripetizione. Prima chiede conferma; passa `--allow-mutation` (o imposta `benchmark.allow-mutation = true`) per saltare la domanda negli script, e `--edit src/lib.rs` per scegliere il file. Il file viene ripristinato al termine, anche in caso di fallimento o interruzione. Se QuickRust è stato terminato bruscamente, il successivo `qrust benchmark` lo ripristina oppure, se nel frattempo il file è cambiato, si ferma e ti dice dove si trova l'originale.

Le ripetizioni la cui build è fallita, è andata in timeout, ha visto cambiare dei file sorgente o ha usato un'altra toolchain vengono scartate e conteggiate nella riga `samples`.

## Confronta un'impostazione con la build attuale {#compare-a-setting-against-the-current-build}

```bash
qrust experiment codegen --codegen-units 16 --reps 5 --verify test
```

Un esperimento esegue il benchmark delle impostazioni attuali (la baseline) e di quelle modificate (la candidata) nello stesso scenario, `clean` a meno che tu non passi `--scenario`, e, con `--verify`, esegue un comando di Cargo con le impostazioni candidate per verificare che il risultato funzioni ancora. Termina con un verdetto e i due benchmark affiancati:

```text
Experiment `codegen`: VERIFIED
  measured and functionally verified
  candidate settings: profile.dev.codegen-units=16
  applied through `cargo --config` and the environment for these runs only; no file in the repository was changed
  build time: -9.7%  (run-to-run noise +/-7.5%)
  runtime and debugging:
    profile.dev.codegen-units=16: more codegen units parallelise code generation but can reduce generated-code performance; fewer do the opposite
  verification: passed
```

Modi per descrivere la candidata:

```bash
qrust experiment --debug line-tables-only                     # impostazioni del profilo: --codegen-units, --debug, --opt-level, --lto
qrust experiment --opt-level 1 -- build --release             # con --release, modificano il profilo release
qrust experiment linker --linker mold                         # collega con un altro linker (passa -fuse-ld)
qrust experiment --set 'profile.dev.split-debuginfo="unpacked"'   # qualsiasi valore di configurazione di Cargo
qrust experiment --env CARGO_INCREMENTAL=0 --scenario small-edit --allow-mutation
qrust experiment --name lto-thin --lto thin -- build --release
```

`--set` e `--env` possono essere ripetuti. `--name` dà un'etichetta all'esperimento (lettere, cifre, `-` e `_`).

### Leggi il verdetto {#read-the-verdict}

| Verdetto | Significato | Che cosa fare |
|---|---|---|
| `VERIFIED` | più veloce o più lenta oltre il rumore, e `--verify` è passato | Adotta l'impostazione se è più veloce. |
| `MEASURED-UNVERIFIED` | una differenza oltre il rumore, senza `--verify` | Esegui di nuovo con `--verify` prima di adottarla. |
| `INCONCLUSIVE` | entro il rumore tra un'esecuzione e l'altra, oppure le due esecuzioni non sono confrontabili | Esegui di nuovo con più `--reps`, su una macchina scarica. |
| `INVALID` | la candidata non è stata compilata, oppure `--verify` è fallito | Non adottarla; lo stato di uscita è 1. |

Per adottare un'impostazione, inseriscila tu stesso nel profilo del tuo `Cargo.toml` o in `.cargo/config.toml`; QuickRust non la scrive mai lì.

## Confronta due esecuzioni o benchmark qualsiasi {#compare-any-two-runs-or-benchmarks}

```bash
qrust compare latest~1 latest
qrust compare benchmark:01M4SFS07EDAVZ5DSWCZQM1HZ6 benchmark:01M4SFS0JWGYG8MBA5NM6C8Q4N
qrust compare baseline:main latest
```

Le prime righe dicono se i due sono confrontabili e che cosa differisce tra i loro ambienti; poi ogni metrica con la sua variazione. Una metrica che uno dei due lati non ha potuto osservare risulta `unavailable`, mai zero.

## Vedi anche {#related}

- [Benchmark](../reference/benchmarks.md) per gli scenari e i campi dei risultati
- [Come eseguire QuickRust in CI e porre controlli sul tempo di build](set-up-ci.md)
- [Misurare le build](../explanation/measurement.md) per il rumore e la confrontabilità
