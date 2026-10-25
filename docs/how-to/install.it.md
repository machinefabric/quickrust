# Come installare, aggiornare e disinstallare QuickRust

Questa guida mostra come installare QuickRust su una macchina, mantenerlo aggiornato, verificare che funzioni e rimuoverlo di nuovo.

QuickRust è l'unica cosa che installi: porta con sé le proprie toolchain Rust, quindi non ti serve rustup. Per sapere che cosa cambia setup e perché, vedi [Perché QuickRust possiede il proprio ambiente](../explanation/owning-the-environment.md).

## Installa una release {#install-a-release}

QuickRust è pubblicato nel repository di pacchetti di MachineFabric e come archivio per ciascuna piattaforma. Ognuno di questi mette il programma `qrust` sulla tua macchina; `qrust setup` lo rende poi il tuo `cargo`.

Con Homebrew (macOS, Linux):

```bash
brew install machinefabric/tap/quickrust
qrust setup
```

Con apt (Debian, Ubuntu):

```bash
curl -fsSL https://release.machinefabric.com/packages/keys/repo-signing-prod.asc \
  | sudo gpg --dearmor -o /usr/share/keyrings/machinefabric.gpg
echo "deb [signed-by=/usr/share/keyrings/machinefabric.gpg] https://release.machinefabric.com/packages/apt stable main" \
  | sudo tee /etc/apt/sources.list.d/machinefabric.list
sudo apt update && sudo apt install quickrust
qrust setup
```

Con dnf (Fedora, RHEL):

```bash
sudo tee /etc/yum.repos.d/machinefabric.repo <<'REPO'
[machinefabric]
name=MachineFabric
baseurl=https://release.machinefabric.com/packages/rpm
enabled=1
gpgcheck=1
gpgkey=https://release.machinefabric.com/packages/keys/repo-signing-prod.asc
REPO
sudo dnf install quickrust
qrust setup
```

Da un archivio (su qualsiasi piattaforma, ed è il metodo su Windows). Ogni release è elencata nel manifest delle release, `https://release.machinefabric.com/quickrust/manifest`, con le piattaforme per cui è stata compilata e lo SHA-256 di ciascun archivio. L'archivio di una piattaforma si trova in

```text
https://release.machinefabric.com/quickrust/release/<version>/qrust-<version>-<platform>.tar.gz
https://release.machinefabric.com/quickrust/release/<version>/qrust-<version>-windows-x86_64.zip
```

dove `<platform>` è uno tra `darwin-arm64`, `darwin-x86_64`, `linux-x86_64`, `linux-arm64` (entrambe le build Linux sono statiche e funzionano su qualsiasi distribuzione) (ciascuno un `.tar.gz`) e `windows-x86_64` (uno `.zip`). Scaricalo, confronta il suo SHA-256 con quello del manifest, estrailo ed esegui setup da lì:

```bash
version=0.150.0          # l'ultima versione del canale release è nel manifest
platform=linux-x86_64
curl -fLO "https://release.machinefabric.com/quickrust/release/$version/qrust-$version-$platform.tar.gz"
sha256sum "qrust-$version-$platform.tar.gz"      # confrontalo con lo sha256 del manifest (macOS: shasum -a 256)
tar xzf "qrust-$version-$platform.tar.gz"          # estrae quickrust/
quickrust/qrust setup
```

Su Windows (PowerShell):

```powershell
$version = "0.150.0"
Invoke-WebRequest "https://release.machinefabric.com/quickrust/release/$version/qrust-$version-windows-x86_64.zip" -OutFile qrust.zip
(Get-FileHash qrust.zip -Algorithm SHA256).Hash      # confrontalo con lo sha256 del manifest
Expand-Archive qrust.zip -DestinationPath .          # estrae quickrust\
.\quickrust\qrust.exe setup
```

`qrust setup` rende `~/.qrust/bin` di QuickRust e installa Rust stable. Il modo dipende da che cosa ha installato il `qrust` da cui lo avvii:

- **Un gestore di pacchetti** (Homebrew, apt, dnf): setup non copia nulla. `~/.qrust/bin/qrust`, `qrust-rustc-wrapper`, `cargo` e gli altri programmi della toolchain sono link al `qrust` del pacchetto, tramite il percorso che resta lo stesso tra le sue versioni (`opt/quickrust` di Homebrew). Sulla macchina c'è un solo `qrust`, e l'aggiornamento del gestore di pacchetti aggiorna quello in esecuzione. Una copia che un setup precedente ha messo in `~/.qrust/bin` viene sostituita dal link, e setup lo dice.
- **Un archivio o una build dal sorgente**: setup copia `qrust` e `qrust-rustc-wrapper` in `~/.qrust/bin`, e quella copia è la tua installazione, quindi la directory estratta o il checkout possono essere eliminati in seguito. Non si installa sopra l'installazione di un pacchetto: rimuovi prima il pacchetto.

Gli argomenti scelgono che cosa fa setup ([più sotto](#choose-what-setup-does)):

```bash
qrust setup --no-modify-path
```

Se il manifest non elenca la tua piattaforma per una release, quella release non ha una build per essa: installa invece dal sorgente.

Quando setup ha terminato, apri una nuova shell, oppure esegui `source` sul file che indica:

```bash
. "$HOME/.qrust/env"
```

## Installa dal sorgente {#install-from-source}

Per questa sola build ti serve una toolchain Rust 1.80 o più recente:

```bash
git clone https://github.com/machinefabric/quickrust
cd quickrust
cargo build --release --locked
target/release/qrust setup
```

Mantieni `--locked`: senza di esso Cargo sceglie le versioni più recenti delle dipendenze, e alcune richiedono un Rust più recente della 1.80. `qrust setup` copia `qrust` e `qrust-rustc-wrapper` in `~/.qrust/bin`, quindi il checkout può essere eliminato in seguito.

## Scegli che cosa fa setup {#choose-what-setup-does}

- Se gestisci `PATH` da solo (dotfile, CI, container), passa `--no-modify-path` e metti tu stesso `~/.qrust/bin` per primo in `PATH`:

  ```bash
  qrust setup --no-modify-path
  export PATH="$HOME/.qrust/bin:$PATH"
  ```

- Se vuoi come prima toolchain una diversa da stable, passala: `qrust setup --toolchain 1.98`. Se non ne vuoi installare nessuna adesso, passa `--toolchain none`; la prima build installerà poi la toolchain di cui ha bisogno.
- Se vuoi la home di QuickRust in una posizione diversa da `~/.qrust`, imposta `QRUST_HOME` prima di eseguire setup e mantienila impostata in ogni shell.

Setup importa, una sola volta, ciò che è tuo da `~/.cargo` (o `$CARGO_HOME`): le credenziali dei registry e le tabelle `[registries]`, `[registry]`, `[source]`, `[net]` e `[http]` del suo `config.toml`. Stampa ciò che ha importato e ciò che non ha importato. Una tabella che non ha importato (per esempio `[build]`) si applica comunque ai progetti sotto la tua directory home, perché Cargo legge `~/.cargo/config.toml` come configurazione di ogni directory sottostante; per i progetti altrove non si applica. Se vuoi una tabella del genere per ogni build, copiala in `~/.qrust/cargo/config.toml`. Eseguire di nuovo `qrust setup` ripara un'installazione danneggiata e non importa mai una seconda volta.

Se setup si ferma perché una voce in `~/.qrust/bin` "esiste e non è QuickRust", sposta altrove quel file ed esegui di nuovo setup.

## Verifica l'installazione {#check-the-installation}

```bash
qrust doctor
```

`doctor` stampa quale QuickRust è in esecuzione e che cosa l'ha installato, la toolchain che usa la directory corrente e che cosa l'ha selezionata, il Cargo e il rustc che stanno dietro, le variabili d'ambiente rilevanti e la directory target del workspace. Termina con `OK`, oppure con `FAIL` e il numero di problemi (exit status 1), ciascun problema sulla propria riga `FAIL` più sopra.

Indica ogni `qrust` o `cargo` che in `PATH` viene prima di quello di QuickRust ed è un altro programma, e la riga dei file di avvio della tua shell che vi mette la sua directory (seguendo i file che questi includono). I casi comuni sono `~/.cargo/bin` (il `cargo` di rustup) e il `bin` di Homebrew, da una riga `brew shellenv` eseguita dopo `~/.qrust/env`: il suo `cargo` se è installata la formula `rust` di Homebrew, e il suo `qrust` se `~/.qrust/bin` contiene ancora una copia di QuickRust di prima (esegui `qrust setup` dall'installazione di Homebrew, e la copia diventa un link a essa). Sposta la riga che include `~/.qrust/env` dopo quella indicata, oppure rimuovi ciò che indica. Se `doctor` dice che nessun `cargo` in `PATH` è quello di QuickRust, apri una nuova shell.

## Aggiorna QuickRust {#update-quickrust}

```bash
qrust self update
```

Installa l'ultima release del canale su cui è stato pubblicato questo QuickRust (`release` o `nightly`; `qrust doctor` dice quale, nella sua `release line`), al posto di quello in esecuzione in `~/.qrust/bin`. Se sei già all'ultima release, lo dice e non cambia nulla.

Un QuickRust installato da un gestore di pacchetti viene aggiornato da quel gestore, e `qrust self update` non cambia nulla e indica il comando: `brew upgrade quickrust`, `sudo apt install --only-upgrade quickrust` o `sudo dnf upgrade quickrust`. Poiché `~/.qrust/bin` è un link al `qrust` del pacchetto, quell'aggiornamento aggiorna il QuickRust in esecuzione.

Nulla viene installato sulla fiducia. QuickRust legge prima il charter del registro, che deve essere firmato da MachineFabric e dire dove si trova il manifest delle release; poi il manifest delle release e la firma pubblicata accanto a esso, e accetta il manifest solo se la sua firma risale alle chiavi radice con cui questo QuickRust è stato compilato: una chiave di release certificata da due di quelle radici, per l'ambiente giusto, entro il suo periodo di validità, deve aver firmato esattamente i byte del manifest. L'archivio deve poi essere quello descritto dal manifest (SHA-256 e dimensione), e il binario al suo interno deve avviarsi e riportare la versione con cui è stato pubblicato. Se uno qualsiasi di questi controlli fallisce, l'aggiornamento si ferma indicando il motivo e non sostituisce nulla. Vedi [Sicurezza e privacy](../explanation/security.md#updates).

Un QuickRust compilato dal sorgente non ha chiavi radice, e quindi nessuna release che possa verificare: `qrust self update` lo dice. Aggiornalo compilando il sorgente più recente.

Se le tue release provengono da un altro registro delle release, imposta `self.update-url` (o `QRUST_UPDATE_URL`) sulla sua ancora. Viene letto esattamente come il registro in cui è stato pubblicato questo QuickRust: solo dopo che MachineFabric gli ha dato un charter (il suo charter si trova in `<url>/charter.json`, per l'ambiente di questo QuickRust), e poi in base alla firma, non a dove si trova.

Le toolchain vengono aggiornate separatamente: vedi [Come scegliere e gestire le toolchain](choose-a-toolchain.md#keep-toolchains-current).

## Disinstalla QuickRust {#uninstall-quickrust}

```bash
qrust self uninstall --yes
```

Questo rimuove `~/.qrust` con tutto il suo contenuto (toolchain, programmi installati con `qrust install`, cache, registrazioni delle esecuzioni) e le righe che setup ha aggiunto ai file di avvio della tua shell (su Windows, la voce nel `PATH` dell'utente). Senza `--yes` si limita a dirti che cosa rimuoverebbe.

Un QuickRust installato da un gestore di pacchetti viene rimosso da quel gestore (`brew uninstall quickrust`, `sudo apt remove quickrust`, `sudo dnf remove quickrust`), e `qrust self uninstall` non cambia nulla e indica il comando. Il gestore di pacchetti rimuove solo il programma: elimina tu stesso `~/.qrust` e le righe marcate `# Added by `qrust setup`` nei file di avvio della tua shell.

I file che QuickRust ha scritto in un repository su tua richiesta restano dove sono: `.qrust/config.toml`, `.qrust/budgets.toml`, `.qrust/baselines/` e gli eventuali percorsi dei report che hai configurato. Eliminali tu stesso se non ti servono più.

rustup e `~/.cargo` non vengono mai toccati da setup o dalla disinstallazione; se li avevi prima, Cargo puro torna a funzionare non appena QuickRust non c'è più.

## Vedi anche {#related}

- [Come provare QuickRust prima di farne il tuo cargo](adopt-gradually.md)
- [Come eseguire QuickRust in CI](set-up-ci.md)
- [File e directory](../reference/files.md) per tutto ciò che si trova sotto `~/.qrust`
- [Comandi](../reference/commands.md) per ogni opzione di `setup`, `doctor` e `self`
