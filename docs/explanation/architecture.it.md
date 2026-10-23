# Come funziona QuickRust

QuickRust sta dove sta `cargo`, eppure le build che esegue sono ancora prodotte dal Cargo e dal rustc della toolchain stessa. Questa pagina spiega come entrambe le cose possano essere vere: che cosa QuickRust mette attorno alla toolchain, come vede ogni compilazione senza cambiare il funzionamento di Cargo, e perché è costruito in questo modo invece che come fork di Cargo o come plugin.

## La struttura {#the-shape-of-it}

```text
developer / IDE / CI --> cargo (QuickRust) --> toolchain's Cargo --> qrust-rustc-wrapper --> [your wrapper / sccache] --> rustc
                           ^   |                      |                  |
                           |   +-- session record     |                  +-- one event per rustc invocation
                           +--------------------------+  $CARGO: Clippy, nextest, build scripts: nested builds come back to QuickRust
```

Tre programmi si dividono il lavoro. `qrust` è quello con cui gli utenti hanno a che fare; dopo `qrust setup` è anche `cargo`, `rustc`, `rustfmt`, `cargo-clippy`, `rust-analyzer` e gli altri programmi della toolchain in `~/.qrust/bin`. Il Cargo della toolchain è il motore di build: QuickRust non ha sostituito il resolver di Cargo, il suo scheduler né i suoi fingerprint, ed esegue Cargo per ogni comando con gli argomenti digitati dall'utente. `qrust-rustc-wrapper` si interpone davanti a ogni invocazione di rustc fatta da Cargo, come `RUSTC_WRAPPER` di Cargo. Dal lavoro sull'early cut-off esiste un quarto elemento, il driver del compilatore di QuickRust, che è rustc stesso eseguito tramite le callback di QuickRust ([early cut-off](early-cutoff.md)).

## Perché avvolgere Cargo invece di sostituirlo {#why-wrap-cargo-instead-of-replacing-it}

Un sostituto diretto vive o muore di compatibilità. La riga di comando di Cargo, la sua configurazione, il resolver, la gestione del lockfile, il protocollo degli script di build e i messaggi JSON sono una superficie ampia da cui l'ecosistema dipende nei dettagli, e ogni sua reimplementazione finisce per divergere. Eseguire il Cargo della toolchain stessa significa che QuickRust eredita tutto questo esattamente, per ogni toolchain, comprese quelle pubblicate dopo la compilazione di QuickRust. Ciò che QuickRust aggiunge deve passare attraverso i punti di giunzione che Cargo già offre: gli argomenti `--config`, le variabili d'ambiente, la directory di target e il wrapper di rustc. Questo vincolo determina quasi tutto ciò che segue, ed è anche il motivo per cui `mode = "compat"` può promettere esattamente le build di Cargo: con le proprie aggiunte disattivate, QuickRust è solo un osservatore di un Cargo puro.

La decisione non è definitiva. Il piano è possedere prima tutto ciò che sta attorno al motore, poi sostituire i pezzi del motore uno alla volta dove le misurazioni mostrano un guadagno ([possedere l'ambiente](owning-the-environment.md)).

## Un comando, una sessione {#one-command-one-session}

Quando `cargo build` raggiunge QuickRust, diventa una sessione. QuickRust carica la sua configurazione, determina quale toolchain usa la directory (`+toolchain`, `RUSTUP_TOOLCHAIN`, un file di toolchain, la predefinita), la installa se manca, trova il workspace e la sua directory di build, decide che cosa aggiunge la fast mode ed esegue il Cargo della toolchain. Cargo eredita stdin, stdout e stderr, l'intero ambiente e gli argomenti grezzi; QuickRust aggiunge soltanto le proprie variabili e i propri argomenti `--config`. Quando Cargo termina, QuickRust scrive un piccolo record di sessione, restituisce l'exit status di Cargo (o rilancia il suo segnale) e ha finito.

Non tutti i comandi hanno bisogno di tanto. I comandi che si limitano a chiedere qualcosa a Cargo (`cargo metadata`, `cargo --version`, `cargo locate-project` e simili) vengono passati direttamente al motore con l'ambiente di QuickRust, senza record di sessione: rust-analyzer pone domande del genere di continuo, e non devono costare nulla. (`cargo tree` e `cargo metadata` in un terminale sono l'eccezione: possono scaricare ciò che descrivono, quindi QuickRust mostra quell'avanzamento.) `qrust upstream` va oltre: esegue il Cargo della toolchain come sé stesso, nell'ambiente di QuickRust (la toolchain, la Cargo home, la directory di build del workspace) ma senza nessuna delle impostazioni di QuickRust, senza wrapper e senza record, e senza nemmeno leggere la configurazione di QuickRust, così la via d'uscita funziona anche quando quella configurazione è rotta.

## Il motore e il nome con cui gira {#the-engine-and-the-name-it-runs-under}

Cargo dice a tutto ciò che avvia dove si trova Cargo: sottocomandi esterni, script di build, binari di test ed `env!("CARGO")` ricevono tutti una variabile `CARGO`, e strumenti come Clippy, nextest o `cargo llvm-cov` la usano per richiamare Cargo nelle build annidate. Se QuickRust eseguisse il binario `cargo` della toolchain con il suo nome, Cargo distribuirebbe il percorso di quel binario, e ogni build annidata scavalcherebbe QuickRust.

Cargo accetta un `CARGO` ereditato (o il percorso nel suo `argv[0]` quando questo si chiama `cargo`) solo quando il suo eseguibile non si chiama `cargo`. Perciò QuickRust esegue un hard link del `cargo` della toolchain chiamato `.qrust-engine`, con `argv[0]` e `CARGO` che puntano entrambi al `cargo` di QuickRust in `~/.qrust/bin`. Cargo allora passa il percorso di QuickRust a tutto ciò che avvia, e il suo help e i suoi errori dicono ancora `cargo`. Su Windows non esiste un `argv[0]` separato, quindi lì l'help di Cargo nomina il link. Una toolchain collegata non viene mai modificata, quindi il suo Cargo gira come sé stesso.

Il resto dell'ambiente del motore segue la stessa idea di mantenere i programmi annidati sullo stesso piano di quello esterno. `CARGO_HOME` è quella di QuickRust (`~/.qrust/cargo`), quindi i sorgenti del registry e le credenziali sono condivisi da tutti i progetti della macchina e la `~/.cargo` di rustup non viene più consultata. `RUSTUP_TOOLCHAIN` nomina la toolchain esatta, così il `rustc` di uno script di build, o `rustfmt` eseguito da un sottocomando, raggiunge i proxy di QuickRust e ottiene la stessa toolchain anche se il canale passa a una nuova release a metà build. `~/.qrust/bin` viene per primo nel `PATH`. A Cargo viene detto di non ripulire mai da solo le sue cache di download (`CARGO_CACHE_AUTO_CLEAN_FREQUENCY=never`), perché è QuickRust a gestire quello spazio ([possedere l'ambiente](owning-the-environment.md)).

## Build annidate {#nested-builds}

Poiché le invocazioni annidate di Cargo tornano a QuickRust, tramite `PATH` o tramite `$CARGO`, ciascuna diventa una sessione a sé, collegata a quella esterna da `QRUST_SESSION_ID`. Una build annidata all'interno di una sessione strumentata scrive i suoi eventi di rustc nello stream della sessione genitore, quindi un `cargo clippy` che esegue di nuovo Cargo resta un unico quadro. Se non lo si desidera, `compatibility.nested-cargo = "upstream"` (`QRUST_NESTED=upstream`) passa le invocazioni annidate direttamente al Cargo della toolchain.

Un QuickRust annidato non deve mai finire per eseguire sé stesso come proprio motore, per esempio tramite una "toolchain" collegata il cui `cargo` è una copia di QuickRust. QuickRust contrassegna il motore che avvia con il proprio process id (`QRUST_EXEC_GUARD`); un QuickRust il cui processo genitore è quello che intendeva avviare Cargo sa di essere stato avviato al posto di Cargo, e si ferma con una diagnostica invece di entrare in un ciclo. Una build annidata legittima, avviata da Cargo o da uno script di build, ha un genitore diverso e non ne è toccata.

## Il wrapper di rustc {#the-rustc-wrapper}

Il wrapper è il punto in cui QuickRust vede le compilazioni. Cargo esegue `qrust-rustc-wrapper <rustc> <args...>` per ogni unità; il wrapper esegue il wrapper dell'utente se ne è configurato uno (sccache, per esempio), altrimenti rustc, con argomenti, ambiente e stdio intatti, e poi lascia un piccolo file di evento JSON: che cosa è stato compilato, quanto ci è voluto, con quale risultato, che cosa ha fatto la cache. Un file per invocazione significa che i processi del compilatore non si contendono mai un lock condiviso, e un processo andato in crash perde solo il proprio evento.

L'osservazione è rigorosamente fail-open. Un problema nello scrivere un evento, nel leggere la cache o nel calcolare una chiave può significare soltanto che l'unità viene compilata normalmente e registrata meno bene; non può mai cambiare il risultato del compilatore. È anche qui che la fast mode svolge la maggior parte del suo lavoro, perché il wrapper è l'unico punto che vede ogni unità prima che venga compilata: consulta la [cache degli artefatti](artifact-cache.md), aggiunge il frontend parallelo di rustc alle unità compilate da zero, seleziona il linker wild quando è presente e passa le unità al driver del compilatore ([veloce per impostazione predefinita](fast-by-default.md)). Un wrapper configurato dall'utente viene sempre eseguito, e le leve di QuickRust si fanno da parte dietro di esso, dato che un wrapper del genere può essere una cache con regole proprie.

## Registrare ora, analizzare dopo {#recording-now-analysing-later}

Tutto ciò che QuickRust può dire su una build (i tempi, il grafo delle dipendenze osservato e il suo percorso critico, perché ogni unità è stata ricompilata, le duplicazioni, l'ingombro, le raccomandazioni) proviene dal record di sessione e dai suoi eventi. Trasformarli in un report richiede tempo che uno sviluppatore in attesa di `cargo build` non dovrebbe pagare, quindi per impostazione predefinita una build si limita a registrare. L'analisi avviene la prima volta che qualcosa la richiede: `qrust analyze`, `qrust report`, `qrust compare`, oppure subito dopo la build quando è impostato un percorso di report (`QRUST_REPORT`), `measurement.finalize = "sync"`, o quando i budget vengono applicati alla build. Un report, una volta prodotto, è immutabile.

L'analisi lazy ha un costo, che QuickRust dichiara invece di nasconderlo: un'esecuzione analizzata dopo build successive ha ancora tutto ciò che è stato registrato al momento della build, ma la directory di build nel frattempo è cambiata, quindi l'ingombro dei suoi artefatti e lo stato del workspace in quel momento vengono riportati come non disponibili ([misurazione](measurement.md)).

## Dove andare ora {#where-to-go-next}

- [Veloce per impostazione predefinita](fast-by-default.md) spiega che cosa QuickRust cambia in una build, e perché quei cambiamenti sono sicuri.
- [Perché QuickRust possiede il proprio ambiente](owning-the-environment.md) spiega le toolchain, la Cargo home e le directory di build.
- [Compatibilità](../reference/compatibility.md) definisce con precisione il contratto di sostituzione diretta.
- [Variabili d'ambiente](../reference/environment.md) elenca ciò che QuickRust legge e ciò che imposta per Cargo.
