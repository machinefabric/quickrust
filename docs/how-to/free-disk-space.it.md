# Come mantenere la cache di QuickRust entro i limiti del disco

Questa guida ti mostra come vedere quanto spazio usa QuickRust, liberare spazio subito e impostare i limiti che QuickRust rispetta da solo.

Tutto ciò che si trova sotto `~/.qrust/cache` (directory di build, la cache degli artefatti, driver del compilatore, download) può essere ricompilato o scaricato di nuovo, e QuickRust rimuove ciò che è stato usato meno di recente ogni volta che la cache supera il suo limite di dimensione o il disco resta a corto di spazio libero. Questa guida ti serve solo quando i default non sono adatti alla tua macchina. Il modo in cui la pulizia decide è descritto in [Perché QuickRust possiede il proprio ambiente](../explanation/owning-the-environment.md).

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

Ogni rimozione viene elencata, poi una riga riassume il tutto:

```text
OK   Freed 0 B: QuickRust's cache 102.8 MiB -> 102.8 MiB (at most 35.1 GiB; keeps 17.6 GiB of the disk free, 12.0 GiB free now), 22 in use left alone [9ms]
```

Una pulizia non rimuove mai nulla che sia stato usato negli ultimi 15 minuti, né una directory di build occupata da un comando in esecuzione o da Cargo. Se libera meno di quanto ti aspettavi, `in use left alone` indica quante voci ha dovuto conservare; eseguila di nuovo quando quelle build sono terminate.

## Imposta i limiti {#set-the-limits}

In `~/.qrust/config.toml`:

```toml
[cache]
max-size = "40GiB"   # lo spazio massimo che la cache usa
min-free = "10%"     # lo spazio libero che lascia sul disco
```

Entrambe accettano una dimensione (`500MB`, `40GiB`), una quota del disco (`10%`) oppure `auto`. I default sono `auto`: al massimo il 10% del disco, tra 10 e 100 GiB, e almeno il 5% del disco libero, tra 2 e 20 GiB. Questo file è il posto per un limite che valga sulla macchina: ogni comando eseguito come te lo legge, chiunque lo avvii (uno script, un editor, la build di un altro strumento). Per una singola shell o un job di CI, usa `QRUST_CACHE_MAX_SIZE` e `QRUST_CACHE_MIN_FREE`. `qrust policy explain` mostra quale è in vigore e da dove viene.

Se il disco è condiviso con altri dati di grandi dimensioni, aumenta `min-free`; QuickRust allora restituisce spazio man mano che il disco si riempie, qualunque sia la sua dimensione. Restituisce quanto manca al disco più un quarto di `min-free`, a partire da ciò che è stato usato meno di recente, e di nuovo quando il disco ha perso quel margine. Su un disco che di solito è sotto `min-free` a causa di altri dati, abbassa `min-free` allo spazio che vuoi mantenere libero: altrimenti la cache resta più piccola della differenza.

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

Anche cancellare `~/.qrust/cache` a mano funziona, purché non sia in esecuzione nessuna build: le build successive compilano e scaricano di nuovo ciò di cui hanno bisogno.

## Se una build esaurisce lo spazio {#if-a-build-runs-out-of-space}

Quando il disco è quasi pieno (sotto un quarto di `cache.min-free`), un comando esegue prima una pulizia per un massimo di dieci secondi e lo segnala. Se una build fallisce comunque con il disco pieno, il riepilogo indica quanto spazio è libero e quanto occupa la cache di QuickRust; libera spazio con `qrust gc --to <size>` o altrove sul disco, poi compila di nuovo.

## Vedi anche {#related}

- [Configurazione](../reference/configuration.md) per `cache.max-size` e `cache.min-free`
- [File e directory](../reference/files.md) per che cosa si trova dove sotto `~/.qrust`
