# Database Contatti

Applicazione desktop locale per importare, cercare, classificare e mantenere
elenchi di contatti email. L'interfaccia è React, il runtime desktop è Tauri e
i dati applicativi sono conservati in SQLite.

## Funzioni principali

- importazione di `.xlsx`, `.xlsm`, `.xlsb`, `.xla`, `.xlam`, `.ods`, `.odt`,
  `.csv`, `.txt` e `.tsv` (`.xls` legacy non è supportato);
- normalizzazione delle email, classificazione IA e validazione DNS;
- registro di importazione riconciliabile con manifest SHA-256, righe sorgente,
  candidati separati e motivi di quarantena/soppressione;
- filtri inclusivi/esclusivi, ricerca e paginazione a blocchi di 100 righe;
- blacklist persistente, applicata anche ai contatti già presenti;
- verifica caselle opzionale con rivalidazione DNS, coda SMTP durevole e
  STARTTLS obbligatorio, senza inviare messaggi;
- arricchimento con passo deterministico, poi Cascade Jev (TypeSafe) e
  escalation OpenCode Muse Spark 1.3 solo sui gap di tassonomia; apply automatico
  di etichette e geografia, revisione manuale solo per i gap, con pausa/ripresa;
- esportazione CSV progressiva, senza caricare l'intero database in memoria.

## Dati e privacy

Il database dei contatti rimane sul computer. La classificazione IA invia batch
di contatti a TypeSafe Jev e, solo per i gap di tassonomia, a OpenCode Muse Spark 1.3.
Nella pagina **Cerca lead**, se è impostata una chiave TinyFish (campo nella
pagina oppure variabile `TINYFISH_API_KEY`), **Avvia ricerca** e **Trova siti**
chiedono a TinyFish i siti a partire dal testo libero o dal nome di un ente, e
l'esplorazione chiede a TinyFish le pagine che il sito blocca (HTTP 403, 406,
429, 451, 503 o pagina di verifica anti-bot). Se anche TinyFish non basta, la
pagina viene letta dalla copia del Wayback Machine (archive.org) o, in mancanza,
di Common Crawl, che ricevono quindi l'indirizzo della pagina. Con **Usa IA** i link di una pagina e il testo
vicino agli indirizzi trovati vanno a OpenCode Muse Spark 1.3, eseguito
senza strumenti né permessi: il testo dei siti non può fargli leggere file o
aprire URL. TinyFish, archive.org, Common Crawl e OpenCode non ricevono mai il database
contatti; i domini dei contatti diventano siti da esplorare solo quando lo
chiedi con **Aggiungi dai contatti**. Le pagine escluse da `robots.txt` non
vengono mai lette, nemmeno tramite TinyFish o archivio.
La validazione dei domini esegue invece richieste DNS, che possono essere
visibili al resolver DNS configurato sul sistema. La verifica caselle, soltanto
quando avviata esplicitamente, apre connessioni dirette agli MX sulla porta 25:
i server destinatari vedono l'indirizzo IP, l'hostname EHLO e il mittente
envelope configurati.

I file sorgente possono contenere dati personali. Il corpus di compatibilità
contiene contatti reali e non fa parte di questo repository: i manutentori lo
tengono solo in locale in `data/email/` (cartella ignorata da Git). Senza corpus i test che lo leggono vengono saltati; benchmark e
`bun run test:corpus` lo richiedono. Non aggiungere al repository dati di
contatto reali, esportazioni, database, backup o log. Per nuovi esempi e fixture
usare dati sintetici, per esempio `utente@example.test`.

## Uso responsabile

L'app raccoglie indirizzi da file e siti web e può verificare le caselle via
SMTP. Chi la usa è titolare del trattamento dei dati che importa o raccoglie e
deve rispettare il GDPR e le norme sulle comunicazioni commerciali (base
giuridica, informativa, opposizione). Il software è fornito senza garanzie,
secondo la licenza MIT.

I backup esportati non contengono le chiavi API (TypeSafe, TinyFish); il ripristino
conserva quelle del database sostituito. Prima di ripristinare, il database attivo
viene salvato come `contatti.sqlite3.pre-restore` nella cartella dati.

La cancellazione di una voce dalla blacklist non ripristina automaticamente un
contatto: l'evidenza conservata passa in quarantena e richiede una nuova
verifica. La pulizia del database conserva la blacklist; la pulizia della
blacklist conserva i contatti già importati.

## Accesso per assistenti (MCP)

`database-contatti-mcp` è un server [MCP](https://modelcontextprotocol.io) su
stdio, in sola lettura, che apre lo stesso database dell'app e riusa la ricerca
e i filtri dell'interfaccia (blacklist e contatti soppressi non compaiono mai).
Strumenti: `search_contacts` (testo libero, filtri include/exclude, paginazione)
e `list_filter_options`. Compilarlo e registrarlo nel client:

```sh
cargo build --release --manifest-path src-tauri/Cargo.toml --bin database-contatti-mcp
claude mcp add contatti -- "$PWD/src-tauri/target/release/database-contatti-mcp"
```

Il percorso del database è quello dell'app (cartella dati `com.mark.database-contatti`);
`DATABASE_CONTATTI_DB` lo sostituisce. L'app deve essere stata avviata almeno una
volta. Attenzione: i contatti restituiti vengono inviati al fornitore dell'assistente
collegato.

## Prerequisiti

L'ambiente richiede le versioni stabili correnti di Bun e Rust/Cargo, le
dipendenze di sistema richieste da Tauri 2 per la piattaforma in uso e OpenCode v2
autenticato con accesso a `opencode-go/muse-spark-1.3-contributor`. Vite+ riunisce
server di sviluppo, build, test, lint Oxlint e formattazione Oxfmt. Le versioni
delle dipendenze JavaScript e Rust sono risolte dai lockfile.

Le piattaforme supportate sono Windows e macOS. Su macOS sono supportati sia i
Mac Intel sia i Mac Apple Silicon. Linux non è un target supportato e la build
Rust lo rifiuta esplicitamente. Per lo sviluppo locale servono Bun, Rust/Cargo
e le dipendenze di sistema richieste da Tauri 2; non è impostata una versione
minima di macOS. Su Windows usare un terminale PowerShell o compatibile; su
macOS usare il terminale predefinito.

Per installare esattamente le dipendenze bloccate:

```sh
bun install --frozen-lockfile
```

## Sviluppo, verifica e pacchetti

Avvio dell'applicazione desktop in sviluppo:

```sh
bun run tauri dev
```

Verifica completa obbligatoria prima di ogni modifica condivisa:

```sh
bun run check
```

Il controllo esegue verifica Oxfmt, lint Oxlint, typecheck TypeScript, build web,
test frontend e test Rust. I singoli comandi disponibili includono
`bun run format`, `bun run lint`, `bun run check:js`, `bun run test:js` e
`bun run test:rust`. Il benchmark diagnostico dell'app (import, list, search,
export; senza IA) usa il corpus locale e si esegue con:

```sh
bun run test:performance
```

I tempi dipendono dalla macchina e vanno confrontati sulla stessa build e sullo
stesso hardware; la metodologia e i campi da compilare dopo una run sono in
[`docs/performance.md`](docs/performance.md). Il benchmark live della cascade
Jev→Muse Spark (API TypeSafe + OpenCode autenticato) si esegue con:

```sh
bun run test:cascade
```

Simula l'arricchimento reale: contatti SQLite + `contact_sources`, contesto via
`classification_source_context`, apply tassonomia/geografia sul percorso di produzione,
metriche separate per fill/replace (category, subcategory) e fill geo (`comune`).
Di default usa 100 casi recensiti. Opzionale: `CASCADE_BENCH_LIMIT=16` o
`CASCADE_BENCH_LIMIT=all`. Escalation di default: OpenCode Muse Spark 1.3 (`xhigh`).
Modello ed effort si possono cambiare con `CASCADE_BENCH_ESCALATION_MODEL` e
`CASCADE_BENCH_ESCALATION_EFFORT`.

Richiede `TYPESAFE_API_KEY` e il provider di escalation autenticato.

Per creare gli installer e i pacchetti di produzione:

```sh
bun run tauri build
```

Il frontend compilato viene scritto in `dist/`; i pacchetti Tauri vengono
prodotti sotto `src-tauri/target/release/bundle/`. Entrambe sono directory
generate e non devono essere commesse.

La configurazione Tauri include i bundle per Windows e macOS. Una build su un
Mac Intel produce il pacchetto Intel; una build su Apple Silicon produce il
pacchetto Apple Silicon. I tag Git nel formato `v*` pubblicano gli artefatti
di release nella sezione **Releases** di GitHub, compresi gli installer
Windows e i pacchetti macOS disponibili per ciascuna architettura supportata.

I pacchetti macOS distribuiti da GitHub sono attualmente senza firma Apple
Developer e senza notarizzazione. Il workflow applica soltanto una firma ad hoc,
senza credenziali Apple, per evitare che il pacchetto Apple Silicon venga
segnalato come danneggiato. Alla prima apertura macOS può comunque mostrare un
avviso di Gatekeeper o segnalare che lo sviluppatore non è verificato: aprire
**Impostazioni di Sistema → Privacy e sicurezza**, scorrere fino al messaggio
sull'app bloccata, scegliere **Apri comunque** e confermare. La firma ad hoc non
equivale alla firma Apple Developer e alla notarizzazione, che richiederebbero
credenziali e una configurazione separata.

Anche gli installer Windows non sono firmati: SmartScreen può mostrare
«Windows ha protetto il PC». Scegliere **Ulteriori informazioni → Esegui
comunque**. Verificare di aver scaricato l'installer dalla pagina **Releases**
di questo repository.

Per controllare le vulnerabilità note nelle dipendenze Bun e Rust è necessario
avere `cargo-audit` installato, quindi eseguire:

```sh
bun run audit
```

L'audit Rust filtra esplicitamente i target `macos` e `windows`. Le dipendenze
GTK/WebKitGTK possono restare nel lockfile risolto da Tauri, ma non appartengono
ai target distribuiti e non vengono presentate come avvisi “consentiti”.

Le pull request eseguono controlli condizionali per le aree frontend e native,
seguiti da un unico gate finale stabile. I tag Git nel formato `v*` avviano la
verifica, l'audit e la compilazione degli installer Windows MSI e NSIS e dei
pacchetti macOS. Gli artefatti vengono pubblicati in una GitHub Release con
note generate automaticamente; non avviene alcuna pubblicazione su registri
di pacchetti.

## Posizione del database e backup

Tauri risolve la directory dati applicativa in base al sistema operativo e
all'identificatore `com.mark.database-contatti`. L'app crea al suo interno:

- `contatti.sqlite3`, database principale che include contatti, blacklist,
  revisioni e run IA;
- l'eventuale `blacklist.sqlite3` legacy, letto una sola volta per consolidarne
  i dati nel database principale.

Il percorso effettivo di `contatti.sqlite3` è scritto nel log all'apertura. Non
indovinare o codificare un percorso specifico di Windows o macOS.

Prima di una migrazione legacy che ricostruisce i contatti viene creato, nella
stessa directory, un backup denominato
`contatti.sqlite3.backup-<timestamp>.sqlite3`. Le migrazioni sono transazionali:
ogni migrazione salva insieme le proprie modifiche, il suo marcatore e
`PRAGMA user_version`, quindi dopo un errore l'app riparte dalla prima migrazione
non applicata senza ripetere quelle già salvate. In caso di errore l'app conserva
il backup e mostra un messaggio con il percorso.

La migrazione 57 corregge dati scritti prima dell'audit della classificazione:
svuota comune, provincia e regione letti dal testo email tramite nomi
alternativi GeoNames o domini dei provider (per esempio `.com` → Como,
`virgilio.it` → Borgo Virgilio) quando né le regole attuali né le righe di
origine li confermano, e riporta a `classified` i contatti per cui Jev
concordava con la categoria esistente. Ogni valore modificato è registrato in
`classification_system_changes` con il valore precedente. Non crea un backup
automatico: farne uno prima di aprire la nuova versione.

Per un backup operativo:

1. chiudere completamente l'applicazione;
2. copiare `contatti.sqlite3` e gli eventuali file
   `contatti.sqlite3.backup-*.sqlite3` in una posizione protetta;
3. verificare che la copia abbia dimensione non nulla.

Per recuperare dati, conservare anzitutto il file guasto, chiudere l'app e
ripristinare una copia verificata come `contatti.sqlite3`. Non modificare
manualmente `PRAGMA user_version`: lo schema viene aggiornato dalle migrazioni.

## Flussi operativi

### Importare e consultare contatti

1. In **Contatti**, scegliere **Importa contatti** e selezionare uno o più file.
2. Attendere la fine dell'avanzamento. Ogni file viene salvato in una
   transazione; il riepilogo distingue occorrenze candidate, contatti nuovi,
   contatti aggiornati, quarantene, soppressioni e righe malformate. I file di
   una cartella vengono letti uno alla volta, quindi la memoria non cresce con il
   numero di file. È possibile una sola importazione alla volta: le selezioni
   successive restano in coda nella pagina.
3. Usare ricerca e filtri. Ogni filtro è un chip sotto la ricerca; un chip attivo
   mostra quanti valori include ed esclude. Un valore filtro può essere neutro,
   incluso o escluso. Ricerca e filtri aggiornano solo l'elenco; le opzioni dei facet restano quelle
   dell'ultimo caricamento completo (avvio, import, ripristino). I conteggi dei
   facet considerano solo i contatti esportabili, cioè le stesse righe che
   l'elenco e l'export possono mostrare. La ricerca testuale
   usa un indice FTS5 sui campi anagrafici.
4. Spostarsi con **Precedente** e **Successiva**. Cambiare filtro riporta alla
   prima pagina. La paginazione non ricalcola il conteggio totale a ogni pagina.
   Un clic sull'intestazione di una colonna ordina l'elenco: alfabetico
   (senza distinzione tra maiuscole e minuscole) per le colonne di testo,
   dal valore più alto per fonti, punteggio e PEC (la classificazione ordina
   per metodo, in ordine alfabetico); un secondo
   clic inverte, un terzo torna all'ordine per email. I valori vuoti restano
   in fondo in entrambe le direzioni. L'ordinamento vale per l'elenco, non per
   le esportazioni, e **Azzera filtri** lo lascia com'è.
   Le colonne hanno larghezza fissa, così non si spostano quando cambiano le
   righe: i valori lunghi sono troncati con i puntini (il titolo della cella e
   **Dettagli** mostrano il valore completo). A parità di valore l'ordine segue
   l'indirizzo nella stessa direzione, quindi le pagine non si rimescolano.
5. La tabella mostra le colonne di lavoro (email, ente, categoria, sottocategoria,
   comune, provincia, regione, PEC, fonti indipendenti, punteggio, classificazione); la
   classificazione è in ambra per gli stati esclusi di default dall'export. La colonna azioni resta fissa a destra: **Dettagli** (icona
   occhio) apre la scheda con tutti i campi del contatto. **Cancella** (icona
   cestino) chiede
   conferma: il contatto esce da elenco ed export, le righe sorgente restano in
   quarantena e nessuna rivalidazione DNS lo ripristina; solo una nuova
   importazione dell'indirizzo lo riporta tra i contatti.
6. Dopo blacklist, rivalidazione DNS, run IA o revisione di un gap, la pagina
   **Contatti** si ricarica alla successiva apertura.
7. Il **punteggio** (0-100) stima quanto il contatto sia ricco di dati utili e
   raggiungibile: 30 punti di specificità (persona e ruolo noti battono una
   casella generica come `info@`), 25 di ricchezza dei dati (ente, comune,
   sito, indirizzo o CAP, codice ufficiale), 15 di classificazione non
   generica (una categoria vera e una sottocategoria specifica: "Altro ente
   formativo", "Associazione generica", "Ente non determinato" e simili valgono
   meno, la categoria "Varie" nulla), 15 di freschezza (tempo dall'ultima
   conferma o dall'ultima volta che l'indirizzo è comparso in una fonte; le
   caselle personali o di ruolo invecchiano in 12 mesi, le altre in 24) e 15 di
   recapito (routing, casella, PEC, segnaposto, indirizzo usa e getta o
   sospetto refuso). Dipende solo dai dati del contatto: il numero di fonti e il
   metodo o la confidenza della classificazione non contano, e non ci sono
   parole chiave di campagna né bonus legati alla ricerca. È salvato nel
   contatto e il database lo ricalcola da solo ogni volta che cambia un dato
   che lo determina (importazione, aggiunta da Cerca lead, modifica, IA,
   verifica caselle). Poiché la freschezza cala col tempo, all'avvio
   un ricalcolo completo, al più una volta al giorno, aggiorna solo i
   contatti il cui punteggio è cambiato. Il filtro **Punteggio minimo** vale
   per elenco ed esportazioni; la scheda **Dettagli** mostra il valore.

L'ortografia originale resta nella riga sorgente; la forma canonica conserva le
maiuscole della parte locale e normalizza il dominio, mentre una chiave separata
senza distinzione tra maiuscole e minuscole gestisce confronto e deduplicazione.
La classificazione conserva tutte le etichette osservate nelle sorgenti; una
nuova importazione aggiorna la categoria solo dei contatti non ancora classificati
dal passo deterministico o dall'IA. Un indirizzo è PEC se la colonna sorgente
(`tipo_mail`) lo dichiara, se un'etichetta intera del dominio è PEC (`pec.`,
`cert.`, `postacert`, `legalmail`, …) o se un MX appartiene a un gestore PEC:
`concert.it` o `certaldo` non contano. I
controlli DNS sono raggruppati e memorizzati per dominio con scadenza, ma
indicano solo il routing del dominio, non l'esistenza della casella. Lo storico
delle verifiche registra per singolo indirizzo solo gli esiti che lo escludono
(nessun routing, errore temporaneo, sintassi non valida): un esito positivo resta
una sola volta nella cache del dominio.

Gli indirizzi non esportabili e le righe di parsing malformate restano conservati
con file, foglio, riga, motivo e punteggio iniziale. Sono consultabili nella sezione
**Verifica caselle**; non appaiono nella tabella **Contatti** e non entrano mai nelle
esportazioni.

Gli indirizzi chiaramente automatici (`no-reply`, `do-not-reply`,
`mailer-daemon`, `bounce`, `autoresponder` e varianti prudenti) sono conservati
come candidati soppressi, ma non entrano in **Contatti**, export, rivalidazione
DNS o coda SMTP. Caselle istituzionali utili come `info`, `segreteria`,
`postmaster`, `privacy` e `webmaster` non sono escluse da questa regola.

### Rivalidare DNS e verificare le caselle

La pagina **Verifica caselle** unifica la gestione del recupero DNS e la verifica SMTP.
Offre due azioni principali per l'invio: **Avvia rollout raccomandato** (pilot di 1.000 indirizzi e, se il
gate di sicurezza lo consente, fino a 5.000) e **Probe Everything**. Prima di
usarle occorre salvare un hostname EHLO FQDN controllato e un mittente envelope
valido. STARTTLS, verifica del certificato e verifica del nome host non sono
disattivabili; il client si ferma dopo `RCPT TO` e non invia mai `DATA`.

**Rivalida solo DNS** non richiede l'identità SMTP. La prima importazione esegue
già un controllo per ogni dominio distinto. La rivalidazione dedicata ricontrolla
solo i domini con errore temporaneo (`temporary_failure`); il rollout SMTP
rivaluta invece anche cache mancanti o scadute, rispettando i TTL DNS;
non è necessario rinominare o modificare i file sorgente. Un dominio tornato
instradabile può recuperare il contatto dalla quarantena (salvo i contatti
cancellati manualmente e gli indirizzi tolti dalla blacklist), mentre un
`no_routing` definitivo lo rimuove soltanto dalla proiezione esportabile e ne
conserva candidato, riga sorgente e cronologia.

La coda SMTP è persistente, rate-limited a livello globale, dominio e MX, con
una sola connessione alla volta, retry dei soli errori temporanei e cooldown.
Alla chiusura dell'app il job viene messo in pausa e deve essere ripreso
esplicitamente: non continua mentre l'app è chiusa. Un'accettazione SMTP indica
solo “accettato dal server”; catch-all, greylisting, policy e accettazione con
bounce successivo impediscono di usarla come prova dell'esistenza di una
casella. Gli esiti non cancellano contatti e non trasformano automaticamente
un indirizzo in esportabile.

### Gestire la blacklist

Importare file `.csv` o `.txt` dalla pagina **Blacklist**. L'operazione è
atomica: inserimento delle nuove voci, soppressione dei candidati e rimozione
dalla proiezione dei contatti esportabili avvengono nella stessa transazione.
Le righe sorgente non vengono eliminate. Duplicati e differenze di maiuscole
non creano voci multiple. Se un'importazione fallisce, non rimane applicata a
metà.

Rimuovere una singola voce consente importazioni future di quell'indirizzo, ma
non ricrea dati precedentemente eliminati. Rimozione e pulizia della blacklist
riportano in quarantena soltanto i candidati soppressi dalla blacklist: gli
indirizzi automatici (`no-reply`, `mailer-daemon`, …) restano soppressi. Se
alcuni file di una selezione non si leggono, gli altri vengono importati e un
avviso elenca i file scartati.

### Esportare

**Esporta CSV** apre una finestra nativa per scegliere la destinazione.
L'esportazione dei contatti rispetta i filtri correnti; quella della blacklist
rispetta la ricerca corrente. La scrittura è progressiva e pubblica il file solo
al completamento. Per evitare perdite di dati, un file di destinazione già
esistente non viene sovrascritto: scegliere un nuovo nome. Su dischi senza
collegamenti fisici (exFAT/FAT, alcune condivisioni di rete) il file viene
pubblicato con una rinomina dopo aver ricontrollato che la destinazione non
esista. Nei CSV completi le
celle descrittive che iniziano con `=`, `+`, `-`, `@`, tabulazione o ritorno a
capo sono precedute da un apostrofo, così un foglio di calcolo non le esegue come
formule; la colonna email e gli export di sole email restano invariati.
Per impostazione predefinita l'esportazione esclude solo le classificazioni da
verificare (`conflicting` e gap di tassonomia in attesa); **Includi anche
classificazioni da verificare** le aggiunge. I contatti con evidenza
insufficiente secondo Jev conservano i valori precedenti e restano esportati.

La pagina **Cerca lead** è un'area di prova con gli stessi campi dei contatti
(email, ente, ufficio, persona, ruolo, categoria, sottocategoria, indirizzo,
CAP, comune, provincia, regione, PEC, sito, pagina di origine). Tutto ciò che
importi o trovi finisce prima qui; **Aggiungi al database** copia nei contatti
i lead che corrispondono ai filtri correnti (PEC comprese, segnate come PEC),
unendo sugli indirizzi già presenti e saltando la blacklist, in una sola
transazione. La tabella mostra una riga per indirizzo anche se più ricerche lo
hanno trovato (colonna **fonti**); la colonna **stato** dice se l'indirizzo è
**Nuovo**, **Già nel database** o **Completa il contatto**: è già nei contatti
e aggiungerlo lo cambia. Le regole in caso di valori diversi:

- un campo vuoto del contatto (ente, ufficio, persona, ruolo, codice,
  sottocategoria, sito, località) prende il valore del lead, anche se la
  classificazione è stata fatta dall'IA; la località si completa solo se il
  contatto non ne ha nessuna, e sempre per intero;
- ente, ufficio, persona, ruolo, sottocategoria e località non vengono mai
  uniti con `|`: un valore già presente resta;
- i registri ufficiali (scuole MIUR, IndicePA, biblioteche ICCU) invece
  sostituiscono ente, codice e località; il valore precedente resta nelle
  **Evidenze** del contatto ("sostituito da indicepa: ente: …");
- il sito diverso si aggiunge dopo quello esistente con `|`, come nelle
  altre importazioni;
- indirizzo e CAP restano nell'area Cerca lead e non vengono copiati nei
  contatti: servono solo a ricavare comune, provincia e regione quando la
  fonte non dà un comune riconosciuto (vuoto, una frazione, un quartiere).
  Prima vale il comune scritto dopo il CAP nell'indirizzo; altrimenti il CAP
  nella tabella CAP-comuni ISTAT (comuni-json, scaricata solo quando serve):
  un CAP di un solo comune dà comune, provincia e regione, un CAP condiviso
  dai comuni di una provincia dà solo provincia e regione.

I lead non hanno un punteggio: ne ricevono uno quando diventano contatti con
**Aggiungi al database**, calcolato sul contatto unito, mai copiato dal lead.

**Dettagli** apre la scheda completa, con metodo di estrazione. I filtri sono
quelli dei contatti più ricerca, metodo e stato; **Esporta Mail** scrive le
caselle PEO filtrate.

La pagina ha due schede, provabili separatamente: **Dati aperti** (dataset
scaricati e letti direttamente, nessuna visita ai siti) e **Ricerca web**
(elenchi, siti da esplorare, crawler, TinyFish e IA). Ogni scheda mostra,
filtra, esporta e aggiunge al database solo i lead delle proprie ricerche.

**Dati aperti** contiene:

- dataset con email in chiaro, importati con **Importa**: Anagrafe scuole
  statali e paritarie, IndicePA, sezioni e gruppi ANA, Anagrafe delle
  biblioteche ICCU, OpenStreetMap (luoghi di culto; cultura; municipi, forze
  dell'ordine e tribunali; scuole; associazioni e circoli; uffici turistici e
  sanità) e Wikidata (musei, archivi, teatri, cinema; diocesi, parrocchie,
  chiese, monasteri);
- **Cataloghi dati.gov.it** (vedi sotto).

L'importazione salva tutte le email della fonte, anche quelle già nei
contatti: nella tabella risultano **Completa il contatto** se portano ente,
ufficio o località mancanti. Il responsabile dell'ente IndicePA (nome e
titolo, per esempio il sindaco) e il presidente di sezione ANA diventano
persona e ruolo delle caselle generali dell'ente o della sezione. Per ANA,
oltre all'indirizzo mostrato, vale anche quello del link `mailto:` quando è
diverso, sullo stesso dominio e non è una copia troncata.

Registri senza download né elenco leggibile (RUNTS, annuario parrocchie CEI)
non compaiono: per le parrocchie i punti di partenza sono le diocesi di
Wikidata, i luoghi di culto di OpenStreetMap e gli elenchi diocesani.

**Ricerca web** contiene gli elenchi di enti da esplorare con **Esplora
elenco**: sezioni CAI, Caritas diocesane, club Rotary del Distretto 2080,
oratori ambrosiani FOM, pubbliche assistenze ANPAS, Azione Cattolica
diocesana, più qualunque altra pagina elenco indicata a mano con categoria e
sottocategoria. Il crawler segue le pagine sotto il percorso dell'elenco (e la
paginazione): le email diventano lead con la categoria dell'elenco, i link ad
altri siti diventano siti da esplorare.

**Cataloghi dati.gov.it** cerca nel catalogo nazionale (che raccoglie anche i
portali regionali) tabelle CSV o Excel: l'importazione riconosce dalle
intestazioni le colonne di email, PEC, denominazione, comune, provincia,
regione, CAP, indirizzo e sito e assegna la categoria scelta.

Gli enti che una fonte elenca senza email, ma con un sito, diventano **Siti da
esplorare** nella scheda Ricerca web, raggruppati per elenco; quelli con solo il nome e il comune
aspettano **Trova siti** (TinyFish, al massimo 30 ricerche al minuto). Altri
siti arrivano da **Aggiungi dai contatti** (un sito per dominio già presente
nei contatti, filtrato per categoria, sottocategoria e regione; i domini di
provider e quelli condivisi da più di tre comuni sono esclusi) o da URL
incollati. **Esplora** visita i siti in attesa, sei alla volta, ciascuno in
sequenza: rispetta `robots.txt` (incluso `Crawl-delay`), attende almeno 1,5
secondi più una variazione casuale tra due richieste, usa la sitemap per
arrivare subito alle pagine dei referenti e, se il sito chiede di rallentare,
attende il tempo indicato da `Retry-After` prima di riprovare una volta. Un
sito che blocca o limita le richieste passa a TinyFish e poi alla copia in
archivio (Wayback Machine, poi Common Crawl). Un sito esplorato non viene riesplorato per 30 giorni, anche da un
altro elenco. Gli indirizzi del sito stesso (o una casella webmail pubblicata
sul sito) ereditano ente, categoria e località del sito; con **Usa IA** il
modello sceglie le pagine da visitare e legge ufficio, ruolo, persona, comune e
categoria dal testo vicino a ogni indirizzo, e negli elenchi il nome e il comune
di ogni ente. Le risposte del modello sono accettate solo se riguardano link e
indirizzi presenti nella pagina e voci della tassonomia. I lead di ogni sito
sono salvati appena il sito è finito: **Ferma esplorazione** conserva quelli
già trovati.

### Classificazione IA

L'arricchimento considera solo i contatti esportabili: indirizzi soppressi,
automatici o senza routing non vengono inviati a provider a pagamento.
Il controllo **Arricchisci database** parte dal passo deterministico (codici
scuola, domini istituzionali esatti come forze dell'ordine e PEC collegate,
regioni, ministeri, università note, comuni `comune.<nome>.<pv>.it`,
`comune.<sigla>.it` (capoluogo) e `comune.<nome>.it` (solo se il nome è univoco
nel gazetteer); anche con la provincia scritta per esteso
(`comune.montemarciano.ancona.it`) o il nome senza spazi
(`comune.massalombarda.ra.it`). Se il comune resta ambiguo (omonimi), il
contatto è comunque **Comune** ma senza geografia. Domini istituzionali e loro
sottodomini: università (`pec.unicampania.it`), ASL/ASST/ATS, aziende
ospedaliere, camere di commercio; le caselle personali `@istruzione.it` sono
Istruzione. Prefissi di dominio
come Pro Loco, token esatti su denominazione/URL per CAI, Rotary, Lions,
parrocchie, conservatori, …). I token lunghi valgono solo all'inizio di una
parola di denominazione o URL (`parrocchiasanmarco` sì, `laboratorio` non è un
oratorio), anche nella parte locale (`prolocomilo@`, `biblioteca.samassi@`);
`cai` nella parte locale vale solo come primo segmento
(`cai.bergamo@`, non `mario.cai@`). Vigili del Fuoco, forze armate e ministero
della Salute sono **Ministero / Ente statale**, non forze dell'ordine. Un nuovo
catalogo sostituisce i risultati deterministici precedenti. Per i codici scuola la provincia del codice vale
solo se il contatto non ha già una geografia: i codici precedono le province
più recenti (BT, MB, Sud Sardegna, Roma Capitale). Poi Cascade Jev sui contatti restanti (soglia di
confidenza 0.8). Jev valuta i contatti a
lotti di 32 (un lotto della run è una sola richiesta del primo passaggio): la
tassonomia è nello state una sola volta, il primo passaggio chiede solo la
categoria (e geografia ambigua), la domanda sul gap solo quando nessuna
categoria chiusa va bene (`none_fit`), e la sottocategoria Jev solo se la
categoria è già abbastanza confidente da applicare. Le domande di follow-up
inviano solo i contatti interessati. Se Jev resta sotto soglia ma la sua
categoria coincide con quella già presente (confidenza ≥ 0.6), la categoria è
**confermata**: il contatto resta `classified` con i suoi valori. I comuni candidati vengono dal campo Comune quando
c'è, altrimenti dal testo delle altre fonti (email, denominazione, indirizzo),
mai dai campi Provincia o Regione: così il capoluogo non viene proposto come
comune di un contatto della sua provincia. Nel testo libero (email, sito,
denominazione) contano solo i nomi ufficiali dei comuni, non i nomi alternativi
GeoNames (`Com` per Como); l'estensione del dominio e i domini dei provider di
posta (`gmail.com`, `virgilio.it`, `pec.it`, …) non sono mai luoghi, e alcune
parole comuni che sono anche comuni (`lettere`, `poli`, `pero`, `dello`, …) sono
ignorate. I luoghi univoci dal gazetteer si risolvono in locale
(anche nel passo deterministico, senza consumare token; queste scritture sono
registrate nella run e il rollback le annulla) e Jev riceve campi
compatti (`Domain`, `Local`, `Denominazione`). La sottocategoria a pagamento
parte solo sui segmenti ad alto valore (scuole, PA, enti, …) nella fascia di
confidenza media; i casi facili restano categoria-only. Il secondo passaggio
usa campi più ricchi (indirizzo, sito, telefono).
OpenCode Muse Spark 1.3
interviene solo quando Jev segnala un **gap di tassonomia**: serve un'etichetta
nuova fuori dal catalogo chiuso, che Jev non può inventare (noul gap ≥ 0.55). Se
la categoria chiusa è troppo debole o assente senza gap, il contatto resta senza
modifica (insufficiente), senza Muse Spark.
Le escalation Muse Spark vanno in lotti da al massimo 4 contatti, fino a 2 lotti in
parallelo. Se la prima escalation Muse Spark non restituisce un risultato strutturale
valido, il contatto ripassa solo su Muse Spark (`xhigh`), senza richiamare Jev. Il feed
live indica se ogni modifica arriva da Jev, Muse Spark o Muse Spark (retry). Categoria,
sottocategoria e geografia (campi vuoti) si applicano subito. I **gap di
tassonomia** restano in attesa di approvazione o rifiuto.
Un comune, una provincia o una regione con più valori separati da `|` (fonti in
conflitto) conta come da risolvere: il passo deterministico e la geografia Jev
lo sostituiscono con un solo luogo e registrano il valore precedente, quindi il
rollback lo ripristina. Il passo deterministico lo fa solo se il luogo della
regola è una delle opzioni elencate; altrimenti le fonti sono davvero in
conflitto e il contatto passa a Jev. Nomi alternativi dello stesso luogo
(Florence/Firenze, Roma Capitale/Roma) non contano come conflitto. La macroarea non si scrive mai: il database la calcola
dalla regione, quindi cambia insieme a lei e resta vuota finché la regione ha più
valori.

Ogni batch completato è un checkpoint. L'avvio restituisce subito il controllo
alla UI: l'arricchimento continua in background.

- **Ferma** porta la run da `running` a `pausing`, quindi a `paused`;
- **Riprendi** ripete il passo deterministico sui contatti ancora in coda (le
  regole nuove evitano chiamate Jev), poi continua dalla coda persistita;
- un limite del provider mette la run in attesa e riprova da sola: TypeSafe
  con rate limit o sovraccarico (HTTP 429/503/529, o 5xx con messaggio di
  carico), timeout/trasporto verso Jev, oppure il limite di OpenCode/Pi.
  In questi casi **non** si passa a Muse Spark: si aspetta che Jev torni
  disponibile. Jev ritenta in locale con backoff breve; se il limite resta,
  la run aspetta (circa 45s per TypeSafe, 5 minuti per OpenCode) e riparte
  senza toccare la coda. Dopo tante attese senza progresso, oppure con
  credito TypeSafe esaurito / accesso negato (402/403), la run va in pausa:
  basta **Riprendi** (dopo aver ricaricato il credito se serve);
- se il database è momentaneamente occupato da altre operazioni, la run attende
  30 secondi e riparte dalla coda (fino a 20 volte) invece di fallire;
- **Annulla** rende la run terminale (disponibile anche durante l'arresto);
- dopo una chiusura o un crash, all'apertura successiva le run `running` /
  `pausing` di un'altra sessione tornano in `paused` e si possono riprendere
  o annullare dalla pagina Classificazione IA.

Una run completata, annullata o fallita non può essere ripresa. La pagina mostra
un feed di attività live (progresso e modifiche campo per campo) aggiornato
durante la run.

L'arricchimento IA è automatico: categoria, sottocategoria e geografia (campi
vuoti) vengono applicati subito come **Aggiunto** o **Modificato**. L'unica
eccezione sono i **gap di tassonomia**: l'IA propone una nuova etichetta, il
contatto resta in attesa e l'utente deve approvare o rifiutare. La pagina mostra
il numero totale di proposte e le 100 più vecchie; approvandole o rifiutandole
compaiono le successive. Un gap identico
è condiviso da tutti i contatti collegati; l'approvazione crea una nuova
versione e aggiorna quei contatti in una sola operazione.

## Risoluzione dei problemi

**OpenCode non disponibile.** Verificare che `opencode` v2 sia nel PATH,
che `opencode auth login` sia stato eseguito e che `opencode models --standalone`
elenchi `opencode-go/muse-spark-1.3-contributor`. OpenCode v1 non è più supportato.

**TypeSafe/Jev non disponibile.** Impostare la API key nella pagina IA oppure la
variabile d'ambiente `TYPESAFE_API_KEY`, poi riprovare.

**Validazione DNS lenta o incerta.** Controllare connettività, VPN, firewall e
resolver DNS. Un esito DNS non prova che una casella esista; indica soltanto le
caratteristiche osservabili del dominio.

**Verifica SMTP non avviabile o tutta sconosciuta.** Configurare un hostname
EHLO e un mittente envelope realmente controllati. Verificare che rete, ISP,
VPN e firewall permettano TCP in uscita sulla porta 25. I server senza STARTTLS
valido restano deliberatamente `unknown`; non abbassare la sicurezza TLS per
ottenere un risultato.

**Importazione senza risultati.** Verificare formato, codifica e presenza di
indirizzi validi. I file `blacklist.txt` e `cancellami.txt` sono riconosciuti
come blacklist, non come corpus di contatti.

**Migrazione non riuscita.** Non cancellare database o backup. Annotare il
percorso mostrato dall'errore, chiudere l'app e lavorare su copie dei file.

**Esportazione rifiutata.** Scegliere una destinazione che non esiste già e
verificare i permessi della cartella.

## Architettura

Il frontend in `src/` separa:

- `api/`, confine tipizzato dei comandi Tauri;
- `components/`, componenti riutilizzabili;
- `App.tsx`, shell e coordinamento dei riepiloghi condivisi;
- `pages/`, stato, flussi e rendering delle funzionalità;
- `hooks/`, hook React condivisi;
- `platform.ts`, confine testabile per dialoghi nativi ed eventi Tauri;
- `features/`, logica frontend pura e relativi test;
- `types.ts`, contratti condivisi del frontend.

Il backend Rust in `src-tauri/src/` separa database e migrazioni (`database.rs`,
`schema/`, con le migrazioni raggruppate per area), contatti ed export
(`contacts.rs`, `imports.rs`), parsing e validazione (`parsing.rs`), blacklist
(`blacklist.rs`) e IA (`ai_commands.rs`, `ai_enrichment.rs`, `ai_runs/`,
`ai_opencode.rs`, `ai_typesafe.rs`, `ai_cascade.rs`,
`ai_selection.rs`). `lib.rs` compone i moduli e registra i comandi pubblici.
Ogni comando che usa SQLite o il disco è `async` e passa da
`database::blocking`, così non blocca il thread dell'interfaccia né i worker
async; un test verifica che tutti i comandi registrati siano `async`.

La proiezione dei contatti esportabili è materializzata nella colonna
`contacts.is_exportable`, mantenuta da trigger SQLite su contatti, blacklist e
soppressioni nella stessa transazione della scrittura. Elenco, ricerca, facet ed
export leggono questa colonna tramite indici parziali; le importazioni massive
sospendono i trigger per riga e ricalcolano la colonna una sola volta alla fine.

I test di comportamento frontend sono in `src/App.test.tsx`; i confini di
piattaforma e la logica pura hanno test dedicati in `src/platform.test.ts` e
`src/features/`. I test Rust di funzionalità e schema sono in
`src-tauri/src/tests_features.rs` e `src-tauri/src/tests_schema.rs`.

## Contribuire

- Eseguire `bun run check` prima di consegnare modifiche.
- Ogni modifica allo schema deve essere una migrazione versionata,
  transazionale, idempotente e coperta da test; non aggiungere reset distruttivi.
- Non cambiare nomi dei comandi Tauri o forme serializzate senza aggiornare
  insieme API tipizzate, test e documentazione.
- Conservare paginazione, export progressivo e guardie contro risposte asincrone
  obsolete.
- Non aggiungere dati di contatto reali: nuovi esempi e fixture devono usare
  dati sintetici. Le vulnerabilità vanno segnalate come indicato in
  [SECURITY.md](SECURITY.md).

## Licenza

Database Contatti è distribuito secondo la licenza [MIT](LICENSE).
