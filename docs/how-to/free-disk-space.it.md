# Come mantenere la cache di QuickRust entro i limiti del disco

Questa guida ti mostra come vedere quanto spazio usa QuickRust, liberare spazio subito, impostare i limiti che QuickRust rispetta da solo e spostare la cache su un altro disco.

Tutto ciò che si trova sotto `~/.qrust/cache` (directory di build, la cache degli artefatti, driver del compilatore, download) può essere ricompilato o scaricato di nuovo, e QuickRust rimuove ciò che nulla sta usando, a partire dal più grande e inutilizzato da più tempo, ogni volta che la cache supera il suo limite di dimensione o il disco resta a corto di spazio libero. Questa guida ti serve solo quando i default non sono adatti alla tua macchina. Il modo in cui la pulizia decide è descritto in [Perché QuickRust possiede il proprio ambiente](../explanation/owning-the-environment.md).

## Vedi che cosa contiene la cache {#see-what-the-cache-holds}

```bash
qrust cache status
```

```text
QuickRust's cache: 102.8 MiB, counted just now, /home/me/.qrust/cache
  build directories          92.2 MiB  5
  artifact cache             37.1 MiB  17
  remembered answers        312.0 KiB  84
  leftovers                   4.0 KiB  2
  crate and git downloads     9.7 MiB  2
  shared                     36.6 MiB  in files two kinds share (hard links), counted once in the total
  bounds                   at most 35.1 GiB (cache.max-size = auto), keeps 17.6 GiB of the disk free (cache.min-free = auto), 12.0 GiB free now
```

I valori sono quelli dell'ultimo conteggio; ogni pulizia conta di nuovo. Se la prima riga dice `not counted yet`, finora non è stata eseguita alcuna pulizia; QuickRust conta la cache in background dopo il comando successivo, e nel frattempo `qrust gc --dry-run` ne stampa la dimensione attuale.

Una riga `kept by snapshots` dice quanto hanno rimosso le pulizie che gli snapshot del volume conservano ancora. Uno snapshot conserva i blocchi di ogni file modificato prima che fosse creato, finché non viene cancellato: su macOS, gli snapshot locali orari di Time Machine (`tmutil listlocalsnapshots /`) sono conservati per un giorno. QuickRust elenca gli snapshot del volume e conta come libero ciò che conservano finché esistono, per un giorno al massimo, così non rimuove altro per compensare spazio che ha già restituito. Lo spazio che il disco non mostra per qualsiasi altro motivo è stato usato da altri programmi, e non viene contato. QuickRust non vede snapshot su Linux o Windows. Se uno snapshot di btrfs o ZFS copre `~/.qrust/cache`, lì le pulizie restituiscono più di quanto manca al disco; tieni la cache fuori dagli snapshot (su btrfs, rendila un subvolume a sé). Una riga `collected` che termina con `QuickRust command was running Cargo` significa che quella pulizia ha lasciato i download di Cargo, perché rimuoverli mentre un Cargo è in esecuzione può fermare quel Cargo. La pulizia successiva li prende.

## Libera spazio subito {#free-space-now}

```bash
qrust gc --dry-run          # che cosa rimuoverebbe una pulizia, senza rimuovere nulla
qrust gc                    # esegue la pulizia ora, entro i limiti configurati
qrust gc --to 5GiB          # pulisce finché la cache non occupa al massimo 5 GiB
```

Ogni rimozione viene elencata con il suo motivo; se la pulizia non è riuscita a riportare la cache entro i suoi limiti, ogni cosa conservata viene elencata con il motivo per cui è rimasta; poi una riga riassume il tutto:

```text
     removed 4.5 MiB    build directories  /home/me/.qrust/cache/builds/app-aa6b388726e8cac5 (the bounds needed room, and nothing had used it for 3 hours)
     kept    4.5 MiB    build directories  /home/me/.qrust/cache/builds/tool-f5025e19e2918129 (process 1261843 (sleep) works in /home/me/src/tool/src, in its workspace's checkout /home/me/src/tool)
OK   Freed 4.5 MiB: QuickRust's cache 9.0 MiB -> 4.5 MiB (at most 37.6 GiB; keeps 18.8 GiB of the disk free, 153.4 GiB free now), short of its goal: 1 in use left alone [29ms]
```

Una pulizia prende per primo ciò che libera di più per quanto a lungo è rimasto inutilizzato: una directory di build grande in cui nessuno compila da ore va prima di una piccola usata mezz'ora fa. Non rimuove mai nulla che sia stato usato negli ultimi 15 minuti, né una directory di build occupata da un comando in esecuzione o da Cargo, da cui è stato avviato un programma in esecuzione, o in cui lavora un processo: nella directory stessa, o in qualsiasi punto del checkout del suo workspace (una shell, un editor o la sessione di un agente aperti lì). Se libera meno di quanto ti aspettavi, le righe `kept` dicono che cosa tiene ciascuna directory; termina quei processi, o eseguila di nuovo quando quelle build sono terminate. La regola completa è in [File e directory](../reference/files.md#collection).

Quando una pulizia ha rimosso la directory di build di un workspace, il comando successivo di QuickRust in quel workspace lo dice, una volta:

```text
NOTE QuickRust's cache removed this workspace's build directory /home/me/.qrust/cache/builds/app-aa6b388726e8cac5 just now (the bounds needed room, and nothing had used it for 3 hours; 4.5 MiB): what was built there is built again by the next build
```

Compila di nuovo prima di eseguire i suoi programmi. Uno script che continua a usare i programmi di una build dopo che la build è finita prenota invece la directory ([Come trovare gli output di build senza target/](find-build-outputs.md#use-a-builds-outputs-for-longer-than-the-build)).

## Imposta i limiti {#set-the-limits}

In `~/.qrust/config.toml`:

```toml
[cache]
max-size = "40GiB"   # lo spazio massimo che la cache usa
min-free = "10%"     # lo spazio libero che lascia sul disco
```

Entrambe accettano una dimensione (`500MB`, `40GiB`), una quota del disco (`10%`) oppure `auto`. I default sono `auto`: al massimo il 10% del disco, tra 10 e 100 GiB, e almeno il 5% del disco libero, tra 2 e 20 GiB. Questo file è il posto per un limite che valga sulla macchina: ogni comando eseguito come te lo legge, chiunque lo avvii (uno script, un editor, la build di un altro strumento). Per una singola shell o un job di CI, usa `QRUST_CACHE_MAX_SIZE` e `QRUST_CACHE_MIN_FREE`. `qrust policy explain` mostra quale è in vigore e da dove viene.

Se il disco è condiviso con altri dati di grandi dimensioni, aumenta `min-free`; QuickRust allora restituisce spazio man mano che il disco si riempie, qualunque sia la sua dimensione. Restituisce quanto manca al disco più un quarto di `min-free`, e di nuovo quando il disco ha perso quel margine. Su un disco che di solito è sotto `min-free` a causa di altri dati, abbassa `min-free` allo spazio che vuoi mantenere libero: altrimenti la cache resta più piccola della differenza.

## Sposta la cache su un altro disco {#move-the-cache-to-another-disk}

Quando il disco su cui si trova `~/.qrust` è a corto di spazio, o ne è collegato uno più veloce, sposta lì la cache:

```bash
qrust cache move /Volumes/Fast/qrust-cache
```

```text
OK   Moved QuickRust's cache from /Users/me/.qrust/cache to /Volumes/Fast/qrust-cache: 18.4 GiB in 212034 files [2m14.07s]
```

Ogni comando successivo usa la cache lì, e nulla di ciò che contiene va perso: le build restano aggiornate com'erano, e la cache degli artefatti conserva ogni voce. La directory non deve ancora esistere, o deve essere vuota. Lo spostamento aspetta che finiscano i comandi di QuickRust in esecuzione, e si ferma mentre un programma tiene una directory di build (`qrust path --hold-for`). Da quel momento i limiti descritti sopra valgono per il disco su cui si trova la cache, e `qrust cache status` mostra dove si trova.

Se quel disco non è montato, i comandi di QuickRust si fermano e lo dicono invece di compilare in una cache nuova e vuota: montalo, oppure sposta la cache altrove, dove riparte vuota. `qrust cache move ~/.qrust/cache` la riporta indietro. Dettagli: [File e directory](../reference/files.md#where-the-cache-is).

## Guarda dentro la directory di build di un workspace {#look-inside-one-workspaces-build-directory}

```bash
qrust inventory
qrust inventory --older-than 14 --max-size 2GiB
qrust inventory --caches
```

`inventory` scompone per categoria la directory di build del workspace ed elenca ciò che al suo interno è non aggiornato (inutilizzato da `--older-than` giorni, 30 per default), con una stima di quanto farebbe risparmiare rimuoverlo e di quanto costerebbe ricompilarlo. Non cancella mai nulla. `--caches` aggiunge le cache condivise (il registry di Cargo e i download da git).

Per svuotare la directory di build di un workspace, esegui `cargo clean` al suo interno.

## Rimuovi altre cose di grandi dimensioni {#remove-other-large-things}

- Le toolchain non fanno parte della cache. Elencale con `qrust toolchain list` e rimuovi quelle che non usi più con `qrust toolchain remove <name>`; le release da cui un canale si è spostato vengono rimosse da QuickRust un giorno dopo il loro ultimo utilizzo.
- I programmi installati con `qrust install` restano finché non li rimuovi con `qrust uninstall`; i loro sorgenti di build fanno parte della cache.
- Una directory target che hai scelto tu (`--target-dir`, `CARGO_TARGET_DIR`) non viene mai ripulita: puliscila con `cargo clean`.

Anche cancellare `~/.qrust/cache` a mano funziona, purché non sia in esecuzione nessuna build: le build successive compilano e scaricano di nuovo ciò di cui hanno bisogno. Una cache che hai spostato resta dove l'hai messa: svuota la sua directory invece di cancellarla, altrimenti QuickRust la prende per un disco non montato.

## Se una build esaurisce lo spazio {#if-a-build-runs-out-of-space}

Quando il disco è quasi pieno (sotto un quarto di `cache.min-free`), un comando esegue prima una pulizia per un massimo di dieci secondi e lo segnala. Se una build fallisce comunque con il disco pieno, il riepilogo indica quanto spazio è libero e quanto occupa la cache di QuickRust; libera spazio con `qrust gc --to <size>` o altrove sul disco, poi compila di nuovo.

## Vedi anche {#related}

- [Configurazione](../reference/configuration.md) per `cache.max-size` e `cache.min-free`
- [File e directory](../reference/files.md) per che cosa si trova dove sotto `~/.qrust`
