# Come QuickRust sa che una build è aggiornata

Esegui `cargo build` due volte in un terminale attraverso QuickRust e la seconda volta risponde all'istante:

```text
OK   Up to date: 1 fresh
```

Cargo non è stato avviato. Questa pagina spiega come QuickRust può dare questa risposta senza interpellare Cargo, perché è disposto a farlo, e che cosa garantisce che la risposta sia attendibile.

## Perché non chiedere semplicemente a Cargo {#why-not-just-ask-cargo}

Chiedere a Cargo è corretto, ma non è gratis. Anche quando non c'è niente da fare, Cargo carica i manifest e il lockfile, risolve il grafo delle dipendenze e controlla il fingerprint di ogni unità; sul servizio di benchmark axum sono circa 120 ms. Attraverso QuickRust c'è anche la sessione che lo circonda. Per il comando più comune che uno sviluppatore digita, dopo aver salvato un file senza aver cambiato nulla, o prima di eseguire un programma già compilato, quel ritardo è tutta l'esperienza.

QuickRust sa già, dalla build che ha appena presentato, tutto ciò che quella build ha letto. Se nulla di tutto ciò è cambiato, la build non ha niente da fare, e la risposta può essere data a partire da quel record: 21 ms invece di 123 ms su axum.

## Lo stamp {#the-stamp}

Dopo che una build presentata da QuickRust in un terminale va a buon fine, QuickRust scrive uno stamp: un elenco di file con le loro dimensioni e date di modifica, sotto una chiave.

La chiave contiene tutto ciò che, oltre ai file, determina che cosa fa la build: gli argomenti esatti, la directory di lavoro, ogni variabile d'ambiente (tranne quelle che la shell usa per la propria contabilità, `OLDPWD` e `_`, che cambiano senza cambiare nulla di ciò che una build legge), l'ambiente che QuickRust fornisce a Cargo, e l'identità sia del binario di QuickRust sia del Cargo della toolchain. Una riga di comando diversa, una variabile diversa o una nuova release di QuickRust sono una chiave diversa, con uno stamp proprio.

I file sono tutto ciò da cui dipende l'aggiornamento della build e che QuickRust è in grado di nominare:

- i sorgenti letti dal compilatore di ogni unità, dal dep-info di rustc;
- gli output e gli eseguibili di ogni unità, così ci si accorge di un programma cancellato;
- per ogni script di build locale, gli input che ha dichiarato con `rerun-if-changed`, dove una directory significa tutto ciò che contiene; per uno script che non ha dichiarato nulla, ogni file del suo package, perché è quello che Cargo sorveglia;
- i manifest, il `Cargo.toml` e il `Cargo.lock` del workspace;
- i file di configurazione di Cargo, presenti o assenti, in ogni directory da cui Cargo li legge, e nella Cargo home di QuickRust;
- i file della toolchain (`rust-toolchain.toml`, `rust-toolchain`) in ogni directory risalendo dalla directory di lavoro, il record di quale release indica ciascun canale, e il compilatore stesso.

I sorgenti dal registry e da git sono esclusi: si trovano nella Cargo home di QuickRust sotto nomi che includono la loro versione, e non cambiano mai sul posto.

Un file viene registrato come assente quando non esiste, così creare un `.cargo/config.toml` che prima non c'era è altrettanto visibile quanto modificarne uno. Un workspace conserva stamp per un massimo di 32 righe di comando distinte; quelli scritti meno di recente sono i primi a essere eliminati.

## Quando QuickRust giudica {#when-quickrust-judges}

La volta successiva che viene eseguito lo stesso comando, con la stessa chiave, QuickRust controlla la dimensione e la data di modifica di ogni file registrato nello stamp. Se ognuno è come è stato registrato, la build è aggiornata, e QuickRust stampa il numero di unità aggiornate e la riga dei warning che aveva quella build, così un warning non scompare solo perché non è stato ricompilato nulla. Lo stamp conserva accanto a sé l'output memorizzato di quella build, e l'esecuzione aggiornata lo fa proprio, così il `qrust warnings` a cui rimanda la riga mostra quei warning.

Lo fa solo dove è lui a presentare la build, cioè in un terminale con l'output di QuickRust attivo. Una pipe, un IDE, un log di CI, `-v`, `-q`, `--message-format`, o la compat mode con l'output di Cargo ricevono ogni volta la risposta di Cargo stesso; lì l'interfaccia è l'output di Cargo, e QuickRust non si sostituisce a esso.

Ciò che succede dopo dipende dal comando:

- Per `build`, `check` e `clippy`, il verdetto di QuickRust è la risposta, e Cargo non viene avviato.
- Per `run`, `test` e `bench`, QuickRust normalmente prima compila e poi esegue il comando ([output della build](build-output.md)). Quando lo stamp dice che la build è aggiornata, la fase di build viene saltata e il comando viene eseguito subito. Cargo controlla comunque l'aggiornamento da sé in quel comando, quindi uno stamp a cui è sfuggito un input può significare soltanto che la build avviene con le parole di Cargo invece che con quelle di QuickRust, mai che venga eseguito un programma non aggiornato.

## Perché dimensione e data di modifica bastano {#why-size-and-modification-time-are-enough}

Lo stamp confronta ciò che confronta Cargo. Cargo decide che un sorgente locale è cambiato in base alla sua data di modifica rispetto a quella degli output, non in base al suo contenuto; toccare un file senza modificarlo fa ricompilare Cargo, e lo stamp concorda, perché la data di modifica è cambiata. Calcolare l'hash del contenuto sarebbe più lento e a volte dichiarerebbe aggiornata una build che Cargo non considererebbe tale, che è l'unica direzione in cui QuickRust non deve mai sbagliare. La dimensione viene registrata insieme, senza costi aggiuntivi, perché entrambe provengono dalla stessa chiamata al file system.

## Che cosa lo mantiene attendibile {#what-keeps-it-honest}

Un verdetto sbagliato anche una sola volta insegnerebbe agli utenti a diffidare di ogni "Up to date" successivo, quindi questa funzionalità ha un controllo tutto suo. Il test `test0231_qrust_says_up_to_date_only_when_cargo_would_build_nothing` compila un piccolo workspace con due tipi di script di build, uno che non dichiara nulla e uno che dichiara una directory, poi applica una serie di modifiche: modificare e toccare i sorgenti, modificare un input non dichiarato di uno script di build, modificare un altro file del suo package, aggiungere un file a una directory dichiarata, modificare un file che lo script non ha dichiarato, impostare una variabile d'ambiente che il programma legge, aggiungere e rimuovere configurazione di Cargo, cambiare un manifest, cancellare il programma compilato, e non cambiare nulla. Dopo ciascuna, compila attraverso QuickRust in un terminale. Ogni volta che QuickRust dice che la build è aggiornata, il test chiede direttamente a Cargo, attraverso una pipe dove QuickRust non giudica mai, e fallisce se Cargo trova del lavoro da fare. Richiede anche che QuickRust risponda davvero quando non è cambiato nulla, che è lo scopo stesso del giudicare.

Il progetto ammette di proposito un solo tipo di errore: un input che QuickRust non è in grado di nominare, come un file che una macro procedurale legge senza dichiararlo. Neanche Cargo può vederli, quindi in quel caso la risposta di QuickRust e quella di Cargo concordano, ed entrambe sono non aggiornate nello stesso modo in cui lo è Cargo da solo.
