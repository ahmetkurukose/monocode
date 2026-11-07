# Database (PostgreSQL) — uygulama planı

Monocode Native'e, sol menüdeki Inbox / Notes / Automations ile aynı kalıpta
bir **Database** görünümü. Yalnızca PostgreSQL. Arayüz düzeni IntelliJ'in
Database aracından (openidea'nın da aldığı yerden): solda veri kaynağı ağacı,
sağ üstte SQL konsolu, sağ altta sonuç grid'i.

Durum: plan. Kod yok. Hedef branch `AK/monocode_enhancements`.

## 1. Kararlar ve gerekçeleri

| Karar | Seçim | Neden |
|---|---|---|
| Sürücü | `tokio-postgres` (`NoTls` ile başla) | `simple_query` her değeri metin getirir, tip bazlı decode yok; `CancelToken` ile sunucu tarafı iptal; sqlx'e göre çok daha hafif derleme. |
| Bağlantı kaynağı | Monocode'un kendi listesi | dbports'a veya başka bir araca bağımlılık yok. Tünel nasıl açılırsa açılsın (dbports, elle proxy, SSH) Monocode yalnızca host:port görür. |
| openidea'dan alınanlar | model (`source.rs`), `split.rs`, introspection SQL'leri, `QueryResult` şekli, arayüz düzeni | UI (React) ve sürücü katmanı (`conn.rs`, sqlx çok-sürücülü) alınmaz. Referans commit: openidea `f665beb1`. |
| Şifre | işletim sistemi anahtarlığı (`keyring`, zaten bağımlılıkta) | openidea ile aynı; dosyada düz metin yok. |
| Saklama | global (uygulama verisi dizini) | DB'ler projeden bağımsız kullanılıyor. Proje etiketi ileride. |
| Konsol | veri kaynağı başına N konsol, **her konsolun kendi bağlantısı** | IntelliJ modeli: uzun sorgu diğer konsolu bloklamaz; `SET`, geçici tablo ve transaction konsol başına izole. |
| Read-only | veri kaynağı bayrağı, oturumda `default_transaction_read_only=on` ile zorlanır | dbports MCP'deki disiplinle aynı; yeni kaynakta varsayılan **açık**. |
| Satır sınırı | 500 (ayarlanabilir: 100 / 500 / 1000 / 5000) | grid ve bellek için. `has_more` ile "daha fazla var" gösterilir. |

## 2. Kapsam

### MVP (bu plan)
- Sol menüde Database; `AppView::Database`.
- Veri kaynağı ekle / düzenle / sil / **Test Connection**; şifre anahtarlıkta.
- Bağlan; ağaç: veri kaynağı → schema → tablo/görünüm/materialized view → kolon.
  Tembel yükleme, düğüm başına yenile.
- Veri kaynağı başına çoklu konsol (sekmeler), konsol metinleri diskte kalıcı.
- Çalıştır: imlecin üstündeki statement ya da seçim (`Cmd+Enter`), tüm script sırayla
  (`Cmd+Shift+Enter`); iptal (`Esc` / Stop düğmesi); satır sınırı.
- Sonuç grid'i: sanal kaydırma (dikey), yatay kaydırma, kolon genişliği, NULL,
  hücre seç + kopyala, satır kopyala, çift tıkla → önizleme (JSON pretty-print).
- Durum satırı: süre (ilk yanıt / toplam), satır sayısı, "daha fazla var",
  etkilenen satır, hata (mesaj + detay + ipucu + konum; konum editörde işaretlenir).
- Read-only bayrağı.

### Tur 2
- Tabloya çift tık → sayfalı veri görüntüleme (`SELECT * ... LIMIT/OFFSET`, sıralama).
- DDL görüntüleme (tablo, görünüm, fonksiyon).
- Sorgu geçmişi (veri kaynağı başına, tarih + metin).
- Konsol başına "Writes" anahtarı (kaynak izin veriyorsa yeniden bağlanır).
- Konsol başına birden çok sonuç sekmesi.
- `EXPLAIN (FORMAT JSON)` ağaç görünümü (openidea `plan.rs`).
- CSV / JSON / INSERT olarak export.
- TLS (`rustls`), SSH tüneli, SQL completion (şema bilgisiyle), tablo içinde satır düzenleme.
- Proje etiketi: bir kaynağı yalnızca belirli projelerde göster.

### Kapsam dışı
MySQL / SQLite / SQL Server / ClickHouse; dbports ile bütünleşme; şema düzenleme (DDL üretme).

## 3. Mimari

```
crates/monocode-core/src/db/
  mod.rs          pub API: DataSource, Console*, QueryResult, DbError, Introspection
  source.rs       DataSource modeli, datasources.json okuma/yazma, keyring
  split.rs        statement ayırıcı (openidea'dan; byte offset)
  conn.rs         tokio-postgres sarmalayıcı: Connection, execute, cancel, zaman ölçümü
  introspect.rs   pg_catalog sorguları → Schema / Table / Column
  consoles.rs     konsol metinlerinin diskte saklanması
  runtime.rs      paylaşılan tokio runtime (mobile/host.rs'teki OnceLock kalıbı genelleştirilir)

crates/monocode-gpui/src/database.rs        AppView gövdesi: düzen, bölücüler, olaylar
crates/monocode-gpui/src/database/
  tree.rs         sol ağaç (uniform_list), bağlam menüsü
  console.rs      sekme şeridi + Input::editor + araç çubuğu (Run, Stop, limit, read-only rozeti)
  grid.rs         sonuç grid'i
  status.rs       durum satırı ve hata paneli
  preview.rs      hücre önizleme (JSON / uzun metin)
  source_dialog.rs  veri kaynağı diyaloğu
  state.rs        UI durumu (genişlikler, açık konsollar, seçili düğüm) → db/state.json
```

### İş parçacığı modeli
- Tüm DB işi tokio runtime'da (`monocode_core::db::runtime()`), GPUI iş parçacığına asla dokunmaz.
- GPUI tarafı `cx.background_executor().spawn` + `async_channel` ile sonucu alır ve
  `this.update(cx, …)` ile modele yazar; Inbox'taki kalıp (`inbox.rs:272`).
- Her konsolun `Connection` nesnesi tokio tarafında yaşar; GPUI yalnızca
  `ConsoleHandle { id, cancel: CancelToken, tx }` tutar.

## 4. Veri modeli

```rust
pub struct DataSource {
    pub id: String,            // uuid
    pub name: String,          // "socradar@prod"
    pub host: String,          // 127.0.0.1
    pub port: u16,             // 5400
    pub database: String,
    pub user: String,
    pub save_password: bool,   // keyring'de tut
    pub read_only: bool,       // default_transaction_read_only=on
    pub color: Option<DbColor>,// sekme/rozet rengi (prod kırmızı gibi)
    pub comment: String,
    pub application_name: String, // varsayılan "Monocode"
    pub statement_timeout_ms: Option<u32>,
    pub search_path: Option<String>,
}

pub struct Console { pub id: u32, pub source: String, pub title: String, pub text: String }

pub struct ResultColumn { pub name: String } // simple protocol sends no type info; the grid infers JSON / numbers from the text
pub struct QueryResult {
    pub statement: String,
    pub columns: Vec<ResultColumn>,
    pub rows: Vec<Vec<Option<String>>>, // None = NULL
    pub affected: Option<u64>,
    pub has_more: bool,
    pub execution_ms: f64,  // ilk yanıta kadar
    pub elapsed_ms: f64,    // toplam
}

pub struct DbError {
    pub message: String, pub detail: Option<String>, pub hint: Option<String>,
    pub position: Option<usize>, // statement içinde byte offset
    pub code: Option<String>,    // SQLSTATE
    pub cancelled: bool,
}
```

Ağaç düğümleri: `Source → Consoles / Schemas → Schema → Tables | Views | MatViews → Table → Columns → Column`.
Kolon: ad, tip, nullable, default, PK işareti.

## 5. Saklama

Kök: `~/Library/Application Support/com.monocode.desktop/db/`

| Dosya | İçerik |
|---|---|
| `datasources.json` | `Vec<DataSource>` (şifre yok) |
| `consoles/<source-id>/<n>.sql` | konsol metni; başlık dosya adından veya ilk yorum satırından |
| `state.json` | açık konsollar, aktif sekme, ağaç genişliği, editör/grid bölücü oranı, seçili düğüm, son satır sınırı |

Keyring: service `com.monocode.db`, account = `source-id`. `save_password=false` ise şifre
yalnızca bellekte, uygulama kapanınca sorulur.

Yazma kuralı: dosyalar atomik yazılır (tmp + rename), `preferences.rs` ile aynı yol.

## 6. Sürücü katmanı (`db/conn.rs`)

- Bağlantı: `tokio_postgres::Config` ← host, port, user, password, dbname,
  `application_name`, `options = -c default_transaction_read_only=on [-c statement_timeout=…] [-c search_path=…]`.
  `NoTls`. Bağlantı görevi (`connection.await`) runtime'da spawn edilir; kopunca konsol "disconnected" olur,
  bir sonraki çalıştırma yeniden bağlar.
- Çalıştırma: `client.simple_query(sql)` → `SimpleQueryMessage::{Row, CommandComplete, RowDescription}`.
  Satırlar metin; NULL `None`. `max_rows` dolunca okumayı bırakır (`has_more=true`); kalan
  mesajlar atılır ya da sorgu iptal edilir (büyük sonuçlarda iptal daha ucuz).
- Simple protokolde kolon tipi gelmez (`SimpleColumn` yalnızca ad taşır); grid hizalama ve JSON önizlemeyi hücre metninden çıkarır. Tablo kolonlarının tipi introspection'dan bilinir.
- İptal: `client.cancel_token()` konsol başına saklanır; `Esc` → `cancel_query(NoTls)`.
  Sunucu `57014 query_canceled` döndürür; `cancelled=true` ile işaretlenir.
- Zaman: ilk mesaj gelince `execution_ms`, son mesajda `elapsed_ms`.
- Hata: `tokio_postgres::Error::as_db_error()` → mesaj, detail, hint, position (karakter → byte),
  SQLSTATE. Position, çalıştırılan statement'ın editördeki başlangıcına eklenip işaretlenir.
- Çoklu statement: `split.rs` böler; `Cmd+Shift+Enter` sırayla çalıştırır, ilk hatada durur.
  Her statement kendi `QueryResult` üretir; MVP'de yalnızca sonuncusu grid'de, öncekiler durum
  satırında özetlenir (tur 2: sonuç sekmeleri).

`split.rs` değişiklikleri: UTF-16 yerine byte offset; yalnızca Postgres kuralları
(`$$ … $$`, `$tag$`, `--`, `/* */`, `'…'`, `"…"`, `E'…'`).

## 7. Introspection (`db/introspect.rs`)

Hepsi `pg_catalog` üzerinden, `information_schema`'dan hızlı:

- Schemas: `pg_namespace` (`pg_catalog`, `information_schema`, `pg_toast*` gizli; "Show system schemas" ile açılır).
- Tables: `pg_class` + `relkind in ('r','p','v','m','f')`, `pg_description` ile yorum.
- Columns: `pg_attribute` + `format_type` + `pg_attrdef` + PK için `pg_index`.
- Satır sayısı tahmini: `reltuples` (ağaçta tablo yanında soluk).

Yükleme düğüm genişletilince; sonuç bellekte, düğüm bağlam menüsünden "Refresh".
Introspection konsol bağlantısını kullanmaz; veri kaynağı başına ayrı bir "meta" bağlantısı
açılır (konsollardaki transaction'ları etkilememek için).

## 8. Arayüz

### Düzen
```
┌──────────────┬─────────────────────────────────────────┐
│ Data sources │ [socradar@prod · 1] [· 2] [+]            │
│ ▸ socradar@… │ ▶ Run  ■ Stop   limit [500 ▾]  ● read-only│
│   ▸ Consoles │ SELECT …                                  │
│   ▸ public   │                                           │
│     ▸ tables ├───────────────────────────── (bölücü) ────┤
│ ▸ jarvis@…   │ id │ name   │ payload (jsonb)   │ created │
│              │ 1  │ …      │ {"a":1,"b":[…     │ 2026-…  │
│   [+ Add]    ├─────────────────────────────────────────┤
│              │ 500 rows (more) · 12 ms / 180 ms          │
└──────────────┴─────────────────────────────────────────┘
```
- Sol genişlik ve editör/grid oranı sürüklenir; Inbox'taki `diff_tree_width` kalıbı
  (`inbox/ui.rs:2379`) ve `shell.rs` `ResizeTarget`.
- Konsol sekmeleri seçili veri kaynağına ait. Veri kaynağı değişince sekme şeridi değişir;
  her konsolun son sonucu kendisiyle kalır.

### Ağaç (`tree.rs`)
`uniform_list`; düğüm başına ikon, ad, soluk tip/sayı. Bağlam menüsü: Connect / Disconnect,
New console, Refresh, Edit, Remove. Veri kaynağı rengi ad önünde nokta olarak.

### Konsol (`console.rs`)
- Editör `Input::editor` + `set_editor_path("<n>.sql")`; SQL için tree-sitter yok, `syntax.rs`
  syntect fallback'i (`.sql`) renklendirir. Doğrulanacak: fallback bu yoldan tetikleniyor mu.
- `Cmd+Enter`: seçim varsa seçim; yoksa `split` ile imlecin içinde olduğu statement.
  Çalışan statement editörde soluk arka planla işaretlenir.
- Araç çubuğu: Run, Stop (yalnızca çalışırken), limit seçici, read-only rozeti, bağlantı durumu.
- Konsol metni 500 ms debounce ile diske yazılır; sekme kapatılınca dosya silinir (onay ile).

### Grid (`grid.rs`)
- Satırlar `uniform_list`; sabit satır yüksekliği 22 px. Yatay kaydırma dış `overflow_x_scroll`.
- Kolon genişliği: ilk 100 satırın ve başlığın ölçümünden, min 60 / max 400 px; başlık kenarından sürüklenebilir.
- Hücre metni **tek satır, ≤ 200 karakter**, `\n` → `⏎`; shaping yalnızca bu kısa metne uygulanır.
  Tam değer `rows` içinde durur.
- NULL italik soluk `NULL`; sayısal tipler (`int*`, `numeric`, `float*`) sağa hizalı; `bool` olduğu gibi.
- Tıkla: hücre seç; `Cmd+C`: hücre (tam değer); `Shift+Cmd+C`: satır TSV; başlık tıkla: kolon seç.
- Çift tık / `Space`: önizleme paneli. `json`/`jsonb` ise `serde_json` ile pretty-print, syntect
  ile renklendirme; diğerleri düz metin, monospace, kaydırılabilir.
- Sütun başlığında tip adı tooltip olarak.

### Durum satırı ve hata (`status.rs`)
Sol: sonuç özeti. Sağ: süreler. Hata: kırmızı panel, mesaj + detail + hint; `position` varsa
editörde ilgili konum altı çizili ve tıklayınca imleç oraya.

### Veri kaynağı diyaloğu (`source_dialog.rs`)
Alanlar: Name, Host, Port, Database, User, Password (anahtarlıkta sakla ☐), Read-only ☑,
Color, Comment; Advanced: application_name, statement_timeout, search_path.
**Test Connection**: bağlanır, `SELECT version()` çalıştırır, sonucu ya da hatayı diyalogda gösterir.
`modal::overlay` ile; Commit diyaloğu kalıbı.

### Klavye
| Kısayol | İşlev |
|---|---|
| `Cmd+Enter` | statement / seçimi çalıştır |
| `Cmd+Shift+Enter` | tüm script |
| `Esc` | çalışan sorguyu iptal et (editörde odak varken) |
| `Cmd+T` | yeni konsol (Database görünümü aktifken) |
| `Cmd+W` | konsolu kapat |
| `Cmd+C` / `Shift+Cmd+C` | hücre / satır kopyala |
| `Cmd+R` | seçili ağaç düğümünü yenile |

## 9. Performans kuralları
- GPUI iş parçacığında DB çağrısı yok; her şey runtime'da, sonuç kanal ile.
- Grid'e verilen metin her zaman kısaltılmış; 500 satır × 50 kolon bile shaping bütçesini aşmaz.
- Sonuç `Arc<QueryResult>`; sekme değişimi kopya yapmaz.
- Ağaç düğümleri tembel; kapalı schema için sorgu atılmaz.
- Hang watchdog (`STALL = 2s`) ihlalleri `frames.log` ile izlenir; grid ilk kez açılınca ölçüm alınır.

## 10. Güvenlik
- Şifre loglanmaz, `Debug` çıktısında maskelenir; `datasources.json` şifre içermez.
- Yeni kaynakta read-only varsayılan açık; kapatmak diyalogda bilinçli tercih.
- Konsol dosyaları 0600.
- `Test Connection` ve bağlantı hataları ham sunucu mesajını gösterir; parolayı içermez.

## 11. İş paketleri

Her paket ayrı commit; derleme + clippy + ilgili testler geçmeden sonrakine geçilmez.
Push yalnızca istendiğinde.

| # | Paket | Çıktı | Tahmin |
|---|---|---|---|
| 1 | `db/runtime.rs`, `db/source.rs`, `datasources.json`, keyring | model + saklama, birim testleri | ½ oturum |
| 2 | `db/split.rs` taşıma (byte offset, Postgres kuralları) | testler openidea'dan uyarlanır | ¼ |
| 3 | `db/conn.rs`: bağlan, `simple_query`, limit, cancel, zaman, hata | entegrasyon testleri (`MONOCODE_TEST_PG_URL` varsa) | ¾ |
| 4 | `db/introspect.rs` | schema/tablo/kolon; entegrasyon testi | ½ |
| 5 | `AppView::Database`, sidebar öğesi, boş gövde, `state.json` | görünüm açılıyor; gpui testi | ½ |
| 6 | Veri kaynağı diyaloğu + Test Connection | kaynak eklenebiliyor | ¾ |
| 7 | Ağaç + bağlan/kopar + tembel yükleme | ağaç geziliyor | ¾ |
| 8 | Konsol sekmeleri + editör + Run/Stop + diske yazma | sorgu çalışıyor, sonuç henüz ham | 1 |
| 9 | Grid + durum satırı + hata paneli | MVP görünür | 1–1½ |
| 10 | Önizleme paneli, kopyalama, kısayollar, cila | MVP tamam | ½ |

Toplam ≈ 6–7 oturum. Paket 1–4 arayüzden bağımsız, 5–10 sırayla.

## 12. Testler
- `monocode-core`: split (dollar quoting, yorumlar, string içinde `;`), config üretimi
  (read-only option satırı), hata eşlemesi (position byte dönüşümü), `datasources.json` gidiş-dönüş,
  keyring (yalnızca macOS'ta, `#[ignore]` değil ama anahtarlık yoksa atlar).
- Entegrasyon: `MONOCODE_TEST_PG_URL` tanımlıysa gerçek Postgres'e karşı execute / limit / cancel /
  introspection; yoksa test atlanır (CI'da yok).
- `monocode-gpui`: sidebar'da Database öğesi, görünüm açma/kapama, konsol ekleme/kapatma,
  grid'in 500 satırı sanal render etmesi (`uniform_list` öğe sayısı), kısaltma kuralı.

## 13. Açık sorular
1. Yerel test için Postgres: Docker ile `postgres:16` kullanabilir miyim, yoksa `pg_ctl` ile geçici
   küme mi? (Entegrasyon testlerinin nasıl koşacağını belirler.)
2. Prod kaynaklarında `statement_timeout` varsayılanı istiyor musun (örn. 60 s)? Yanlışlıkla
   ağır sorguyu engeller; iptal zaten var.
3. Konsol sekmesi başlıkları: `socradar@prod · 1` mi, yoksa ilk satırdaki `-- başlık` yorumu mu?
   Önerim: yorum varsa o, yoksa numara.
