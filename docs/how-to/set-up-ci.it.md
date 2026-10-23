# Come eseguire QuickRust in CI e porre controlli sul tempo di build

Questa guida mostra come compilare con QuickRust in un job di CI, conservare un report di ogni build e far fallire il job (o emettere un avviso) quando una modifica rende la build più lenta o più grande di quanto consenti.

## Installare QuickRust nel job {#install-quickrust-in-the-job}

Fissa la release con cui compila il job e verifica l'archivio rispetto allo SHA-256 che il manifest delle release (`https://release.machinefabric.com/quickrust/manifest`) indica per esso:

```bash
version=0.150.0
sha256=<the manifest's sha256 of qrust-$version-linux-x86_64.tar.gz>
curl -fLO "https://release.machinefabric.com/quickrust/release/$version/qrust-$version-linux-x86_64.tar.gz"
echo "$sha256  qrust-$version-linux-x86_64.tar.gz" | sha256sum -c
tar xzf "qrust-$version-linux-x86_64.tar.gz"
quickrust/qrust setup --no-modify-path
export PATH="$HOME/.qrust/bin:$PATH"
```

Su GitHub Actions, aggiungi la directory al `PATH` per i passi successivi con `echo "$HOME/.qrust/bin" >> "$GITHUB_PATH"` al posto dell'`export`. Un job che aggiunge il repository dei pacchetti di MachineFabric può eseguire `apt install quickrust` (o `brew install machinefabric/tap/quickrust`) e poi `qrust setup --no-modify-path`: vedi [Come installare QuickRust](install.md#install-a-release).

Il setup installa Rust stable; un `rust-toolchain.toml` nel repository fa sì che la prima build installi invece la toolchain che indica. Per evitare di scaricare le toolchain a ogni esecuzione, metti in cache `~/.qrust/toolchains` tra un'esecuzione e l'altra (per esempio con `actions/cache`, usando il file della toolchain come chiave).

In CI, QuickRust non aggiorna mai le toolchain in background e applica la policy `incremental.ci` invece di `incremental.local-dev`. Riconosce la CI da `CI` oppure da `GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, `TF_BUILD` o `TEAMCITY_VERSION`.

## Conservare un report di ogni build {#keep-a-report-of-every-build}

```bash
QRUST_REPORT="$RUNNER_TEMP/qrust/{session}.json" cargo build --locked
```

`{session}` diventa l'id dell'esecuzione, così più build nello stesso job non si sovrascrivono a vicenda; un percorso relativo è relativo alla directory in cui viene eseguito il comando. Carica la directory come artefatto se vuoi avere i report dopo il job. Il formato è descritto in [Schema del report](../reference/report-schema.md); vedi anche [Come esportare un report di build](export-a-report.md).

La scrittura del report non cambia mai il risultato della build. Se vuoi che un errore nel report faccia fallire il job, imposta `measurement.strict = true`.

## Porre controlli tramite i budget {#gate-on-budgets}

### 1. Scrivere i budget {#1-write-the-budgets}

Crea `.qrust/budgets.toml` nel repository:

```toml
[policy]
mode = "warn"            # warn: solo report; blocking: fa fallire il job (exit status 3)
incomparable = "warn"    # quando baseline e candidato non sono confrontabili: warn, oppure fail (exit status 4)
noise-percent = 3        # le variazioni relative entro +/-3% contano come nessuna variazione

[[budget]]
metric = "wall_time_ns"
scenario = "clean"
max-increase-percent = 10

[[budget]]
metric = "artifact_allocated_bytes"
max-value = "20GiB"
```

Inizia con `mode = "warn"` e passa a `blocking` quando i numeri si sono dimostrati stabili. Le metriche e le chiavi sono elencate in [Budget](../reference/budgets.md).

### 2. Registrare una baseline sull'hardware della CI {#2-record-a-baseline-on-the-ci-hardware}

I budget con `max-increase-percent` o `max-decrease-percent` fanno il confronto con una baseline, che deve provenire dallo stesso tipo di macchina su cui gira la CI. Esegui lì un benchmark (per esempio in un job avviato manualmente), poi promuovilo:

```bash
qrust benchmark --scenario clean --reps 5 --format json > bench.json
qrust baseline set main --benchmark "$(jq -r .id bench.json)"
```

Questo scrive `.qrust/baselines/main.json`. Fanne il commit. `main` è il nome che i budget cercano, a meno che tu non imposti `ci.baseline` o passi `--baseline`.

### 3. Valutare in ogni esecuzione della CI {#3-evaluate-in-every-ci-run}

```bash
qrust benchmark --scenario clean --reps 3 --format json > bench.json
qrust ci evaluate --benchmark "$(jq -r .id bench.json)"
```

```text
Budget result: PASS  (mode blocking, policy /work/app/.qrust/budgets.toml)
  baseline: baseline main   comparability: comparable
  METRIC        SCENARIO  THRESHOLD                 OBSERVED      RESULT  NOTE
  wall_time_ns  clean     max-increase-percent 10%  86ms (+3.3%)  PASS
```

Senza `--benchmark`, `qrust ci evaluate` giudica l'ultima esecuzione di build (`--run` ne sceglie un'altra). I budget con uno `scenario` si applicano solo ai benchmark di quello scenario.

Se il risultato è `incomparable`, baseline e candidato differiscono per compilatore, host, classe di hardware, comando, profilo, target, scenario o stato della cache, e i budget relativi non vengono giudicati. Registra una nuova baseline dopo aver cambiato di proposito uno qualsiasi di questi elementi (un aggiornamento della toolchain, nuovi runner).

### Porre i controlli sulla build ordinaria {#gate-the-ordinary-build-instead}

Se preferisci non eseguire un benchmark separato, valuta i budget dopo ogni comando di build:

```toml
# .qrust/config.toml
[ci]
enforce-on-build = true
```

A quel punto un budget bloccante che fallisce trasforma l'exit status di successo della build in 3. Il verdetto va su stderr; lo stdout di Cargo resta intatto.

## Vedi anche {#related}

- [Comandi](../reference/commands.md) per `benchmark`, `baseline` e `ci evaluate`, e ogni exit status
- [Come misurare l'effetto di un'impostazione di build](measure-a-change.md)
- [Misurare le build](../explanation/measurement.md) per sapere perché le baseline devono provenire dallo stesso hardware
