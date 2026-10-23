# Come esaminare e applicare le raccomandazioni di QuickRust

Questa guida mostra come vedere che cosa raccomanda QuickRust per una build, visualizzare in anteprima la modifica che ogni raccomandazione comporta e applicarla o annullarla.

QuickRust non modifica mai il tuo repository di sua iniziativa. Scrive un file solo quando applichi una modifica specifica, già vista in anteprima, indicandone l'id, e in questo modo si possono applicare solo modifiche alla configurazione di QuickRust stesso.

## Vedi le raccomandazioni {#see-the-recommendations}

```bash
qrust analyze recommendations
qrust analyze recommendations --run latest~1
qrust analyze recommendations --format json
```

Ogni raccomandazione ha un id, le prove su cui si basa, l'impatto atteso, il rischio e il grado di confidenza, e il modo per verificarla. Quando le prove non giustificano nulla, QuickRust lo dice:

```text
Recommendations
  None: the recorded evidence supports no recommendation.
```

## Visualizza in anteprima le modifiche {#preview-the-changes}

```bash
qrust fix --dry-run                    # tutte le raccomandazioni dell'ultima esecuzione
qrust fix --dry-run <REC_ID>           # una sola raccomandazione
qrust fix --dry-run <REC_ID> --option keep-incremental   # una delle sue alternative
qrust fix --dry-run --run latest~1
```

Per ogni raccomandazione l'anteprima mostra la classe e lo stato e, quando comporta una modifica a un file, l'id della correzione, il file e un diff unificato. Non viene scritto nulla. Una raccomandazione senza modifica a un file riceve invece delle indicazioni (per esempio, quali righe `cargo::rerun-if-changed` servono a uno script di build).

La classe decide che cosa puoi farne:

- `safe`: una modifica alla configurazione di QuickRust stesso (`.qrust/config.toml`). Si può applicare con `qrust fix --apply`.
- `review_required`: una modifica al modo in cui il progetto viene compilato (profili in `Cargo.toml`, dipendenze, script di build). QuickRust si limita a mostrarla.

## Applica una modifica sicura {#apply-a-safe-change}

```bash
qrust fix --apply rem-1a2b3c4d5e6f
```

```text
OK   Applied rem-1a2b3c4d5e6f to .qrust/config.toml; undo with: qrust fix --rollback rem-1a2b3c4d5e6f
```

QuickRust applica esattamente ciò che ha mostrato l'anteprima. Se il file è cambiato dopo l'anteprima, si rifiuta; esegui di nuovo `qrust fix --dry-run`. Se il risultato non è una configurazione valida di QuickRust, ripristina il file e spiega il motivo.

## Applica una modifica che richiede revisione {#apply-a-change-that-needs-review}

Stampa solo i diff, leggili e applicali tu:

```bash
qrust fix --dry-run <REC_ID> --patch > change.patch
git apply change.patch
```

Poi verifica la modifica come indica la riga `verify:` della raccomandazione, per esempio con un esperimento (vedi [Come misurare l'effetto di un'impostazione di build](measure-a-change.md)), prima di farne il commit.

## Annulla una modifica applicata {#undo-an-applied-change}

```bash
qrust fix --rollback rem-1a2b3c4d5e6f
```

Questo ripristina il file com'era prima (o lo rimuove, se è stato creato applicando la modifica). Se hai modificato il file dopo l'applicazione, QuickRust si rifiuta di sovrascrivere le tue modifiche; annulla la modifica a mano.

## Se intendevi il fix di Cargo {#if-you-meant-cargos-fix}

`qrust fix` è il comando di QuickRust. Per `cargo fix` di Cargo (che applica i suggerimenti del compilatore), esegui `cargo fix`, oppure `qrust upstream fix` quando `cargo` non è QuickRust.

## Vedi anche {#related}

- [Comandi](../reference/commands.md) per tutte le opzioni di `qrust fix`
- [Sicurezza e privacy](../explanation/security.md) per che cosa QuickRust può scrivere e dove
