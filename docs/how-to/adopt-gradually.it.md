# Come provare QuickRust prima di farne il tuo cargo

Questa guida mostra come adottare QuickRust per fasi, ciascuna reversibile, e come tornare al Cargo puro.

Nessuna delle fasi modifica un manifest, un lockfile o un `.cargo/config.toml`.

## Fase 1: confronta una build con e senza QuickRust {#stage-1-compare-a-build-with-and-without-quickrust}

Installa QuickRust senza lasciare che prenda il posto di `cargo`, e rendi disponibile alla tua shell solo `qrust`:

```bash
qrust setup --no-modify-path
alias qrust="$HOME/.qrust/bin/qrust"
```

Poi, nel tuo progetto:

```bash
qrust doctor               # quale toolchain, quale Cargo e quale directory di target userebbe QuickRust
qrust upstream build       # il Cargo della toolchain stessa: nessuna impostazione di QuickRust, nulla di registrato
qrust build                # la stessa build tramite QuickRust, registrata
qrust analyze              # ciò che QuickRust ha visto
```

Il tuo `cargo` è ancora quello di prima. Se `qrust build` fallisce dove `qrust upstream build` riesce, eseguilo di nuovo con `QRUST_MODE=compat` (esattamente la build di upstream) per distinguere un'impostazione della fast mode da qualsiasi altra causa, e consulta [Come compilare esattamente come fa Cargo](build-exactly-like-cargo.md).

## Fase 2: usa qrust al posto di cargo a mano {#stage-2-use-qrust-instead-of-cargo-by-hand}

Digita `qrust <command>` ovunque digiteresti `cargo <command>`: `qrust test`, `qrust run -- --flag`, `qrust clippy`. Ogni comando e flag di Cargo funziona; quelli sconosciuti vanno a Cargo, che decide se sono validi. Le eccezioni sono i nomi che QuickRust usa per comandi propri: `qrust fix`, `qrust report`, `qrust help` e `qrust version` sono di QuickRust, quindi per il `fix` o il `report` di Cargo digita `qrust upstream fix` (oppure, dopo la fase 3, `cargo fix`).

Resta qui finché il tuo progetto non viene compilato, testato ed eseguito come prima.

## Fase 3: rendi cargo QuickRust {#stage-3-make-cargo-quickrust}

```bash
qrust setup
```

Questa volta setup mette `~/.qrust/bin` per primo nel `PATH`, dove `cargo`, `rustc`, `rustfmt`, `cargo-clippy`, `rust-analyzer` e gli altri programmi della toolchain sono QuickRust. Apri una nuova shell. Anche gli editor e gli altri strumenti che eseguono `cargo` ora eseguono QuickRust.

Se uno strumento deve continuare a usare il Cargo puro, dagli il percorso del tuo vecchio Cargo (per esempio `~/.cargo/bin/cargo`); QuickRust non modifica rustup né `~/.cargo`.

## Torna al Cargo puro {#go-back-to-plain-cargo}

Per smettere di usare QuickRust per un po', togli `~/.qrust/bin` dal `PATH` (rimuovi la riga che `qrust setup` ha aggiunto a `~/.profile` e agli altri file di avvio, oppure avvia una shell senza di essa). Per rimuoverlo del tutto, esegui `qrust self uninstall --yes`.

Il Cargo puro torna a compilare in `target/`, da zero: le build di QuickRust risiedono in una directory propria per ogni workspace, e i due non toccano mai gli artefatti l'uno dell'altro.

## Condividi l'output di build tra QuickRust e il Cargo puro {#share-build-output-between-quickrust-and-plain-cargo}

Se passi avanti e indietro e non vuoi che il Cargo puro riparta da zero, indirizzalo alla directory di QuickRust:

```bash
CARGO_TARGET_DIR="$(qrust path)" ~/.cargo/bin/cargo build
```

In fast mode i due usano impostazioni di profilo diverse, quindi ciascuno mantiene il proprio insieme di artefatti in quella directory e nessuno dei due ricompila quelli dell'altro. Con `QRUST_MODE=compat` entrambi producono gli stessi artefatti e li condividono.

## Se gli script di build vengono rieseguiti quando cambi {#if-build-scripts-rerun-when-you-switch}

Uno script di build che dichiara `cargo::rerun-if-env-changed=RUSTC_WRAPPER` viene rieseguito ogni volta che alterni QuickRust e il Cargo puro, perché il wrapper di rustc di QuickRust si trova in quella variabile. Se per te è importante, esegui QuickRust senza il suo wrapper mentre alterni:

```bash
QRUST_MEASUREMENT_ENABLED=false cargo build
```

Questo disattiva anche ciò che richiede il wrapper: l'osservazione a livello di rustc e le impostazioni della fast mode che esso applica (la cache degli artefatti, il frontend parallelo, l'early cut-off e il linker veloce).

## Vedi anche {#related}

- [Come installare, aggiornare e disinstallare QuickRust](install.md)
- [Come trovare gli output di build senza target/](find-build-outputs.md)
- [Compatibilità](../reference/compatibility.md) per sapere esattamente che cosa passa senza modifiche
- [Perché QuickRust è veloce per impostazione predefinita](../explanation/fast-by-default.md)
