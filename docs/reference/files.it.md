# File e directory

Tutto ciò che QuickRust conserva su disco: la sua home directory, la sua cache, ciò che scrive in un repository e per quanto tempo vengono conservati i record.

QuickRust crea le sue directory con modalità 0700. I percorsi qui sotto usano la home predefinita, `~/.qrust`; `QRUST_HOME` la sposta per intero.

## La home di QuickRust {#quickrusts-home}

| Percorso | Contenuto | Se lo cancelli |
|---|---|---|
| `~/.qrust/bin/` | QuickRust (`qrust`, `qrust-rustc-wrapper`): link all'installazione di un gestore di pacchetti, oppure copie proprie di QuickRust quando l'ha installato un archivio o una build dal sorgente; `cargo` e i programmi della toolchain come link a `qrust` ([Comandi](commands.md#toolchain-programs)); i programmi installati da `qrust install` | i link vengono ricreati dal comando successivo; i programmi installati vanno persi |
| `~/.qrust/config.toml` | la configurazione di QuickRust dell'utente ([Configurazione](configuration.md)) | è tua |
| `~/.qrust/setup.json` | ciò che ha fatto `qrust setup`: dove ha installato, l'installazione del gestore di pacchetti a cui è collegato (`installed_by`), quali file della shell ha modificato, che cosa ha importato, e il digest di ogni binario di QuickRust che esso o `qrust self update` ha installato (così un aggiornamento riconosce i proxy creati da una release precedente, che su Windows ne sono copie); letto da `qrust doctor`, `qrust self update` e `qrust self uninstall` | `qrust setup` lo riscrive |
| `~/.qrust/env`, `~/.qrust/env.fish` | gli script che i file di avvio della shell caricano per mettere `~/.qrust/bin` al primo posto nel `PATH` | `qrust setup` li riscrive |
| `~/.qrust/installs.json` | ciò che `qrust install` ha installato, per radice e pacchetto (`qrust install --list`) | usa invece `qrust uninstall` |
| `~/.qrust/toolchains/` | le toolchain Rust installate da QuickRust (`qrust toolchain list`); `channels.json` indica quale release corrisponde a ciascun canale, quella predefinita e le toolchain collegate | usa `qrust toolchain remove`; una toolchain cancellata viene installata di nuovo al suo uso successivo |
| `~/.qrust/cargo/` | la home di Cargo di QuickRust: `config.toml`, `credentials.toml` (modalità 0600), l'indice e i sorgenti del registro, i checkout git | le directory del registro e di git vengono scaricate di nuovo; `config.toml` e `credentials.toml` sono tuoi |
| `~/.qrust/workspaces/<id>/` | per ogni workspace: `workspace.json` (la sua radice), `sessions/` (i record delle esecuzioni), `events/` (gli eventi grezzi di rustc e l'output salvato), `runs/` (i report analizzati), `index/units.json` (gli ultimi fingerprint noti delle unità), `benchmarks/`, `remediations/`, `restore/` (i journal delle modifiche dei benchmark in corso) | la cronologia va persa |
| `~/.qrust/workspaces/<id>/prebuilt/` | gli stamp delle build riuscite in un terminale: i file da cui ciascuna dipendeva, con dimensioni e orari (al massimo 32 per workspace) | la build successiva interroga Cargo |
| `~/.qrust/active/` | un file per ogni sessione in corso | solo mentre non è in esecuzione alcun comando di QuickRust |
| `~/.qrust/cargo-runs/` | un file vuoto per ogni processo di QuickRust che può eseguire un Cargo con la home di Cargo di QuickRust, chiamato con il suo id di processo, e il `.lock` sotto cui vengono scritti. Un programma della toolchain che QuickRust esegue con il suo nome (`rust-analyzer`, `rustfmt`, …) non ne ha: un Cargo che esegue è il `cargo` di QuickRust, che ha il proprio, così il rust-analyzer di un editor non impedisce alla pulizia di Cargo di essere eseguita; una pulizia esegue la pulizia di Cargo solo mentre nessuno di essi è in esecuzione ([Raccolta](#collection)). Un file il cui processo non esiste più non prenota nulla e viene rimosso dal comando o dalla pulizia successivi. Un comando che non riesce a scrivere il proprio file si ferma prima di eseguire Cargo | solo mentre non è in esecuzione alcun comando di QuickRust |
| `~/.qrust/linker/ld` | un link al `wild` in uso ([Fast mode](fast-mode.md#linker)) | ricreato dalla build successiva |
| `~/.qrust/notices/` | i marcatori delle note mostrate una volta sola o una volta all'ora | le note vengono mostrate di nuovo |
| `~/.qrust/cache/` | la cache di QuickRust (vedi sotto) | vedi sotto |

`<id>` sono le prime 16 cifre esadecimali dello SHA-256 del percorso canonico della radice del workspace.

## La cache di QuickRust {#quickrusts-cache}

Tutto ciò che si trova sotto `~/.qrust/cache/` può essere ricompilato o scaricato di nuovo. QuickRust la mantiene entro `cache.max-size` e lascia libero `cache.min-free` del disco ([Configurazione](configuration.md#keys)), rimuovendo ciò che è stato usato meno di recente.

| Percorso | Contenuto |
|---|---|
| `builds/<name>-<id>/` | la directory target di ogni workspace (`qrust path`): l'output della build di Cargo. `<name>` è il nome della directory del workspace (al massimo 24 caratteri; tutto ciò che non è lettera, cifra, `-` o `_` viene sostituito da `_`) |
| `builds/<name>-<id>.<variant>/` | la directory target di una [variante di build](configuration.md#keys) di quel workspace (`build.variant`), con un record e un lock propri come qualsiasi directory di build |
| `builds/<name>-<id>.hold-<pid>` | una prenotazione di quella directory di build da parte del processo in esecuzione `<pid>` (`qrust path --hold-for`); una volta terminato il processo non prenota più nulla, e la prenotazione successiva o la pulizia successiva la rimuove |
| `builds/<name>-<id>.json`, `builds/<name>-<id>.lock` | a quale workspace appartiene una directory di build e quando è stata usata l'ultima volta; il lock che un comando tiene mentre usa la directory |
| `builds/<name>-<id>/qrust-bench/` | le directory di build di benchmark ed esperimenti, ciascuna con un marcatore `.qrust-owned` e una propria cache degli artefatti (`.qrust-artifacts/`) |
| `artifacts/` | la cache degli artefatti |
| `drivers/<commit>-<host>-<source>/` | il driver del compilatore di QuickRust per un compilatore e un sorgente del driver (`qrust driver status`); `build-failed` dice perché non è stato possibile compilarlo, `failed/` conserva il report di ogni crate su cui ha fallito |
| `src/` | i sorgenti dei programmi in corso di installazione o già installati: crate copiati dalla home di Cargo di QuickRust (con il loro lockfile pubblicato conservato come `.qrust-published.lock`) e cloni `--git`; `<copy>.lock` mentre un'installazione ne usa uno |
| `downloads/` | gli archivi delle toolchain in corso di download (ripresi dopo un'interruzione) |
| `memo/rustc/` | il `rustc -vV` di ogni compilatore, richiesto una sola volta per binario |
| `memo/roots/` | a quale workspace appartiene una directory, secondo quanto detto da `cargo locate-project` (richiesto di nuovo quando cambia un `Cargo.toml` da lì verso l'alto) |
| `tmp/` | ciò che una pulizia sta cancellando; prima viene rinominato qui |
| `usage.json` | l'ultimo conteggio della cache, lo spazio libero del disco in quel momento, e ciò che le pulizie hanno rimosso e che il disco non mostrava ancora come libero; un conteggio che QuickRust non riesce a scrivere fa fallire la pulizia, e uno in un formato più vecchio non vale come conteggio (la pulizia successiva conta di nuovo) |
| `gc-log.jsonl` | una riga per pulizia (le ultime 200): per tipo e motivo, quante cose sono state rimosse e i loro byte; le directory di build e i sorgenti dei programmi rimossi, per percorso; il riepilogo di Cargo, o perché i download di Cargo sono stati lasciati; lo spazio libero da cui è partita e il suo obiettivo |
| `gc.lock` | tenuto da una pulizia in corso |

### Raccolta {#collection}

Una pulizia rimuove, in quest'ordine:

1. tutto ciò che nulla può più usare: directory di build di workspace che non esistono più, driver di toolchain che non esistono più, download incompleti vecchi di una settimana, memo vecchi di un mese, residui in `tmp/`;
2. finché la cache supera `cache.max-size` o il disco scende sotto `cache.min-free`: directory di build, voci degli artefatti e sorgenti dei programmi installati, a partire da quelli usati meno di recente, finché la cache non scende sotto il 90% di `cache.max-size` e il disco non ha libero il 125% di `cache.min-free`;
3. i crate e i checkout git scaricati nella home di Cargo di QuickRust, rimossi da Cargo stesso (`cargo -Zgc clean gc`): secondo le età consuete di Cargo a ogni pulizia e, finché i limiti sono ancora superati, quelli inutilizzati da un mese, una settimana, un giorno, un'ora e 15 minuti, nell'ordine. Solo mentre nessun comando di QuickRust sta eseguendo un Cargo (`~/.qrust/cargo-runs/`). Altrimenti vengono lasciati alla pulizia successiva, cosa che `qrust gc` e `qrust cache status` riportano. La pulizia di Cargo cancella i suoi record dei registri che non trova nella home di Cargo, come quello temporaneo su cui compila `cargo package`, e un Cargo in esecuzione che tiene uno di quei record si ferma con un errore interno. Un comando che parte mentre la pulizia di Cargo è in corso la attende e lo dice.

Una pulizia conta da sé ciò che libera e non misura di nuovo il disco finché non termina, perché il disco potrebbe non mostrare ciò che libera. Uno snapshot (gli snapshot locali orari di Time Machine su macOS, gli snapshot di btrfs o ZFS) conserva i blocchi di ogni file più vecchio di sé, e ciò che è stato usato meno di recente lo è sempre. Ciò che il disco non mostra alla fine viene registrato in `usage.json` e contato come libero, prima e dopo i comandi e dalla pulizia successiva, finché il disco non lo mostra. Questo dura al massimo un giorno, la vita di uno snapshot locale di Time Machine. Dopo un giorno, torna a contare il valore del disco stesso.

Non vengono mai rimossi: tutto ciò che è stato usato negli ultimi 15 minuti; una directory di build tenuta da un comando di QuickRust o in cui un Cargo tiene un lock; un sorgente tenuto da un'installazione; una voce degli artefatti i cui file sono collegati da una directory di build in uso. Una pulizia dura minuti, quindi subito prima di rimuovere una directory di build verifica di nuovo, sotto il lock della directory, se è prenotata o è stata usata: un'esecuzione che parte durante una pulizia conserva ciò che ha prenotato. Le dimensioni sono i blocchi allocati; un file con più hard link viene contato una sola volta e liberato con il suo ultimo link.

| Innesco | Raccolta |
|---|---|
| dopo ogni comando, quando il disco è sotto `cache.min-free` (contando ciò che le pulizie hanno rimosso e che esso non mostra ancora), la cache supera `cache.max-size` dello spazio libero perso dall'ultimo conteggio, oppure è passato un giorno dall'ultimo conteggio; non entro 15 minuti da una pulizia che non è riuscita a rientrare nei limiti | `qrust gc --background`, scollegato, alla priorità più bassa |
| prima di un comando, quando il disco è sotto un quarto di `cache.min-free` (contato allo stesso modo) | nel comando, per al massimo dieci secondi, con una `NOTE`; i download di Cargo non vengono toccati |
| `qrust gc` | subito |

`qrust cache status` mostra l'ultimo conteggio e le pulizie recenti; `qrust gc --dry-run` elenca ciò che una pulizia rimuoverebbe.

## Programmi della toolchain all'interno di una toolchain {#toolchain-programs-inside-a-toolchain}

| Percorso | Contenuto |
|---|---|
| `~/.qrust/toolchains/<id>/bin/.qrust-engine` (`.qrust-engine.exe` su Windows) | un hard link al `cargo` della toolchain, che QuickRust esegue affinché Cargo passi oltre il `cargo` di QuickRust come `CARGO` ([Compatibilità](compatibility.md#nested-invocations)); non per le toolchain collegate |

## In una directory di build {#in-a-build-directory}

| Percorso | Contenuto |
|---|---|
| `<target>/<profile>/deps/<unit>.qrust-svh` | l'hash dell'interfaccia di una libreria (early cut-off) |
| `<target>/<profile>/deps/<unit>.rlink`, `<unit>.qrust-objects/`, `<unit>.qrust-relink` | gli oggetti conservati di un binario e ciò da cui sono stati compilati, per ricollegarlo |

Se ne vanno insieme al resto della directory (`cargo clean`).

## In un repository {#in-a-repository}

QuickRust scrive in un repository solo su richiesta.

| Percorso | Scritto da |
|---|---|
| `.qrust/config.toml` | te, oppure `qrust fix --apply` per una correzione `safe` |
| `.qrust/budgets.toml` | te ([Budget](budgets.md)) |
| `.qrust/baselines/<name>.json` | `qrust baseline set` |
| il percorso di `measurement.report` / `QRUST_REPORT` | ogni build, quando è impostato |
| il file di `qrust report -o FILE` | quel comando |
| un file sorgente del workspace, temporaneamente | lo scenario di benchmark `small-edit`, che lo ripristina ([Benchmark](benchmarks.md#source-edits)) |

In un progetto non viene creata alcuna directory `target/`.

## Setup {#setup}

`qrust setup` scrive:

- `~/.qrust/bin/` con `qrust`, `qrust-rustc-wrapper` e i link ai programmi; si rifiuta di sostituire lì un file che non sia di QuickRust. Avviato dall'installazione di un gestore di pacchetti, `qrust` e `qrust-rustc-wrapper` sono link a quelli del pacchetto, tramite il percorso indicato dal suo marcatore; avviato da qualsiasi altra cosa, sono copie;
- `~/.qrust/env` e `~/.qrust/env.fish`;
- una riga contrassegnata `# Added by `qrust setup`: QuickRust's bin first on PATH` in `~/.profile`, e in `~/.bash_profile`, `~/.bashrc` e `~/.zshenv` dove esistono (`~/.zshenv` anche quando `zsh` è nel `PATH`); `~/.config/fish/conf.d/qrust.fish` quando esiste `~/.config/fish`; su Windows, il `PATH` dell'utente tramite PowerShell (niente di tutto questo con `--no-modify-path`);
- una sola volta, da `$CARGO_HOME` o da `~/.cargo`: `credentials.toml` e le tabelle `[registries]`, `[registry]`, `[source]`, `[net]` e `[http]` di `config.toml`, in `~/.qrust/cargo/` (le tabelle che non ha importato vengono elencate);
- `~/.qrust/setup.json`.

`qrust self uninstall --yes` rimuove `~/.qrust` e le righe e i file elencati sopra che `qrust setup` ha aggiunto. rustup e `~/.cargo` non vengono mai modificati.

### L'installazione di un pacchetto {#a-packages-installation}

La formula di Homebrew, il deb e l'rpm mettono `qrust-installed-by.json` accanto al `qrust` che installano (Homebrew: `$(brew --prefix)/opt/quickrust/libexec/`; deb e rpm: `/opt/quickrust/`). Indica il gestore di pacchetti, il percorso del `qrust` del pacchetto che resta lo stesso tra le sue versioni, e i comandi che lo aggiornano e lo rimuovono. QuickRust lo legge accanto al file a cui si risolve `qrust`: `qrust setup` crea allora i link a quel percorso, e `qrust self update` e `qrust self uninstall` indicano il comando del gestore e non cambiano nulla. Un marcatore che non si può leggere, o che indica un altro binario, è un errore, e non viene mai preso per un'installazione senza marcatore.

## Conservazione {#retention}

| Chiave | Predefinito | Conservati per workspace |
|---|---|---|
| `measurement.keep-runs` | 200 | record delle esecuzioni e report |
| `measurement.keep-raw-events` | 20 | eventi grezzi di rustc e output salvato, delle esecuzioni analizzate |

La potatura viene eseguita circa un comando ogni 32. Stamp delle build già compilate: al massimo 32 per workspace. Log delle pulizie: le ultime 200 voci.
