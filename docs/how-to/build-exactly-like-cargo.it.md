# Come compilare esattamente come fa Cargo

Questa guida ti mostra come disattivare ciò che QuickRust cambia in una build, tutto insieme o un'impostazione alla volta, per esempio per escludere QuickRust mentre dai la caccia a un problema.

Per default QuickRust funziona in fast mode, che cambia il modo in cui una build viene realizzata ma mai ciò che fa il programma. Che cosa fa ogni impostazione è descritto in [Fast mode](../reference/fast-mode.md); perché sono sicure è spiegato in [Perché QuickRust è veloce per impostazione predefinita](../explanation/fast-by-default.md).

## Esegui il Cargo della toolchain senza aggiungere nulla {#run-the-toolchains-cargo-with-nothing-added}

```bash
qrust upstream build
qrust upstream test -- --nocapture
```

`qrust upstream` esegue il Cargo della toolchain selezionata con i tuoi argomenti: nessuna impostazione di QuickRust, nessun output presentato da QuickRust, nulla di registrato. Usa comunque la toolchain di QuickRust, la sua Cargo home (`~/.qrust/cargo`) e la directory di build del workspace, così trova ciò che QuickRust ha compilato.

Usalo per rispondere alla domanda "succede anche senza QuickRust?".

## Compila esattamente ciò che compila Cargo, sempre attraverso QuickRust {#build-exactly-what-cargo-builds-still-through-quickrust}

La compat mode fa sì che QuickRust compili esattamente ciò che compila il Cargo upstream, continuando a registrare le esecuzioni:

```bash
QRUST_MODE=compat cargo build          # per un solo comando
```

```toml
# .qrust/config.toml nel progetto, oppure ~/.qrust/config.toml per tutti i progetti
mode = "compat"
```

In compat mode anche il terminale mostra l'output di Cargo stesso, a meno che tu non imposti anche `output = "qrust"`.

## Disattiva una sola impostazione {#turn-off-one-setting}

Se hai ricondotto una differenza a una sola impostazione della fast mode, disattiva solo quella:

| Da disattivare | In `.qrust/config.toml` | Per un solo comando |
|---|---|---|
| il frontend parallelo di rustc | `[fast]` `frontend-threads = "off"` | `QRUST_FAST_FRONTEND_THREADS=off` |
| l'early cut-off (il driver del compilatore di QuickRust) | `[fast]` `early-cutoff = "off"` | `QRUST_EARLY_CUTOFF=off` |
| il link con wild | `[fast]` `linker = "off"` | `QRUST_FAST_LINKER=off` |
| la cache degli artefatti | `[cache]` `artifacts = "off"` | `QRUST_CACHE_ARTIFACTS=off` |

Per esempio:

```toml
[fast]
early-cutoff = "off"

[cache]
artifacts = "off"
```

L'early cut-off dipende dalla cache degli artefatti, quindi disattivare la cache degli artefatti disattiva anche l'early cut-off.

## Riottieni le informazioni di debug complete {#get-full-debug-information-back}

La fast mode compila i profili di sviluppo con le sole tabelle delle righe e senza informazioni di debug per le dipendenze. Se ti servono le informazioni di debug complete, per esempio per ispezionare le variabili locali in un debugger, impostale come faresti con Cargo da solo:

```toml
# Cargo.toml
[profile.dev]
debug = true
```

oppure, per un solo comando, `CARGO_PROFILE_DEV_DEBUG=true cargo build`. Un'impostazione di profilo che fai tu, in `Cargo.toml`, in un `.cargo/config.toml`, in una variabile `CARGO_PROFILE_*` o con `--config`, prevale sempre: una volta che imposti `profile.dev.debug`, QuickRust lascia stare entrambe le sue impostazioni di debug.

## Ottieni l'output di Cargo nel terminale {#get-cargos-terminal-output}

Per vedere le righe di Cargo stesso invece della riga di stato e del riepilogo di QuickRust:

```bash
QRUST_OUTPUT=cargo cargo build
cargo build -v                     # anche -v, -q, --message-format e --color lasciano intatto l'output di Cargo
```

oppure `output = "cargo"` in un file di configurazione. La build in sé non cambia.

## Controlla che cosa ha applicato una build {#check-what-a-build-applied}

Ogni esecuzione registra quali impostazioni della fast mode ha applicato, quali ha lasciato stare e perché:

```bash
qrust report --format json | jq .policy.fast
```

`qrust policy explain` elenca ogni impostazione di QuickRust in vigore, il suo valore e da dove proviene quel valore.

## Vedi anche {#related}

- [Configurazione](../reference/configuration.md)
- [Come provare QuickRust prima di farne il tuo cargo](adopt-gradually.md)
