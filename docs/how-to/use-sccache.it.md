# Come usare sccache con QuickRust

Questa guida mostra come compilare con sccache tramite QuickRust, sia con la configurazione di sccache che hai già sia lasciando che sia QuickRust ad aggiungerlo, e come leggere che cosa ha fatto.

Prima di iniziare, valuta se ti serve davvero: la cache degli artefatti di QuickRust condivide già le dipendenze compilate tra tutti i tuoi checkout su una stessa macchina. sccache aggiunge una cache condivisa tra macchine diverse (uno store remoto), a un prezzo: ogni volta che è configurato un qualsiasi wrapper di rustc, QuickRust disattiva per quelle build la propria cache degli artefatti, il frontend parallelo, l'early cut-off e il linker veloce. Vedi [Fast mode](../reference/fast-mode.md).

## Mantenere la configurazione di sccache che hai {#keep-the-sccache-setup-you-have}

Se hai già impostato `RUSTC_WRAPPER=sccache` nell'ambiente o `build.rustc-wrapper` in un `.cargo/config.toml`, non fare nulla. Con l'impostazione predefinita `cache.provider = "inherit"`, QuickRust mantiene il tuo wrapper, esegue il proprio wrapper all'esterno di esso e registra le statistiche di sccache per ogni build.

## Lasciare che QuickRust aggiunga sccache {#let-quickrust-add-sccache}

Se sccache è installato (nel `PATH`) ma non configurato come wrapper, chiedi a QuickRust di concatenarlo:

```toml
# .qrust/config.toml (un progetto) o ~/.qrust/config.toml (tutti i progetti)
[cache]
provider = "sccache"
fail = "open"
```

Configura sccache stesso (directory locale, store remoto, credenziali) come indicato nella documentazione di sccache; QuickRust non legge né memorizza quelle impostazioni.

Un wrapper che hai configurato tu vince sempre su `provider = "sccache"`: QuickRust mantiene il tuo, emette un warning e registra il conflitto.

## Decidere che cosa succede quando sccache non è disponibile {#decide-what-happens-when-sccache-is-unavailable}

QuickRust verifica che sccache risponda prima di ogni build in cui lo ha concatenato.

- `fail = "open"` (predefinito): la build compila senza sccache, QuickRust stampa una riga `WARN` e l'esecuzione viene contrassegnata come `degraded`.
- `fail = "closed"`: il comando si ferma prima di compilare. Usalo dove compilare senza la cache sarebbe peggio che non compilare affatto, per esempio in un job di CI il cui budget presuppone hit della cache.

Un wrapper che hai configurato tu stesso non viene mai saltato: se non riesce ad avviarsi, la build fallisce come fallirebbe con il Cargo puro.

## Scegliere una policy per la compilazione incrementale {#choose-an-incremental-policy}

sccache non mette in cache i crate compilati in modo incrementale, e Cargo compila in modo incrementale i crate del workspace nei profili di sviluppo. Se vuoi che sccache metta in cache anche i crate del tuo workspace, disattiva la compilazione incrementale, separatamente per le macchine degli sviluppatori e per la CI:

```toml
[incremental]
local-dev = "cargo-default"   # mantieni le ricompilazioni incrementali sulla tua macchina
ci = "off"                    # lascia che sccache metta in cache tutto in CI
```

I valori sono `cargo-default` (nessuna modifica), `on`, `off` ed `experiment`. Un `CARGO_INCREMENTAL` esplicito nell'ambiente vince sempre. QuickRust considera un'esecuzione come CI quando è impostata `CI` o una variabile di CI nota (`GITHUB_ACTIONS`, `GITLAB_CI`, `BUILDKITE`, `JENKINS_URL`, `TF_BUILD`, `TEAMCITY_VERSION`).

Per scoprire quale policy è più veloce per il tuo progetto, misurale entrambe; vedi [Come misurare l'effetto di un'impostazione di build](measure-a-change.md):

```bash
qrust experiment --env CARGO_INCREMENTAL=0 --scenario small-edit --allow-mutation -- build
```

## Vedere che cosa ha fatto sccache {#see-what-sccache-did}

```bash
qrust cache status          # versione, posizione e statistiche del server di sccache, e l'ultima esecuzione analizzata
qrust analyze cache         # hit, miss e richieste non memorizzabili in cache dell'ultima build
```

I numeri sono i contatori del server sccache tra l'inizio e la fine della build, quindi includono anche altre build che usano lo stesso server nello stesso momento.

## Vedi anche {#related}

- [Configurazione](../reference/configuration.md) per `cache.provider`, `cache.fail` e `incremental.*`
- [La cache degli artefatti](../explanation/artifact-cache.md)
- [Sicurezza e privacy](../explanation/security.md) per come QuickRust compone i wrapper
