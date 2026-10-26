# Comandi

Tutti i comandi accettati dal binario `qrust`, i programmi della toolchain che sostituisce e gli stati di uscita che restituisce.

`qrust help` stampa lo stesso elenco in forma breve. Le opzioni dei comandi di QuickRust sono rigide: un'opzione che un comando non prevede è un errore (`` unexpected argument "..."; see `qrust help` ``), e `--format` accetta solo i valori elencati per il comando. Tutto ciò che segue un `--` letterale viene passato a Cargo.

## Punti di ingresso {#entry-points}

| Invocazione | Che cosa viene eseguito |
|---|---|
| `cargo <args...>` | qualsiasi comando di Cargo attraverso QuickRust, quando `~/.qrust/bin` è il primo elemento del `PATH` (`qrust setup`) |
| `qrust <cargo-command> [args...]` | lo stesso, con il nome di QuickRust; qualsiasi primo argomento che non sia uno dei comandi di QuickRust elencati sotto (compresi `+toolchain` e i flag globali di Cargo) è sintassi di Cargo |
| `qrust upstream <args...>` | il Cargo della toolchain, eseguito così com'è: nessuna impostazione della fast mode, nessun wrapper di rustc, nessuna resa dell'output, nulla di registrato; la configurazione di QuickRust non viene letta, quindi funziona anche quando quella configurazione non è valida. Usa comunque la toolchain di QuickRust, la sua Cargo home e la directory di build del workspace |
| `qrust help`, `qrust --help`, `qrust -h`, `qrust` | l'elenco dei comandi |
| `qrust version`, `qrust --version`, `qrust -V` | `qrust <version>` |

### Programmi della toolchain {#toolchain-programs}

`~/.qrust/bin` contiene `cargo` e questi nomi come collegamenti al binario `qrust`. Quando viene eseguito con uno di questi nomi, QuickRust esegue quel programma della toolchain selezionata dalla directory corrente, installando al primo utilizzo la toolchain e il componente che fornisce il programma. Un argomento iniziale `+<toolchain>` seleziona la toolchain, come per `cargo`.

| Programma | Componente installato al primo utilizzo |
|---|---|
| `rustc`, `rustdoc`, `rust-gdb`, `rust-gdbgui`, `rust-lldb` | nessuno (fanno parte della toolchain) |
| `rustfmt`, `cargo-fmt` | `rustfmt` |
| `cargo-clippy`, `clippy-driver` | `clippy` |
| `rust-analyzer` | `rust-analyzer` |
| `cargo-miri` | `miri` |

Come viene selezionata la toolchain: [Configurazione](configuration.md#toolchain-selection).

## Comandi di Cargo {#cargo-commands}

QuickRust classifica ogni comando di Cargo in una famiglia. La famiglia decide se una sessione viene registrata, se si applicano le impostazioni della fast mode e come viene presentato l'output nel terminale ([Output](output.md)).

| Famiglia | Comandi (e alias di Cargo) | Registrato, osservato tramite il wrapper di rustc | Impostazioni del profilo della fast mode |
|---|---|---|---|
| build | `build` (`b`), `check` (`c`), `test` (`t`), `bench`, `run` (`r`), `doc` (`d`), `rustdoc`, `rustc`, `clippy`, `fix` | sì | sì |
| clean | `clean` | registrato | no |
| passthrough | `add`, `remove` (`rm`), `update`, `fetch`, `vendor`, `generate-lockfile`, `publish`, `package`, `login`, `logout`, `owner`, `yank`, `new`, `init` | registrato | no |
| query | `metadata`, `tree`, `search`, `info`, `locate-project`, `pkgid`, `verify-project`, `read-manifest`, `version`, `help`, `config`, `report`, `fmt` | no: Cargo sostituisce QuickRust (`tree` e `metadata` in un terminale vengono resi e registrati) | no |
| external | qualsiasi altro comando (`cargo nextest`, `cargo llvm-cov`, ...) | registrato; osservato solo con `compatibility.unknown-command = "instrument"` | no |

`cargo install` e `cargo uninstall` sono comandi di QuickRust (vedi sotto). `cargo fix` è di Cargo; `qrust fix` di QuickRust è un comando diverso.

## Programmi {#programs}

```text
qrust install <CRATE>[@VERSION]... [--git URL [--branch B | --tag T | --rev R] | --path DIR]
              [--locked] [--force] [--features F] [--bin NAME]... [--root DIR] [--profile P] [--target T]
qrust install --list
qrust uninstall <CRATE>... [--bin NAME]... [--root DIR]
```

`cargo install` e `cargo uninstall` portano agli stessi comandi. QuickRust compila il pacchetto attraverso il proprio percorso di build e colloca i suoi binari in `~/.qrust/bin`, oppure in `<DIR>/bin` con `--root`. Guida: [Come installare programmi](../how-to/install-programs.md).

| Opzione | Significato |
|---|---|
| `<CRATE>[@VERSION]` | un pacchetto dal registry; se ne possono indicare diversi. `@VERSION` (o `--version`/`--vers` con esattamente un crate) è un requisito di versione; una versione semplice indica esattamente quella versione |
| `--git URL`, con `--branch`, `--tag` o `--rev` | un pacchetto da un repository Git (richiede `git` nel `PATH`) |
| `--path DIR` | un pacchetto locale, compilato sul posto |
| `--registry NAME` | un registry indicato nella configurazione di Cargo; `--index` viene rifiutato |
| `--locked` | compila con il lockfile: per un crate del registry, quello con cui è stato pubblicato; altrimenti il `Cargo.lock` del pacchetto stesso. Senza questa opzione, le dipendenze di un crate del registry vengono risolte da capo |
| `-f`, `--force` | sostituisce i binari installati da un altro pacchetto |
| `-F`, `--features F`, `--all-features`, `--no-default-features` | feature |
| `--bin NAME`, `--bins`, `--example NAME`, `--examples` | quali eseguibili installare |
| `--root DIR` | installa in `DIR/bin` invece che in `~/.qrust/bin` |
| `--profile P`, `--debug` | profilo di build (predefinito `release`; `--debug` equivale a `dev`) |
| `--target T` | tripla del target |
| `--list` | che cosa è installato, nel formato di Cargo |
| `--no-track` | non registra l'installazione in `installs.json` |
| `-j`, `--jobs`, `--config`, `--offline`, `--frozen`, `--ignore-rust-version`, `-q`, `--quiet`, `-v`, `-vv`, `--verbose` | passate alla build |

Qualsiasi altra opzione è un errore.

Impostare `CARGO_INSTALL_ROOT` senza `--root` è un errore. I binari con lo stesso nome dei programmi di QuickRust (`cargo`, `qrust`, i programmi della toolchain) non vengono mai installati. `qrust uninstall` accetta `--bin NAME` e `--root DIR`. Le installazioni vengono registrate in `~/.qrust/installs.json`.

## Output di una build {#output-of-a-build}

Viene salvato per ogni build che QuickRust ha reso in un terminale ([Output](output.md#stored-output)). `--run REF` seleziona l'esecuzione (predefinita `latest`; vedi [Riferimenti alle esecuzioni](#run-references)).

| Comando | Stampa |
|---|---|
| `qrust warnings [--run REF] [--crate NAME] [--lint LINT]` | i warning dell'esecuzione come li ha resi rustc; `--crate` corrisponde al nome del target o al pacchetto, `--lint` al nome del lint (`dead_code`, `clippy::len_zero`) |
| `qrust errors [--run REF] [--crate NAME]` | gli errori dell'esecuzione |
| `qrust log [--run REF] [--raw]` | ogni riga salvata, in ordine, senza colori; `--raw` stampa le righe come le ha scritte Cargo (compresi i messaggi JSON di Cargo) |

## Analisi {#analysis}

| Comando | Stampa |
|---|---|
| `qrust analyze [SECTION] [--run REF] [--format text\|json] [--filter FIELD=VALUE]... [--sort FIELD[:asc\|desc]] [--limit N]` | l'analisi di un'esecuzione registrata: tutte le sezioni, o una sola |
| `qrust report [--run REF] [--format text\|json\|html] [-o FILE]` | il report completo; `-o` (o `--output`) lo scrive in `FILE` invece che su stdout. `json` e `html` sono documenti esportati (percorsi tokenizzati, [Schema del report di esecuzione](report-schema.md)) |
| `qrust runs [--limit N] [--format text\|json]` | le esecuzioni registrate di questo workspace, dalla più recente (predefinito 20) |
| `qrust inspect [--format text\|json]` | il workspace, la toolchain e il fingerprint dell'ambiente, misurati in questo momento |
| `qrust driver [status\|build] [--rustc PATH]` | il driver del compilatore di QuickRust per la toolchain usata qui (o per il compilatore in `PATH`): `status` (predefinito) indica se è compilato, `build` lo compila e ne stampa il percorso |
| `qrust compare <BASE> <CANDIDATE> [--format text\|json]` | le metriche di due esecuzioni, baseline o benchmark affiancate, con la loro confrontabilità |

Sezioni di `qrust analyze`: `timing`, `rebuilds`, `duplication`, `scripts`, `proc-macros`, `codegen`, `cache`, `storage`, `recommendations` e `worktrees` (unità identiche compilate in altri worktree e workspace; non accetta `--run`, `--filter`, `--sort` né `--limit`).

`--filter`, `--sort` e `--limit` si applicano ai record di una sezione e richiedono una sezione:

- `--filter FIELD=VALUE` conserva i record il cui campo è uguale al valore o, per un campo lista, lo contiene; più filtri devono essere soddisfatti tutti; un campo che nessun record possiede è un errore che elenca i campi esistenti.
- `--sort FIELD[:asc|desc]` ordina in base a un campo: i numeri in ordine decrescente e il testo in ordine crescente, a meno che non sia indicata una direzione.
- `--limit N` conserva i primi `N` record (`N` almeno 1).

```bash
qrust analyze timing --filter source=registry --sort duration_ns --limit 10
```

L'analisi è pigra: un'esecuzione viene analizzata quando uno di questi comandi ne ha bisogno per la prima volta. Un'esecuzione analizzata dopo build successive riporta come non disponibile il proprio ingombro di artefatti.

## Misurazione {#measurement}

```text
qrust benchmark [--scenario NAME] [--reps N] [--warmups N] [--edit FILE] [--allow-mutation]
                [--timeout DURATION] [--format text|json] [-- <cargo args>]
qrust benchmark --list
qrust experiment [codegen|linker] [--name NAME] [--set KEY=VALUE]... [--env KEY=VALUE]...
                 [--codegen-units N] [--debug VALUE] [--opt-level VALUE] [--lto VALUE] [--linker NAME]
                 [--scenario NAME] [--reps N] [--warmups N] [--verify '<cargo args>'] [-- <cargo args>]
```

Scenari, valori predefiniti, il journal delle mutazioni, le esecuzioni scartate e i verdetti degli esperimenti: [Benchmark](benchmarks.md).

## Budget {#budgets}

| Comando | Che cosa fa |
|---|---|
| `qrust baseline set NAME [--run REF \| --benchmark ID]` | scrive `<workspace>/.qrust/baselines/NAME.json` a partire da un'esecuzione (predefinita `latest`) o da un benchmark; un'esecuzione fallita viene rifiutata |
| `qrust baseline list` | le baseline in `.qrust/baselines/` |
| `qrust baseline show NAME` | una baseline in formato JSON |
| `qrust ci evaluate [--run REF \| --benchmark ID] [--baseline NAME] [--budgets FILE] [--format text\|json]` | valuta i budget (predefinito `ci.budget-policy`) per un'esecuzione o un benchmark rispetto a una baseline (predefinita `ci.baseline`); una baseline mancante è un errore solo quando un budget è relativo |

`--run` e `--benchmark` si escludono a vicenda. Formato del file dei budget e verdetti: [Budget](budgets.md).

## Spazio su disco {#storage}

| Comando | Che cosa fa |
|---|---|
| `qrust inventory [--caches] [--older-than DAYS] [--max-size SIZE] [--format text\|json]` | che cosa contiene la directory di build di questo workspace, per categoria, e un piano per ciò che non è più attuale (ciò che è più vecchio di `DAYS`, predefinito `artifacts.stale-days`, o che supera `SIZE`); non elimina nulla. `--caches` aggiunge le cache condivise |
| `qrust gc [--dry-run] [--to SIZE] [--format text\|json]` | esegue subito la pulizia della cache di QuickRust ([File](files.md#quickrusts-cache)); `--dry-run` elenca che cosa rimuoverebbe, `--to SIZE` riduce la cache fino a `SIZE` |

Le dimensioni sono un numero con un'unità facoltativa `B`, `K`/`KB`/`KiB`, `M`/`MB`/`MiB`, `G`/`GB`/`GiB`, `T`/`TB`/`TiB` (tutte potenze di 1024).

## Policy e correzioni {#policy-and-remediation}

| Comando | Che cosa fa |
|---|---|
| `qrust policy explain [--format text\|json]` | ogni chiave di configurazione con il suo valore effettivo, la variabile d'ambiente, l'origine e la descrizione ([Configurazione](configuration.md)) |
| `qrust cache status [--format text\|json]` | la cache di QuickRust: la sua dimensione per tipo secondo l'ultimo conteggio, i suoi limiti, lo spazio libero, le pulizie recenti; poi la cache del compilatore (sccache): policy, stato, statistiche del server, i risultati della cache dell'ultima esecuzione analizzata |
| `qrust fix --dry-run [REC_ID] [--run REF] [--option ALTERNATIVE] [--patch] [--format text\|json]` | anteprime delle correzioni per le raccomandazioni di un'esecuzione (una sola con `REC_ID`); `--patch` le mostra come patch; non viene scritto nulla |
| `qrust fix --apply REMEDIATION_ID` | applica una correzione di classe `safe` già vista in anteprima (solo la configurazione di QuickRust stesso); il file deve essere identico byte per byte a quello dell'anteprima |
| `qrust fix --rollback REMEDIATION_ID` | annulla una correzione applicata |

## Posizione dell'output di build {#build-output-location}

| Comando | Stampa |
|---|---|
| `qrust path` | la directory target di questo workspace (quella della variante di build in vigore, `build.variant`) |
| `qrust path bin <NAME> [--release \| --profile P] [--target T]` | il percorso in cui `cargo build` colloca il binario `NAME`, per `--target T`, altrimenti per il `build.target` della configurazione di Cargo, altrimenti per l'host; con il nome che il compilatore dà a un binario per quel target (`NAME.exe`, `NAME.wasm`) |
| `qrust path [bin …] --hold-for <PID>` | la stessa risposta, e la directory di build viene prenotata finché il processo `PID` è in esecuzione: la cache di QuickRust non la rimuove, per quanto a lungo sia rimasta inutilizzata ([Come trovare gli output di build](../how-to/find-build-outputs.md#use-a-builds-outputs-for-longer-than-the-build)). La prenotazione termina con il processo. `PID` deve essere in esecuzione. Una directory target scelta da te non viene mai rimossa, e prenotarla non ha alcun effetto |

I profili `dev` e `test` corrispondono a `debug`, `release` e `bench` a `release`, qualsiasi altro profilo al proprio nome. `--release` e `--profile` si escludono a vicenda.

## Toolchain {#toolchains}

| Comando | Che cosa fa |
|---|---|
| `qrust toolchain` o `qrust toolchain list` | le toolchain installate e collegate, quella predefinita, e la toolchain selezionata qui con ciò che l'ha selezionata |
| `qrust toolchain install <TOOLCHAIN>... [--profile minimal\|default\|complete] [--component C]... [--target T]...` | installa toolchain, componenti e target |
| `qrust toolchain update [--channel C]` | porta i canali `stable`, `beta`, `nightly` e `1.xx` (o solo `C`) alla loro release più recente, poi rimuove le release non indicate da alcun canale rimaste inutilizzate per un giorno |
| `qrust toolchain remove <TOOLCHAIN>...` | rimuove toolchain; il nome di un canale rimuove la release che indica |
| `qrust toolchain default [<TOOLCHAIN>]` | stampa o imposta la toolchain predefinita (un canale, una release o un nome collegato; non un percorso) |
| `qrust toolchain link <NAME> <DIR>` | dà un nome a una toolchain compilata altrove; `DIR` deve contenere `bin/rustc` e `bin/cargo`, e nessuno dei due può essere QuickRust. QuickRust non modifica mai una toolchain collegata |
| `qrust toolchain unlink <NAME>` | rimuove il nome |
| `qrust toolchain which [<TOOL>] [--toolchain T]` | la directory della toolchain selezionata (o indicata), oppure il percorso di uno dei suoi programmi |
| `qrust toolchain component add\|remove <NAME>... [--toolchain T]` | componenti della toolchain selezionata (o indicata) |
| `qrust toolchain target add\|remove <TRIPLE>... [--toolchain T]` | librerie standard per i target |

Profili: `minimal` (rustc, cargo, la libreria standard), `default` (`minimal` più clippy, rustfmt e rust-src), `complete` (tutti i componenti che la release offre per l'host). Nomi delle toolchain: [Configurazione](configuration.md#toolchain-names).

## Installazione {#installation}

| Comando | Che cosa fa |
|---|---|
| `qrust doctor` | stampa l'installazione, che cosa l'ha installata (un gestore di pacchetti, con il comando che la aggiorna, oppure essa stessa) e la sua linea di release (il canale e il registry che legge `qrust self update`, oppure il fatto che questo QuickRust è stato compilato dai sorgenti); ogni `qrust` o `cargo` che in `PATH` precede quello di QuickRust ed è un altro programma, come problema che indica la riga dei file di avvio che vi mette la sua directory; la toolchain attiva e ciò che l'ha selezionata, i suoi Cargo e rustc, la Cargo home di QuickRust, le variabili di Cargo e di rustc presenti nell'ambiente, le directory target e di build del workspace, la directory di stato, la policy della cache e sccache; ogni problema è una riga `FAIL`, e l'ultima riga è `OK` o `FAIL <n> problem(s)` |
| `qrust setup [--no-modify-path] [--toolchain T\|none]` | installa QuickRust per questo utente ([File](files.md#setup)): avviato dall'installazione di un gestore di pacchetti, collega `~/.qrust/bin` a essa (sostituendo una copia presente); avviato da qualsiasi altra cosa, vi si copia, a meno che `~/.qrust/bin` non sia collegato all'installazione di un pacchetto; `--toolchain` indica la toolchain da installare (predefinita `stable`, `none` per nessuna toolchain); eseguirlo di nuovo ripristina ciò che manca |
| `qrust self update` | installa la release più recente del canale di questo QuickRust al posto di quello in esecuzione, dal registry delle release in cui è stato pubblicato (o dal registry in `self.update-url`), dopo aver verificato il charter del registry, il suo manifest firmato, l'archivio e il binario ([Come installare, aggiornare e disinstallare QuickRust](../how-to/install.md#update-quickrust)); un QuickRust compilato dai sorgenti non ha una linea di release e lo dice; l'installazione di un gestore di pacchetti la aggiorna quel gestore, e il comando lo indica e non cambia nulla |
| `qrust self uninstall --yes` | rimuove `~/.qrust` e le righe del `PATH` aggiunte da `qrust setup`; senza `--yes` si limita a dire che cosa rimuoverebbe; l'installazione di un gestore di pacchetti la rimuove quel gestore, e il comando lo indica e non cambia nulla |

## Riferimenti alle esecuzioni {#run-references}

`--run REF` e gli argomenti di `qrust compare` accettano:

| Riferimento | Significato |
|---|---|
| `latest` | l'esecuzione di build registrata più recente di questo workspace |
| `latest~N` | l'`N`-esima esecuzione di build precedente |
| un id di esecuzione, o un suo prefisso o suffisso univoco | quell'esecuzione |
| `baseline:NAME` | una baseline (solo `qrust compare`) |
| `benchmark:ID` | il risultato di un benchmark (solo `qrust compare`) |

`latest` e `latest~N` contano le esecuzioni osservate direttamente dal wrapper di rustc; le sessioni annidate i cui eventi sono andati alla sessione madre vengono saltate.

## Stati di uscita {#exit-statuses}

| Stato | Significato |
|---|---|
| Quello di Cargo | ogni volta che Cargo è stato eseguito; un segnale di terminazione viene risollevato |
| 1 | un benchmark che non si è completato, un esperimento il cui verdetto è `invalid`, oppure `qrust doctor` con un problema |
| 2 | QuickRust non ha potuto eseguire il comando: nessuna toolchain utilizzabile, ricorsione in QuickRust, configurazione di QuickRust non valida, argomenti non validi per un comando di QuickRust; inoltre un record o un report non riuscito con `measurement.strict = true` |
| 3 | un budget bloccante non è stato rispettato (`qrust ci evaluate`, o una build con `ci.enforce-on-build`) |
| 4 | baseline e candidato non sono confrontabili e la policy dei budget indica `incomparable = "fail"` |

Uno stato di policy (2 da `measurement.strict`, 3, 4) sostituisce sempre e solo uno stato di Cargo di successo.
