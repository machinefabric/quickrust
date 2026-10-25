# La tua prima build con QuickRust

In questo tutorial creeremo un piccolo programma Rust, lo compileremo e lo eseguiremo tramite QuickRust, lo romperemo e lo ripareremo, e gli daremo una dipendenza e un test. Lungo la strada incontreremo l'output di build di QuickRust, la directory di build che tiene per ogni progetto, il suo registro di ogni warning e la sua cache.

Ci vogliono circa dieci minuti. Ti servono un terminale e una connessione a internet; non è necessario conoscere bene Cargo.

## Prima di iniziare {#before-we-start}

QuickRust deve essere installato. Se non lo è, segui [Come installare QuickRust](../how-to/install.md) e poi apri un nuovo terminale.

Verifica che `cargo` ora sia QuickRust:

```bash
qrust doctor
```

Il comando stampa ciò che QuickRust usa (la sua home, la toolchain, Cargo e rustc) e termina con una riga che dice soltanto:

```text
OK
```

Se invece termina con `FAIL`, le righe sopra dicono che cosa non va; [Come installare QuickRust](../how-to/install.md) tratta le cause più comuni.

## Crea un programma {#create-a-program}

Lavoreremo in un nuovo progetto chiamato `hello`. In una directory in cui tieni i tuoi progetti, esegui:

```bash
cargo new hello
cd hello
```

QuickRust risponde con una riga, più una nota di Cargo:

```text
OK   Created binary (application) `hello` package
NOTE see more `Cargo.toml` keys and their definitions at https://doc.rust-lang.org/cargo/reference/manifest.html
```

Nota come inizia ogni riga: `OK` per qualcosa che ha funzionato, `NOTE` per qualcosa che vale la pena sapere. Ogni riga che QuickRust stampa a proposito di un esito inizia con una parola del genere, nella stessa colonna.

## Compilalo ed eseguilo {#build-and-run-it}

Esegui il programma:

```bash
cargo run
```

Per un momento una riga di stato mostra che cosa si sta compilando; poi viene sostituita da un unico riepilogo, e il programma parte:

```text
OK   Built: 1 compiled [59ms]
Hello, world!
```

Il tempo tra parentesi sarà diverso sulla tua macchina. Laddove il semplice Cargo stampa una riga `Compiling` per ogni crate, QuickRust stampa una sola riga per l'intera build.

Ora elenca i file del progetto:

```bash
ls
```

```text
Cargo.lock  Cargo.toml  src
```

Non c'è alcuna directory `target`. QuickRust tiene l'output di build di ogni progetto nella propria cache, fuori dal progetto. Chiedigli dove:

```bash
qrust path
```

```text
/home/you/.qrust/cache/builds/hello-38cd7ad91fa9d702
```

Il tuo percorso termina con un numero diverso. Esegui di nuovo il programma:

```bash
cargo run
```

```text
OK   Up to date: 1 fresh
Hello, world!
```

Dall'ultima build non è cambiato nulla, e QuickRust lo dice prima che il programma parta.

## Provoca un warning {#make-a-warning}

Apri `src/main.rs` e sostituiscine il contenuto con:

```rust
fn main() {
    let name = "world";
    println!("Hello, world!");
}
```

Abbiamo creato una variabile senza usarla. Esegui il programma:

```bash
cargo run
```

```text
OK   Built: 1 compiled [58ms]
WARN 1 warning in hello (bin): unused_variables x1; `qrust warnings` shows them
Hello, world!
```

La build è riuscita, quindi il riepilogo dice ancora `OK`, e una riga `WARN` conta il warning per tipo invece di stamparlo. Leggiamolo:

```bash
qrust warnings
```

```text
warning: unused variable: `name`
 --> src/main.rs:2:9
  |
2 |     let name = "world";
  |         ^^^^ help: if this is intentional, prefix it with an underscore: `_name`
  |
  = note: `#[warn(unused_variables)]` (part of `#[warn(unused)]`) on by default
```

È il messaggio del compilatore stesso, conservato da QuickRust quando la build è stata eseguita.

## Provoca un errore {#make-an-error}

Ora modifica la riga `println!` in modo che usi la variabile, ma scrivine male il nome:

```rust
fn main() {
    let name = "world";
    println!("Hello, {}!", nme);
}
```

Esegui il programma:

```bash
cargo run
```

Questa volta l'errore viene mostrato per intero, sotto un'intestazione che indica il crate a cui appartiene, e il riepilogo dice `FAIL`:

```text
--- hello (bin)
error[E0425]: cannot find value `nme` in this scope
 --> src/main.rs:3:28
  |
3 |     println!("Hello, {}!", nme);
  |                            ^^^
  |
help: a local variable with a similar name exists
  |
3 |     println!("Hello, {}!", name);
  |                             +

FAIL Build: 1 error in hello (bin), first at src/main.rs:3:28 [41ms]
```

Il programma non è stato eseguito. Correggi `nme` in `name` ed eseguilo di nuovo:

```bash
cargo run
```

```text
OK   Built: 1 compiled [59ms]
Hello, world!
```

Anche il warning è sparito, perché ora la variabile viene usata.

## Aggiungi una dipendenza {#add-a-dependency}

Il nostro programma stamperà un numero con l'aiuto di un piccolo crate, `itoa`. Aggiungilo:

```bash
cargo add itoa
```

QuickRust mostra la dipendenza che ha aggiunto e le feature che offre, poi il risultato:

```text
     added      itoa                     1.0.18 to dependencies
     features -no-panic
     locked +itoa 1.0.18
OK   [117ms]
```

Potresti vedere una versione più recente di `1.0.18`. Le righe che appartengono a un esito sono rientrate sotto di esso, e la riga `OK` non ha nulla da aggiungere se non il tempo: le righe sopra dicono già che cosa è successo.

Sostituisci `src/main.rs` con un programma che usa il crate:

```rust
fn main() {
    let mut buffer = itoa::Buffer::new();
    println!("The answer is {}", buffer.format(answer()));
}

fn answer() -> u32 {
    6 * 7
}
```

Eseguilo:

```bash
cargo run
```

QuickRust scarica il crate, riporta il download in una riga e compila entrambi i crate:

```text
     Downloaded 1 crate, 15.6 KiB [0.08s]
OK   Built: 2 compiled [241ms]
The answer is 42
```

## Pulisci e compila di nuovo {#clean-and-build-again}

Rimuovi l'output di build:

```bash
cargo clean
```

```text
OK   Removed 123 files, 21.7 MiB
```

Poi esegui il programma ancora una volta:

```bash
cargo run
```

```text
OK   Built: 1 compiled, 1 restored from QuickRust's cache [119ms]
The answer is 42
```

È stato compilato solo il nostro crate. QuickRust aveva conservato `itoa` dalla prima build e l'ha ripristinato invece di compilarlo di nuovo; ogni progetto su questa macchina che usa lo stesso crate, compilato allo stesso modo, lo condivide. (Perché questo sia sicuro è l'argomento de [La cache degli artefatti](../explanation/artifact-cache.md).)

## Aggiungi un test {#add-a-test}

Aggiungi un test in fondo a `src/main.rs`:

```rust
#[test]
fn the_answer_is_42() {
    assert_eq!(answer(), 42);
}
```

Esegui i test:

```bash
cargo test
```

QuickRust prima compila, come prima; poi i test vengono eseguiti e il loro output viene mostrato così come lo ha stampato l'harness dei test, e un'ultima riga somma i risultati:

```text
OK   Built: 1 compiled, 1 fresh [73ms]
     unittests src/main.rs

running 1 test
test the_answer_is_42 ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

OK   Tests: 1 passed [12ms]
```

## Che cosa abbiamo fatto {#what-we-have-done}

Abbiamo creato un programma, lo abbiamo compilato ed eseguito, abbiamo letto un warning che QuickRust ha conservato per noi, abbiamo corretto un errore a partire dal suo messaggio completo, abbiamo aggiunto una dipendenza, abbiamo visto QuickRust ripristinarla dalla sua cache dopo una pulizia e abbiamo eseguito un test. Per tutto il tempo, ogni esito è stato una riga che iniziava con `OK`, `WARN` o `FAIL`.

Tutto ciò che abbiamo digitato era un normale comando di Cargo: QuickRust prende il posto di `cargo` ed esegue sotto di sé il Cargo della tua toolchain Rust.

## Dove andare ora {#where-to-go-next}

- [Scoprire dove va il tempo di una build](where-the-time-goes.md), il tutorial successivo, guarda all'interno di una build più grande.
- [Come vedere i warning, gli errori e il log completo di una build](../how-to/read-warnings-and-errors.md) e [Come trovare gli output di build senza target/](../how-to/find-build-outputs.md) per il lavoro di tutti i giorni.
- [L'output delle build di QuickRust](../explanation/build-output.md) e [Come funziona QuickRust](../explanation/architecture.md) per le ragioni dietro ciò che hai visto.
