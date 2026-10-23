# L'output delle build di QuickRust

In un terminale, una build di QuickRust non ha l'aspetto di una build di Cargo. Non c'è una riga `Compiling` per ogni crate: c'è una sola riga di stato, gli errori per intero, i warning contati anziché stampati e un verdetto di una riga. Questa pagina spiega perché QuickRust prende il controllo del terminale, come è progettato il suo modo di esprimersi e dove lascia deliberatamente intatto l'output di Cargo.

## Perché non mostrare semplicemente l'output di Cargo {#why-not-just-show-cargos-output}

L'output di Cargo nel terminale è stato pensato per un altro momento. Una riga `Compiling serde v1.0.210` per ogni unità aveva senso quando guardare una build era interessante; in un progetto con qualche centinaio di dipendenze è uno scorrere di nomi che spinge fuori dallo schermo l'unica riga che conta, quella dell'errore. Con i warning va peggio: ognuno occupa diverse righe di estratti di codice sorgente e di note, così qualche decina di warning stampa centinaia di righe prima di ogni risultato della build, e la gente smette di leggerli, o smette di vedere l'errore in mezzo a loro.

Quello che a uno sviluppatore serve da una build in un terminale è poco: sta funzionando, ha funzionato e, se no, dov'è il primo problema. L'output di QuickRust risponde a queste domande, in quest'ordine:

```text
OK   Built: 12 compiled, 140 fresh, 37 restored from QuickRust's cache [4.21s]
WARN 23 warnings in app and util: unused_variables x17, dead_code x4, +2 more; `qrust warnings` shows them
```

Mentre la build è in corso, un'unica riga di stato mostra l'avanzamento riportato da Cargo stesso (unità completate sul totale, crate in download, un lock in attesa) e viene sovrascritta sul posto. Gli errori vengono stampati per intero man mano che arrivano, esattamente come li ha resi rustc, sotto un'intestazione che nomina il crate, perché un errore è l'unica cosa che non andrebbe mai riassunta. I warning vengono contati per lint e per crate, perché un conteggio ti dice se è cambiato qualcosa, e il testo completo è a un comando di distanza. Il verdetto chiude la build: che cosa è stato compilato, che cosa era già aggiornato, che cosa è arrivato dalla cache degli artefatti e quanto tempo ci è voluto; oppure, in caso di fallimento, quanti errori ci sono e dove si trova il primo:

```text
FAIL Build: 1 error in demo (bin), first at src/main.rs:3:18 [78ms]
```

## Non si perde nulla {#nothing-is-lost}

Compattare l'output è accettabile solo se ciò che è stato compattato si può recuperare esattamente. Ogni riga scritta da Cargo e da rustc durante una build resa da QuickRust viene salvata insieme all'esecuzione. `qrust warnings` stampa i warning come li ha scritti rustc, filtrati per crate o per lint se richiesto; `qrust errors` stampa di nuovo gli errori; `qrust log` stampa tutto in ordine e `qrust log --raw` il flusso non elaborato. [Come vedere warning ed errori](../how-to/read-warnings-and-errors.md) ne mostra l'uso. Il riepilogo è quindi una vista, non una perdita: i warning sono contati sullo schermo e conservati per intero su disco.

## Un solo modo di esprimersi {#one-way-of-speaking}

L'output di QuickRust segue uno stesso schema ovunque, per le build, le installazioni delle toolchain, le pulizie della cache, `qrust doctor` e tutto il resto:

- Una riga che riporta un esito inizia con la sua etichetta in una colonna a sé: `OK` (ha funzionato), `FAIL` (un errore, un comando fallito, un'interruzione), `WARN` (ha funzionato, e qualcosa merita un'occhiata) oppure `NOTE` (che cosa fa o sa QuickRust).
- Una riga che appartiene a un esito, come un download, una dipendenza che è cambiata o la riga di stato in tempo reale, è rientrata fino al punto in cui iniziano i messaggi, sotto l'esito a cui appartiene.
- Un messaggio è un'intestazione seguita dai suoi dettagli dopo `: `, separati da `, `; una durata, se c'è, chiude la riga tra parentesi quadre.

Lo scopo è che una persona che scorre un terminale, o uno script che cerca con grep in un log, possa distinguere gli esiti dai dettagli guardando solo la prima colonna, e che le stesse parole significhino la stessa cosa in ogni comando. Un breve "ha funzionato" senza nulla da aggiungere è solo `OK` e il tempo; una riga che si limita a confermare il successo non dice mai più di questo.

Tutto è ASCII. I caratteri per disegnare riquadri, i segni di spunta e le emoji vengono resi in modo diverso, o non vengono resi affatto, a seconda di terminali, font, console di Windows, visualizzatori di log della CI e console seriali, e copiati in una issue diventano rumore. Le etichette sono colorate quando il colore è disponibile (verde, rosso, giallo, ciano), ma prima di tutto sono parole, così anche senza colore l'etichetta dice quello che direbbe il colore. QuickRust chiede l'ASCII anche a Cargo (`term.unicode = false`), ed è per questo che `cargo tree` disegna il suo albero con `|--` e `` `-- ``.

## Comandi che non compilano {#commands-that-do-not-build}

Lo stesso ragionamento vale per i comandi che modificano un progetto senza compilarlo. `cargo update` elenca che cosa è cambiato, una dipendenza per riga, e ne fa il riepilogo; `cargo add` mostra la dipendenza e le sue feature in una riga; `cargo new`, `clean`, `package` e `publish` terminano con una riga di risultato. Il loro stdout e il loro stdin restano quelli del terminale, quindi tutto ciò che stampano perché tu lo legga, o che ti chiedono, resta intatto. Le righe che QuickRust non riconosce, come il fallimento di uno script di build, una riga `cargo:warning=` o un errore di risoluzione, vengono mostrate esattamente come le ha stampate Cargo. Un renderer che scartasse una riga che non capisce nasconderebbe proprio i messaggi insoliti, che sono quelli che contano di più.

## `run` e `test`: prima compilare, poi farsi da parte {#run-and-test-build-first-then-get-out-of-the-way}

`cargo run` e `cargo test` mescolano due tipi di output: quello della build e quello del programma. L'output del programma appartiene al programma e deve arrivare al terminale esattamente come arriverebbe senza QuickRust. Per questo QuickRust divide il comando in due. Prima compila esattamente ciò di cui il comando ha bisogno (per `test`, lo stesso comando con `--no-run`; per `run`, la selezione del binario fatta da Cargo stesso, con un runner configurato per questa sola invocazione che non lo esegue), resa come qualsiasi build. Poi esegue il comando vero, che trova tutto aggiornato, con l'output di build di Cargo silenziato così che i warning non vengano riproposti una seconda volta.

I programmi di test vengono poi eseguiti in un terminale tutto loro: uno pseudo-terminale delle stesse dimensioni di quello reale, così che il colore, `isatty` e l'ordine del loro output siano esattamente quelli che darebbe loro un terminale. QuickRust lo inoltra byte per byte man mano che arriva, tranne le righe di Cargo stesso (le sue righe `Running` e `Doc-tests` vengono mostrate attenuate senza il percorso del programma, e `Finished` sparisce, dato che l'ha già detto il verdetto della build), e chiude con i conteggi di tutti i programmi di test sommati:

```text
OK   Built: 2 compiled [113ms]
     unittests src/lib.rs

running 1 test
test tests::it_works ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     doc-tests lib1

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

OK   Tests: 1 passed [26ms]
```

Quando non è cambiato nulla di ciò da cui dipendeva l'ultima build di questo tipo, la fase di build viene saltata del tutto ([controlli di aggiornamento](up-to-date-checks.md)).

## Quando l'output di Cargo resta intatto {#when-cargos-output-is-left-alone}

QuickRust rende l'output solo dove lo sta leggendo una persona. Il suo output sostituisce quello di Cargo in un terminale, e da nessun'altra parte:

- Pipe, IDE e log della CI ricevono l'output di Cargo byte per byte. Un programma che analizza le righe di Cargo, o rust-analyzer che legge `--message-format=json`, deve vedere ciò che stampa Cargo.
- `-v`, `-q`, `--message-format` e `--color` sono richieste dell'output di Cargo stesso, e lo ottengono.
- `cargo test --doc` ottiene l'output di Cargo, dato che i doc test vengono compilati mentre vengono eseguiti e non c'è una build da mostrare prima.
- La modalità compat lascia il terminale a Cargo a meno che non sia impostato esplicitamente `output = "qrust"`; `output = "cargo"` (o `QRUST_OUTPUT=cargo`) disattiva la resa di QuickRust in qualsiasi modalità.

Poiché l'output fa parte dell'interfaccia, viene testato come tale. Ciò che il terminale mostra per ogni comando è conservato come snapshot in `tests/snapshots/`, confrontato per intero nella suite di test, e per ogni snapshot si verifica che contenga solo ASCII. Il [riferimento dell'output](../reference/output.md) elenca le forme delle righe.
