# Schema del report di esecuzione, versione 1.0

Un documento JSON per ogni esecuzione registrata, prodotto da `QRUST_REPORT=<path> cargo …`,
`measurement.report` o `qrust report --format json`.

Regole (API-001 … API-004):

- `schema_version` è sempre presente. I consumatori devono ignorare i campi che non
  conoscono all'interno della stessa versione major; una modifica incompatibile incrementa la major.
- **`null` significa non disponibile, mai zero.** Il motivo si trova nell'oggetto
  `unavailable` di livello superiore (indicizzato per percorso del campo) o in un oggetto
  `unavailable` accanto al campo.
- Le durate sono in nanosecondi (`*_ns`), le dimensioni in byte (`*_bytes`); gli istanti
  dell'orologio di sistema sono stringhe RFC 3339 in UTC (`*_at`) con il valore grezzo in `*_wall_ns`.
- I documenti esportati contengono percorsi tokenizzati (`$WORKSPACE`, `$HOME`,
  `$CARGO_HOME`, `$QRUST_HOME`).
- Un report memorizzato è immutabile.

## Livello superiore {#top-level}

| Campo | Contenuto |
|---|---|
| `schema_version` | `"1.0"` |
| `run` | identità, comando, risultato |
| `workspace` | membri, digest, directory, stato dei sorgenti |
| `toolchain` | identità di Cargo e rustc, host, funzionalità |
| `environment` | `machine` (sistema operativo, CPU, memoria, filesystem, `class`) e `variables` (in allowlist, oscurate) |
| `policy` | modalità, le impostazioni fast applicate e quelle lasciate stare, classe di carico di lavoro, decisione sull'incrementale, stato della cache |
| `metrics` | misurazioni scalari |
| `cache` | statistiche di sccache per l'esecuzione, e che cosa ha fatto la cache degli artefatti di QuickRust (`artifacts`) |
| `critical_path`, `critical_path_model` | la catena di dipendenze più lunga e quanto è affidabile |
| `units`, `edges` | il DAG di build osservato |
| `rebuilds` | perché è stata eseguita ogni unità compilata |
| `duplication` | versioni multiple (`versions[]`: `state` `necessary` per versioni incompatibili secondo SemVer, `separate-sources` per quelle compatibili, che Cargo tiene separate solo perché provengono da sorgenti diverse) e varianti di build |
| `build_scripts`, `proc_macros`, `codegen` | analisi per singolo aspetto |
| `artifacts` | inventario delle directory di build e piano di GC, una voce per directory |
| `recommendations` | risultati ordinati per rilevanza |
| `budget` | valutazione della politica, quando ne è stata eseguita una |
| `unavailable` | percorso del campo → motivo |

## `run` {#run}

`id` (26 caratteri, ordinabile nel tempo), `parent_session_id`, `state`
(`completed` \| `failed` \| `cancelled`), `started_at`, `started_wall_ns`,
`duration_ns`, `command` { `family`, `entry` (`cargo-shim` \| `qrust`),
`argv_redacted`, `cwd_token`, `profile`, `targets` }, `result`
{ `cargo_exit_code`, `signal`, `policy` (`not-evaluated` a meno che i budget non siano stati
applicati a questa build) }, `scenario` (solo benchmark),
`upstream` { `path`, `canonical`, `rule` }, `wrappers` { `qrust`, `inner`,
`inner_source`, `workspace_wrapper`, `order` }, `overhead`
{ `preflight_ns`, `post_ns` }, `notes`.

## `metrics` {#metrics}

`wall_time_ns`, `critical_path_ns`, `sum_unit_time_ns`,
`build_script_run_time_ns`, `average_parallelism`, `max_concurrent_compilers`,
`cpu_time_ns`, `peak_rss_bytes`, `units_observed`, `units_compiled`,
`units_restored`, `units_failed`, `units_fresh`, `units_fresh_referenced`, `build_scripts_run`,
`probe_invocations`, `build_script_compiler_invocations`,
`build_script_compiler_time_ns`, `output_bytes_written`, `artifact_logical_bytes`,
`artifact_allocated_bytes`.

`cache.artifacts` fornisce `restored`, `stored`, `missed`, `not_cacheable`, `restore_time_ns` e
`restored_with_foreign_out_dir`: le unità ripristinate il cui codice generato dallo script di build
è stato compilato sotto un'altra directory di target, che le loro posizioni di panic e le loro
informazioni di debug nominano.

`units_compiled` conta le unità compilate da rustc; `units_restored` le unità che la cache degli
artefatti di QuickRust ha invece ripristinato (Cargo le ha richieste; non è stato compilato nulla).
Il tempo di compilazione e le metriche di CPU riguardano solo le unità compilate.
`units_fresh` è `null` a meno che non sia stato osservato lo stream dei messaggi JSON di Cargo (lo è
quando QuickRust ha presentato l'output della build, o quando l'utente ha richiesto JSON su una pipe).
`units_fresh_referenced` conta gli artefatti con cui le unità compilate hanno effettuato il link
senza che venissero ricompilati. `build_script_compiler_*` riguarda i processi rustc
che gli script di build avviano da sé per sondare la toolchain; sono un costo reale
ma non unità di build, e si distinguono per l'assenza del `-C metadata`
di Cargo.

## `units[]` {#units}

`id` (`unit:<crate>-<cargo hash>`), `stem`, `package`, `version`, `source`
(`path` \| `registry` \| `git` \| `unknown`), `crate_name`, `kind` (`lib`, `bin`,
`test`, `example`, `proc-macro`, `build-script`, `build-script-run`, `cdylib`,
…), `mode` (`check` \| `build` \| `test` \| `run`), `target`, `role` (`host` \|
`target` \| `unknown`), `primary`, `features`, `freshness` (`compiled` \|
`restored` \| `failed` \| `fresh` \| `ran`), `start_wall_ns`, `duration_ns`,
`rmeta_ready_ns`, `cpu_ns`, `max_rss_bytes`, `output_bytes`, `incremental`,
`links`, `codegen` (le opzioni `-C` ricevute da rustc), `linker`, `cacheability` (le classi
di sccache), `artifact_cache` (che cosa ha fatto la cache degli artefatti di QuickRust: `hit`, `stored`, `miss`, `off`; null
per le unità aggiornate),
`on_critical_path`, `fingerprint` (digest dei componenti: `compiler`, `target`,
`profile`, `features`, `rustflags`, `environment`, `source`, `generated`,
`dependencies`), `exact_id`, `portable_id`, `logical_key`.

Le unità `fresh` non hanno tempi: non sono state compilate in questa esecuzione. La durata di
un'unità `restored` è il tempo impiegato per ripristinarla.

## `edges[]` {#edges}

`from`, `to` (id delle unità, produttore → consumatore), `kind` (`metadata` \| `link`),
`confidence`, `evidence`.

| Confidenza | Significato |
|---|---|
| `observed` | un argomento `--extern` effettivamente ricevuto da rustc |
| `inferred` | una relazione con uno script di build ricostruita dall'identità del pacchetto e da `OUT_DIR` |
| `modeled` | (solo percorso critico) un'unità che esegue il link in attesa di una dipendenza transitiva |

## `critical_path[]`, `critical_path_model` {#critical_path-critical_path_model}

Ogni passo: `unit_id`, `package`, `kind`, `freshness`, `duration_ns`,
`edge_confidence`. Il modello ha `completeness` (`observed` \| `partial`),
`partial_reasons`, `method`.

## `rebuilds` {#rebuilds}

`summary` { `units_compiled`, `previously_known`, `known_units_in_index`,
`invalidated_ratio`, `rebuild_time_of_known_units_ns`, `by_reason` } e
`units[]` { `unit_id`, `package`, `kind`, `mode`, `reason`, `detail`,
`differing`, `evidence`, `confidence`, `cost_ns` }.

`reason` è uno tra `source`, `dependency`, `compiler`, `profile`,
`feature-set`, `target`, `rustflags`, `build-script-input`, `environment`,
`generated-input`, `cache-miss`, `unknown`, oppure `new` (prima osservazione; non è una
ricompilazione).

## `cache` {#cache}

`provider`, `state` (`disabled` \| `local` \| `degraded` \| `conflict`),
`requests`, `cacheable_requests`, `hits`, `misses`, `non_cacheable`,
`non_cacheable_reasons`, `errors`, `timeouts`, `writes`, `read_bytes` (sempre
`null`: sccache non lo riporta), `write_bytes`, `lookup_time_ns`,
`write_time_ns`, `units` { `cacheable`, `non_cacheable_linking`,
`non_cacheable_incremental` }, `scope` — tutti relativi a sccache, `null` con un motivo quando
non ce n'è uno attivo.

`artifacts` descrive la cache degli artefatti di QuickRust in questa esecuzione: `state` (`on`, `off` oppure
`off: a rustc wrapper is configured`), `restored`, `stored`, `missed` (cercate e non
trovate, poi compilate senza essere memorizzate, per esempio un binario del workspace collegato rapidamente),
`not_cacheable` (unità che la cache non accetta: quelle che collegano librerie native, quelle a cui
viene passato un file di argomenti, o i binari delle dipendenze diversi dagli script di build), `restore_time_ns`.

## `policy` {#policy}

- `fast` { `mode` (`fast` \| `compat`), `applied` [ { `key`, `value`, `why` } ]: le
  impostazioni di profilo che QuickRust ha passato a Cargo, il frontend parallelo (`rustc -Zthreads`) e
  il linker (`linker`, quando viene usato wild),
  `yielded` [ { `key`, `because` } ]: le impostazioni che QuickRust ha lasciato stare perché le ha
  fatte l'utente, indicando dove }
- `workload` (`local-dev` \| `ci`)
- `incremental`: impostazione, decisione, motivazione, unità incrementali osservate, conflitto
  con sccache
- `cache`: politica e stato del provider

## `artifacts[]` {#artifacts}

`root`, `files`, `logical_bytes`, `allocated_bytes`,
`hardlink_duplicate_bytes_excluded`, `shared_outside_bytes` (byte allocati di file che hanno anche
nomi al di fuori della directory, come la cache degli artefatti di QuickRust: eliminarli qui non libera
nulla, e `candidate_bytes` li esclude), `symlinks_not_followed`, `complete`,
`errors`, `categories` (per `final`, `dependency`, `metadata`, `incremental`,
`debug`, `relink` (ciò che il driver del compilatore di QuickRust conserva di un binario per ricollegarlo: il suo
`.rlink`, gli oggetti e il record), `build-script-binary`, `build-script-output`, `proc-macro`,
`fingerprint`, `documentation`, `package-output`, `unknown-generated`),
`reclaimability` (per `required`, `reusable`, `rebuildable`, `uncertain`),
`active`, `gc_plan` { `mode` (`dry-run`), `stale_days`, `candidate_groups`,
`candidate_bytes`, `reclaimable_now_bytes`, `blocked_by`,
`regeneration_estimate`, `size_budget`, `candidates[]` (ciascuno con i suoi
`shared_outside_bytes`) }.

## `recommendations[]` {#recommendations}

`id`, `rule`, `category`, `title`, `status`, `evidence[]`,
`impacted_scenarios[]`, `confidence`, `risk`, `effort`, `impact`
{ `kind` (`measured` \| `modeled` \| `unknown`), `avoidable_time_ns`?,
`reclaimable_bytes`?, `basis` }, `verification`, `compatibility`,
`remediation` (`safe` \| `review_required` \| `none`), `alternatives[]`
(opzioni mutuamente esclusive), `score`.

Un guadagno numerico è presente solo quando `impact.kind` è `measured` o `modeled`.

## `budget` {#budget}

`result` (`pass` \| `warn` \| `fail` \| `incomparable` \| `not-evaluated` \|
`error`), `mode`, `baseline`, `comparability` { `state`, `differences[]` },
`evaluations[]` { `metric`, `unit`, `scenario`, `result`, `checks[]` }.

## Altri documenti {#other-documents}

`qrust compare --format json`, `qrust benchmark --format json`,
`qrust experiment --format json`, `qrust inventory --format json`,
`qrust gc --dry-run --format json`, `qrust analyze worktrees --format json` e
`qrust fix --dry-run --format json` seguono le stesse convenzioni e contengono
`schema_version` dove vengono memorizzati.
