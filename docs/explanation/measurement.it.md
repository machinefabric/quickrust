# Misurare le build

QuickRust riporta numeri sulle build (durate, percorsi critici, cause delle ricompilazioni, hit della cache, byte su disco) e permette alla CI di fallire in base a essi. I numeri che guidano le decisioni devono essere affidabili, e le misurazioni delle build sono facili da sbagliare in modi silenziosi. Questa pagina spiega i principi con cui QuickRust misura e perché ciascuno di essi esiste.

## Non disponibile non vuol dire zero {#unavailable-is-not-zero}

La regola più importante riguarda ciò che QuickRust fa quando non sa qualcosa. Un report che dice che una build ha avuto zero unità aggiornate, zero byte di artefatti o zero hit della cache quando semplicemente non era in grado di saperlo è peggio di nessun report: lo zero sembra un fatto, viene confrontato con altri numeri e supera un budget che non dovrebbe superare.

Per questo in ogni report `null` significa non disponibile, mai zero, e il motivo lo accompagna, in un oggetto `unavailable` indicizzato per campo. Un'esecuzione analizzata dopo che la sua directory di build è andata avanti riporta il proprio ingombro come non disponibile, con quel motivo. Un'esecuzione senza cache del compilatore riporta le statistiche della cache come non disponibili perché nessuna cache era attiva, non come un tasso di hit pari a zero. Una ricompilazione che QuickRust non riesce ad attribuire a una causa viene riportata come `unknown`, con bassa confidenza, invece di essere assegnata alla causa che sembra più probabile. Chi usa lo [schema del report](../reference/report-schema.md) deve gestire `null` ovunque una misurazione possa mancare.

## Registrare subito, analizzare su richiesta {#record-now-analyse-when-asked}

Una build è il momento peggiore per analizzarla: lo sviluppatore sta aspettando, e qualsiasi cosa QuickRust aggiunga è un sovraccarico su ogni build a vantaggio di report che la maggior parte delle build non riceve mai. Per questo la build si limita a registrare, a basso costo: un record di sessione e un piccolo evento per ogni invocazione di rustc. L'analisi, che esegue `cargo metadata`, ricostruisce il grafo delle dipendenze, classifica le ricompilazioni e fa l'inventario della directory di build, avviene quando qualcosa la richiede, oppure subito dopo la build quando per quella build sono stati richiesti un report o un controllo dei budget.

Il costo di questa scelta viene dichiarato invece che nascosto. Alcuni fatti si possono osservare solo finché la directory di build è come la build l'ha lasciata, come l'ingombro degli artefatti o se il working tree aveva modifiche non committate. Un'esecuzione analizzata dopo build successive conserva tutto ciò che è stato registrato al momento della build, e riporta quei fatti come non disponibili invece di descrivere l'albero di oggi sotto l'esecuzione di ieri.

## I confronti devono confrontare cose omogenee {#comparisons-must-compare-like-with-like}

"La build è diventata più lenta del 30%" ha senso solo se le due build avrebbero potuto avere la stessa velocità. Una nuova release del compilatore, una classe di macchina diversa, una build di release contro una build di debug, una cache fredda contro una calda: ognuno di questi fattori cambia i tempi per motivi che non hanno nulla a che fare con la modifica in esame.

Per questo, quando QuickRust confronta due esecuzioni, o un'esecuzione con una baseline, controlla prima che siano confrontabili. Differenze nella versione del compilatore, nella tripla dell'host, nella classe di hardware, nel comando, nel profilo, nei target di compilazione, nello scenario o nello stato della cache rendono il confronto non confrontabile: la differenza viene mostrata, ma non è un verdetto. Differenze che potrebbero benissimo essere proprio ciò che si sta testando (versione di Cargo, variabili d'ambiente rilevanti per la build, la cache del compilatore, la revisione del sorgente, un working tree con modifiche non committate, il lockfile) lo rendono parzialmente confrontabile e vengono elencate.

I budget seguono la stessa logica. Una soglia relativa ("non più del 10% più lenta di main") richiede una baseline, e viene valutata solo quando baseline e candidato sono confrontabili; altrimenti il risultato è `incomparable`, mai un successo e mai un fallimento. Se un risultato non confrontabile debba bloccare un merge è una decisione di policy che spetta al progetto, quindi è un'impostazione (`incomparable = "warn"` o `"fail"`), non un'ipotesi. Le variazioni relative entro la banda di rumore fissata dal file dei budget (`noise-percent`) contano come nessuna variazione, perché un budget che fallisce per il rumore insegna alle persone a ignorarlo. Il [riferimento dei budget](../reference/budgets.md) ne descrive il formato.

## Ripetizione, e scarto dei campioni non validi {#repetition-and-discarding-bad-samples}

Un singolo tempo di build è un aneddoto. `qrust benchmark` esegue uno scenario più volte, riporta distribuzioni invece di un unico numero e si rifiuta di far entrare nel risultato una ripetizione non valida: le esecuzioni la cui build è fallita, che sono andate in timeout, durante le quali sono cambiati dei file sorgente o che hanno usato una toolchain diversa dalle altre vengono scartate e contate, così il report dice su quanti campioni si basa.

I benchmark isolano anche ciò che misurano. Ogni benchmark compila con una propria cache degli artefatti, così gli scenari a freddo sono davvero a freddo (e pagano comunque il costo della memorizzazione), e i due lati di un esperimento non condividono mai i risultati. Uno scenario che modifica un file sorgente salva prima in un journal i byte originali e li ripristina all'uscita, in caso di errore o di interruzione; se QuickRust viene terminato bruscamente, il benchmark successivo ripristina il file, ma solo se è ancora esattamente l'originale più le righe marcatore di QuickRust, e altrimenti si ferma e dice dove si trova l'originale. Uno strumento di misurazione che può corrompere il codice che misura non otterrà la fiducia necessaria per i progetti reali. [Come misurare un cambiamento](../how-to/measure-a-change.md) mostra benchmark ed esperimenti in uso.

Lo speed index di QuickRust stesso è soggetto a regole ancora più severe, perché decide che cosa diventa un'impostazione predefinita ([veloce per impostazione predefinita](fast-by-default.md)). Gira in un ambiente sigillato con una toolchain fissata e niente della configurazione di Cargo della macchina; le varianti girano intercalate in un ordine a rotazione, così le oscillazioni del carico della macchina le influenzano tutte allo stesso modo; e un'esecuzione non è valida, e non viene mai registrata, quando più della metà dei campioni di uno scenario è stata raccolta con la CPU sotto pressione o quando i campioni di uno scenario lungo hanno una dispersione superiore al 25%. Il metodo è descritto in [`benchmarks/README.md`](../../benchmarks/README.md).

## Che cosa una toolchain stabile non può mostrare {#what-a-stable-toolchain-cannot-show}

QuickRust osserva le build attraverso i punti di aggancio che Cargo offre su una toolchain stabile, e alcune cose non passano da lì. Invece di stimarle, QuickRust le riporta come non disponibili:

- quali unità Cargo ha considerato aggiornate, cosa che dicono solo i messaggi JSON di Cargo. QuickRust osserva quel flusso solo quando l'hai già richiesto tu su una pipe, lasciandolo passare intatto, perché richiederlo in un terminale cambierebbe ciò che vedi;
- la quota del linker in un'unità che esegue il linking;
- il tempo speso a espandere le macro procedurali all'interno dei crate dipendenti;
- le invocazioni di rustdoc, dato che `cargo doc` esegue rustdoc al di fuori di `RUSTC_WRAPPER`;
- CPU e memoria per unità quando un client di una cache del compilatore esegue rustc per tuo conto, perché il processo che QuickRust vede è quello del client.

Una parte di queste informazioni è disponibile da interfacce solo nightly (`--unit-graph`, `-Zbuild-analysis`). QuickRust le rileva e le elenca tra le capacità della toolchain nel report, ma nulla dipende da esse, così ogni report significa la stessa cosa su ogni toolchain. Il [riferimento delle piattaforme](../reference/platforms.md) elenca questi limiti insieme alle differenze tra piattaforme.
