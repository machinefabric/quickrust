# Budget

Il file dei budget, la sua policy e le sue soglie, le metriche che un budget può indicare e come un budget viene valutato rispetto a una baseline.

I budget vengono valutati da `qrust ci evaluate` e, quando `ci.enforce-on-build = true`, dopo ogni comando di build riuscito ([Comandi](commands.md#budgets)). Guida: [Come configurare QuickRust in CI](../how-to/set-up-ci.md).

## File {#file}

`.qrust/budgets.toml` nella radice del workspace, oppure il percorso indicato in `ci.budget-policy` (relativo alla radice del workspace), oppure il file passato con `qrust ci evaluate --budgets FILE` (relativo alla directory corrente).

```toml
[policy]
mode = "warn"            # warn | blocking
incomparable = "warn"    # warn | fail
noise-percent = 3

[[budget]]
metric = "wall_time_ns"
scenario = "clean"
max-increase-percent = 10

[[budget]]
metric = "critical_path_ns"
max-increase-percent = 15

[[budget]]
metric = "artifact_allocated_bytes"
max-value = "20GiB"

[[budget]]
metric = "cache_hit_ratio"
min-value = 0.6
```

Tabelle, chiavi e metriche sconosciute sono errori, così come un budget che non imposta alcuna soglia.

### `[policy]` {#policy}

| Chiave | Valori | Predefinito | Significato |
|---|---|---|---|
| `mode` | `warn`, `blocking` | `warn` | se un budget non rispettato fa fallire il comando (stato di uscita 3) o produce solo un avviso |
| `incomparable` | `warn`, `fail` | `warn` | se un risultato non confrontabile fa fallire il comando (stato di uscita 4) |
| `noise-percent` | un numero non inferiore a 0 | `0` | una variazione relativa di entità non superiore a questo valore conta come nessuna variazione |

### `[[budget]]` {#budget}

| Chiave | Valore | Significato |
|---|---|---|
| `metric` | il nome di una metrica (obbligatorio) | che cosa limita il budget |
| `scenario` | il nome di uno scenario | il budget si applica solo a un candidato di questo scenario |
| `max-value` | un numero, o una stringa con un'unità | il valore del candidato non deve superarlo |
| `min-value` | un numero, o una stringa con un'unità | il valore del candidato non deve scendere al di sotto |
| `max-increase-percent` | un numero | l'aumento rispetto alla baseline non deve superare questa percentuale |
| `max-decrease-percent` | un numero | la diminuzione rispetto alla baseline non deve superare questa percentuale |

Una soglia espressa come stringa prende l'unità della metrica: una durata per le metriche in `ns` (`500ms`, `1.5s`, `2m`, `1h`, `100ns`, o un numero semplice di nanosecondi), una dimensione per le metriche in `bytes` (`20GiB`, `500MB`; potenze di 1024). `max-increase-percent` e `max-decrease-percent` richiedono una baseline.

## Metriche {#metrics}

| Metrica | Unità |
|---|---|
| `wall_time_ns` | ns |
| `critical_path_ns` | ns |
| `sum_unit_time_ns` | ns |
| `cpu_time_ns` | ns |
| `peak_rss_bytes` | byte |
| `units_compiled` | conteggio |
| `units_restored` | conteggio |
| `artifact_allocated_bytes` | byte |
| `artifact_logical_bytes` | byte |
| `output_bytes_written` | byte |
| `cache_hits` | conteggio |
| `cache_misses` | conteggio |
| `cache_non_cacheable` | conteggio |
| `cache_hit_ratio` | rapporto |
| `cache_lookup_time_ns` | ns |

Che cosa misura ogni metrica: [Schema del report di esecuzione](report-schema.md#metrics).

## Baseline {#baselines}

Una baseline è il riepilogo di un'esecuzione o di un benchmark scritto da `qrust baseline set NAME` in `<workspace>/.qrust/baselines/NAME.json`. I budget vengono valutati rispetto alla baseline indicata da `--baseline` o da `ci.baseline` (predefinita `main`). Una baseline mancante è un errore solo quando un budget è relativo.

## Confrontabilità {#comparability}

Prima di valutare una soglia relativa, baseline e candidato vengono confrontati. Una differenza in uno qualsiasi di questi campi li rende non confrontabili, e il risultato di ogni soglia relativa diventa `incomparable`:

| Campo | Differenza |
|---|---|
| `environment.rustc_version` | versione del compilatore |
| `environment.host` | tripla dell'host |
| `environment.machine_class` | classe dell'hardware |
| `environment.command_family` | comando |
| `environment.profile` | profilo |
| `environment.targets` | target di compilazione |
| `scenario.name` | scenario |
| `scenario.cache_state` | stato della cache (fredda o calda) |

Una differenza in uno qualsiasi di questi campi li rende parzialmente confrontabili; le soglie vengono comunque valutate e le differenze vengono elencate:

| Campo | Differenza |
|---|---|
| `environment.cargo_version` | versione di Cargo |
| `environment.variables_digest` | variabili d'ambiente rilevanti per la build |
| `environment.cache_provider` | cache del compilatore |
| `environment.source_revision` | revisione dei sorgenti |
| `environment.dirty` | presenza di modifiche non salvate nel working tree |
| `environment.lockfile_digest` | lockfile |

## Risultati {#results}

Ogni verifica di soglia dà `pass`, `fail`, `incomparable` (soglia relativa senza una baseline confrontabile) o `unavailable` (la metrica non è disponibile nel candidato o nella baseline). Il risultato di un budget è la sua verifica peggiore, nell'ordine `fail`, `incomparable`, `unavailable`, `pass`.

| Risultato complessivo | Quando | Stato di uscita di `qrust ci evaluate` |
|---|---|---|
| `fail` | un budget è stato violato e `mode = "blocking"` | 3 |
| `warn` | un budget è stato violato e `mode = "warn"` | 0 |
| `incomparable` | nessun budget è stato violato e un budget non era confrontabile | 4 con `incomparable = "fail"`, altrimenti 0 |
| `pass` | in tutti gli altri casi | 0 |

`--format json` stampa il risultato con ogni valutazione, verifica e baseline e con le differenze di confrontabilità.

### Durante una build {#on-a-build}

Con `ci.enforce-on-build = true`, ogni comando di build riuscito valuta i budget rispetto alla propria esecuzione: un `fail` stampa `FAIL budget: ...` su stderr ed esce con 3, un `warn` stampa `WARN budget: ...`, un `incomparable` stampa `WARN budget incomparable: ...` (ed esce con 4 con `incomparable = "fail"`). Budget mancanti, o una baseline mancante di cui un budget ha bisogno, stampano un `FAIL` ed escono con 2. Il risultato viene salvato nel campo `budget` del report dell'esecuzione. Lo stdout di Cargo non viene toccato.
