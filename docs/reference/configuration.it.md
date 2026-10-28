# Configurazione

Le impostazioni di QuickRust, da dove vengono lette, ogni chiave con i suoi valori e il suo default, e come viene selezionata la toolchain di un comando.

QuickRust non legge mai la configurazione di Cargo (`.cargo/config.toml`) per le proprie impostazioni, e Cargo non legge mai quella di QuickRust. `qrust policy explain` stampa ogni chiave con il suo valore effettivo, la variabile d'ambiente e l'origine; `--format json` stampa le stesse informazioni in JSON.

## Fonti e precedenza {#sources-and-precedence}

Le fonti successive prevalgono su quelle precedenti, chiave per chiave:

| Ordine | Fonte |
|---|---|
| 1 | default incorporati |
| 2 | `~/.qrust/config.toml` (`$QRUST_HOME/config.toml`): quello dell'utente |
| 3 | `.qrust/config.toml` nella directory in cui viene eseguito il comando o nella directory più vicina sopra di essa: quello del repository |
| 4 | la variabile d'ambiente `QRUST_*` della chiave |

Una variabile d'ambiente vuota conta come non impostata. Un file di configurazione che non esiste conta come vuoto.

## Formato del file {#file-format}

TOML. `output` e `mode` sono chiavi di primo livello; ogni altra chiave è `name` dentro la tabella che ha come nome la sua prima parte:

```toml
mode = "fast"

[cache]
provider = "sccache"
max-size = "40GiB"

[measurement]
report = ".qrust/runs/{session}.json"
```

## Validazione {#validation}

Una configurazione non valida ferma ogni comando tranne `qrust upstream`, con exit status 2 e un messaggio che indica la chiave e la sua fonte:

- una chiave che non esiste (`unknown configuration key `cache.providr` in <file>`);
- un valore al di fuori del tipo o dei valori ammessi per la chiave (`invalid value "maybe" for `cache.fail` from <file>: expected one of open, closed`);
- un valore del tipo TOML sbagliato;
- TOML non valido;
- qualsiasi variabile d'ambiente il cui nome inizia con `QRUST_` e che non è la variabile di una chiave né una delle variabili interne di QuickRust ([Ambiente](environment.md#internal-variables)): `unknown configuration variable `QRUST_...``.

## Tipi di valore {#value-types}

| Tipo | Valori accettati |
|---|---|
| enum | esattamente una delle stringhe elencate |
| bool | `true`, `false`; da una stringa o dall'ambiente anche `1`/`0`, `on`/`off`, `yes`/`no` (maiuscole o minuscole) |
| integer | un intero non negativo |
| string | qualsiasi stringa |
| space | `auto`; una dimensione (`40GiB`, `500MB`, `1024`; unità `B`, `K`/`KB`/`KiB`, `M`/`MB`/`MiB`, `G`/`GB`/`GiB`, `T`/`TB`/`TiB`, tutte potenze di 1024; maggiore di zero); oppure una quota del disco (`10%`, maggiore di 0 e al massimo 100) |
| threads | `auto`, `off`, oppure un numero non inferiore a 2 |

## Chiavi {#keys}

| Chiave | Tipo e valori | Default | Variabile d'ambiente | Significato |
|---|---|---|---|---|
| `mode` | `fast`, `compat` | `fast` | `QRUST_MODE` | `fast`: si applicano le impostazioni della [Fast mode](fast-mode.md); `compat`: esattamente ciò che compila il Cargo della toolchain |
| `output` | `qrust`, `cargo` | `qrust` | `QRUST_OUTPUT` | chi presenta l'output di un comando in un terminale ([Output](output.md)). In compat mode il default è `cargo`; un `output` impostato esplicitamente vale in entrambe le modalità |
| `compatibility.nested-cargo` | `orchestrator`, `upstream` | `orchestrator` | `QRUST_NESTED` | un Cargo avviato dentro una sessione QuickRust (il `cargo` o `$CARGO` di uno script di build o di un sottocomando): una sessione QuickRust propria collegata a quella esterna, oppure direttamente il Cargo della toolchain |
| `compatibility.unknown-command` | `delegate`, `instrument` | `delegate` | `QRUST_COMPATIBILITY_UNKNOWN_COMMAND` | sottocomandi esterni: delegati, oppure delegati con il wrapper di rustc che ne osserva le compilazioni |
| `measurement.enabled` | bool | `true` | `QRUST_MEASUREMENT_ENABLED` | osserva le invocazioni di rustc attraverso `qrust-rustc-wrapper`; `false` disattiva anche tutto ciò che il wrapper applica (cache degli artefatti, frontend parallelo, linker, early cut-off) |
| `measurement.report` | string | vuoto | `QRUST_REPORT` | scrive il report di ogni build in questo percorso (relativo alla directory corrente); `{session}` si espande nell'id dell'esecuzione |
| `measurement.finalize` | `lazy`, `sync` | `lazy` | `QRUST_MEASUREMENT_FINALIZE` | analizza un'esecuzione la prima volta che viene richiesta, oppure subito dopo ogni build |
| `measurement.cargo-json` | `auto`, `off` | `auto` | `QRUST_MEASUREMENT_CARGO_JSON` | legge i messaggi JSON di Cargo quando l'utente li ha richiesti su uno stdout che non è un terminale |
| `measurement.strict` | bool | `false` | `QRUST_MEASUREMENT_STRICT` | un'esecuzione che non è stato possibile registrare o riportare fa terminare con exit 2 un comando altrimenti riuscito |
| `measurement.keep-runs` | integer | `200` | `QRUST_MEASUREMENT_KEEP_RUNS` | esecuzioni registrate conservate per workspace |
| `measurement.keep-raw-events` | integer | `20` | `QRUST_MEASUREMENT_KEEP_RAW_EVENTS` | esecuzioni analizzate per workspace di cui si conservano gli eventi grezzi di rustc e l'output memorizzato |
| `incremental.local-dev` | `cargo-default`, `on`, `off`, `experiment` | `cargo-default` | `QRUST_INCREMENTAL_LOCAL_DEV` | compilazione incrementale fuori dalla CI: decidono i profili di Cargo, `CARGO_INCREMENTAL=1`, `CARGO_INCREMENTAL=0`, oppure decidono i profili di Cargo (con un rimando a `qrust experiment`) |
| `incremental.ci` | come sopra | `cargo-default` | `QRUST_INCREMENTAL_CI` | lo stesso nella CI |
| `cache.provider` | `inherit`, `none`, `sccache` | `inherit` | `QRUST_CACHE_PROVIDER` | cache del compilatore: osserva il `RUSTC_WRAPPER` configurato, non ne aggiunge nessuna, oppure concatena `sccache` (dal `PATH`) dentro il wrapper di QuickRust quando non è configurato alcun wrapper |
| `cache.fail` | `open`, `closed` | `open` | `QRUST_CACHE_FAIL` | quando uno sccache concatenato non è disponibile: compila senza, oppure fallisce prima di compilare |
| `cache.artifacts` | `on`, `off` | `on` | `QRUST_CACHE_ARTIFACTS` | fast mode: la cache degli artefatti di QuickRust |
| `cache.artifacts-min-link-ms` | integer (millisecondi) | `300` | `QRUST_CACHE_ARTIFACTS_MIN_LINK_MS` | gli output linkati del workspace (binari, test, proc-macro) che sono stati compilati in meno di questo tempo non vengono memorizzati nella cache degli artefatti |
| `build.variant` | string | vuoto | `QRUST_BUILD_VARIANT` | un nome per questa configurazione del workspace: compila in una directory propria, `builds/<name>-<id>.<variant>`, così che le configurazioni di uno stesso workspace tengano separati i loro output ([Come trovare gli output di build](../how-to/find-build-outputs.md#build-one-workspace-in-several-configurations)). Al massimo 32 lettere, cifre, `-` o `_`; vuoto indica l'unica directory di build del workspace. Nessun effetto su una directory target scelta da te |
| `cache.max-size` | space | `auto` | `QRUST_CACHE_MAX_SIZE` | lo spazio massimo che la cache di QuickRust usa; `auto` è il 10% del disco, tra 10 GiB e 100 GiB |
| `cache.min-free` | space | `auto` | `QRUST_CACHE_MIN_FREE` | lo spazio libero su disco che la cache di QuickRust lascia: sotto di esso, la cache restituisce quanto manca al disco più un quarto di questo valore, a partire da ciò che è stato usato meno di recente, contando ciò che ha rimosso e che il disco non mostra ancora come libero ([Raccolta](files.md#collection)); `auto` è il 5% del disco, tra 2 GiB e 20 GiB |
| `fast.frontend-threads` | threads | `auto` | `QRUST_FAST_FRONTEND_THREADS` | fast mode: il frontend parallelo di rustc; `auto` è il numero di core, al massimo 8, sulle macchine con 4 o più core, e disattivato sotto i 4 |
| `fast.early-cutoff` | `auto`, `off` | `auto` | `QRUST_EARLY_CUTOFF` | fast mode con la cache degli artefatti: compila con il driver del compilatore di QuickRust |
| `fast.linker` | `auto`, `off` | `auto` | `QRUST_FAST_LINKER` | fast mode su un host Linux glibc: linka con `wild` quando è installato |
| `artifacts.inventory` | bool | `true` | `QRUST_ARTIFACTS_INVENTORY` | include un inventario della directory di build quando viene analizzata un'esecuzione |
| `artifacts.stale-days` | integer (giorni) | `30` | `QRUST_ARTIFACTS_STALE_DAYS` | età oltre la quale gli artefatti inutilizzati contano come non aggiornati in `qrust inventory` |
| `ci.budget-policy` | string | `.qrust/budgets.toml` | `QRUST_CI_BUDGET_POLICY` | il file dei budget, relativo alla radice del workspace ([Budget](budgets.md)) |
| `ci.baseline` | string | `main` | `QRUST_CI_BASELINE` | la baseline rispetto alla quale vengono valutati i budget |
| `ci.enforce-on-build` | bool | `false` | `QRUST_CI_ENFORCE_ON_BUILD` | valuta i budget dopo ogni comando di build riuscito; un fallimento bloccante termina con exit 3 |
| `self.update-check` | `background`, `off` | `background` | `QRUST_SELF_UPDATE_CHECK` | una volta al giorno dopo una build, controlla in background se sul canale di questo QuickRust è pubblicato un QuickRust più recente e, dopo una build in un terminale, lo dice al massimo una volta al giorno, con il comando che lo aggiorna ([Aggiornare QuickRust](../how-to/install.md#update-quickrust)); un controllo che non si può fare non dice nulla; mai nella CI |
| `self.update-url` | string | vuoto | `QRUST_UPDATE_URL` | l'ancora di un altro registro delle release da cui `qrust self update` e `qrust self check` leggono le release di QuickRust: uno a cui MachineFabric ha dato un charter, che serve in `<url>/charter.json`. Vuoto: il registro in cui è stato pubblicato questo QuickRust (`qrust doctor`, `release line`) |
| `toolchain.auto-update` | `background`, `off` | `background` | `QRUST_TOOLCHAIN_AUTO_UPDATE` | una volta al giorno dopo una build, controlla in background se il canale usato dalla build ha una release più recente, e la installa per la build successiva; mai nella CI |
| `toolchain.dist-server` | string | `https://static.rust-lang.org` | `QRUST_DIST_SERVER` | da dove vengono scaricate le release di Rust |
| `security.persist-environment` | `allowlist`, `none` | `allowlist` | `QRUST_SECURITY_PERSIST_ENVIRONMENT` | quali valori dell'ambiente conservano i record delle esecuzioni: solo le variabili rilevanti per la build, oppure nessuno |
| `security.redact-path-home` | bool | `true` | `QRUST_SECURITY_REDACT_PATH_HOME` | sostituisce la radice del workspace e le directory home, Cargo home e QuickRust home con `$WORKSPACE`, `$HOME`, `$CARGO_HOME`, `$QRUST_HOME` nei report esportati |
| `benchmark.allow-mutation` | bool | `false` | `QRUST_BENCHMARK_ALLOW_MUTATION` | permette allo scenario di benchmark `small-edit` di modificare un file sorgente senza chiedere |

La variabile d'ambiente di una chiave è `QRUST_` seguito dalla chiave in maiuscolo con `.` e `-` sostituiti da `_`, tranne per le chiavi la cui variabile è indicata diversamente qui sopra (`QRUST_MODE`, `QRUST_OUTPUT`, `QRUST_NESTED`, `QRUST_REPORT`, `QRUST_EARLY_CUTOFF`, `QRUST_CACHE_MAX_SIZE`, `QRUST_CACHE_MIN_FREE`, `QRUST_UPDATE_URL`, `QRUST_TOOLCHAIN_AUTO_UPDATE`, `QRUST_DIST_SERVER`).

Un'esecuzione conta come CI quando una tra `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, `TF_BUILD`, `TEAMCITY_VERSION` è impostata a un valore diverso da vuoto, `false` o `0`. La CI seleziona `incremental.ci` al posto di `incremental.local-dev` e disattiva `toolchain.auto-update` e `self.update-check`. Un `CARGO_INCREMENTAL` esplicito nell'ambiente prevale sempre su entrambe le chiavi `incremental.*`.

## Selezione della toolchain {#toolchain-selection}

La toolchain di un comando è la prima tra:

| Ordine | Fonte | Mostrata come |
|---|---|---|
| 1 | `+<toolchain>` come primo argomento (`cargo +nightly build`, `rustc +1.98 -V`); consumato da QuickRust, mai passato a Cargo | `` `+toolchain` argument `` |
| 2 | `RUSTUP_TOOLCHAIN` | `RUSTUP_TOOLCHAIN` |
| 3 | un file della toolchain nella directory corrente o nella directory più vicina sopra di essa | il percorso del file |
| 4 | la toolchain di default (`qrust toolchain default`; `stable` finché non viene impostata) | `default toolchain` |

`qrust toolchain list` e `qrust doctor` mostrano la toolchain selezionata e che cosa l'ha selezionata. Gli override per directory di rustup non hanno un equivalente.

### Nomi delle toolchain {#toolchain-names}

| Forma | Esempio | Passa alle release più recenti |
|---|---|---|
| un canale | `stable`, `beta`, `nightly` | sì |
| una nightly datata | `nightly-2026-10-11` | no |
| una release minor | `1.98` (la sua patch release più recente) | sì |
| una release | `1.98.1` | no |
| una qualsiasi delle precedenti con un host | `stable-x86_64-unknown-linux-gnu` | come sopra |
| il nome di una toolchain collegata | `my-build` (`qrust toolchain link my-build <dir>`) | no; mai modificata da QuickRust |
| un percorso assoluto a una directory di toolchain | `/opt/rust` | no; usata così com'è |

Qualsiasi altra forma è un errore, prima di qualsiasi download. Una `nightly` senza data si risolve nella nightly più recente degli ultimi sette giorni che abbia tutti i componenti e i target richiesti.

### File della toolchain {#toolchain-files}

`rust-toolchain.toml`, oppure `rust-toolchain` (lo stesso TOML, oppure una singola riga che indica la toolchain). Una directory che li contiene entrambi è un errore.

```toml
[toolchain]
channel = "1.98"                       # oppure path = "../my-toolchain", mai entrambi
components = ["rust-src", "llvm-tools"]
targets = ["wasm32-unknown-unknown"]
profile = "minimal"                    # minimal | default | complete
```

| Chiave | Contenuto |
|---|---|
| `channel` | un nome di toolchain |
| `path` | una directory di toolchain, relativa al file; usata così com'è, quindi `components`, `targets` e `profile` sono errori insieme a essa |
| `components` | componenti installati con la toolchain |
| `targets` | target triple la cui libreria standard viene installata con la toolchain |
| `profile` | il profilo assegnato a una nuova installazione della toolchain |

Qualsiasi altra chiave o tabella è un errore. La toolchain, i suoi componenti e target, e la libreria standard di ogni target per cui un comando compila (il suo `--target`, o `build.target` nella configurazione di Cargo) vengono installati al primo utilizzo.

### Profili delle toolchain {#toolchain-profiles}

| Profilo | Componenti |
|---|---|
| `minimal` | rustc, cargo, la libreria standard |
| `default` | `minimal` con clippy, rustfmt e rust-src |
| `complete` | ogni componente che la release ha per l'host |

Una toolchain installata senza un profilo riceve `default`.

## File che QuickRust legge in un repository {#files-quickrust-reads-in-a-repository}

| File | Contenuto |
|---|---|
| `.qrust/config.toml` | configurazione di QuickRust (sopra) |
| `.qrust/budgets.toml` (`ci.budget-policy`) | [Budget](budgets.md) |
| `.qrust/baselines/<name>.json` | baseline scritte da `qrust baseline set` |
| `rust-toolchain.toml`, `rust-toolchain` | selezione della toolchain (sopra) |
