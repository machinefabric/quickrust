# Output

Ciò che QuickRust scrive in un terminale: le forme delle righe, quali comandi presenta, quando l'output di Cargo passa senza modifiche e che cosa conserva.

Perché l'output ha questo aspetto: [L'output delle build di QuickRust](../explanation/build-output.md).

## Forme delle righe {#line-forms}

Tutto ciò che QuickRust scrive per le persone è in ASCII.

| Forma | Esempio |
|---|---|
| Un esito: un tag, completato con spazi fino a quattro caratteri più uno spazio, poi il messaggio | `OK   Built: 12 compiled, 140 fresh [4.21s]` |
| Una riga che appartiene a un esito: rientrata di cinque spazi, fino alla colonna del messaggio | `     Downloaded 38 crates, 5.6 MiB [1.84s]` |

Un messaggio è un'intestazione, seguita dai suoi dettagli dopo `: `, separati da `, `; una durata, quando c'è, chiude la riga tra parentesi quadre. Un tag che non ha nulla da aggiungere sta da solo: `OK`, oppure `OK   [0.42s]` quando vale la pena di indicare solo il tempo.

| Tag | Colore | Significa |
|---|---|---|
| `OK` | verde in grassetto | ha funzionato |
| `FAIL` | rosso in grassetto | non ha funzionato: un errore, un comando fallito, un'interruzione |
| `WARN` | giallo in grassetto | ha funzionato, e qualcosa merita un'occhiata |
| `NOTE` | ciano in grassetto | ciò che QuickRust fa o sa, a titolo informativo |

Di una riga di esito è colorato solo il tag. La riga di stato e le righe di dettaglio come `Locked ...` sono attenuate; le parole delle righe delle dipendenze sono colorate ([più sotto](#commands-that-do-not-build)). Non c'è colore quando `NO_COLOR` è impostata (non vuota) o con `CARGO_TERM_COLOR=never`, e quando la riga non va a un terminale, a meno che non valga `CARGO_TERM_COLOR=always`. Senza colore, è il tag a portare il significato.

Le durate sono mostrate come `120ms`, `4.21s`, `2m05.3s`; le dimensioni come `942 B`, `281.0 KiB`, `5.6 MiB`.

## Quando QuickRust presenta l'output {#when-quickrust-presents-the-output}

QuickRust presenta l'output di un comando quando valgono tutte queste condizioni:

- `output = "qrust"` (il default in fast mode; in compat mode solo se impostato esplicitamente);
- stderr è un terminale;
- il comando non è un alias di Cargo;
- la riga di comando non contiene nessuno tra `--message-format`, `-v`, `-vv`, `--verbose`, `-q`, `--quiet`, `--color` (prima di un eventuale `--`).

Altrimenti l'output di Cargo raggiunge il terminale, la pipe o il file senza modifiche. Per comando:

| Comandi | Presentazione |
|---|---|
| `build`, `check`, `clippy`, `doc` (e i loro alias `b`, `c`, `d`, `rustdoc`), `test --no-run`, `bench --no-run` | la build viene resa: riga di stato, errori per intero, warning contati, riepilogo |
| `run` (per l'host, o per una sola target triple, da `--target` o `build.target`) | la build viene resa, poi `cargo run` esegue il programma con il terminale così com'è |
| `test`, `bench` | la build viene resa, poi i test o i benchmark vengono eseguiti in un terminale che QuickRust inoltra (Unix) |
| `test --doc`, `bench --doc` | l'output di Cargo |
| `update`, `fetch`, `add`, `remove` (`rm`), `new`, `init`, `generate-lockfile`, `package`, `publish`, `vendor`, `clean` | le righe di stato di Cargo vengono rese a partire dal suo stderr; stdout e stdin restano quelli del terminale |
| `tree`, `metadata` (senza `--help`) | i download vengono resi a partire da stderr; l'output proprio del comando su stdout resta invariato |
| `install`, `uninstall` | l'output di QuickRust stesso |
| ogni altro comando (`rustc`, `fix`, `login`, `logout`, `owner`, `yank`, `search`, sottocomandi esterni, ...) | l'output di Cargo |

Per un comando presentato, QuickRust chiede a Cargo i suoi messaggi JSON (per le build) e il suo avanzamento, e gli chiede di disegnare in ASCII (`term.unicode = false`, così `cargo tree` disegna `|--` e `` `-- ``). Questi arrivano a quel solo Cargo come argomenti `--config`; gli script di build, i Cargo annidati e i programmi che un comando esegue non li vedono.

## Build {#builds}

### Riga di stato {#status-line}

Una riga, ridisegnata sul posto, rientrata fino alla colonna del messaggio:

```text
     Building [=========>          ] 112/240, 98 compiled, 14 fresh; tokio, syn
     Downloading [=====>              ] 7/25 crates, 2.1 MiB left
     Updating crates.io index: 26 packages looked up
     Waiting for the lock on the package cache (another Cargo holds it)
```

La barra è larga 20 celle. La riga viene troncata alla larghezza del terminale con `...`. Viene rimossa prima del riepilogo.

### Righe sopra la riga di stato {#lines-above-the-status-line}

| Riga | Esempio |
|---|---|
| Un errore del compilatore: il crate in rosso, poi la resa di rustc, per intero, man mano che arriva | `--- app (bin)` seguito da `error[E0425]: cannot find function ...` |
| I download, una volta terminati | `     Downloaded 1 crate, 4.4 KiB [0.32s]` |
| Le modifiche al lockfile | `     Locked 1 package to highest compatible version` |
| Le righe che QuickRust non riconosce (fallimenti degli script di build, output `cargo:warning=`, errori di risoluzione) | così come Cargo le ha stampate |

I warning non vengono stampati man mano che arrivano; vengono contati.

### Riepilogo {#summary}

| Esito | Riga |
|---|---|
| successo | `OK   Built: 1 compiled, 1 fresh, 3 restored from QuickRust's cache [4.21s]` |
| successo di `check`, `clippy` | `OK   Checked: 2 compiled [0.80s]` |
| successo di `doc`, seguito dalla posizione della documentazione | `OK   Documented: 1 compiled, 1 fresh [1.10s]` e `     docs <target>/doc/app/index.html` |
| fallimento con errori del compilatore | `FAIL Build: 1 error in app (bin), first at src/main.rs:2:27 [1.20s]` |
| fallimento senza errori del compilatore (l'errore di Cargo stesso è mostrato sopra) | `FAIL Build [0.10s]` |
| fallimento di `check`, `clippy`; `doc` | `FAIL Check: ...`; `FAIL Documentation: ...` |
| un disco pieno | dopo la riga `FAIL`: `NOTE the disk is full: <free> free where QuickRust keeps its cache, which holds <size>; ...` |
| interruzione | `FAIL Interrupted [3.02s]` |
| nulla è cambiato dall'ultima build riuscita | `OK   Up to date: 152 fresh` ([I controlli di aggiornamento](../explanation/up-to-date-checks.md)) |

Ogni unità viene contata una volta sola: `compiled` (rustc è stato eseguito), `fresh` (Cargo l'ha trovata aggiornata) oppure `restored from QuickRust's cache`. I conteggi pari a zero vengono omessi. I warning seguono il riepilogo in una sola riga, con i tre lint più frequenti per primi:

```text
WARN 23 warnings in app and util: unused_variables x17, dead_code x4, +2 more; `qrust warnings` shows them
```

### `run`, `test`, `bench` {#run-test-bench}

Dopo il riepilogo della build viene eseguito il comando vero e proprio, che trova tutto aggiornato. Le righe "Finished" e "Running" di Cargo per `cargo run` non vengono mostrate, né i warning che Cargo riproporrebbe. L'output del programma non viene toccato.

Per `test` e `bench`, QuickRust mostra le righe di Cargo a modo suo e chiude con i conteggi di tutti i programmi di test sommati:

```text
OK   Built: 1 compiled, 1 fresh [0.42s]
     unittests src/lib.rs
running 132 tests
...
test result: ok. 132 passed; 0 failed; 2 ignored; 0 measured; 0 filtered out; finished in 0.31s
     doc-tests app
...
OK   Tests: 140 passed, 2 ignored [1.12s]
```

| Esito | Riga |
|---|---|
| tutti superati | `OK   Tests: <counts> [<time>]`; conteggi nell'ordine failed, passed, ignored, measured, filtered out, con gli zeri omessi |
| un test è fallito | `FAIL Tests: 1 failed, 139 passed [<time>]` |
| un programma di test è fallito senza contare un fallimento | `FAIL Tests: a test program failed, 139 passed [<time>]` |
| nessun conteggio | `none ran` al posto dei conteggi |
| un harness che non è quello di libtest | nulla in caso di successo; `FAIL Tests [<time>]` in caso di fallimento |
| interruzione | `FAIL Interrupted [<time>]` |

Su Windows le righe dell'esecuzione dei test sono quelle di Cargo.

## Comandi che non compilano {#commands-that-do-not-build}

```text
     updated    serde                    1.0.209 -> 1.0.210
     added      hashbrown                0.15.0
     removed    autocfg                  1.3.0
OK   Lockfile updated: 1 updated, 1 added, 1 removed [0.92s]
```

| Riga | Forma |
|---|---|
| Una dipendenza nel lockfile (`update`, `generate-lockfile`) | `     <word> <name> <change>`: parola `updated` (verde), `downgraded` (giallo), `added` (verde), `removed` (rosso), `unchanged` (attenuato), completata con spazi fino a 10; nome completato fino a 24 |
| Una modifica al manifest (`add`, `remove`) | `     added      greet                    =1.0.0 to dependencies`, `     removed    greet                    from dependencies` |
| Le feature di `cargo add` | `     features +derive -alloc` |
| Le modifiche al lockfile di una modifica al manifest, in una sola riga | `     locked +greet 1.0.0` |
| I download | `     Downloaded 1 crate, 4.4 KiB [0.32s]` |
| Un risultato che Cargo riporta (`Creating`, `Removed`, `Packaged`, `Uploaded`, `Published`, `Vendored`) | `OK   Created binary (application) `tool` package`, `OK   Removed 125 files, 75.8 MiB`, `OK   Packaged 4 files, 1.2 KiB (942 B compressed)` |
| Le righe `note:` di Cargo | `NOTE see more `Cargo.toml` keys ...` |
| Una riga che QuickRust non riconosce | così come Cargo l'ha stampata |

La riga di chiusura:

| Esito | Riga |
|---|---|
| `update`, `generate-lockfile`, nulla è cambiato | `OK   Lockfile up to date [<time>]` |
| `update`, `generate-lockfile`, modifiche | `OK   Lockfile updated: <counts> [<time>]` |
| è stata stampata una riga di risultato (`new`, `init`, `clean`, `package`, `publish`, `vendor`), oppure `tree`, `metadata` | nessuna |
| qualsiasi altro successo (`add`, `remove`, `fetch`) | `OK   [<time>]` |
| fallimento | `FAIL cargo <command> [<time>]` |
| interruzione | `FAIL Interrupted [<time>]` |

## I comandi propri di QuickRust {#quickrusts-own-commands}

| Tipo | Forma |
|---|---|
| Un comando di QuickRust che non è potuto partire (exit status 2) | `FAIL <message>` su stderr |
| Un warning su una build che QuickRust non ha potuto osservare o registrare del tutto (è stata comunque eseguita) | `WARN <message>` su stderr |
| Qualcosa che QuickRust fa o sa (il driver del compilatore è in fase di build, il disco è quasi pieno, i crate ripristinati indicano un'altra directory, è disponibile un QuickRust più recente) | `NOTE <message>` su stderr |
| Il risultato di un comando di gestione (`qrust toolchain ...`, `qrust baseline set`, `qrust fix --apply`, `qrust self uninstall`) | `OK   <message>` su stdout |
| Una fase di lunga durata (installazione della toolchain, `qrust setup`, `qrust install`, `qrust gc`, `qrust self check`, `qrust self update`) | una riga di stato, poi `OK   <message> [<time>]` oppure `FAIL <message> [<time>]` |

Esempi:

```text
OK   Rust 1.99.0 ready (1.99.0-x86_64-unknown-linux-gnu) [38.20s]
OK   Installed hello 0.3.0: hello -> ~/.qrust/bin [12.41s]
OK   Freed 1.2 GiB: QuickRust's cache 9.8 GiB -> 8.6 GiB (at most 10.0 GiB; keeps 2.0 GiB of the disk free, 40.1 GiB free now) [0.84s]
FAIL toolchain no-such-toolchain-xyz (from `+toolchain` argument): no toolchain is named `no-such-toolchain-xyz` (`qrust toolchain link no-such-toolchain-xyz <dir>` names one)
```

## Output conservato {#stored-output}

Per ogni build che QuickRust ha presentato, ogni riga scritta da Cargo e da rustc (al massimo 200.000 per esecuzione) viene conservata insieme all'esecuzione, in `cargo-output.jsonl` nella directory degli eventi dell'esecuzione. I ridisegni della riga di stato non vengono conservati. Un'esecuzione trovata aggiornata contiene l'output conservato della build rispetto alla quale è aggiornata, i cui warning vengono ripetuti dalla sua riga `WARN`. `qrust warnings`, `qrust errors` e `qrust log` lo stampano di nuovo ([Comandi](commands.md#output-of-a-build)). L'output conservato viene mantenuto finché vengono mantenuti gli eventi grezzi dell'esecuzione (`measurement.keep-raw-events`). I comandi che non compilano non vengono conservati.
