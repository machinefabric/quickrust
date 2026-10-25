# Documentazione di QuickRust

QuickRust è un sostituto diretto del comando `cargo` che rende più veloci le build di Rust, gestisce da sé le toolchain di Rust e registra ogni build, così da poter spiegare dove sono finiti il tempo e lo spazio su disco.

Questa documentazione è divisa in quattro parti, una per ciascuna delle cose di cui potresti aver bisogno:

| | Parte | Usala quando vuoi |
|---|---|---|
| Imparare | [Tutorial](#tutorials) | conoscere QuickRust usandolo, passo dopo passo |
| Lavorare | [Guide pratiche](#how-to-guides) | portare a termine un compito preciso |
| Consultare | [Riferimento](#reference) | verificare un comando, un'impostazione, un file o una garanzia |
| Capire | [Spiegazioni](#explanation) | sapere perché QuickRust funziona in questo modo |

Non conosci ancora QuickRust? Inizia da [La tua prima build con QuickRust](tutorials/first-build.md).

## Tutorial {#tutorials}

Lezioni che ti guidano attraverso QuickRust facendoti fare qualcosa con esso.

- [La tua prima build con QuickRust](tutorials/first-build.md): crea, compila, rompi, ripara e testa un piccolo programma, e scopri l'output di QuickRust, le sue directory di build e la sua cache.
- [Scoprire dove va il tempo di una build](tutorials/where-the-time-goes.md): compila un programma con dipendenze, trova il suo percorso critico, scopri perché è avvenuta una ricompilazione, confronta due build.

## Guide pratiche {#how-to-guides}

Indicazioni per un compito, per quando sai che cosa vuoi fare.

Installazione e passaggio a QuickRust:

- [Come installare, aggiornare e disinstallare QuickRust](how-to/install.md)
- [Come provare QuickRust prima di farne il tuo cargo, e come tornare indietro](how-to/adopt-gradually.md)
- [Come scegliere e gestire le toolchain](how-to/choose-a-toolchain.md)
- [Come installare programmi con qrust install](how-to/install-programs.md)

Lavoro quotidiano:

- [Come trovare gli output di build senza target/](how-to/find-build-outputs.md)
- [Come vedere i warning, gli errori e il log completo di una build](how-to/read-warnings-and-errors.md)
- [Come compilare esattamente come fa Cargo](how-to/build-exactly-like-cargo.md)
- [Come mantenere la cache di QuickRust entro i limiti del disco](how-to/free-disk-space.md)
- [Come usare sccache con QuickRust](how-to/use-sccache.md)

Analizzare e migliorare le build:

- [Come scoprire perché i crate sono stati ricompilati](how-to/find-why-crates-rebuilt.md)
- [Come misurare l'effetto di un'impostazione di build](how-to/measure-a-change.md)
- [Come esaminare e applicare le raccomandazioni di QuickRust](how-to/apply-a-recommendation.md)
- [Come esportare un report di build](how-to/export-a-report.md)
- [Come eseguire QuickRust in CI e porre controlli sul tempo di build](how-to/set-up-ci.md)

## Riferimento {#reference}

Descrizioni dei meccanismi, da consultare mentre lavori.

- [Comandi](reference/commands.md): ogni comando e opzione di `qrust`, i riferimenti alle esecuzioni, gli stati di uscita
- [Configurazione](reference/configuration.md): i file di configurazione, la precedenza e ogni impostazione
- [Variabili d'ambiente](reference/environment.md): che cosa legge QuickRust e che cosa imposta per Cargo, rustc e gli script di build
- [Fast mode](reference/fast-mode.md): ogni impostazione di build applicata dalla fast mode, quando, e come disattivarla
- [Output](reference/output.md): le righe `OK` / `FAIL` / `WARN` / `NOTE`, e quando passa l'output di Cargo stesso
- [File e directory](reference/files.md): che cosa conserva QuickRust e dove, e che cosa si può eliminare senza rischi
- [Budget](reference/budgets.md): il formato di `.qrust/budgets.toml` e le sue metriche
- [Benchmark](reference/benchmarks.md): scenari, ripetizioni e che cosa viene scartato
- [Compatibilità con Cargo](reference/compatibility.md): che cosa passa invariato e che cosa è diverso
- [Piattaforme](reference/platforms.md): sistemi supportati, lacune note e i limiti dell'osservazione
- [Schema del report di esecuzione](reference/report-schema.md): il report JSON con versione
- [Specifica](reference/specification.md) e [Conformità](reference/conformance.md): i requisiti che QuickRust implementa e dove se ne discosta

## Spiegazioni {#explanation}

Discussione delle idee e delle decisioni alla base di QuickRust.

- [Come funziona QuickRust](explanation/architecture.md)
- [Perché QuickRust è veloce per impostazione predefinita](explanation/fast-by-default.md)
- [La cache degli artefatti](explanation/artifact-cache.md)
- [L'early cut-off e il driver del compilatore di QuickRust](explanation/early-cutoff.md)
- [Come QuickRust sa che una build è aggiornata](explanation/up-to-date-checks.md)
- [Perché QuickRust possiede il proprio ambiente](explanation/owning-the-environment.md)
- [L'output delle build di QuickRust](explanation/build-output.md)
- [Misurare le build](explanation/measurement.md)
- [Sicurezza e privacy](explanation/security.md)
- [Ridurre il tempo di compilazione di Rust e l'ingombro su disco delle build](explanation/compile-time-research.md): la ricerca da cui è partito QuickRust

I risultati dei benchmark e il loro metodo sono conservati insieme ai dati, in [benchmarks/PROGRESS.md](../benchmarks/PROGRESS.md) e [benchmarks/README.md](../benchmarks/README.md).

## Come è scritta questa documentazione {#how-this-documentation-is-written}

Ogni pagina appartiene a uno dei quattro tipi descritti sopra e svolge soltanto il compito di quel tipo: un tutorial insegna facendo fare e non si dilunga nelle spiegazioni; una guida pratica dà indicazioni per un solo obiettivo; il riferimento descrive e non dà istruzioni; una spiegazione discute e non indica passaggi. Quando una pagina ha bisogno di qualcosa di un altro tipo, rimanda a esso con un link. Una nuova pagina va nella parte di cui svolge il compito, e gli esempi di output sono copiati da esecuzioni reali.
