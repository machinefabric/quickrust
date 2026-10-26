# Piattaforme

Dove QuickRust funziona e viene testato, che cosa differisce tra le piattaforme e che cosa non si può osservare su una toolchain stable.

## Supporto {#support}

| Piattaforma | Archivio di release (`<platform>` nel suo nome) | Test |
|---|---|---|
| Linux x86_64 | `linux-x86_64` (statico, musl) | ogni suite di test, su Ubuntu; a mano su Debian 12, anche come root |
| Linux aarch64 | `linux-arm64` (statico, musl) | compilato per ogni release; le suite di test non vengono eseguite |
| macOS aarch64 | `darwin-arm64` | ogni suite di test |
| macOS x86_64 | `darwin-x86_64` | compilato per ogni release; le suite di test non vengono eseguite |
| Windows x86_64 | `windows-x86_64` | un'esecuzione configura QuickRust da zero (Rust stable dal server di distribuzione, il driver del compilatore), poi crea, compila, esegue e testa un progetto; le suite di integrazione sono solo per Unix |

Un archivio si chiama `qrust-<version>-<platform>.tar.gz` (`.zip` per Windows) e si estrae in un'unica directory, `quickrust/`, che contiene `qrust`, `qrust-rustc-wrapper`, la licenza e il README. Il manifest delle release (`https://release.machinefabric.com/quickrust/manifest`) elenca, per ogni release, le piattaforme per cui è stata compilata: una piattaforma che non vi compare non ha una build di quella release.

QuickRust si compila con Rust 1.80 o più recente. Le toolchain che installa sono per l'host su cui gira; su Linux, `-gnu` o `-musl` a seconda della libreria C del sistema, quella con cui è stato compilato QuickRust stesso.

## Differenze tra le piattaforme {#platform-differences}

| Ambito | Linux | macOS | Windows |
|---|---|---|---|
| Programmi della toolchain in `~/.qrust/bin` | link simbolici | link simbolici | hard link |
| `PATH` configurato da `qrust setup` | file di avvio della shell | file di avvio della shell | il `PATH` dell'utente, tramite PowerShell |
| Librerie della toolchain per i suoi programmi | `LD_LIBRARY_PATH` | `DYLD_FALLBACK_LIBRARY_PATH` | `PATH` |
| Il linker wild | host glibc | no | no |
| Esecuzioni dei test (`cargo test`, `cargo bench`) | terminale inoltrato, riga dei totali | terminale inoltrato, riga dei totali | le righe di Cargo |
| L'help di Cargo indica il programma che esegue | `cargo` | `cargo` | `.qrust-engine.exe` |
| Segnali di terminazione | inoltrati | inoltrati | solo ciò che la console consegna all'intero gruppo di processi |
| `qrust benchmark --timeout` | sì | sì | no |
| Inventario: deduplicazione degli hard link, dimensioni allocate | sì | sì | no |
| Rilevamento di una build di Cargo in corso (inventario) | sì | sì | no |
| Driver: link prima che rustc salvi il suo stato incrementale | sì | sì | no |
| Tipo di filesystem, modello di CPU, memoria nei report | sì | sì | `null`, con un motivo |

## Limiti dell'osservazione su una toolchain stable {#limits-of-observation-on-a-stable-toolchain}

Questi dati vengono riportati come non disponibili (`null` con un motivo), mai come zero:

- quali unità Cargo ha considerato aggiornate: si sa solo quando l'utente passa `--message-format=json` e stdout non è un terminale (QuickRust legge allora il flusso mentre lo lascia passare) oppure quando è QuickRust a rendere la build;
- la quota del linker in un'unità che esegue il link;
- il tempo speso a espandere le macro procedurali nei crate dipendenti;
- le invocazioni di rustdoc: `cargo doc` esegue rustdoc al di fuori di `RUSTC_WRAPPER`;
- il tempo di CPU e la memoria per unità quando un client di cache (sccache) esegue rustc per conto del wrapper;
- i byte ripristinati da sccache, che non li riporta.

Le fonti di dati disponibili solo su nightly (`--unit-graph`, `-Zbuild-analysis`, tempi per sezione) vengono rilevate ed elencate in `toolchain.capabilities` del report; nulla dipende da esse. Perché: [Misurare le build](../explanation/measurement.md).
