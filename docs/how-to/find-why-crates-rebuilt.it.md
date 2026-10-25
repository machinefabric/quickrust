# Come scoprire perché i crate sono stati ricompilati

Questa guida mostra come scoprire perché una build ha compilato crate che ti aspettavi aggiornati, e che cosa fare per le cause più comuni.

## Chiedi a QuickRust dell'ultima build {#ask-quickrust-about-the-latest-build}

Subito dopo la build che ti ha sorpreso:

```bash
qrust analyze rebuilds
```

```text
Rebuild reasons
  REASON  UNITS  TIME
  source  1      42ms
  previously known units rebuilt: 1 of 1 in the index (ratio 1.0)
  UNIT        KIND  REASON  COST  CONFIDENCE  DETAIL
  demo@0.1.0  bin   source  42ms  high        timestamp-only change (content identical): src/main.rs
```

Ogni unità ricompilata ha un motivo, il tempo che è costata e quanto QuickRust ne è sicuro. In una build grande, restringi l'elenco:

```bash
qrust analyze rebuilds --filter reason=environment
qrust analyze rebuilds --filter package=serde --sort cost_ns:desc --limit 10
qrust analyze rebuilds --format json
```

Per esaminare una build precedente, aggiungi `--run latest~1` (o un id di esecuzione da `qrust runs`).

## Agisci in base al motivo {#act-on-the-reason}

| Motivo | Che cosa è cambiato | Che cosa fare |
|---|---|---|
| `new` | QuickRust non ha mai visto questa unità prima | Nulla: è la sua prima build. |
| `source` | un file letto dal compilatore; `timestamp-only change (content identical)` significa che è cambiata solo la sua data di modifica | Previsto dopo una modifica. Se non l'hai modificato tu, scopri che cosa lo tocca (un generatore di codice, un `git checkout`, un formatter che riscrive file invariati). |
| `dependency` | una dipendenza è stata ricompilata | Cerca il motivo della dipendenza stessa: risolvi quello e questo scompare. |
| `build-script-input` | lo script di build del pacchetto è stato eseguito di nuovo | Controlla che cosa dichiara lo script con `cargo::rerun-if-changed` / `rerun-if-env-changed` (`qrust analyze scripts`); uno script che non dichiara nulla viene rieseguito ogni volta che cambia un qualsiasi file del suo pacchetto. |
| `environment` | una variabile d'ambiente da cui l'unità dipende | Mantieni quella variabile uguale tra una build e l'altra; editor e terminali spesso differiscono (`PATH`, `RUSTFLAGS`, variabili specifiche di uno strumento). |
| `rustflags` | `RUSTFLAGS` o i rustflags del target | Usa gli stessi flag in ogni punto da cui compili, oppure dai allo strumento che differisce una propria directory target. |
| `feature-set` | il pacchetto è stato compilato con altre feature | Compilare singoli pacchetti (`-p`) e compilare l'intero workspace risolvono le feature in modo diverso; `qrust analyze duplication` elenca le varianti e chi le richiede. |
| `profile` | le impostazioni del profilo | Di solito è l'alternanza tra comandi o modalità con impostazioni diverse, per esempio tra fast mode e compat mode. |
| `compiler` | la toolchain | Il canale è passato a una nuova release (vedi più sotto), oppure `+toolchain` / un file di toolchain ne ha selezionata un'altra. |
| `target` | il target di compilazione | Previsto quando compili per un altro `--target`. |
| `cache-miss` | nulla: esiste un'unità identica, ma non in questa directory di build, oppure `cargo clean` l'ha rimossa | Normale per un nuovo checkout o dopo una pulizia; la cache degli artefatti di QuickRust ripristina la maggior parte di queste unità. |
| `unknown` | QuickRust non ha potuto vedere che cosa è cambiato | Vedi "Quando il motivo è sconosciuto" più sotto. |

## Confronta due build {#compare-two-builds}

```bash
qrust compare latest~1 latest
```

stampa le metriche delle due esecuzioni affiancate e ciò che differisce tra i loro ambienti (toolchain, lockfile, impostazioni). Una differenza elencata lì come `notable` è spesso la causa.

## Ricompilazioni dopo il passaggio tra QuickRust e Cargo puro {#rebuilds-after-switching-between-quickrust-and-plain-cargo}

Cargo puro e QuickRust mantengono directory di build separate, quindi non ricompilano mai l'output l'uno dell'altro. Se condividi un'unica directory tra i due, gli script di build che dichiarano `cargo::rerun-if-env-changed=RUSTC_WRAPPER` vengono rieseguiti a ogni passaggio; [Come provare QuickRust prima di farne il tuo cargo](adopt-gradually.md#if-build-scripts-rerun-when-you-switch) mostra come evitarlo.

## Ricompilazioni dopo un aggiornamento della toolchain {#rebuilds-after-a-toolchain-update}

QuickRust aggiorna in background i canali come `stable`, e la prima build sulla nuova release compila di nuovo tutto (motivo `compiler`). Se preferisci scegliere tu quando accade, fissa la release in `rust-toolchain.toml` oppure disattiva gli aggiornamenti in background; vedi [Come scegliere e gestire le toolchain](choose-a-toolchain.md#keep-toolchains-current).

## Unità compilate due volte in altri worktree {#units-built-twice-in-other-worktrees}

```bash
qrust analyze worktrees
```

elenca gli altri checkout e workspace che QuickRust conosce, e quante unità identiche ciascuno ha compilato che ha compilato anche questo.

## Quando il motivo è sconosciuto {#when-the-reason-is-unknown}

`unknown` significa che l'identità dell'unità è cambiata senza che cambiasse alcun componente che QuickRust può osservare, oppure che è stata ricompilata con un'identità invariata (input che rustc non elenca, o artefatti rimossi al di fuori di Cargo). Per vedere che cosa ne pensa Cargo stesso, esegui la stessa build con il log dei fingerprint di Cargo:

```bash
CARGO_LOG=cargo::core::compiler::fingerprint=info cargo build 2>&1 | grep -i dirty
```

## Vedi anche {#related}

- [Schema del report](../reference/report-schema.md) per i record `rebuilds`
- [Misurare le build](../explanation/measurement.md) per ciò che QuickRust può e non può osservare
