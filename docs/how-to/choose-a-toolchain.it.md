# Come scegliere e gestire le toolchain

Questa guida mostra come decidere quale toolchain Rust usa una build, aggiungerle componenti e target, usare una toolchain compilata da te e controllare gli aggiornamenti.

QuickRust installa ogni toolchain di cui ha bisogno al primo utilizzo, quindi la maggior parte dei passaggi seguenti si limita a dichiarare ciò che vuoi; il primo comando successivo che ne ha bisogno la installa. L'elenco completo dei comandi per le toolchain si trova in [Comandi](../reference/commands.md).

## Vedi quale toolchain usa una directory {#see-which-toolchain-a-directory-uses}

```bash
qrust toolchain list
```

Le ultime due righe indicano qual è la toolchain predefinita e quale toolchain usa la directory corrente, e perché:

```text
default: stable
here:    1.80 (from /home/me/app/rust-toolchain.toml)
```

`qrust toolchain which` stampa la directory di quella toolchain, e `qrust toolchain which rustc` il percorso di uno dei suoi programmi.

QuickRust sceglie nell'ordine di rustup: `+<toolchain>` come primo argomento, poi `RUSTUP_TOOLCHAIN`, poi un file di toolchain nella directory o in quella più vicina sopra di essa, poi la predefinita.

## Fissa una toolchain per un progetto {#pin-a-toolchain-for-a-project}

Metti un `rust-toolchain.toml` nella radice del progetto:

```toml
[toolchain]
channel = "1.98"
components = ["rust-src", "llvm-tools"]
targets = ["wasm32-unknown-unknown"]
profile = "minimal"
```

Solo `channel` è obbligatorio. Il comando successivo in quella directory installa la toolchain, i componenti elencati e le librerie standard dei target elencati. Una chiave che il formato non prevede è un errore, e lo è anche una directory che contiene sia `rust-toolchain` sia `rust-toolchain.toml`.

`channel` accetta `stable`, `beta`, `nightly`, una nightly datata (`nightly-2026-10-11`), una release minor (`1.98`, che segue le sue release di patch) o una release esatta (`1.98.1`), eventualmente seguita da una host triple. Se la toolchain si trova in una directory propria, usa `path = "../my-toolchain"` invece di `channel`; componenti, target e profilo non si applicano a essa.

## Usa un'altra toolchain per un solo comando {#use-another-toolchain-for-one-command}

```bash
cargo +nightly build
cargo +1.80 test
RUSTUP_TOOLCHAIN=beta cargo check
```

Gli editor e gli strumenti che impostano `RUSTUP_TOOLCHAIN`, anche alla directory di una toolchain, funzionano allo stesso modo.

## Cambia la predefinita {#change-the-default}

```bash
qrust toolchain default 1.98
qrust toolchain default            # stampa la predefinita attuale
```

La predefinita si applica ovunque non decidano un `+toolchain`, `RUSTUP_TOOLCHAIN` o un file di toolchain. Può essere un canale, una release o il nome di una toolchain collegata.

## Aggiungi componenti e target {#add-components-and-targets}

```bash
qrust toolchain component add rust-analyzer llvm-tools
qrust toolchain target add aarch64-unknown-linux-musl
qrust toolchain target add wasm32-unknown-unknown --toolchain nightly
```

Senza `--toolchain`, modificano la toolchain usata dalla directory corrente. `remove` al posto di `add` li toglie di nuovo.

Raramente ti serve `target add`: una build con `--target <triple>` installa da sé la libreria standard di quel target.

## Installa le toolchain in anticipo {#install-toolchains-ahead-of-time}

```bash
qrust toolchain install stable 1.80 nightly
qrust toolchain install nightly --profile minimal --component miri --target wasm32-unknown-unknown
```

Fallo dove la prima build non deve attendere un download, per esempio quando costruisci un'immagine di container. `--profile` è `minimal` (rustc, Cargo, la libreria standard), `default` (il predefinito: `minimal` con clippy, rustfmt e rust-src) oppure `complete`.

## Usa una toolchain compilata da te {#use-a-toolchain-you-built-yourself}

```bash
qrust toolchain link dev ~/src/rust/build/host/stage1
cargo +dev build
```

La directory deve contenere `bin/rustc` e `bin/cargo`. Una toolchain collegata viene usata così com'è: QuickRust non vi installa mai componenti o target, e `qrust toolchain unlink dev` si limita a dimenticarne il nome.

## Scarica le toolchain da un mirror {#download-toolchains-from-a-mirror}

Se la tua rete non riesce a raggiungere static.rust-lang.org, indirizza QuickRust a un suo mirror, in `~/.qrust/config.toml`:

```toml
[toolchain]
dist-server = "https://rust-mirror.example.com"
```

oppure, per una sola shell, con `QRUST_DIST_SERVER`. Il mirror deve servire la stessa struttura (`dist/channel-rust-<channel>.toml` e gli archivi che nomina).

## Mantieni aggiornate le toolchain {#keep-toolchains-current}

Una volta al giorno, dopo una build, QuickRust controlla in background se il canale usato dalla build (`stable`, `beta`, `nightly`, `1.98`) ha una release più recente, e la installa per la build successiva. Le toolchain richieste con un nome esatto (`1.98.1`, `nightly-2026-10-11`) non cambiano mai.

- Per aggiornare subito: `qrust toolchain update`, oppure `qrust toolchain update --channel stable` per un solo canale.
- Per disattivare il controllo in background, imposta `toolchain.auto-update = "off"` in `~/.qrust/config.toml` oppure `QRUST_TOOLCHAIN_AUTO_UPDATE=off`. Non viene mai eseguito in CI.
- Per restare su una release, fissala con una versione esatta in `rust-toolchain.toml`.

Una release lasciata indietro da un canale viene rimossa dopo un giorno in cui nulla l'ha usata. Per rimuovere una toolchain da te:

```bash
qrust toolchain remove nightly                         # la release indicata dal canale
qrust toolchain remove 1.97.0-x86_64-unknown-linux-gnu
```

Una toolchain rimossa viene installata di nuovo la volta successiva che qualcosa la richiede.

## Vedi anche {#related}

- [Perché QuickRust possiede il proprio ambiente](../explanation/owning-the-environment.md) per sapere perché QuickRust sostituisce rustup
- [File e directory](../reference/files.md) per sapere dove sono conservate le toolchain
- [Configurazione](../reference/configuration.md) per `toolchain.auto-update` e `toolchain.dist-server`
