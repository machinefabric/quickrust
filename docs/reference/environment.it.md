# Variabili d'ambiente

Le variabili d'ambiente che QuickRust legge, quelle che imposta per il Cargo che esegue e per i programmi che Cargo avvia, e quelle che sono interne a QuickRust.

## Lette da QuickRust {#read-by-quickrust}

### Proprie di QuickRust {#quickrusts-own}

| Variabile | Effetto |
|---|---|
| `QRUST_HOME` | la directory home di QuickRust; default `~/.qrust` |
| `QRUST_MODE`, `QRUST_OUTPUT`, `QRUST_REPORT`, `QRUST_NESTED`, ... | chiavi di configurazione, una variabile per chiave; l'elenco completo è in [Configurazione](configuration.md#keys) |

Qualsiasi altra variabile il cui nome inizia con `QRUST_` è un errore, tranne le [variabili interne](#internal-variables).

### Toolchain e build {#toolchain-and-build}

| Variabile | Effetto |
|---|---|
| `RUSTUP_TOOLCHAIN` | seleziona la toolchain, dopo un argomento `+toolchain` e prima di un file di toolchain ([Configurazione](configuration.md#toolchain-selection)); un nome di toolchain o una directory di toolchain assoluta |
| `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR` | una directory target scelta dall'utente: usata così com'è al posto della directory del workspace nella cache di QuickRust, a meno che il suo valore non sia uguale a `QRUST_MANAGED_TARGET_DIR` (impostata da un QuickRust che lo racchiude) |
| `RUSTC_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER` | un wrapper di rustc configurato dall'utente (letto anche da `build.rustc-wrapper` nella configurazione di Cargo): viene mantenuto ed eseguito dentro `qrust-rustc-wrapper`. Quando ce n'è uno, la cache degli artefatti, il frontend parallelo, il linker e l'early cut-off sono disattivati, e sccache non viene concatenato |
| `RUSTC_WORKSPACE_WRAPPER` | lasciata a Cargo, che la applica dentro la catena dei wrapper |
| `RUSTC`, `RUSTDOC` | un compilatore o un rustdoc scelto dall'utente: lasciati così come sono; altrimenti QuickRust li imposta su quelli della toolchain |
| `RUSTC_BOOTSTRAP` | quando è impostata dall'utente, non si applica alcuna impostazione della fast mode che richieda flag `-Z` (frontend parallelo, linker, ripetizione del link) |
| `CARGO_INCREMENTAL` | quando è impostata, prevale su `incremental.local-dev` e `incremental.ci` |
| `CARGO_PROFILE_*` | un'impostazione di profilo fatta dall'utente: l'impostazione della fast mode che la riguarda non viene toccata ([Fast mode](fast-mode.md)) |
| `CARGO_INSTALL_ROOT` | un errore per `qrust install` senza `--root` |
| `CARGO_HOME` | letta da `qrust setup` per trovare la home di Cargo da cui importare (default `~/.cargo`); mai usata per le build, che usano `~/.qrust/cargo` (`qrust doctor` segnala quando è impostata) |

### Rilevamento della CI {#ci-detection}

Un comando viene eseguito in CI quando una tra `CI`, `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, `TF_BUILD`, `TEAMCITY_VERSION` è impostata su un valore diverso da vuoto, `false` o `0`. La CI seleziona `incremental.ci`, viene registrata nei report delle esecuzioni e disattiva l'aggiornamento della toolchain in background.

### Terminale {#terminal}

| Variabile | Effetto |
|---|---|
| `NO_COLOR` (non vuota) | nessun colore in tutto ciò che QuickRust scrive |
| `CARGO_TERM_COLOR` | `never`: nessun colore; `always`: colore sulle righe con tag di QuickRust anche quando non vanno a un terminale |
| `COLUMNS` | la larghezza del terminale quando non è possibile chiederla al terminale (default 100) |

Il fatto che l'output di un comando venga reso dipende dal fatto che stderr sia un terminale ([Output](output.md#when-quickrust-presents-the-output)).

## Impostate per il Cargo che QuickRust esegue {#set-for-the-cargo-quickrust-runs}

Ogni comando esegue il Cargo della toolchain con queste variabili. Cargo le trasmette agli script di build, ai sottocomandi esterni, ai programmi di `cargo run` e ai test.

| Variabile | Valore |
|---|---|
| `CARGO_HOME` | `~/.qrust/cargo` |
| `RUSTUP_TOOLCHAIN` | l'id della toolchain selezionata (`1.99.0-x86_64-unknown-linux-gnu`), così che un `cargo` o un `rustc` annidato usi la stessa toolchain |
| `RUSTC`, `RUSTDOC` | il `rustc` e il `rustdoc` della toolchain, a meno che l'utente non li abbia impostati su qualcos'altro |
| `PATH` | prima `~/.qrust/bin`, poi il `PATH` ereditato, poi la `bin` della toolchain |
| `LD_LIBRARY_PATH` (Linux), `DYLD_FALLBACK_LIBRARY_PATH` (macOS) | prima la `lib` della toolchain, poi il valore ereditato |
| `CARGO_CACHE_AUTO_CLEAN_FREQUENCY` | `never`: Cargo non elimina di sua iniziativa i crate scaricati; quello spazio è gestito dalla cache di QuickRust |
| `CARGO` | `~/.qrust/bin/cargo`, così che una build annidata tramite `$CARGO` passi per QuickRust (tranne sotto `qrust upstream`, e con una toolchain collegata) |
| `CARGO_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR` | la directory del workspace nella cache di QuickRust (`qrust path`), a meno che l'utente non abbia scelto una directory target |
| `CARGO_INCREMENTAL` | `1` o `0` quando `incremental.*` è `on` o `off` (e l'utente non l'ha impostata) |

In un comando di build registrato, inoltre:

| Variabile | Valore |
|---|---|
| `RUSTC_WRAPPER` | `qrust-rustc-wrapper` |

`+toolchain` viene consumato da QuickRust e non raggiunge Cargo. Le impostazioni di profilo della fast mode e le impostazioni del terminale (`term.progress.*`, `term.unicode`, `term.color`) raggiungono Cargo come argomenti `--config`, mai come variabili d'ambiente, quindi non raggiungono gli script di build, i Cargo annidati o i programmi.

## Impostate per i processi del compilatore {#set-for-compiler-processes}

`qrust-rustc-wrapper` esegue rustc (o il driver del compilatore di QuickRust) per le unità che Cargo pianifica con:

| Variabile | Valore |
|---|---|
| `RUSTC_BOOTSTRAP` | `1`, quando all'unità si applica un'impostazione della fast mode che richiede flag `-Z`; su un compilatore stable o beta insieme a `-Zallow-features=`, così che `#![feature]` resti rifiutato. Mai per gli script di build o per le sonde del compilatore che eseguono, e mai quando l'utente ha impostato `RUSTC_BOOTSTRAP` |

## Variabili interne {#internal-variables}

QuickRust le imposta per i propri processi. Non sono configurazione, e impostarle a mano non è supportato.

| Variabile | Scopo |
|---|---|
| `QRUST_SESSION_ID` | la sessione a cui appartiene un comando annidato |
| `QRUST_EVENT_DIR` | dove il wrapper di rustc registra i suoi eventi |
| `QRUST_INNER_WRAPPER` | il wrapper dell'utente (o sccache) che il wrapper di rustc esegue al proprio interno |
| `QRUST_WRAPPER_FAIL` | `open` o `closed`: se una cache concatenata può essere saltata quando non riesce ad avviarsi |
| `QRUST_EXEC_GUARD` | rilevamento della ricorsione |
| `QRUST_ENGINE` | la directory della toolchain per cui QuickRust ha impostato `RUSTC`, `RUSTDOC` e `PATH`, così che un QuickRust annidato possa distinguere le proprie impostazioni da quelle dell'utente |
| `QRUST_MANAGED_TARGET_DIR` | indica che `CARGO_TARGET_DIR` e `CARGO_BUILD_BUILD_DIR` sono state impostate da QuickRust |
| `QRUST_DRIVER_INTERFACE_HASH`, `QRUST_DRIVER_KEEP_OBJECTS`, `QRUST_DRIVER_READY_FD` | tra il wrapper di rustc e il driver del compilatore di QuickRust |

## Registrate nei report delle esecuzioni {#recorded-in-run-reports}

Con `security.persist-environment = "allowlist"` (il default), la registrazione di un'esecuzione conserva i valori di queste variabili quando sono impostate: `RUSTFLAGS`, `CARGO_ENCODED_RUSTFLAGS`, `RUSTDOCFLAGS`, `CARGO_ENCODED_RUSTDOCFLAGS`, `CARGO_INCREMENTAL`, `CARGO_BUILD_TARGET`, `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, `CARGO_BUILD_BUILD_DIR`, `CARGO_BUILD_JOBS`, `CARGO_BUILD_RUSTFLAGS`, `RUSTC`, `RUSTC_WRAPPER`, `RUSTC_WORKSPACE_WRAPPER`, `CARGO_BUILD_RUSTC_WRAPPER`, `RUSTUP_TOOLCHAIN`, `RUSTC_BOOTSTRAP`, `CC`, `CXX`, `AR`, `CFLAGS`, `CXXFLAGS`, `LDFLAGS`, `MACOSX_DEPLOYMENT_TARGET`, `SDKROOT`, `PKG_CONFIG_PATH`, `CI`, `CARGO_NET_OFFLINE`, `CARGO_HOME`, `MAKEFLAGS`, e ogni variabile `CARGO_PROFILE_*`, `CARGO_TARGET_*`, `CARGO_UNSTABLE_*` e `SCCACHE_*`. Le variabili i cui nomi sembrano credenziali vengono registrate come impostate, con il valore `<redacted>` ([Sicurezza](../explanation/security.md#secrets)). `security.persist-environment = "none"` non ne registra nessuna.
