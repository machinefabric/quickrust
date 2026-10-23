# Come trovare gli output di build senza target/

Questa guida ti mostra come trovare i binari e gli altri file prodotti da una build, ora che nel tuo progetto non c'è più una directory `target/`, e come far funzionare uno script o uno strumento che se ne aspetta una.

QuickRust compila ogni workspace in una directory della propria cache, `~/.qrust/cache/builds/<name>-<id>/`. Il perché è spiegato in [Perché QuickRust possiede il proprio ambiente](../explanation/owning-the-environment.md).

## Stampa la directory target {#print-the-target-directory}

Nel workspace, o in qualsiasi directory al suo interno:

```bash
qrust path
```

```text
/home/me/.qrust/cache/builds/app-fca2211b77f340ae
```

Tutto ciò che Cargo avrebbe messo in `target/` si trova lì, organizzato allo stesso modo: `debug/`, `release/`, `<triple>/release/`, `doc/` e così via.

## Stampa dove si trova un binario {#print-where-a-binary-is}

```bash
qrust path bin app                         # build di debug
qrust path bin app --release
qrust path bin app --profile profiling
qrust path bin app --release --target aarch64-unknown-linux-musl
```

Il percorso è quello in cui `cargo build` con le stesse opzioni mette il binario; aggiunge `.exe` per i target Windows. Non compila nulla, e il file esiste solo dopo che lo hai compilato.

Per eseguire un binario che hai appena compilato, `cargo run` resta il modo più semplice:

```bash
cargo run --release -- --help
```

## Aggiorna uno script che ha ./target scritto nel codice {#update-a-script-that-hard-codes-target}

Sostituisci il percorso fisso con ciò che stampa QuickRust:

```bash
# prima
cp target/release/app dist/
# dopo
cp "$(qrust path bin app --release)" dist/
```

Se lo script viene eseguito sotto Cargo (uno script di build, un `xtask` avviato con `cargo run`, un sottocomando di Cargo), non ha affatto bisogno di QuickRust: Cargo e ogni strumento sotto di esso vedono la directory in `CARGO_TARGET_DIR`, e `cargo metadata` la riporta come `target_directory`:

```bash
cargo metadata --format-version 1 --no-deps | jq -r .target_directory
```

## Tieni la build in una directory a tua scelta {#keep-the-build-in-a-directory-of-your-choice}

Se uno strumento pretende una posizione fissa, di' a Cargo dove compilare, come faresti senza QuickRust. Ognuna di queste opzioni prevale sulla directory di QuickRust:

```bash
cargo build --target-dir out
cargo --config 'build.target-dir="out"' build
CARGO_TARGET_DIR=out cargo build
```

Una directory scelta da te viene usata così com'è: QuickRust non la ripulisce mai né la conteggia nella sua cache. `qrust path` la stampa quando l'hai scelta tramite `CARGO_TARGET_DIR` (o `CARGO_BUILD_TARGET_DIR`) nell'ambiente, e altrimenti stampa la propria directory. `build.target-dir` in un file `.cargo/config.toml` è l'unica eccezione: QuickRust la sovrascrive, quindi metti invece l'impostazione sulla riga di comando o nell'ambiente.

## Compila un workspace in più configurazioni {#build-one-workspace-in-several-configurations}

Alcuni workspace vengono compilati in più di una configurazione che non deve condividere l'output: contro un servizio di staging e uno di produzione il cui indirizzo è incluso nella compilazione, per due canali di release, con e senza una feature per la GPU. In un'unica directory di build si alternano: ogni build ricompila ciò che la configurazione cambia e sovrascrive i binari della precedente, così un programma che hai avviato da una configurazione può venire sostituito sotto i tuoi piedi dalla build successiva.

Dai a ogni configurazione il nome di una variante, e otterrà una propria directory di build:

```bash
QRUST_BUILD_VARIANT=staging cargo build --release
QRUST_BUILD_VARIANT=prod    cargo build --release

QRUST_BUILD_VARIANT=staging qrust path bin app --release
```

```text
/home/me/.qrust/cache/builds/app-fca2211b77f340ae.staging/release/app
```

Ogni comando eseguito con la variante compila lì e lì trova i suoi output: `cargo run`, `cargo test`, `qrust path`, gli strumenti che interrogano `cargo metadata`. Il nome di una variante è composto al massimo da 32 lettere, cifre, `-` o `_`. La variante può anche essere impostata come `build.variant` in un file di configurazione, per un checkout che corrisponde sempre a una sola configurazione.

La directory di una variante è una directory di build come le altre: viene conteggiata nella cache di QuickRust, mantenuta entro `cache.max-size` e `cache.min-free`, e rimossa quando non viene usata da un po'. Ciò che le configurazioni hanno in comune non viene compilato due volte: le dipendenze arrivano dalla cache degli artefatti, qualunque sia la variante che le compila per prima.

## Usa gli output di una build più a lungo della build stessa {#use-a-builds-outputs-for-longer-than-the-build}

Una directory di build fa parte della cache di QuickRust, e la cache si mantiene entro i propri limiti: quando supera la sua dimensione, o il disco è a corto di spazio libero, rimuove le directory di build che non vengono usate da un po', a partire da quelle usate meno di recente. Una directory in cui un comando sta compilando non viene mai rimossa, né una usata negli ultimi 15 minuti. Questo basta per `cargo run` e `cargo test`, e per uno script che copia fuori un binario subito dopo averlo compilato.

Non basta per un programma che compila una volta e continua a usare ciò che ha compilato: un harness di test che avvia i binari dopo un'ora di esecuzione, un programma di packaging che li legge alla fine. Un programma del genere tiene la directory per tutto il tempo in cui è in esecuzione, chiedendo il percorso con il proprio id di processo:

```bash
bin=$(qrust path bin app --release --hold-for $$)
```

Finché quel processo è in esecuzione, la cache di QuickRust considera la directory in uso. La prenotazione non va rilasciata: termina quando termina il processo, in qualunque modo termini. Chiederla di nuovo non fa danni. Con una variante di build attiva, la directory prenotata è quella della variante.

## Pulisci la build di un workspace {#clean-a-workspaces-build}

```bash
cargo clean
```

È il `clean` di Cargo stesso, eseguito sulla directory del workspace nella cache di QuickRust. Raramente ti serve per liberare spazio: la cache rimuove da sola le directory di build che non vengono usate da un po' (vedi [Come mantenere la cache di QuickRust entro i limiti del disco](free-disk-space.md)).

## Vedi anche {#related}

- [File e directory](../reference/files.md)
- [Variabili d'ambiente](../reference/environment.md) per ciò che QuickRust imposta per Cargo e per gli strumenti sotto di esso
