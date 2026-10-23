# Sicurezza e privacy

QuickRust si pone davanti a Cargo e a rustc, a ogni build, con i permessi dell'utente che lo esegue. Una posizione del genere merita un esame attento: che cosa aggiunge a ciò di cui l'utente già si fida, che cosa conserva delle build e che cosa può cancellare? Questa pagina spiega il modello di fiducia e le ragioni dietro ciascun confine. I percorsi su disco sono elencati nel [riferimento dei file](../reference/files.md).

## Modello di fiducia {#trust-model}

QuickRust non aggiunge alcun servizio di rete né alcun demone. Gira con i permessi dell'utente che esegue Cargo e delega al Cargo e al rustc di cui quell'utente già si fida. Ecco che cosa aggiunge al percorso fidato:

- il binario `qrust`, al posto di `cargo` (e, dopo `qrust setup`, al posto di `rustc`, `rustfmt` e degli altri programmi della toolchain, che esegue dalla toolchain selezionata);
- `qrust-rustc-wrapper`, davanti a ogni invocazione di rustc;
- in fast mode, il driver del compilatore di QuickRust: il `librustc_driver` di rustc stesso, preso dalla toolchain ed eseguito tramite un piccolo programma che QuickRust compila da sorgenti che porta con sé, contro il componente `rustc-dev` di quella toolchain ([early cut-off](early-cutoff.md)).

QuickRust e il suo wrapper inoltrano invariati gli argomenti e l'ambiente dell'utente. Ciò che aggiungono, in fast mode (la compat mode non aggiunge nulla):

- le impostazioni del profilo, come argomenti `--config` prima del sottocomando;
- `-Zthreads=N` per le unità che Cargo pianifica e che vengono compilate da zero, con `RUSTC_BOOTSTRAP=1` impostato per i processi del compilatore delle unità di Cargo (su un compilatore stable). Quella variabile consentirebbe a un tale processo di accettare anche dei `#![feature]`, perciò QuickRust passa insieme a essa `-Zallow-features=`: un crate che abilita una feature nightly viene rifiutato come accadrebbe con il semplice Cargo stable. Gli script di build e le sonde del compilatore che essi eseguono non la vedono mai, e nulla di tutto questo si applica quando l'utente imposta `RUSTC_BOOTSTRAP`;
- la cache degli artefatti (vedi sotto);
- l'early cut-off: unità compilate dal driver del compilatore, che calcola l'hash del crate di una libreria a partire da ciò che i suoi dipendenti possono osservare e conserva i file oggetto dei binari per ricollegarli; senza alcuna impostazione di questo tipo attiva, i suoi output sono identici byte per byte a quelli di rustc;
- quando l'utente ha installato `wild`, il collegamento con esso: il binario trovato nel `PATH` è considerato fidato come qualsiasi altro strumento lì presente, tramite il link `~/.qrust/linker/ld`;
- l'installazione delle toolchain: manifest e archivi dal server di distribuzione (`toolchain.dist-server`, HTTPS verso static.rust-lang.org per impostazione predefinita), con ogni manifest verificato rispetto al suo SHA-256 pubblicato e ogni archivio rispetto a quello del manifest; gli archivi vengono estratti senza permettere nulla al di fuori della loro directory (niente percorsi assoluti, `..`, link che portano all'esterno né voci al di sotto di link; niente hard link né dispositivi; un limite alla dimensione estratta). I crate che `qrust install` estrae ricevono lo stesso trattamento.

Il wrapper scrive un file di evento per ogni invocazione nella directory privata degli eventi della sessione, e l'output della build viene salvato accanto a essi.

## Aggiornamenti {#updates}

`qrust self update` sostituisce il programma che sta davanti a ogni build, quindi ciò che installerà è la cosa più ristretta di questa pagina.

Una release di QuickRust viene compilata con quattro informazioni sulla propria linea di release: il registro delle release in cui è pubblicata, il suo canale, un ambiente di firma e le metà pubbliche di tre **chiavi radice**. Non ne legge nessuna in fase di esecuzione, e un QuickRust compilato dai sorgenti non ne ha alcuna: non ha una linea di release e non aggiorna nulla.

Una release è descritta da un manifest nel registro, pubblicato con una firma accanto. Il file della firma contiene un **certificato della chiave di release**: la metà pubblica della chiave che firma i manifest, l'ambiente a cui è destinata e l'intervallo di tempo in cui è valida. Un aggiornamento procede solo quando vale tutto quanto segue:

- due radici distinte di questa build hanno firmato i byte esatti del certificato. Una sola radice non può autorizzare una chiave di release, e la perdita di una radice non blocca le release;
- il certificato è per l'ambiente di questa build e l'orologio rientra nella sua validità;
- la chiave di release che esso indica ha firmato i byte esatti del manifest, così come sono stati scaricati;
- ogni certificato nel file è valido, non solo quello che ha firmato: un file che ne contiene uno non valido è un errore di pubblicazione o una manomissione, e ignorarlo nasconderebbe l'uno o l'altra;
- l'archivio è quello descritto dal manifest, per SHA-256 e dimensione;
- il binario che contiene si avvia e dichiara di essere la versione con cui è stato pubblicato.

Così l'host del registro, la CDN davanti a esso e un mirror indicato in `self.update-url` sono tutti non fidati: chi li controlla può trattenere un aggiornamento, ma non può far installare a QuickRust nulla che la chiave di release non abbia firmato. Una chiave di release che trapela è contenuta dalla scadenza e dall'ambiente del suo certificato, e viene sostituita certificandone un'altra.

Ciò da cui questo non protegge: un manifest autentico ma vecchio. Chi controlla il registro può servire un manifest firmato precedente, e QuickRust allora non vede alcuna release più recente. Non ne installa mai una più vecchia di sé.

I gestori di pacchetti hanno una propria catena. Una copia installata con apt o dnf viene verificata da questi rispetto alla chiave OpenPGP del repository di MachineFabric, e una formula Homebrew contiene lo SHA-256 dell'archivio.

## La cache degli artefatti di QuickRust {#quickrusts-artifact-cache}

In fast mode QuickRust ripristina gli output del compilatore invece di compilare quando la stessa unità è già stata compilata in precedenza. La cache è locale all'utente (`~/.qrust/cache/artifacts`, dentro `~/.qrust/cache`, che viene creata con modalità 0700), non ha alcun componente di rete e non viene mai consultata dietro un wrapper di rustc configurato dall'utente.

- **Chiave.** L'identità di rustc secondo quanto dichiara lui stesso (`rustc -vV`: release, commit, LLVM, host — richiesta una sola volta per ogni binario del compilatore e selezione della toolchain), ogni argomento privato dei percorsi di output e di ricerca e con ogni `--extern` sostituito dall'hash del contenuto dell'artefatto che indica, le variabili per unità di Cargo, il contenuto di `OUT_DIR`, poi i valori delle variabili che rustc ha dichiarato di leggere e, per le unità del workspace, il contenuto di ogni file di input letto da rustc. Gli input che rustc non dichiara (file o variabili che una macro procedurale legge senza dichiararli) non fanno parte della chiave: una macro del genere può produrre un hit non aggiornato, come può accadere con sccache.
- **Integrità.** Le voci vengono scritte in una directory temporanea e poi rinominate al loro posto. Gli output vengono collegati con hard link in una voce quando la cache condivide il loro file system (rustc e i linker scrivono ogni output come un file nuovo, quindi una build successiva non tocca quelli della voce) e copiati altrimenti; le dep-info, che rustc riscrive sul posto, vengono sempre copiate. L'hash del contenuto di ogni file (BLAKE3) viene registrato e verificato al momento del ripristino del file, così un file riscritto sul posto attraverso un link, troncato da un crash o danneggiato su disco rende la voce un miss e la rimuove. Una compilazione i cui input sono cambiati mentre era in corso non viene salvata. Chiunque possa scrivere in `~/.qrust` può inserire un artefatto — lo stesso confine di fiducia di `~/.cargo/registry`, che contiene i sorgenti.
- **Percorsi.** Le dep-info e la diagnostica di un'unità ripristinata vengono riscritte con i percorsi di output e di `OUT_DIR` correnti. Il suo contenuto binario no: una dipendenza ripristinata in un'altra directory target indica l'`OUT_DIR` della prima nelle posizioni dei panic e nelle informazioni di debug del codice generato dagli script di build. Le unità del workspace vengono riutilizzate solo all'interno del checkout che le ha compilate.

## sccache {#sccache}

QuickRust non implementa la cache di sccache né ne calcola le chiavi. La correttezza degli artefatti di sccache è responsabilità di sccache; QuickRust compone la catena dei wrapper e legge le statistiche. Conseguenze:

- un'interruzione della cache può portare solo a una compilazione locale (`cache.fail = "open"`) o a un fallimento prima della compilazione (`closed`) — mai a un artefatto sostituito;
- un'esecuzione degradata da un'interruzione viene registrata come `degraded`, e nulla nel suo report è contrassegnato come hit della cache;
- un wrapper configurato da *te* non viene mai saltato: se non riesce ad avviarsi, la build fallisce esattamente come con il Cargo upstream.

Le credenziali per la cache remota, le modalità di sola lettura per CI non fidate e l'amministrazione della cache sono oggi configurazione di sccache; QuickRust non le salva né le stampa.

## Segreti {#secrets}

- I valori dell'ambiente vengono conservati solo per un elenco consentito di variabili rilevanti per la build (`RUSTFLAGS`, `CARGO_PROFILE_*`, `CARGO_TARGET_*`, …). Imposta `security.persist-environment = "none"` per non conservarne nessuno.
- Le variabili il cui nome sembra quello di una credenziale (`*TOKEN*`, `*SECRET*`, `*PASSWORD*`, `*_KEY`, `AWS_*`, `SCCACHE_*`, …) vengono registrate come "impostate" con il valore `<redacted>`.
- Le righe di comando vengono oscurate prima di essere salvate: i valori di `--token` e di flag simili, gli argomenti `key=value` con chiavi simili a credenziali, il token posizionale di `cargo login` e le informazioni utente negli URL.
- I valori dell'ambiente che rustc dichiara come input (`env!`) vengono salvati come digest troncati, e per nulla nel caso di nomi simili a credenziali.
- Il codice sorgente non viene mai raccolto; i file sorgente locali sono rappresentati dal percorso e dal digest del contenuto.

La suite di test inserisce delle credenziali e verifica che non compaiano in alcun file salvato né in alcun output generato.

## Percorsi {#paths}

Lo stato sotto `~/.qrust` conserva i percorsi locali reali (le directory che contengono i record delle esecuzioni e la cache di QuickRust vengono create con modalità 0700). Tutto ciò che viene esportato — `QRUST_REPORT`, `--format json`, HTML, baseline — sostituisce la radice del workspace, la home directory, la home di Cargo di QuickRust e la home di QuickRust con `$WORKSPACE`, `$HOME`, `$CARGO_HOME`, `$QRUST_HOME` (`security.redact-path-home`, attiva per impostazione predefinita).

## Cancellazione {#deleting}

QuickRust cancella solo all'interno della propria cache (`~/.qrust/cache`), il cui contenuto ha creato lui stesso e può ricompilare o scaricare di nuovo: directory di build dei workspace, voci della cache degli artefatti, driver del compilatore, sorgenti dei programmi installati, download incompleti, risposte memorizzate. I crate e i checkout git scaricati nella home di Cargo di QuickRust vengono rimossi da Cargo stesso (`cargo -Zgc clean gc`, che attende le build che li stanno leggendo), su richiesta di QuickRust; QuickRust non cancella mai nient'altro lì, e mai la tua configurazione o le tue credenziali. Non cancella mai nulla in un progetto, in una directory target scelta dall'utente o tra le toolchain (a parte `qrust toolchain remove` e le release superate dei canali dopo un giorno di inutilizzo). Una pulizia non rimuove mai nulla che sia stato usato negli ultimi 15 minuti, una directory di build tenuta da un comando di QuickRust (il suo lock) o una in cui un Cargo tiene un lock; rinomina ciò che rimuove in `cache/tmp` prima di cancellarlo, così nulla viene mai visto cancellato a metà, e registra ciò che ogni pulizia ha rimosso (`cache/gc-log.jsonl`: conteggi e byte per tipo, e le directory di build e i sorgenti dei programmi per percorso). Le scansioni non seguono mai i link simbolici. `qrust gc --dry-run` dice che cosa rimuoverebbe una pulizia.

`qrust inventory` pianifica all'interno di una directory di build e non cancella nulla: classifica tutto ciò che non riconosce come `uncertain`, che non è mai un candidato, e finché un Cargo tiene il lock della build o un'altra sessione di QuickRust è attiva, il piano riporta zero byte come recuperabili subito. Le directory di build dei benchmark create da QuickRust stesso vengono rimosse solo insieme al loro file marcatore.

## Modifiche a un repository {#changes-to-a-repository}

QuickRust scrive in un repository solo su richiesta esplicita. Le correzioni sono anteprime legate al contenuto esatto da cui sono state calcolate. Solo le modifiche alla configurazione di QuickRust stesso sono classificate come `safe` e possono essere applicate (`qrust fix --apply <id>`); l'applicazione verifica che il file sia identico byte per byte alla versione precedente mostrata in anteprima, verifica il risultato e può essere annullata. Le modifiche al profilo, al linker, alle dipendenze e a `build.rs` sono `review_required` e vengono soltanto mostrate.

## Report HTML {#html-reports}

Autonomi: niente script, niente risorse esterne. Ogni stringa controllata dal repository (nomi dei pacchetti, feature, percorsi, diagnostica) viene sottoposta a escape HTML.

## Script di build {#build-scripts}

QuickRust non esegue mai uno script di build. Il loro comportamento viene letto da ciò che Cargo lascia su disco dopo averli eseguiti.
