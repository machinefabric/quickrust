# Compatibilità con Cargo

Che cosa QuickRust passa a Cargo e riceve da Cargo senza modifiche, che cosa aggiunge o cambia, e come vengono gestite le invocazioni annidate, le directory di target e la configurazione di Cargo.

La suite di test verifica questi punti usando come oracolo il Cargo della toolchain stessa (`tests/compat.rs`, il corpus QA della [specifica](specification.md)). Gli identificatori dei requisiti (`COMP-...`) si riferiscono alla specifica; dove questa implementazione se ne discosta, [Conformità](conformance.md) spiega come.

## Invariato {#unchanged}

| Che cosa | Comportamento |
|---|---|
| Argomenti | passati a Cargo così come sono, tranne un `+toolchain` iniziale, che QuickRust consuma (COMP-001, COMP-051) |
| Flag e comandi sconosciuti | inoltrati; è Cargo a decidere se sono validi e a risolvere i sottocomandi esterni (COMP-002, COMP-003) |
| Alias, selezione dei pacchetti, `--manifest-path`, profili, target, feature, comportamento del lockfile | quelli di Cargo (COMP-008) |
| Ambiente | ogni variabile passa a Cargo; QuickRust aggiunge le variabili elencate in [Ambiente](environment.md#set-for-the-cargo-quickrust-runs) e non riscrive mai `RUSTFLAGS`, `CARGO_INCREMENTAL` (a meno che sia impostata una politica `incremental.*` e l'utente non l'abbia impostata), né le impostazioni di proxy o di registry dell'utente (COMP-040, COMP-041) |
| Jobserver | `CARGO_MAKEFLAGS` e i descrittori del jobserver vengono ereditati (COMP-042) |
| Exit status | quello di Cargo, ogni volta che Cargo è stato eseguito; un segnale di terminazione viene rilanciato (COMP-006) |
| `--message-format=json` | l'output su stdout è ciò che stampa il Cargo delegato; QuickRust lo legge, quando stdout non è un terminale, mentre lo inoltra senza modifiche (COMP-005) |
| Configurazione di Cargo | mai interpretata da QuickRust per l'esecuzione; ne legge un sottoinsieme (chiavi dei profili, `build.rustc-wrapper`, alias, directory di target) solo per decidere le proprie impostazioni e per l'analisi (COMP-050, COMP-052) |
| `cargo clean` | il `clean` di Cargo, sulla directory di target del workspace (COMP-009) |
| `qrust upstream <args>` | il Cargo della toolchain senza impostazioni della fast mode, senza wrapper, senza rendering e senza record (COMP-013) |

## Aggiunto o cambiato {#added-or-changed}

| Che cosa | Comportamento |
|---|---|
| Impostazioni di build | in fast mode, le impostazioni di [Fast mode](fast-mode.md); in modalità compat, nessuna |
| Output sul terminale | in un terminale, è QuickRust a presentare l'output ([Output](output.md)); pipe, `-v`, `-q`, `--message-format`, `--color`, la modalità compat e `output = "cargo"` lasciano intatto l'output di Cargo (COMP-007) |
| Toolchain | QuickRust installa e seleziona da sé le toolchain, nell'ordine di rustup (`+toolchain`, `RUSTUP_TOOLCHAIN`, file di toolchain, predefinita); rustup non viene mai chiamato e i suoi override per directory non hanno equivalente (COMP-012, COMP-020) |
| Cargo home | `~/.qrust/cargo`, non `~/.cargo`; `~/.cargo/config.toml` viene comunque letto da Cargo per i progetti sotto la home directory, come qualsiasi file di configurazione in una directory genitore |
| Directory di target | ogni workspace compila in `~/.qrust/cache/builds/<name>-<id>/`, mai in `target/` nel progetto (vedi sotto) |
| `cargo install`, `cargo uninstall` | quelli di QuickRust: il resolver di Cargo sceglie la release, QuickRust la compila come qualsiasi build e mette i suoi binari in `~/.qrust/bin` (o `--root`), mai sopra i programmi di QuickRust stesso o quelli di un altro pacchetto senza `--force` ([Comandi](commands.md#programs)) |
| `RUSTC_WRAPPER` | impostato a `qrust-rustc-wrapper` per i comandi di build registrati; un wrapper configurato dall'utente gira al suo interno |
| `CARGO` | impostato al `cargo` di QuickRust (vedi sotto) |

## Catena dei wrapper {#wrapper-chain}

Per un comando di build registrato, una compilazione passa attraverso:

```text
qrust-rustc-wrapper -> the user's RUSTC_WRAPPER (or sccache chained by cache.provider) -> RUSTC_WORKSPACE_WRAPPER (applied by Cargo) -> rustc
```

Un wrapper configurato dall'utente (`RUSTC_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER`, `build.rustc-wrapper`) non viene mai rimosso né saltato: se non riesce ad avviarsi, la build fallisce come con il Cargo della toolchain. Solo sccache concatenato tramite `cache.provider = "sccache"` può essere saltato, con `cache.fail = "open"`.

## Invocazioni annidate {#nested-invocations}

Un Cargo avviato all'interno di una sessione QuickRust (da uno script di build, da un sottocomando esterno come Clippy, nextest o `cargo llvm-cov`, da un `xtask`, da un test) raggiunge QuickRust:

- tramite `PATH`, dove `~/.qrust/bin/cargo` viene per primo;
- tramite `CARGO`, che Cargo passa a tutto ciò che avvia. QuickRust esegue un hard link del `cargo` della toolchain chiamato `.qrust-engine`, con `argv[0]` e `CARGO` impostati a `~/.qrust/bin/cargo`, in modo che Cargo passi oltre il `cargo` di QuickRust (COMP-030). Su Windows, che non ha un `argv[0]` separato, l'help di Cargo nomina `.qrust-engine.exe`.

Un'invocazione annidata è una sessione QuickRust a sé, collegata alla sessione genitore tramite `QRUST_SESSION_ID` (COMP-031), con la stessa toolchain (`RUSTUP_TOOLCHAIN`). I suoi eventi di rustc vengono registrati insieme a quelli della sessione genitore. `compatibility.nested-cargo = "upstream"` (`QRUST_NESTED=upstream`) passa le invocazioni annidate direttamente al Cargo della toolchain (COMP-032).

Con `qrust upstream` e con una toolchain collegata, Cargo gira come sé stesso e le build annidate tramite `$CARGO` raggiungono direttamente quel Cargo; le loro compilazioni vengono comunque osservate quando il comando esterno ha impostato `RUSTC_WRAPPER`.

Una toolchain collegata il cui `cargo` è QuickRust stesso viene rifiutata con exit status 2 (COMP-022).

## Directory di target {#target-directory}

| L'utente ha scelto | Directory di target |
|---|---|
| `--target-dir DIR` | `DIR`, usata così com'è |
| `--config build.target-dir=DIR` sulla riga di comando | `DIR`, usata così com'è |
| `CARGO_TARGET_DIR` o `CARGO_BUILD_TARGET_DIR` nell'ambiente | il suo valore, usato così com'è (a meno che un QuickRust esterno l'abbia impostato per un altro workspace) |
| `build.target-dir` in un file di configurazione di Cargo | sovrascritta |
| niente | `~/.qrust/cache/builds/<name>-<id>/`, passata a Cargo e a ogni strumento sotto di esso tramite `CARGO_TARGET_DIR` e `CARGO_BUILD_BUILD_DIR` |

Il workspace è quello che `cargo locate-project --workspace` nomina per la directory in cui parte il comando (dopo `-C` e `--manifest-path`). `cargo metadata` riporta la directory, e `qrust path` la stampa. Una directory scelta dall'utente non viene mai gestita né rimossa dalla cache di QuickRust.

## Scritture {#writes}

QuickRust scrive in un repository solo su richiesta ([File](files.md#in-a-repository)). Lo stato risiede in `~/.qrust`.

Una build non fallisce mai perché QuickRust non è riuscito a registrarla o ad analizzarla: il fallimento è un `WARN`, e lo stato di Cargo resta valido, a meno che `measurement.strict = true`.

## Differenze tra piattaforme {#platform-differences}

Vedi [Piattaforme](platforms.md).
