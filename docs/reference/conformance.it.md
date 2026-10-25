# Conformità alla specifica

Stato della [specifica](specification.md) in questa implementazione. "Testato" indica dove il
comportamento viene verificato.

## Checklist P0 {#p0-checklist}

| Voce | Stato | Dove |
|---|---|---|
| Shim di compatibilità `cargo`; eseguibile di gestione `qrust` | fatto | `src/cli.rs`, `src/session.rs` · tests/compat.rs |
| Engine sicuro contro la ricorsione (COMP-021, COMP-022) | fatto: il Cargo della toolchain stessa; una toolchain collegata il cui `cargo` è QuickRust viene rifiutata | `src/engine.rs` · `test0268_recursion_is_refused…`, `test0269_an_unreachable_dist_server…` |
| Toolchain: `+toolchain`, `RUSTUP_TOOLCHAIN`, file di toolchain, predefinita (COMP-012, COMP-020) | fatto: QuickRust installa e seleziona da sé le toolchain (deviazione 6) | `src/toolchain/` · `qa003_*`, `test0238_toolchain_programs_run_through_quickrusts_proxies`, `test0237_builds_and_their_build_scripts_get_quickrusts_toolchain…`, `test0271_environment_reaches_build_scripts_unchanged` |
| Inoltro di argv / stdin / stdout / stderr / segnali, exit status esatto | fatto | `src/proc.rs` · `qa022`, `qa023`, `test0253_stdin_and_exit_codes…` |
| Sottocomandi esterni, alias, flag sconosciuti | fatto | `qa019`, `qa020`, `qa030` |
| Nessuna interferenza con la configurazione e l'ambiente di Cargo | fatto | `test0271_environment_reaches_build_scripts_unchanged`, `test0233_no_rebuild_when_alternating…` |
| Invocazioni annidate + opt-out | fatto, tramite `PATH` e `$CARGO`; vedi deviazione 1 | `qa019`, `test0237_builds_and_their_build_scripts_get_quickrusts_toolchain…` |
| Fingerprint di workspace, toolchain e ambiente (FR-010) | fatto | `src/discover.rs` · `qa002`, `test0188_report_follows…` |
| Raccoglitore degli eventi JSON di Cargo | fatto, quando l'utente richiede JSON su una pipe | `qa018` |
| Raccoglitore tramite wrapper di rustc; composizione con un wrapper esistente | fatto | `src/wrapper.rs` · `qa010_qa011_qa012`, `test0250_wrapper_configured_in_cargo_config…` |
| DAG osservato delle unità di build; percorso critico (FR-030) | fatto | test di unità in `src/analyze.rs` |
| Classificatore di ricompilazioni / invalidazioni (FR-040) | fatto | test di unità in `src/waste.rs` · `test0189_rebuild_causes…`, `test0190_toolchain_change…` |
| Duplicazione di versioni; varianti di feature/profilo/target (FR-050) | fatto | test di unità in `src/waste.rs` |
| Analisi degli script di build (FR-060); identificazione delle macro procedurali (FR-070) | fatto | `qa007_qa008_qa009`, `test0191_build_script_without_rerun…` |
| Visibilità su linker / generazione del codice (FR-080) | fatto | `qrust analyze codegen` |
| Valutatore della politica incrementale (FR-090) | fatto | `test0204_sccache_policy…` |
| Integrazione e report di sccache (FR-100) | fatto | `src/cache.rs` · `test0204_sccache_policy…`, `qa026` |
| Inventario degli artefatti, recuperabilità, GC (FR-110): `qrust gc` mantiene la cache entro i suoi limiti, `--dry-run` mostra un'anteprima | fatto | `src/inventory.rs`, `src/space.rs` · `test0194_inventory_and_its_stale_plan_are_read_only…`, `test0199_the_stale_plan_is_blocked_while_a_build_is_running`, `test0196_quickrusts_cache_removes_what_is_gone_then_the_least_recently_used…` |
| `cargo clean` esattamente come upstream | fatto | `qa016` |
| Identificazione dei worktree (FR-120) | fatto | `test0203_worktrees_report…` |
| Esecutore di scenari (FR-020); confronto (FR-160); esperimenti | fatto | `src/bench.rs`, `src/compare.rs` · `benchmark_*`, `experiment_*` |
| Motore di raccomandazioni (FR-150) | fatto | test di unità in `src/recommend.rs` |
| Anteprima sicura degli interventi correttivi (FR-170) | fatto; la classe `safe` può anche essere applicata e annullata | `src/remedy.rs` |
| Schema del sidecar JSON | fatto | [report-schema.md](report-schema.md) |
| Budget di regressione per la CI (FR-130) | fatto | `test0193_budgets_gate_ci…` |
| Oscuramento di segreti / percorsi | fatto | `test0192_secrets_never_reach…`, `qa021` |
| Astrazione dei processi multipiattaforma | fatto; su Windows un'esecuzione smoke (setup, build, esecuzione, test) | `src/proc.rs` |
| Corpus di compatibilità QA-001…030 | fatto tranne QA-024 (Windows) | tests/compat.rs |
| Test dell'overhead prestazionale | fatto | `test0273_dispatch_overhead_stays_within_budget` |
| Documentazione di migrazione / disinstallazione; `doctor`; via d'uscita | fatto | [Come adottare QuickRust gradualmente](../how-to/adopt-gradually.md), [Come installare QuickRust](../how-to/install.md) · `test0208_doctor_setup_and_uninstall` |

## Non in questa release (P1 e successivi nella specifica) {#not-in-this-release-p1-and-later-in-the-specification}

Esecuzione controllata del GC, amministrazione della cache remota e modalità di cache in sola
lettura, servizio centrale e politiche dell'organizzazione, annotazioni sulle PR, analisi dei
container (FR-140), il protocollo dei plugin (FR-180), gli adattatori nightly per `--unit-graph` /
`-Zbuild-analysis` / tempi per sezione, l'esperimento di incorporamento dei metadati e la
telemetria di rete / I/O su disco. Le funzionalità nightly vengono rilevate ed elencate in
`toolchain.capabilities`; nulla dipende da esse.

## Deviazioni e decisioni {#deviations-and-decisions}

1. **`CARGO` è il `cargo` di QuickRust (COMP-030), eseguendo Cargo sotto un altro nome.**
   Cargo passa a tutto ciò che avvia (sottocomandi esterni, script di build,
   `env!("CARGO")`, binari di test) il proprio percorso, e accetta invece un `CARGO`
   ereditato solo quando il suo eseguibile non si chiama `cargo` (Cargo 1.86 e
   successivi), oppure, in subordine, il percorso nel suo `argv[0]` quando questo si chiama `cargo`.
   QuickRust esegue quindi un hard link del `cargo` della toolchain, `bin/.qrust-engine`,
   con `argv[0]` e `CARGO` impostati a `~/.qrust/bin/cargo` (Windows non ha un `argv[0]`
   separato: lì l'help di Cargo nomina il link, `.qrust-engine.exe`): una build annidata
   tramite `$CARGO` (quella di Clippy, di nextest, di `cargo llvm-cov`) è una sessione QuickRust
   collegata alla sessione genitore (COMP-031), con le impostazioni di QuickRust, come una tramite
   `PATH`. Da entrambe ci si può sottrarre con `QRUST_NESTED=upstream` (COMP-032).
   `qrust upstream` esegue il `cargo` della toolchain come sé stesso, e una toolchain collegata
   non viene mai modificata, quindi lì le build annidate tramite `$CARGO` raggiungono direttamente
   quel Cargo; le loro invocazioni di rustc vengono comunque osservate, perché `RUSTC_WRAPPER`
   viene ereditato.

2. **Lo stato delle esecuzioni risiede in `~/.qrust`, non nel repository.** L'esempio di
   configurazione della specifica scrive i report in `.qrust/runs/`. Creare file in un
   repository a ogni `cargo build` non è una cosa che un sostituto diretto dovrebbe
   fare, quindi la registrazione va in `QRUST_HOME` e un percorso di report nel repository è
   opzionale (`measurement.report`, `QRUST_REPORT`).

3. **L'analisi è lazy per impostazione predefinita.** Per mantenere l'overhead di dispatch entro
   NFR-PERF-001/003, il percorso di build si limita a registrare; i report vengono prodotti su
   richiesta, o in modo sincrono quando sono stati richiesti un report o l'applicazione di un budget.
   Un'esecuzione analizzata dopo esecuzioni successive conserva tutto ciò che è stato registrato
   al momento della build (unità, tempi, revisione) e riporta i metadati del workspace, lo stato
   di modifica e l'ingombro degli artefatti come non disponibili, invece di descrivere l'albero di oggi.

4. **Le unità aggiornate richiedono lo stream JSON di Cargo.** Su una toolchain stabile, nient'altro
   indica quali unità Cargo ha considerato aggiornate. QuickRust osserva lo stream solo
   quando l'utente lo ha già richiesto e stdout non è un terminale, così il comportamento
   sul terminale non viene mai alterato; altrimenti `units_fresh` è `null` con un motivo.

5. **Il tempo di esecuzione degli script di build si ricava dai file che Cargo scrive attorno
   all'esecuzione** (`invoked.timestamp`, poi `stderr` / `root-output`). Cargo retrodata
   `output` al momento dell'invocazione, quindi non può essere usato.

6. **È QuickRust a gestire le toolchain (decisione del 2026-10-12), non rustup (COMP-020).** La
   specifica risolve un Cargo upstream tramite `PATH` e rustup. QuickRust invece
   installa le toolchain dal dist server di Rust in `~/.qrust/toolchains` e le seleziona
   nell'ordine documentato da rustup (`+toolchain`, `RUSTUP_TOOLCHAIN`, file di toolchain,
   predefinita; gli override per directory di rustup non hanno equivalente). Ogni comando esegue il
   Cargo di quella toolchain con la Cargo home di QuickRust (`~/.qrust/cargo`). `QRUST_REAL_CARGO` e
   `compatibility.real-cargo` non esistono più: una toolchain compilata altrove si usa con
   `qrust toolchain link`.

7. **Gli errori di configurazione valgono per ogni comando.** Un file
   `.qrust/config.toml` non valido blocca `cargo --version` come `cargo build`, indicando
   il campo e il file; `qrust upstream` non ne è influenzato.

8. **Nessuna impostazione `cache.mode` o `artifacts.gc`.** La configurazione di esempio della
   specifica le elenca, ma con l'amministrazione della cache remota e l'esecuzione del GC
   escluse da questa release ciascuna avrebbe esattamente un solo valore ammesso. Una
   modalità di cache in sola lettura che un server sccache in esecuzione potrebbe non rispettare
   sarebbe inoltre una falsa garanzia. Le chiavi vengono rifiutate come sconosciute invece di essere
   accettate e ignorate.

9. **L'identità portabile di un'unità include il contenuto dei sorgenti.** Cargo calcola l'hash dei
   membri del workspace in base al loro percorso relativo al workspace, quindi due worktree condividono
   gli hash delle unità anche su branch diversi. I duplicati tra worktree vengono contati solo quando
   anche gli input sono identici.

10. **L'ordine dei messaggi JSON non fa parte dell'oracolo.** Cargo upstream emette
    unità indipendenti in un ordine diverso da un'esecuzione all'altra, quindi il
    test di compatibilità confronta l'insieme dei messaggi byte per byte, non la loro
    sequenza.

11. **Veloce per impostazione predefinita (revisione della specifica del 2026-10-11).** Nel valore
    predefinito `mode = "fast"`, QuickRust cambia ciò che Cargo upstream compilerebbe in modi che
    mantengono identico il comportamento del programma: le impostazioni delle informazioni di debug,
    il frontend parallelo di rustc per le unità compilate da zero, una cache degli artefatti globale
    e il proprio output sul terminale. `mode = "compat"` produce esattamente le build di upstream; i
    test dell'oracolo byte per byte del corpus di compatibilità girano in modalità compat, e la fast
    mode ha test propri (le impostazioni cedono il passo a quelle dell'utente, il frontend parallelo
    non raggiunge mai una ricompilazione incrementale, `#![feature]` resta rifiutato su un compilatore
    stabile, la cache ripristina correttamente e non
    serve mai un sorgente modificato, la fast mode e upstream mantengono artefatti separati invece
    di ricompilarsi a vicenda). Ogni leva viene misurata dallo speed index e verificata
    con le suite di test dei carichi di lavoro dei benchmark prima di diventare predefinita.

12. **`RUSTC_BOOTSTRAP` per le leve `-Z`.** Il compilatore stabile contiene il frontend
    parallelo e la selezione del linker che QuickRust usa dietro `-Z`. Il wrapper di QuickRust imposta
    `RUSTC_BOOTSTRAP=1` per i processi del compilatore delle unità pianificate da Cargo, con
    `-Zallow-features=` in modo che `#![feature]` venga rifiutato come sul canale stabile (con l'errore
    "not in the list of allowed features" di rustc invece di quello del canale stabile).
    Mai per gli script di build o per le sonde del compilatore che eseguono, così i crate non passano
    a percorsi di codice nightly. Un compilatore nightly accetta comunque i flag `-Z` e `#![feature]`,
    quindi lì non viene aggiunto alcun `-Zallow-features=`. Nessuna di queste leve si applica quando
    l'utente imposta `RUSTC_BOOTSTRAP` da sé.

13. **Il driver del compilatore di QuickRust compila le unità pianificate da Cargo (fast mode, `fast.early-cutoff`).**
    Il driver è il rustc della toolchain stessa (`librustc_driver`, il suo LLVM, la sua release) eseguito
    tramite le callback di QuickRust, compilato una volta per toolchain contro il suo componente
    `rustc-dev`. Senza callback attive i suoi output sono byte per byte quelli di rustc (test
    `test0276_quickrusts_compiler_driver_builds_byte_for_byte_what_rustc_builds`). In fast mode tre
    cose differiscono dalle build di upstream, nessuna nel comportamento del programma:
    l'hash del crate di una libreria (SVH) viene calcolato a partire da ciò che i suoi dipendenti
    possono osservare, quindi i metadati e le rlib delle librerie differiscono da quelli di rustc
    (l'hash è un identificatore; nulla lo confronta tra compilatori diversi); gli output vengono
    collegati prima che rustc salvi il suo stato incrementale, che completa dopo che Cargo è andato
    avanti; e i file oggetto dei binari vengono conservati (hard link accanto all'output) in modo che
    un binario il cui codice non è cambiato venga ricollegato invece che ricompilato. Le unità
    ripristinate e ricollegate vengono verificate confrontandole con la compilazione da zero dello
    stesso sorgente tramite `qrust-bench cutoff` (modifiche casuali sui carichi di lavoro dei
    benchmark, byte per byte; registrate in `benchmarks/cutoff-gate.jsonl`). I programmi che Cargo
    esegue al posto di rustc (clippy-driver come wrapper del workspace, il wrapper di un utente)
    girano così come sono; un crate su cui il driver fallisce viene compilato da rustc.

14. **Nessuna `target/` nel progetto (decisione del 2026-10-12).** Cargo upstream compila in
    `<workspace>/target` se non gli viene detto altrimenti; QuickRust glielo dice per ogni comando:
    ogni workspace (come lo nomina `cargo locate-project --workspace`) compila in
    `~/.qrust/cache/builds/<name>-<id>/`, passata a Cargo e a ogni strumento sotto di esso tramite
    `CARGO_TARGET_DIR` e `CARGO_BUILD_BUILD_DIR`, così anche `cargo metadata` la riporta. Una
    directory di target scelta dall'utente (`--target-dir`, `--config build.target-dir`,
    `CARGO_TARGET_DIR`/`CARGO_BUILD_TARGET_DIR`) viene usata così com'è; `build.target-dir` in un
    file di configurazione di Cargo viene sovrascritto. Gli script che codificano `./target/…`
    devono invece chiedere a `qrust path`. Test: `test0239_builds_go_to_quickrusts_cache_and_never_into_the_project`,
    `test0241_a_target_directory_the_user_chose_is_used…`, `test0242_a_build_script_building_another_workspace…`.

## Decisioni aperte della specifica, come risolte qui {#open-decisions-of-the-specification-as-resolved-here}

| | Decisione |
|---|---|
| OQ-001 Rust/Cargo minimi | QuickRust dichiara `rust-version = 1.80`, compilato e testato esattamente con 1.80 in CI a partire dal lockfile (`cargo install --locked`); delega a qualunque Cargo sia selezionato e richiede solo il formato 1 di `cargo metadata` e `RUSTC_WRAPPER` |
| OQ-002 piattaforme | macOS e Linux bloccanti per la release; le suite passano su macOS e Linux x86_64 in CI; Windows ha un'esecuzione smoke in CI, le suite sono solo Unix |
| OQ-003 servizio centrale | nessuno; tutti i contratti sono file locali |
| OQ-004 provider della cache | quello con cui è configurato sccache |
| OQ-005 fail-open o fail-closed | `cache.fail`, predefinito `open` |
| OQ-006 fiducia negli interventi correttivi | solo la configurazione di QuickRust stessa è `safe` |
| OQ-007, OQ-008 | misurati e riportati; nessuna emulazione delle funzionalità di upstream |
