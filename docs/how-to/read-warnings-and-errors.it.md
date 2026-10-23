# Come vedere i warning, gli errori e il log completo di una build

Questa guida mostra come leggere tutto ciò che una build ha stampato, dopo che QuickRust l'ha riassunto in un terminale.

In un terminale, QuickRust mostra ogni errore per intero appena arriva, ma si limita a contare i warning:

```text
OK   Built: 1 compiled [100ms]
WARN 2 warnings in demo (bin): dead_code x1, unused_variables x1; `qrust warnings` shows them
```

Non si perde nulla: l'intero output di ogni build presentata da QuickRust viene memorizzato insieme alla sua esecuzione.

## Mostra i warning {#show-the-warnings}

```bash
qrust warnings
```

stampa ogni warning dell'ultima build così come l'ha scritto rustc. Per restringere il campo:

```bash
qrust warnings --crate demo
qrust warnings --lint dead_code
qrust warnings --crate demo --lint unused_variables
```

Se nulla corrisponde, lo dice (`No warnings in run <id> matching the filter.`).

## Mostra di nuovo gli errori {#show-the-errors-again}

```bash
qrust errors
qrust errors --crate demo
```

È utile quando gli errori sono scorsi via, o per inviarli tramite pipe da qualche parte:

```bash
qrust errors | less -R
```

## Mostra tutto, in ordine {#show-everything-in-order}

```bash
qrust log            # le righe di Cargo e i messaggi di rustc, come li avrebbe stampati una build normale
qrust log --raw      # lo stream memorizzato così come l'ha emesso Cargo, compresi i suoi messaggi JSON
```

## Guarda una build precedente {#look-at-an-earlier-build}

Ognuno di questi comandi accetta `--run`:

```bash
qrust warnings --run latest~1        # la build prima dell'ultima
qrust log --run 01M4SFQZ             # un id di esecuzione, o un suo prefisso o suffisso univoco
```

`qrust runs` elenca le esecuzioni registrate del workspace con i loro id.

## Ottieni invece l'output di Cargo stesso {#get-cargos-own-output-instead}

Se vuoi che sia Cargo stesso a stampare una build, riga per riga:

- per un solo comando, passa `-v` (oppure `-q`, `--message-format` o `--color`): QuickRust lascia intatto l'output di Cargo;
- per una sola shell, `export QRUST_OUTPUT=cargo`;
- in modo permanente, `output = "cargo"` in `~/.qrust/config.toml` o nel `.qrust/config.toml` del progetto.

L'output scritto su una pipe o su un file è sempre quello di Cargo stesso, quindi `cargo build 2>&1 | tee build.log` e i log della CI non richiedono nulla.

## Se l'output non è stato memorizzato {#if-the-output-was-not-stored}

```text
FAIL this run's output was not stored: it was not rendered by QuickRust (not a terminal, or output = "cargo"), or its raw events were pruned
```

L'output viene memorizzato solo per le build che QuickRust ha presentato in un terminale, e solo per le esecuzioni più recenti (`measurement.keep-raw-events`, 20 per impostazione predefinita). Una build il cui output è andato su una pipe, eseguita con `-v` o eseguita con `output = "cargo"` ha stampato direttamente l'output di Cargo. Esegui di nuovo la build in un terminale, oppure leggi il log che hai catturato.

## Vedi anche {#related}

- [Output sul terminale](../reference/output.md) per ogni tipo di riga che QuickRust stampa
- [L'output delle build di QuickRust](../explanation/build-output.md)
