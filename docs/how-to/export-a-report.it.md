# Come esportare un report di build

Questa guida ti mostra come estrarre da QuickRust il report di una build, in JSON per gli strumenti o in HTML per le persone, come parte della build oppure in seguito.

QuickRust registra ogni build che esegue in `~/.qrust`; nel tuo repository non viene scritto nulla, a meno che tu non lo richieda come descritto qui sotto.

## Scrivi un report a ogni build {#write-a-report-with-every-build}

Imposta `QRUST_REPORT` su un percorso:

```bash
QRUST_REPORT=build-report.json cargo build
QRUST_REPORT="reports/{session}.json" cargo test --no-run
```

`{session}` viene sostituito con l'id dell'esecuzione, così build successive scrivono file separati. Un percorso relativo è relativo alla directory in cui viene eseguito il comando. Per farlo per ogni build di un progetto, impostalo in `.qrust/config.toml`:

```toml
[measurement]
report = "target-reports/{session}.json"
```

La build viene analizzata subito dopo che è terminata, il che aggiunge un po' di tempo al comando. Se il report non può essere scritto, QuickRust stampa una riga `WARN` e il risultato della build resta invariato; imposta `measurement.strict = true` se un report mancante deve far fallire il comando.

## Esporta un report dopo la build {#export-a-report-after-the-build}

```bash
qrust report --format json -o build-report.json     # l'esecuzione più recente
qrust report --format html -o report.html           # pagina autonoma, senza script
qrust report --run latest~2 --format json           # su stdout
qrust report                                        # tutte le sezioni come testo
```

`-o` (o `--output`) scrive il file e stampa `OK   Wrote <path>`.

## Trova l'esecuzione che ti interessa {#find-the-run-you-want}

```bash
qrust runs
qrust runs --limit 5 --format json
```

```text
RUN                         STARTED                   FAMILY       STATE           WALL  OBSERVED  COMMAND
01M4SFR4D0T8A2T0XMX6XDVS4Y  2026-10-13T01:59:02.817Z  build        completed       57ms  rustc     cargo build
01M4SFR4AEWS8GV6Q36CH45KAQ  2026-10-13T01:59:02.735Z  build        failed          42ms  rustc     cargo build
```

Ovunque un comando accetti `--run`, indica `latest`, `latest~N` (N esecuzioni prima della più recente), oppure un id di esecuzione o un qualsiasi suo prefisso o suffisso univoco. Le esecuzioni appartengono al workspace in cui ti trovi.

QuickRust conserva le ultime 200 esecuzioni di un workspace (`measurement.keep-runs`); esporta quelle che ti serve conservare più a lungo.

## Esporta una sola sezione {#export-one-section}

```bash
qrust analyze timing --format json
qrust analyze rebuilds --format json --filter reason=source
```

Ogni sezione di `qrust analyze` viene stampata in JSON con `--format json`, ristretta da `--filter`, `--sort` e `--limit`.

## Tieni i percorsi fuori dai report condivisi {#keep-paths-out-of-shared-reports}

I report esportati sostituiscono la radice del workspace, la tua directory home, la Cargo home di QuickRust e la home di QuickRust con `$WORKSPACE`, `$HOME`, `$CARGO_HOME` e `$QRUST_HOME`. I valori dell'ambiente vengono conservati solo per un elenco di variabili rilevanti per la build, e quelli che sembrano credenziali appaiono come `<redacted>`. Per non conservare alcun valore dell'ambiente, imposta `security.persist-environment = "none"`.

## Vedi anche {#related}

- [Schema del report](../reference/report-schema.md) per ogni campo
- [Come eseguire QuickRust in CI e porre controlli sul tempo di build](set-up-ci.md)
- [Sicurezza e privacy](../explanation/security.md)
