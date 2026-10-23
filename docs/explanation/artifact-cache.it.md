# La cache degli artefatti

Cargo compila di nuovo un crate in ogni directory target, anche quando la stessa versione dello stesso crate, con le stesse feature e gli stessi flag, è stata compilata un minuto prima per un altro progetto o un altro checkout. La cache degli artefatti di QuickRust è la risposta a questo spreco: un'unità compilata una volta viene ripristinata invece di essere compilata di nuovo. Questa pagina spiega che cosa viene messo in cache, come si sa che un risultato in cache è quello giusto, e perché la cache ha la forma che ha.

## Che cos'è un'unità {#what-a-unit-is}

Cargo suddivide una build in unità: una compilazione di un target di un crate per un profilo, un target triple e un insieme di feature, con una riga di comando di rustc. Sono i fingerprint di Cargo a decidere se un'unità è aggiornata all'interno di una directory target. La cache degli artefatti lavora un livello più sotto: quando Cargo ha deciso di compilare un'unità e chiama rustc attraverso il wrapper di QuickRust, il wrapper chiede prima se esattamente questa compilazione è già stata fatta, in qualsiasi punto della macchina. In caso di hit mette gli output dove rustc li avrebbe scritti, riproduce i messaggi diagnostici che rustc ha stampato la prima volta e restituisce il successo di rustc. Cargo non è in grado di notare la differenza, se non nel tempo impiegato.

La cache si trova in `~/.qrust/cache/artifacts`, insieme al resto della cache usa e getta di QuickRust, ed è condivisa da tutti i progetti e i checkout dell'utente. Fa parte della fast mode: la compat mode compila senza di essa, e non viene mai consultata dietro un wrapper di rustc configurato da te, che potrebbe essere una cache con regole proprie.

## La chiave {#the-key}

Tutto dipende dalla chiave: due compilazioni con la stessa chiave devono produrre gli stessi output, e tutto ciò che potrebbe cambiare gli output deve farne parte. La chiave è costruita su due livelli.

Il primo livello è tutto ciò che si sa prima di compilare: il formato della cache, l'identità del compilatore secondo quanto dichiara lui stesso (`rustc -vV`: release, commit, versione di LLVM, host), il driver del compilatore di QuickRust quando è lui a compilare l'unità, ogni argomento tranne i percorsi di output e di ricerca, le variabili `CARGO_*` di Cargo specifiche dell'unità, e il contenuto dell'`OUT_DIR` dell'unità, dove gli script di build lasciano il codice generato. Ogni argomento `--extern` viene sostituito dall'identità dell'artefatto che indica, cioè il suo hash del contenuto, così una dipendenza compilata in modo diverso dà chiavi diverse alle unità che dipendono da essa. (Con l'early cut-off, l'identità dei metadati di una libreria è invece il suo hash dell'interfaccia; vedi [early cut-off](early-cutoff.md).) Un'unità che esegue il link (un binario, un test, una macro procedurale) ha come chiave tutto ciò che linka, comprese le rlib rispetto alle quali sono state compilate le sue dipendenze, che il linker trova attraverso i percorsi di ricerca senza che la riga di comando le nomini.

Il secondo livello è ciò che si può sapere solo dopo una compilazione: i valori delle variabili d'ambiente che rustc ha dichiarato di leggere (`env!`, `option_env!`, registrate nel suo dep-info) e, per i tuoi crate, il contenuto di ogni file di input che rustc ha letto. Una ricerca trova prima la voce di primo livello, poi controlla che quelle variabili e quei file abbiano ancora i valori che avevano.

Ciò che la chiave non può contenere è ciò che rustc non riporta. Una macro procedurale che legge un file o una variabile d'ambiente senza dichiararlo può rendere possibile un hit non aggiornato, esattamente come può succedere con sccache. Le unità che linkano librerie native per nome (`-l`) o chiedono al compilatore di stampare informazioni non vengono messe in cache affatto.

## Integrità {#integrity}

Una cache che può servire un file sbagliato è peggio di nessuna cache, quindi ogni passo è difensivo. Una voce viene scritta in una directory temporanea e poi rinominata al suo posto, così chi legge ne vede tutto o niente. Gli output vengono collegati alla voce con hard link quando la cache condivide il loro file system, perché rustc e i linker scrivono ogni output come un file nuovo, e quindi la build successiva dell'unità lascia intatta la copia della voce; altrimenti vengono copiati. Il file dep-info, che rustc riscrive sul posto, viene sempre copiato.

L'hash del contenuto (BLAKE3) di ogni file viene registrato insieme alla voce, e ogni file viene verificato rispetto a esso mentre viene ripristinato. Un file riscritto sul posto attraverso un link, troncato da un crash o danneggiato sul disco trasforma l'hit in un miss e rimuove la voce. Una compilazione i cui file di input sono cambiati mentre rustc era in esecuzione non viene memorizzata, perché i suoi output potrebbero non corrispondere agli input così come sono ora.

I file ripristinati sono copie, mai link: altrimenti una build successiva di quell'unità, qui o in un altro checkout, vedrebbe una data di modifica impostata da un altro checkout, e i controlli di aggiornamento di Cargo dipendono dalle date di modifica. Nel dep-info ripristinato e nei messaggi diagnostici riprodotti, i percorsi di output e di `OUT_DIR` della build originale vengono sostituiti con quelli di questa build, perché Cargo ne ricava i propri fingerprint.

La regola guida è che qualsiasi problema della cache significa "compila normalmente". La cache può solo far risparmiare tempo; non può mai essere il motivo per cui una build fallisce.

## Le dipendenze sono condivise; i tuoi crate restano nel loro checkout {#dependencies-are-shared-your-crates-stay-in-their-checkout}

Le dipendenze dal registry e da git non possono cambiare sotto un dato nome e una data versione, quindi le loro unità sono condivise tra tutti i checkout e i progetti. I crate del tuo workspace sono diversi. I loro sorgenti cambiano di continuo, e di questo si occupa il secondo livello della chiave, ma i loro output contengono anche il percorso assoluto del checkout nelle informazioni di debug e nelle posizioni dei panic. Un crate compilato in `~/work/app` e ripristinato in `~/work/app-feature` indicherebbe la directory sbagliata. Per questo le unità del workspace hanno come chiave anche il loro checkout: vengono riutilizzate all'interno del checkout che le ha compilate (dopo `cargo clean`, quando si passa avanti e indietro tra branch, nella CI con un percorso fisso) e mai in un altro.

Una complicazione simile riguarda le dipendenze il cui script di build genera codice che il crate include con `include!`, tra cui serde. La loro chiave comprende il contenuto di `OUT_DIR` ma non il suo percorso, quindi l'unità ripristinata in un'altra directory target indica l'`OUT_DIR` della prima nelle posizioni dei panic e nelle informazioni di debug del codice generato. Il comportamento è lo stesso, e QuickRust lo segnala la prima volta che succede ([veloce per impostazione predefinita](fast-by-default.md#what-the-levers-cost)).

## Non tutto vale la pena di essere memorizzato {#not-everything-is-worth-storing}

Memorizzare ha un prezzo: calcolare gli hash e collegare o copiare gli output a ogni compilazione. Per le librerie il prezzo è basso e il valore alto, perché formano il percorso critico su cui attende una ricompilazione da zero. Per un piccolo binario del workspace che si linka in un attimo, copiarlo nella cache dopo ogni modifica costa più o meno quanto farebbe mai risparmiare (misurato in circa 30 ms per un binario piccolo). Per questo un output linkato del workspace che è stato compilato in meno di `cache.artifacts-min-link-ms` (300 ms per default) non viene memorizzato, e QuickRust ricorda che l'unità è rapida. Quel ricordo conta anche altrove: un'unità del genere non riceve nemmeno il frontend parallelo di rustc né il relinking, il cui avvio la allungherebbe soltanto.

## Convivere con sccache {#living-with-sccache}

sccache e la cache degli artefatti risolvono problemi che si sovrappongono, da posizioni diverse. sccache è una cache del compilatore generica che può essere condivisa tra macchine attraverso uno storage remoto, e occupa lo slot di `RUSTC_WRAPPER`. La cache di QuickRust è locale a un utente, conosce le unità di Cargo e il driver di QuickRust, e può usare come chiave di un'unità le interfacce delle sue dipendenze anziché i loro byte, ed è questo che rende possibile l'early cut-off.

Le due cache non si sommano. Quando configuri sccache (o un qualsiasi wrapper), QuickRust lo esegue così come lo hai configurato e mette da parte la propria cache, così non c'è mai il dubbio su quale cache abbia servito un file. sccache continua a funzionare come prima, e QuickRust ne riporta le statistiche. [Come usare sccache con QuickRust](../how-to/use-sccache.md) illustra le scelte possibili.

## Spazio {#space}

La cache fa parte della directory della cache di QuickRust e viene mantenuta entro gli stessi limiti di tutto il resto che si trova lì: le voci usate meno di recente sono le prime a essere eliminate quando la cache supera `cache.max-size` o lo spazio libero sul disco scende sotto `cache.min-free`. Un file condiviso da una voce e da una directory di build (un'unità ripristinata) conta una volta sola e viene liberato solo insieme al suo ultimo link. Una voce i cui file sono ancora collegati da una directory di build in uso viene conservata, perché rimuoverla libererebbe poco e farebbe perdere la voce. [Perché QuickRust possiede il proprio ambiente](owning-the-environment.md) spiega la politica dell'intera cache.
