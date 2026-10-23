# Scoprire dove va il tempo di una build

In questo tutorial compileremo un piccolo programma che usa `serde`, poi chiederemo a QuickRust dove la build ha speso il suo tempo: quali crate la build ha dovuto aspettare, quanto sono costati le macro procedurali e gli script di build, e perché una ricompilazione ha compilato ciò che ha compilato. Alla fine confronteremo due build e scriveremo un report da aprire in un browser.

Ci vogliono circa quindici minuti. Diamo per scontato che QuickRust sia installato e che tu abbia seguito [La tua prima build con QuickRust](first-build.md), così `cargo build` e le righe di riepilogo di QuickRust ti sono familiari.

## Compilare un programma con dipendenze {#build-a-program-with-dependencies}

Crea un progetto e dagli due dipendenze:

```bash
cargo new inventory
cd inventory
cargo add serde --features derive
cargo add serde_json
```

Ogni `cargo add` termina con una riga `OK`, con le feature della dipendenza elencate sopra.

Sostituisci `src/main.rs` con:

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Item {
    name: String,
    count: u32,
}

fn main() {
    let items = vec![Item { name: "apple".into(), count: 3 }, Item { name: "pear".into(), count: 5 }];
    let json = serde_json::to_string_pretty(&items).unwrap();
    println!("{json}");
    let back: Vec<Item> = serde_json::from_str(&json).unwrap();
    println!("{} items", back.len());
}
```

Compilalo:

```bash
cargo build
```

Questa build richiede qualche secondo. La riga di stato conta i crate man mano che vengono completati; alla fine vedrai qualcosa come:

```text
     Downloaded 10 crates, 924.3 KiB [0.12s]
OK   Built: 17 compiled, 1 restored from QuickRust's cache [2.00s]
```

Se hai seguito il primo tutorial, un crate (`itoa`, che `serde_json` usa) è stato ripristinato dalla cache di QuickRust invece di essere compilato. I tuoi numeri e le tue versioni possono differire un po'; i passi seguenti funzionano allo stesso modo.

## Vedere il percorso critico {#see-the-critical-path}

QuickRust ha registrato ogni esecuzione del compilatore in quella build. Chiedigli dei tempi:

```bash
qrust analyze timing
```

```text
Timing
  critical path: 1.78s  [observed]
  ON CRITICAL PATH      KIND              DURATION  STATE     EDGE
  proc-macro2@1.0.107   build-script      141ms     compiled  observed
  proc-macro2@1.0.107   build-script-run  115ms     ran       inferred
  proc-macro2@1.0.107   lib               139ms     compiled  inferred
  quote@1.0.47          lib               91ms      compiled  observed
  syn@3.0.9             lib               435ms     compiled  observed
  serde_derive@1.0.229  proc-macro        706ms     compiled  observed
  serde@1.0.229         lib               125ms     compiled  observed
  inventory@0.1.0       bin               106ms     compiled  observed
  slowest units (15 of 24):
  UNIT                  KIND              MODE   DURATION  CRITICAL  ROLE    CACHE
  serde_derive@1.0.229  proc-macro        build  706ms     yes       host    stored
  syn@3.0.9             lib               build  435ms     yes       host    stored
  ...
  serde_core@1.0.229    lib               build  541ms     no        target  stored
  ...
  peak concurrent compilers: 9   cpu time: 9.13s   peak rss: 372.6 MiB
```

La prima tabella è il percorso critico: la catena di passi, ciascuno in attesa di quello precedente, che ha deciso quanto è durata la build. Leggila dall'alto. Lo script di build di `proc-macro2` viene compilato ed eseguito, poi `proc-macro2` stesso, poi `quote`, poi `syn`, poi la macro `serde_derive`, poi `serde`, e solo allora il nostro programma. Insieme occupano 1,78 secondi dei 2,00.

Nota `serde_core` nella seconda tabella: ha richiesto 541 ms, più della maggior parte dei passi sul percorso, ma è contrassegnato con `no` sotto `CRITICAL`. È stato compilato mentre il percorso era impegnato altrove, quindi renderlo più veloce non avrebbe fatto terminare la build prima.

Nota anche l'ultima riga: i compilatori hanno usato 9,13 secondi di tempo di CPU in una build che è durata 2,00 secondi, perché ne giravano fino a 9 contemporaneamente.

## Esaminare la macro e gli script di build {#look-at-the-macro-and-the-build-scripts}

`serde_derive` è il passo più lungo sul percorso. Chiedi informazioni sulle macro procedurali:

```bash
qrust analyze proc-macros
```

```text
Procedural macros
  MACRO CRATE           STATE     OWN COMPILE  DIRECT USERS  DOWNSTREAM  DOWNSTREAM TIME  CLASS
  serde_derive@1.0.229  compiled  706ms        1             2           230ms            critical-path-candidate
  Proc-macro execution timing
    Unavailable: a stable toolchain does not expose time spent expanding macros in dependent crates; only the macro crate's own compilation is measured
```

La classe `critical-path-candidate` dice ciò che abbiamo visto sopra: questa macro si trova sul percorso critico. L'ultima riga è QuickRust che dichiara onestamente ciò che non può misurare; dice "unavailable" (non disponibile) invece di mostrare uno zero.

Ora gli script di build:

```bash
qrust analyze scripts
```

```text
Build scripts
  PACKAGE              THIS RUN  RUN TIME  COMPILE  RERUN-IF       OUT_DIR  RUNS SEEN  STATE
  proc-macro2@1.0.107  ran       115ms     141ms    3 file, 1 env  0 B      1          observed
  quote@1.0.47         ran       13ms      137ms    1 file, 0 env  0 B      1          observed
  serde@1.0.229        ran       12ms      151ms    1 file, 0 env  4.0 KiB  1          observed
  serde_core@1.0.229   ran       15ms      166ms    1 file, 0 env  4.0 KiB  1          observed
  serde_json@1.0.154   ran       2ms       69ms     1 file, 0 env  0 B      1          observed
  zmij@1.0.23          ran       14ms      132ms    1 file, 0 env  0 B      1          observed
  No broad invalidation detected.
```

Sei crate hanno script di build. Ciascuno viene compilato e poi eseguito prima che il suo crate possa essere compilato; la colonna `RERUN-IF` dice da che cosa ciascuno ha dichiarato di dipendere, il che decide quando viene eseguito di nuovo.

## Cambiare una riga e ricompilare {#change-one-line-and-rebuild}

Apri `src/main.rs` e cambia `count: 3` in `count: 4`. Compila di nuovo:

```bash
cargo build
```

```text
OK   Built: 1 compiled, 17 fresh [154ms]
```

È stato compilato solo il nostro programma. Chiedi perché:

```bash
qrust analyze rebuilds
```

```text
Rebuild reasons
  REASON  UNITS  TIME
  source  1      90ms
  previously known units rebuilt: 1 of 18 in the index (ratio 0.056)
  UNIT             KIND  REASON  COST  CONFIDENCE  DETAIL
  inventory@0.1.0  bin   source  90ms  high        changed: src/main.rs (modified)
```

QuickRust indica il motivo (un file sorgente è cambiato), il file e quanto ne è sicuro. Quando una build ricompila più di quanto ti aspettassi, è qui che devi guardare.

## Confrontare le due build {#compare-the-two-builds}

Ogni build è un'esecuzione registrata. Elencale:

```bash
qrust runs
```

```text
RUN                         STARTED                   FAMILY       STATE           WALL  OBSERVED  COMMAND
01M4SFW9PX1526541E7WM1ZJ9F  2026-10-13T02:01:19.327Z  build        completed      154ms  rustc     cargo build
01M4SFW1V3NZTKFD0YHGA3GXES  2026-10-13T02:01:11.267Z  build        completed      2.00s  rustc     cargo build
01M4SFW1JF0TMXB0QNFFZFA7PF  2026-10-13T02:01:10.991Z  passthrough  completed      246ms  session   cargo add serde_json
01M4SFVZWTN755HZZ5TKTQ5W32  2026-10-13T02:01:09.274Z  passthrough  completed      1.69s  session   cargo add serde --features derive
```

Non ci servono gli identificatori lunghi: `latest` è l'ultima esecuzione e `latest~1` quella precedente. Confronta la build completa con la ricompilazione:

```bash
qrust compare latest~1 latest
```

```text
Comparison: run 01M4SFW1V3NZTKFD0YHGA3GXES  ->  run 01M4SFW9PX1526541E7WM1ZJ9F
  comparability: comparable
  METRIC                    BASELINE     CANDIDATE    DELTA
  wall_time_ns              2.00s        154ms        -92.3%
  critical_path_ns          1.78s        90ms         -95.0%
  sum_unit_time_ns          3.61s        90ms         -97.5%
  cpu_time_ns               9.13s        134ms        -98.5%
  peak_rss_bytes            372.6 MiB    126.3 MiB    -66.1%
  units_compiled            17           1            -94.1%
  units_restored            1            0            -100.0%
  ...
```

Nota la seconda riga: QuickRust controlla prima di tutto che le due esecuzioni siano effettivamente confrontabili (stessa toolchain, stessa macchina, stesso tipo di comando) e solo dopo mostra le differenze.

## Scrivere un report {#write-a-report}

Infine, scrivi l'analisi della build completa come pagina web:

```bash
qrust report --run latest~1 --format html -o report.html
```

```text
OK   Wrote report.html
```

Apri `report.html` nel browser. Contiene tutto ciò che abbiamo esaminato, ogni sezione in un'unica pagina, e non ha bisogno della rete.

## Che cosa abbiamo fatto {#what-we-have-done}

Abbiamo compilato un programma con dipendenze reali e trovato il suo percorso critico, visto che una macro procedurale e i crate di cui ha bisogno hanno deciso la maggior parte del tempo della build, esaminato gli script di build, scoperto esattamente perché una ricompilazione ha compilato un crate, confrontato due build e scritto un report.

## Dove andare ora {#where-to-go-next}

- [Come scoprire perché i crate sono stati ricompilati](../how-to/find-why-crates-rebuilt.md) quando una ricompilazione è più ampia di quanto dovrebbe.
- [Come misurare l'effetto di un'impostazione di build](../how-to/measure-a-change.md) per provare un cambiamento, come un'impostazione di codegen diversa, con ripetizioni e un verdetto.
- [Comandi](../reference/commands.md) elenca ogni sezione di analisi e le sue opzioni, tra cui `--filter`, `--sort` e `--limit`.
- [Misurare le build](../explanation/measurement.md) spiega che cosa QuickRust può e non può osservare, e perché alcuni valori sono "unavailable" (non disponibili).
