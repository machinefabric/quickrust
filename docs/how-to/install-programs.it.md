# Come installare programmi con qrust install

Questa guida ti mostra come installare programmi Rust da crates.io, da un altro registro, da un repository Git o da una directory locale, e come elencarli, aggiornarli e rimuoverli.

`cargo install` e `qrust install` sono lo stesso comando. QuickRust compila il programma come qualsiasi altra build (passando per la sua cache degli artefatti, quindi le dipendenze che hai già compilato in precedenza non vengono compilate di nuovo) e mette i binari in `~/.qrust/bin`, che `qrust setup` ha aggiunto al tuo `PATH`.

## Installa da crates.io {#install-from-cratesio}

```bash
cargo install ripgrep
cargo install ripgrep@14.1.1          # una versione esatta
cargo install ripgrep --version '^14' # un requisito di versione
cargo install cargo-nextest --locked  # con il lockfile con cui il crate è stato pubblicato
```

Senza una versione, QuickRust installa la release più recente che non sia stata ritirata (yanked), che non sia una pre-release e che funzioni con la versione di Rust della toolchain.

Usa `--locked` quando gli autori del programma lo consigliano: la build usa allora le versioni delle dipendenze che hanno pubblicato. Se il crate è stato pubblicato senza lockfile, QuickRust lo segnala e risolve le dipendenze da capo.

Più crate in una volta vengono installati uno dopo l'altro; una riga di riepilogo dice quanti sono riusciti, e lo stato di uscita è diverso da zero se qualcuno è fallito:

```bash
cargo install ripgrep fd-find bat
```

## Installa da un altro registro {#install-from-another-registry}

```bash
cargo install mytool --registry internal
```

Il registro deve essere configurato nella home di Cargo di QuickRust, `~/.qrust/cargo/config.toml`, dove `qrust setup` ha importato i tuoi registri; il suo token viene da `~/.qrust/cargo/credentials.toml` o dall'ambiente, come per Cargo. Il `.cargo/config.toml` di un progetto non viene consultato, perché la release viene risolta al di fuori di qualsiasi progetto. La sostituzione dei sorgenti (`[source]` con `replace-with`) si applica come in una build. `--index URL` non è supportato: indica il registro nella configurazione e usa `--registry`.

## Installa da Git o da una directory locale {#install-from-git-or-a-local-directory}

```bash
cargo install --git https://github.com/BurntSushi/ripgrep
cargo install --git https://github.com/BurntSushi/ripgrep --tag 14.1.1
cargo install --git https://example.com/tools.git --branch main mytool   # indica il pacchetto quando il repository ne contiene più di uno
cargo install --path .
```

`--git` richiede `git` nel `PATH`. `--path` compila il pacchetto dove si trova; se la directory è un workspace con più pacchetti che hanno dei binari, indica quello che vuoi.

## Scegli che cosa viene compilato e dove va a finire {#choose-what-gets-built-and-where-it-goes}

- `--bin NAME` (ripetibile) o `--bins`, `--example NAME` o `--examples` selezionano i target.
- `--features F`, `--all-features`, `--no-default-features` selezionano le feature.
- `--profile P` o `--debug` cambiano il profilo (release per impostazione predefinita); `--target T` esegue una compilazione incrociata.
- `--root DIR` installa in `DIR/bin` invece che in `~/.qrust/bin`. `CARGO_INSTALL_ROOT` non viene usata: se è impostata, il comando si ferma e chiede `--root`.
- `-j`, `--config`, `--offline`, `--frozen`, `--ignore-rust-version`, `-q` e `-v` vengono passati alla build.

## Sostituisci o reinstalla {#replace-or-reinstall}

Installare di nuovo un programma con la stessa versione, la stessa origine, le stesse feature, lo stesso profilo e lo stesso target non fa nulla:

```text
OK   tool 0.1.0 is installed already (--force installs it again) [10ms]
```

Una versione più recente, o opzioni diverse, installano sopra quella vecchia. QuickRust si rifiuta di sovrascrivere un file nella directory bin che sia stato installato da un altro pacchetto, o che non abbia installato affatto lui, a meno che tu non passi `--force`. Non installa mai sopra i propri programmi (`cargo`, `rustc`, `qrust` e gli altri nomi della toolchain), nemmeno con `--force`.

## Elenca e rimuovi i programmi {#list-and-remove-programs}

```bash
cargo install --list
cargo uninstall ripgrep
cargo uninstall ripgrep --bin rg      # un solo binario di un pacchetto
cargo install --list --root ~/tools   # con --root, per quella radice
```

```text
tool v0.1.0 (/home/me/src/tool):
    tool
```

## Vedi anche {#related}

- [Comandi](../reference/commands.md) per ogni opzione di `install` e `uninstall`
- [File e directory](../reference/files.md) per `installs.json` e i sorgenti delle installazioni nella cache
