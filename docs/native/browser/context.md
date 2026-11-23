# Browser: Chromium tabanlı tam browser — bağlam ve kararlar

Durum: karar dokümanı; Faz 0–3 bitti. Başlangıç: 2026-11-20. Branch: `browser/cef-spike`.
Bu doküman işin **neden**ini, verilen **bütün kararları**, teknik **detayları** ve **faz planını**
(§12) toplar. Faz bitişleri ve dokümanın güncel tutulma kuralı §13'te.
Araştırma raporları `research/` altında (İngilizce):

| Rapor | İçerik |
|---|---|
| `research/01-orca-features.md` | Orca browser'ının 181 özelliği, 20 grup, kaynakları ve kime hitap ettiği |
| `research/02-orca-core-architecture.md` | Orca'nın gömme, oturum, sekme, yerleşim ve IPC mimarisi |
| `research/03-orca-agent-cdp-network.md` | Agent kontrolü, CDP proxy, snapshot, element grab, cookie import, screencast |
| `research/04-orca-full-browser-subsystems.md` | Eklentiler, şifreler, passkey, izinler, indirmeler, diyaloglar, discard |
| `research/05-rust-chromium-embedding-options.md` | Rust/GPUI'ye Chromium gömme seçenekleri, CEF detayları, Atlas/Dia/Arc karşılaştırması |

---

## 1. İş tanımı

Monocode native uygulamasına (Rust + GPUI, `crates/monocode-gpui`, ortak servisler
`crates/monocode-core`) **Chromium tabanlı, tam özellikli bir browser** gelecek.

- Mevcut WebKit tabanlı browser **tamamen kaldırılır**. Tasarımda ona referans verilmez,
  detayları kullanılmaz; sıfırdan yazılıyormuş gibi davranılır.
- Referans ürün **Orca** (`stablyai/orca`, Electron 43, incelenen sürüm 1.4.259 /
  repo `45aa8730`). Orca'nın browser'ındaki özellikler fazlar hâlinde kopyalanır.
- Önce **geliştirici browser'ı** (localhost, DevTools, agent'ın test etmesi), sonra
  **tam fonksiyonlu browser** (şifreler, eklentiler, profiller, import).
- İlk hedef: Monocode içinde **aşırı performanslı** bir Chromium sekmesi açmak
  (Chrome ile aynı kare hızı ve input gecikmesi, 120 Hz ProMotion).

## 2. Karar listesi

| # | Konu | Karar |
|---|---|---|
| K1 | Motor | **CEF** (Chromium Embedded Framework), Rust'tan `cef` crate'i (tauri-apps/cef-rs) ile. Hedef sürüm CEF 156.x (Chromium 156). |
| K2 | Kurulu Chrome | Kullanıcının kurulu Google Chrome'u **yönetilmez**. Orca gibi kendi gömülü Chromium'umuz + Chrome'dan **içe aktarma** (cookie, şifre, eklenti). İsteğe bağlı "harici Chrome'a ayrı profille bağlan" modu ileride, öncelik değil. |
| K3 | Çizim (compositing) | Ana yüzey **native child view** (macOS NSView). **OSR** (ekran dışı çizim → GPUI dokusu) sadece GPUI'nin sayfa üstüne çizmesi gereken yerlerde (overlay, sekme önizlemesi, agent işaretleri) ve Linux/Wayland'de. |
| K4 | Eklentiler | Gömülü sekmelerde ilk fazlarda eklenti/Chrome şifre yöneticisi/izin balonu **olmayacağı kabul**. Hedef: seçilmiş **20–30 eklenti** çalışsın (tüm Chrome API yüzeyi değil). |
| K5 | Öncelik | Önce dev browser, sonra tam browser. |
| K6 | Agent katmanı | Agent araçları **Orca'nın komut setine göre sıfırdan** tasarlanır, süreç içi **CDP** üzerine kurulur (enjekte JS değil). Ayrıca dış araçlar için sekme başına **Chrome uyumlu CDP uç noktası** (dev-browser `--connect`, agent-browser, Playwright, chrome-devtools-mcp). agent-browser uygulamaya paketlenmez. |
| K7 | CEF dağıtımı | **Revize (Faz 0, 2026-11-20):** CEF framework'ü **uygulama paketinin içinde** gelir (`Contents/Frameworks`). İlk karar "ayrıca indirilir" idi; Chromium'un macOS sandbox'ı helper süreçlerine yalnızca çalışan uygulamanın bundle'ından okuma izni verdiği için indirilen framework sandbox açıkken yüklenemedi (`main_bundle_path` ile ayrı runtime bundle'ı da denendi, işe yaramadı). Sandbox'ı kapatmak yerine gömme seçildi. |
| K8 | İndirme zamanı | **Geçersiz** (K7): indirme yok. CEF ilk sayfa gösterildiğinde başlatılır (lazy). |
| K9 | Framework kaynağı | **Revize:** framework, `cef-dll-sys`'in build sırasında indirdiği CEF SDK'dan (Spotify minimal dağıtımı) alınır; bundle betiği kopyalar ve bizim kimliğimizle imzalar. |
| K10 | Sürüm bağlılığı | Uygulama **tek, tam bir CEF sürümüne kilitli** (API hash; `cef = "=156.7.0"`). CEF yükseltmesi = crate yükseltmesi + yeni uygulama sürümü. |
| K11 | WebKit | **Tamamen kaldırılır**, yedek olarak tutulmaz. |
| K12 | Platform | **macOS, Windows, Linux** desteklenecek. Uygulama sırası: **önce macOS**, diğerleri sonra. Mimari baştan üç platforma uygun kurulur. |
| K13 | Doğrulama testi | MDM/EDR (Iru, SentinelOne) için ayrı ön test **yapılmayacak**; geliştirme sırasında görülür. |
| K14 | Sekmeler (Faz 1) | **Her browser sayfası bir sekme grubunun sekmesi** (`docs/native/tab-groups.md`; pane içinde ayrı sekme şeridi yok, ayrı başlık sekmesi de yok). Yeni sayfa router'ın seçtiği gruba (odaktaki grup) girer. Popup ve `target=_blank` açan sayfanın grubunda, hemen onun arkasına sekme olarak eklenir; arka planda açılan sayfa odağı çalmaz, gizli sekme olarak bekler. |
| K15 | Profil | **Varsayılan tek ortak profil**; isteyen proje ayarlardan **izole profil** açar (ayrı `CefRequestContext`, `Profiles/<proje>`). |
| K16 | Overlay | Sayfayı örten menü/modal/bildirim varken sayfanın **son karesi** (CDP `Page.captureScreenshot`) GPUI'de çizilir; native view gizlenmez, pencerenin dışına taşınır (gizlenen view'da Chromium çizimi durdurur ve görüntü alınamaz). Kalıcı çözüm OSR ile Faz 3'te. |
| K17 | Kısayollar | Sayfa odaktayken uygulamanın ⌘/⌃ kısayolları kazanır; **düzenleme kısayolları (⌘C/V/X/A/Z, ⇧⌘Z) sayfada kalır**. ⌘F bizim sayfada bul, ⌘R sayfa yenile, ⌘L adres çubuğu, ⌘+/−/0 sayfa zoom'u. |
| K18 | DevTools | **Revize (Faz 1):** ayrı pencere. Yapışık hâli denendi; CEF macOS'ta DevTools'u Alloy style açamıyor ("Alloy style is not supported for this browser") ve Chrome style bir host view'un içine konamıyor: `ShowDevTools` child view ile CHECK'te durup uygulamayı kapattı. **Revize (Faz 2):** yapışık DevTools geldi (K28); ayrı pencere yalnız yedek yol. |
| K19 | Link yönlendirme | localhost ve proje portları Monocode browser'ında, diğerleri sistem tarayıcısında açılır; ayarla "her şey Monocode'da" seçilebilir; **⇧ ile tıklama diğer tarafı** açar. |
| K20 | Restore | Önceki oturumun sekmeleri **görünür olunca** yüklenir (lazy). Discard edilen sekme aynı yolla yüklenir (K64). |
| K21 | Agent kapsamı (Faz 2) | Agent **oturumun projesindeki** browser sekmelerini ve **kendi açtıklarını** görür ve sürer; başka projelerin sekmeleri görünmez. |
| K22 | Agent profili | Agent'ın açtığı sekme varsayılan olarak **projenin profilini** kullanır (login'ler dahil); `isolated: true` ile **bellekte geçici profil** (boş `cache_path`, kapanınca silinir). |
| K23 | Arka plan sekmesi | Agent'ın sürdüğü görünmeyen sekme, overlay'deki gibi **pencerenin dışına taşınmış görünür view**'da yaşar (çizim ve screenshot sürer). Gerçek headless Faz 3'te OSR ile. |
| K24 | Faz 2 araç seti | tabs (list/open/close/show), goto/back/forward/reload, wait (text/url/selector/load), snapshot (interactive/selector/depth), click/fill/type/press/scroll, screenshot, eval, console, network, **dialog**. Geri kalanı Faz 3 (dış CDP uç noktasıyla Playwright zaten yapabilir). |
| K25 | Onay | Allow list'te olmayan, kullanıcı onayı isteyen araç: **`browser_eval`** (ve ileride cookie/storage yazma). Gerisi serbest (`db_execute` modeli). |
| K26 | Dış CDP uç noktası | **Profil başına, browser seviyesinde** uç nokta: birden çok target, `Target.createTarget` yeni sekme açar (Playwright `connectOverCDP`, dev-browser `browser.newPage()`). Her zaman bizim proxy'miz; CEF `remote_debugging_port` kapalı. Loopback, rastgele token, `Origin` başlıklı istek red, Host kontrolü. |
| K27 | Uç noktanın dağıtımı | Ayarlardan açılıp kapanır; **varsayılan açık** (2026-11-22, kullanıcı kararı; ilk sürümde kapalıydı, kayıtlı ayarlar bir kez açığa geçirilir). Monocode'un başlattığı agent oturumlarına env (`MONOCODE_BROWSER_CDP`) ile verilir, argv'ye yazılmaz; ayrıca ayarlarda **kopyala** düğmesi. Sabit port (9222) yok. |
| K28 | Yapışık DevTools | **Yapıldı (Faz 2):** DevTools frontend'i (`devtools://devtools/bundled/devtools_app.html?ws=…`) sayfanın altında kendi Alloy view'unda, proxy'nin sayfa uç noktasına **iç token** ile bağlı (yalnız `devtools://devtools` origin'i; dış CDP ayarı kapalıyken de çalışır). Ayrı pencere yedek. |
| K29 | Faz 1 artıkları | Agent işiyle **paralel**, ayrı hatta: `cef::shutdown`, `window.opener`, sahipsiz popup, ölçüm. Diyaloğa odak agent hattında (dialog aracıyla birlikte). |
| K30 | Test | Birim (snapshot fixture, proxy guard) + GPUI testleri + **gerçek CEF ile uçtan uca**: yerel test sayfası, MCP araçlarıyla senaryo, Playwright `connectOverCDP` smoke testi. |
| K31 | Snapshot formatı | Girintili metin: `[@e3] button "Save"`, landmark'lar `[Main Content]`, satır sonunda durum ekleri (`checked`, `value="…"`, `focused`, `disabled`, `expanded`). JSON değil. |
| K32 | Snapshot boyutu | Varsayılan `interactive` (etkileşimli öğeler, başlıklar, landmark'lar); `full` isteğe bağlı. İkisi de **30 KB**; kesilen yerde "`selector`/`depth` ile daralt" ipucu. |
| K33 | Stale ref | Ref'ler belge kimliğine (main frame loader id) bağlı. Aynı belgede node kaybolduysa **role + name + nth** ile kurtarma; navigasyondan sonra `browser_stale_ref`. |
| K34 | Kullanıcı ↔ agent | Kullanıcının bir sekmeye son 2 sn'de input'u varsa agent'ın input aksiyonu `browser_user_active` ile döner; çalışan aksiyon kesilmez. Kullanıcı input'u hiç engellenmez. |
| K35 | Kuyruk | Sekme başına FIFO; CDP çağrıları main thread'de. Komut başına zaman aşımı 30 sn (`wait` en çok 120 sn). Sekme kapanır/çökerse bekleyen komutlar hemen `browser_page_gone`. |
| K36 | Console/network | Her sayfada **açılıştan itibaren** ring buffer (console 500, network 300 girdi; gövde yok, `Network.getResponseBody` isteğe bağlı). |
| K37 | Screenshot | MCP image content; varsayılan viewport, JPEG, en uzun kenar ≤1568 px; `full_page` ve `path` (dosyaya yaz) isteğe bağlı. |
| K38 | Ortak CDP oturumu | **Revize (Faz 2):** sayfanın ham DevTools kanalı tek ve paylaşımlı (`CdpClient`), ama her dış istemci ve DevTools frontend'i kanal üzerinden sayfaya **kendi oturumunu** açar (`Target.attachToTarget` kendine, flatten). Sebep: V8 ikinci `Runtime.enable`'ı yok sayar, Playwright takılırdı. Ortak oturum yalnız yedek yol (o zaman `*.disable` yutulur). `Emulation.*` hâlâ sayfa geneli: istemcinin ayarları iç araçları ve kullanıcıyı da etkiler. İstemcinin `Target.setAutoAttach`'i `waitForDebuggerOnStart:false, flatten:true`'ya zorlanır, kapatılamaz. |
| K39 | Agent sekmesinin yeri | Oturumun **chat'inin bulunduğu grup**, arka planda (odak çalmaz); chat görünmüyorsa odaktaki grup. |
| K40 | URL politikası | **Revize (Faz 2):** izin listesi: http/https, `about:blank`, data, blob. Diğerleri (`file:`, `chrome:`, `devtools:`, `javascript:`, `view-source:`) `browser_url_refused`. |
| K41 | MCP sunucusu | Adı `monocode_browser`, araçlar `browser_*`; `db`/`code` köprüleriyle aynı desen (loopback listener + oturum token'ı + `browser-mcp` stdio alt komutu), ama her çağrı **main thread'e** gider. Hata kodları sabit: `browser_no_page`, `browser_page_gone`, `browser_stale_ref`, `browser_no_ref`, `browser_user_active`, `browser_dialog_open`, `browser_timeout`, `browser_url_refused`, `browser_cdp_error`, `browser_not_started`, `browser_cert_error` (K62: güvenilmeyen sertifika; agent geçemez), `browser_no_login` (K57: `browser_fill_login` için sitede kayıtlı giriş yok, profil geçici, kasa kilitli ya da şifre yöneticisi kapalı). |
| K42 | iframe | Faz 2: aynı origin iframe'leri snapshot'ta; OOPIF (cross-origin) `Target.setAutoAttach` flatten ile ayrı oturum, ref → `sessionId`. |
| K43 | Faz 3 kapsamı | OSR Faz 3'ten çıktı. Faz 3 = agent iyileştirmeleri + Grab + PDF/upload/download + Faz 2'den devralınanlar. |
| K44 | Overlay | OSR yapılmaz; "son kare" (K16) kalıcı. GPUI'nin overlay katmanında (deferred/anchored) **tek kanca**: sayfayla kesişen her overlay kendiliğinden cover bildirir, tooltip'ler dahil. OSR Linux/Wayland fazına (Faz 6) bağlandı. |
| K45 | Aksiyon etkileri | click/fill/type/press/scroll **her zaman** etkilerini döner: navigasyon başlarsa 50 ms pay + en çok 5 sn oturma, sonra 100 ms DOM sessizliği (en çok 750 ms). İçerik: URL/başlık değişimi, yeni sekme/popup, indirme, console hataları, diyalog, **en çok 40 satır delta**; fazlası "snapshot al" der. |
| K46 | Delta snapshot | `browser_snapshot {delta: true}` isteğe bağlı; baseline sayfanın son snapshot'ı. `expect_url` her sayfa aracında isteğe bağlı guard (`browser_expected_url_mismatch`). |
| K47 | Varlık UI'ı | Agent'ın kullandığı sekmede **sekme rozeti** + araç çubuğunda "Agent kullanıyor · <oturum>" etiketi ve kontrol düğmesi; son komuttan 20 sn sonra kalkar. İmleç animasyonu ve parıltı **yok**. |
| K48 | Kontrol | Agent aktifken (son komut ≤20 sn) kullanıcının sayfaya **tıklaması veya tuşu** kontrolü alır (scroll ve fare hareketi almaz). Agent'ın input araçları `browser_user_control` döner (ne zaman yeniden deneyeceğini söyler). Kontrol, "Agent'a geri ver" düğmesiyle ya da kullanıcı **16 sn** dokunmayınca agent'a döner. K34'ün 2 sn kuralının yerini alır. |
| K49 | Grab | Araç çubuğu düğmesi + kısayol + sağ tık "Agent'a gönder". Seçilen öğe chat composer'ına **pill** olarak gider: sekmeyi bir agent açtıysa onun oturumuna, değilse odaktaki chat'e. İçerik: seçici, a11y rol/isim, yakın metin, computed style özeti, ≤4 KB HTML, React bileşen adı + kaynak dosya, kırpılmış görüntü. Sırlar (şifre alanı değerleri, token benzeri öznitelikler) temizlenir. |
| K50 | Yeni araçlar | `browser_pdf`, `browser_upload`, `browser_download` yerel araç. HAR ve intercept yalnız dış CDP (Playwright) üzerinden. |
| K51 | Dosya sınırı | Upload yalnız proje klasöründeki dosyalar; download ve PDF yalnız proje klasörüne yazar. |
| K52 | Battery Saver | **Revize (Faz 4 planı, 2026-11-23):** düşük pilde 30 fps yalnız **arka plan sekmelerine** uygulanır; görünür sekme ve agent'ın sürdüğü sekme tam hızda kalır. Settings → General'dan kapatılabilir. **Uygulama (Faz 4):** Chromium'un Battery Saver'ı küresel (bütün frame sink'leri 30 fps'e kısar, görünen sayfa dahil) ve windowed view'da sekme başına kare hızı yok (`set_windowless_frame_rate` yalnız OSR). Bu yüzden Chromium'unki hep kapalı (`battery_saver_mode.state` = 0). Gizli sekme zaten hiç kare çizmiyor; bizim Battery Saver'ımız düşük pilde (IOKit erken uyarı <%22 veya Low Power Mode) lease'i olmayan, 1 dk boşta arka plan sekmesini **donduruyor** (`Page.setWebLifecycleState frozen`), gösterilince/agent dokununca çözülüyor. |
| K53 | Faz 3 iş bölümü | Paralel agent'lar ayrı worktree'lerde düzenler ama **tek ortak `CARGO_TARGET_DIR`** ile build eder (disk). Doğrulama: birleşik dalda uçtan uca agent + kullanıcının uygulamasında araçlarla tur. |
| K54 | Faz 4 bölünmesi | **4a**: Faz 3 artıkları, Memory Saver/throttle, temel özellikler (izinler, sertifika, geçmiş, yeni sekme, ses, yazdırma). **4b**: şifre yöneticisi, autofill, passkey (hybrid), import. İlk iş `posix_spawn` geçişi (§11 "Çıkıştan sonra kalan süreç"). |
| K55 | Veri kapsamı | Şifreler ve site izinleri **global** (profil başına). Geçmiş **proje bazlı** kaydedilir, global aranabilir. |
| K56 | Şifre kasası | Tek **şifreli SQLite** dosyası; anahtar Keychain'de, tek onay/Touch ID ile açılır (kayıt başına Keychain item'ı yok). **Uygulama (Faz 4):** profil başına `<data>/Browser Passwords/<profil>.db` (geçmiş gibi Chromium'un klasörünün yanında; geçici profil yok). Site ve kullanıcı adı açık, şifre değer başına AES-256-GCM (`ring`; rastgele nonce, satırın origin'i AAD), dosyada anahtar kontrol değeri. Anahtar rastgele 256 bit, Keychain'de tek generic-password item'ı (servis `dev.usemono.gpui.browser-passwords`, hesap veri klasörüne göre), çalışma başına bir kez arka planda okunur (Chromium başlarken, kayıt varsa). Ret/iptal → çökme yok, şifreler "unavailable", bant/Settings'te sebep ve "Try again"; kasalar varken anahtar yoksa yeni anahtar yazılmaz. Göster/kopyala/düzenle öncesi LocalAuthentication (Touch ID ya da hesap parolası), 5 dk geçerli; kopyalanan şifre 2 dk sonra panodan silinir. Kayıt: isolated world'de `watch.js` (submit + şifre alanı → aynı belgede form giderse ya da sonraki belge 2,5 sn şifre alanı göstermezse başarılı; form geri gelirse başarısız; iki adımlı girişte kullanıcı adı 5 dk tutulur), araç çubuğunun altında "Save / Never for this site / ×" bandı, değişen şifre "Update". Autofill: giriş alanı odaklanınca araç çubuğunda "Fill as …" (K44: sayfanın üstüne açılır liste yok), adres çubuğunda anahtar simgesi site girişlerini listeler; doldurma CDP ile isolated world'de (değer argüman, `input`/`change` olayları). |
| K57 | Agent ↔ şifre | Agent kayıtlı girişi **doldurabilir ama göremez**: şifre değerleri snapshot, console, network, Grab ve `browser_eval` sonuçlarında maskelenir. **Uygulama (Faz 4):** `browser_fill_login {ref?, username?}` (input aracı, sonuçları K45 etkileri). Sayfa araçlarının bütün cevapları (hatalar, etkiler/delta dahil) sitenin kayıtlı şifreleri ve kullanıcının o sayfada girdiği şifrelerle `••••` olarak maskelenir (düz, JSON-escape'li, URL-encode'lu biçimler); snapshot şifre alanının değerini hiç göstermez; Grab metni maskelenir; dış CDP uç noktasının istemcileri uygulamanın `__monocode…` binding çağrılarını duymaz. Dönüştürülmüş şifre (hash, base64) maskelenmez. |
| K58 | Passkey | Yalnız **telefonla QR (hybrid/caBLE)**. USB anahtar ve platform passkey (Touch ID / iCloud Keychain, Apple'ın `web-browser.public-key-credential` entitlement'ı) kapsam dışı. QR öncesi `CBManager.authorization` kontrolü; izin yok/Bluetooth kapalı → sayfada "Ayarları aç" bandı; agent'a `passkeyRequest` içinde `bluetooth_denied`. Telefonda 1Password/iCloud passkey'leri bu yoldan çalışır. |
| K59 | Dev imzası | Geliştirici build'lerinde sabit yerel sertifika (`MONOCODE_NATIVE_LOCAL_SIGNING`) varsayılan: ad-hoc imzada TCC izni (Bluetooth, kamera, mikrofon) ve Keychain "Always Allow" her build'de düşüyor. |
| K60 | 1Password | Faz 5'in ilk hedef eklentisi; eklenti mimarisi seçiminde ölçüt (main-world WebAuthn override, content script, native messaging). Eklenti kuruluyken kendi şifre yöneticimiz kayıt/öneri göstermez (ayarla açılır). **Faz 4:** Settings → Passwords "Offer to save passwords" (varsayılan açık); hepsi `browser::passwords::password_manager_enabled()`'a bakar, Faz 5'in 1Password kuralı oradaki `password_extension_installed()`'a girer. |
| K61 | Import | **Chrome, Arc, Brave** birlikte; cookie + şifre. Elle başlatılır, tek seferlik; yeniden import birleştirir. Otomatik senkron yok. **Faz 4:** şifreler hedef profilin kasasına `merge_logins` kuralıyla girer (origin + kullanıcı adı; şifresi farklı ve daha yeni olan kazanır, eşitlikte kayıtlı kalır), "never save" siteleri kasanın listesine; kasa açılamazsa "read but not saved: <sebep>". |
| K62 | Sertifika | `localhost`, `*.localhost`, `*.local`, loopback, özel ve link-local IP'lerde (ve `0.0.0.0`) self-signed otomatik kabul. Diğerlerinde sayfa yerine GPUI uyarısı ("Back to safety" / "Proceed (unsafe)"); istisna host + sertifika parmak izi, yalnız uygulama açıkken, diske yazılmaz. Adres çubuğunda "Not secure". Agent'a `browser_cert_error`. |
| K63 | İzin ↔ agent | Agent'ın arka plan sekmesinde izin istekleri varsayılan **red** ve agent'a bildirilir (`permissionRequest`, `denied_background`); kullanıcının baktığı sekmede soru kullanıcıya gelir (araç çubuğunun altında, sayfayı örtmeyen bant). Agent izin veremez. Kararlar profil başına `Profiles/<profil>/monocode-site-permissions.json`. |
| K64 | Memory Saver | Discard edilmez: ses çalan, aktif, agent'ın son kullandığı (lease), doldurulmamış formu olan, DevTools'u açık sekmeler. Agent ve görünürlük **throttle lease**'leri aynı mekanizma. **Uygulama (Faz 4):** `browser/lifecycle.rs` (saf politika). Lease'ler: görünür/örtülü/agent için pencere dışında çizilen, çalışan komut veya bağlı dış CDP, son 20 sn'de agent, DevTools, capture, süren indirme, bekleyen diyalog, sabitlenmiş sekme; sayfaya sorulanlar (discard/dondurmadan hemen önce, izole world): çalan medya, `submit` edilmemiş elle düzenlenmiş form, `beforeunload` dinleyicisi, cevap yok. Ayar: Settings → General "Browser memory saver" (Kapalı/15/30/60/240 dk, varsayılan 30). Discard: CEF browser kapanır, sekme URL/başlık/zoom/scroll'u tutar, K20 yoluyla görünce ya da agent kullanınca yeniden yüklenir (`browser_tabs` "discarded" der); geri/ileri geçmişi kaybolur. |
| K65 | Geçmiş ↔ agent | Agent yalnız **oturumun projesinin** geçmişini görür (`browser_history`). |
| K66 | Yeni sekme | Projenin çalışan dev sunucuları (açık portlar), son açılanlar, en sık ziyaret edilenler. |

## 3. Referans: Orca'nın yaptığı ve bizim aldığımız

Orca masaüstünde her sayfayı bir Electron `<webview>` olarak gösterir; headless modda
offscreen `BrowserWindow`. Harici Chrome backend'i yoktur, kurulu Chrome'u hiç sürmez.

### 3.1 Alınacak tasarım kararları
1. **Kalıcı `pageId`**: sayfa kimliği canlı renderer'dan bağımsız. Lazy oluşturma, discard,
   crash recovery, profil değişimi sırasında kimlik, sekme ve geçmiş korunur.
2. **Görünüm asla yeniden yaratılmaz**: sekme split'ler arası taşınınca sadece konumu değişir;
   UI unmount olursa görünüm "park" edilir. GPUI'de: sayfa sahibi view ağacının dışında,
   layout sadece konum verir.
3. **Olay güdümlü bounds senkronu**: pencere olayları senkron tetikler, boşta iş yapılmaz.
4. **Tek otorite**: yeni sayfayı URL/profil/politika ile her zaman merkezî host oluşturur;
   politika ilk navigasyondan önce kurulur.
5. **İki aşamalı discard** (Memory Saver): uyanık-saat ile ölçülen boşta süre; yüklenirken,
   DevTools/agent bağlıyken, kaydedilmemiş form, ses çalarken atılmaz; aynı id ile geri gelir.
6. **Sayaçlı throttle lease**: screencast, agent, capture arka plan throttling'ini kapatır;
   son lease bitince açılır.
7. **Doğru sekme görünürlüğü**: arka plan sekmesi gerçekten `hidden` olur.
8. **Popup politikası merkezde**: normal açılışlar sekme; OAuth/isimli popup'lar opener'ı
   koruyan ayrı pencere; jestsiz script açılışlarına hız limiti (4 sekme / 2 sn);
   agent'ın açtığı sekme odağı çalmaz.
9. **Tek kısayol tablosu**: uygulama kısayolları sayfadan önce yakalanır, gerisi sayfaya gider;
   IME kompozisyonu, AltGr ve agent'ın enjekte ettiği tuşlar hariç.
10. **Sayfa başına tek screencast, çok abone, kredi tabanlı akış kontrolü** (uzak/mobil görüntü).

### 3.2 Orca'nın agent katmanı (detay: `research/03`)
- Vercel `agent-browser` CLI'ını kullanır; her sekmenin önünde token korumalı, loopback,
  kendini tek sayfalı Chrome gibi tanıtan **CDP proxy**: discovery'yi yerelde cevaplar,
  `Page.bringToFront`'u yutar, screenshot/PDF/navigasyon/input'u kendi yolundan yürütür,
  gerisini debugger'a iletir.
- Repoda hazır ama kullanılmayan **snapshot motoru** (`src/main/browser/cdp/snapshots/snapshot-engine.ts`):
  accessibility tree + etkileşimli roller + `cursor:pointer`/`onclick` DOM taraması,
  ref → DOM node id, iframe'ler ayrı. **Rust portu için referans.**
- Komut seti: tab list/create/switch/close, goto/back/forward/reload, wait (text/url/selector/load),
  snapshot (`--interactive`, `--delta`, `--selector`, `--depth`), screenshot (`--annotate`),
  full-screenshot, pdf, click/fill/type/select/check/hover/focus/drag/upload/scroll/keypress,
  koordinat mouse, eval, dialog, device/viewport, cookie, storage, console, network,
  HAR capture, intercept.
- **Aksiyon etkileri**: her aksiyondan sonra navigasyonun oturması beklenir; snapshot diff,
  yeni sekme/popup/indirme/konsol hataları döner. `--expect-url` yanlış sayfada işlemi engeller.
- **Agent varlığı**: bağlantıdan tespit edilen agent komutları sayfayı "agent kullanıyor"
  işaretler (son komuttan 20 sn sonra biter); rozet, parıltı, animasyonlu imleç;
  tıklamadan önce imleç hedefe gider (≤1,5 sn el sıkışma). Kullanıcı odağı geri verilir,
  IME sırasında agent input'u reddedilir, agent tuşları uygulama kısayolu tetikleyemez,
  agent'ın sürdüğü sayfada dosya seçici/yazdırma engellenir, kullanıcı tıklaması kontrolü geri alır.
- **Element grab**: kapalı shadow root + top-layer popover overlay; seçici, yollar, yakın metin,
  sırları temizlenmiş öznitelikler, a11y rol/isim, computed style, ≤4 KB HTML, React bileşen
  adı ve kaynak dosya, öğeye kırpılmış PNG (≤2 MB). Markdown "annotation" olarak agent'a gider
  (sayfa başına ≤20); agent `tab annotations` ile okur.

### 3.3 Orca'nın tam browser alt sistemleri (detay: `research/04`)
- **Eklentiler** (~27 bin satır): Web Store'da "Add to Chrome" için sahte
  `chrome.webstorePrivate`; CRX3 indirme (sadece Google hostları) + Chromium `crx_verifier`
  portu; 5 saatte bir güncelleme, yeni izin uyarısı ekleyen güncellemeyi bekletme.
  Kendi yazdıkları: action toolbar + popup, sidePanel, contextMenus, commands, windows, tabs,
  declarativeNetRequest (tam port), webRequest (gözlem), webNavigation, cookies, downloads,
  notifications, permissions, management, storage.sync (yerel), native messaging.
  Desteklenmeyen: identity, MV2, history, bookmarks, tabGroups, debugger, proxy.
- **Şifreler**: profil başına kasa, her şifre OS keychain ile ayrı mühürlü; Chromium şifre form
  parser portu; başarılı giriş sezgisiyle kaydetme önerisi; kendi autofill dropdown'u;
  Touch ID; 9 şifre yöneticisinden CSV ve Chromium `Login Data` import'u.
- **Passkey**: USB FIDO anahtarları, kendi hesap seçici.
- **Sertifika**: sadece yerel dev host'larda self-signed istisnası, sekmeye ve sertifikaya
  bağlı, 5 dk; diğer sekmelere sızmayı engelleyen guard.
- **İzinler**: origin başına sorulanlar (kamera, mikrofon, konum, bildirim, pano okuma, MIDI,
  window-management, idle, local-network); sessiz verilenler (fullscreen, pointer lock,
  storage access, file system access); geri kalanı (ekran paylaşımı dahil) red; tek kuyruk.
- **İndirmeler**: `~/Downloads`'a çakışmasız isim; agent indirmeyi bir yola talep edebilir.
- **Diyaloglar**: alert/confirm/prompt, HTTP auth, harici uygulama açma → kendi in-pane istemleri.
- **Cookie import**: Chrome, Edge, Arc, Brave, Vivaldi, Chromium, Comet, Helium, Firefox, Safari.
  macOS'ta Keychain `"<Browser> Safe Storage"` → PBKDF2-SHA1 (salt `saltysalt`, 1003 tur,
  16 bayt) → AES-128-CBC (IV 16 boşluk); SHA256(host) 32 baytlık önek atılır; `v20`
  app-bound cookie'ler çözülemez. Google cookie'leri kasıtlı olarak alınmaz. Tek seferlik.

### 3.4 Orca'da olmayanlar
Yer imleri yok; kullanıcıya yazdırma yok (agent'a PDF var); platform passkey yok; ekran
paylaşımı yok; browser MCP sunucusu yok (CLI + skill).

## 4. Hedef mimari

### 4.1 Neden CEF, neden child view
- Atlas (OWL), Dia/Arc, Orca'nın hiçbiri pikselleri ekran dışında çizip kopyalamaz; hepsi
  Chromium'un kendi compositor çıktısını native view/layer olarak gösterir.
- Child view = Chrome.app ile aynı sunum yolu: GPU process → `CAContext` → `CALayerHost`;
  kopya yok, ekranla senkron, 120 Hz. IME, VoiceOver, sürükle-bırak, imleçler, `<select>`
  ve renk seçici popup'ları, bağlam menüleri, tam ekran video bedava.
- Bedeli: GPUI sayfanın **üstüne** çizemez. Sayfayla kesişen GPUI overlay'leri için çözüm
  gerekir (bkz. 4.6).
- Kendi Chromium derlemesi (Atlas/Dia modeli) 2–4 Chromium mühendisi ve sürekli bakım ister;
  şimdilik kapsam dışı.

### 4.2 CEF "style" gerçeği
- CEF 156 her zaman Chrome bootstrap kullanır; stil browser başına seçilir.
- **Alloy style**: content layer + CEF callback'leri, OSR ve native parent destekler.
  **Chrome eklenti API'si yok**, Chrome izin istemi yok, şifre/autofill balonu yok,
  macOS'ta native parent ile print preview yok.
- **Chrome style**: eklenti API'si, izin balonları, autofill/şifre UI, indirme UI, cihaz
  seçiciler. Ama CEF Views penceresi ister (pencere CEF'in), OSR yok; macOS'ta native parent
  desteklenmiyor (CEF issue #3294, 2022'den beri açık).
- **Sonuç**: gömülü sekmeler Alloy style. Chrome-style pencere yalnızca gerektiğinde ayrı
  yüzey (eklenti yönetimi, DevTools vb.). Faz 5 spike'ında doğrulanacak açık soru: Chrome-style
  pencereyle aynı profili paylaşan Alloy sekmelerde eklenti content script'leri çalışıyor mu?
  (Evetse hibrit eklenti modeli mümkün.)

### 4.3 Süreç ve dosya yerleşimi (macOS)
```
Monocode.app (GPUI, Rust)               ← CEF browser process burada (UI thread = main thread)
 └─ Contents/Frameworks/
     ├─ Monocode Helper.app             (utility/network)
     ├─ Monocode Helper (GPU).app       (allow-jit)
     ├─ Monocode Helper (Renderer).app  (allow-jit)
     ├─ Monocode Helper (Plugin).app
     └─ Monocode Helper (Alerts).app
   Helper'lar = tek küçük Rust binary (`monocode-cef-helper`, ~560 KB): sandbox'ı başlatır,
   framework'ü yükler, `cef::execute_process`. Helper adları uygulamanın görünen adını
   taşır (`<App> Helper (Renderer).app`); Chromium varyantları ana helper adından türetir.

     └─ Chromium Embedded Framework.framework   (~250–300 MB, bizim imzamızla)

<veri dizini>/Browser/        ← root_cache_path (veri moduna göre: dev / prod ayrı)
     ├─ Profiles/default/
     ├─ Profiles/<project-id>/       (proje bazlı izolasyon)
     └─ (agent oturumları: boş cache_path = bellekte, kapanınca silinir)
```
- `CefSettings`: `root_cache_path` yukarıdaki; `framework_dir_path` bundle içindeki
  framework'ü gösterir; `browser_subprocess_path` helper; sandbox açık;
  `remote_debugging_port = 0` (CDP süreç içi); `windowless_rendering_enabled` sadece OSR
  kullanılıyorsa.
- `cef` crate macOS'ta framework'ü build sırasında linklemez; çalışma anında yolla yükler
  (`cef_load_library`). Build sırasında CEF SDK (header + `libcef_dll_wrapper` + framework)
  gerekir; cmake + ninja kurulu. SDK `CEF_PATH`'te tutulur: varsayılan
  `crates/monocode-gpui/target/cef-sdk` (`.cargo/config.toml`), native betikleri tüm
  checkout'ların paylaştığı `~/Library/Caches/Monocode/cef-sdk`'yı kullanır.
- CEF **lazy** başlatılır: ilk browser pane gösterildiğinde, o karenin ardından (pane önce
  "Starting Chromium…" gösterir). Ölçüm: sıcak başlatma < 1 s; yeni kopyalanmış bundle'ın ilk
  açılışında ~2,5 s.

### 4.4 NSApp ve mesaj döngüsü (macOS)
- NSApp'i GPUI oluşturur (`GPUIApplication`). CEF, NSApp'in `CefAppProtocol`'e uymasını
  ister; uymazsa `cef_initialize` CHECK ile çöker. Çözüm: Objective-C runtime ile
  `isHandlingSendEvent`/`setHandlingSendEvent:` eklemek, `sendEvent:`'i swizzle etmek,
  protokol uyumunu bildirmek. Referans: `bokuweb/gpui-cef` `platform/cef/nsapp.rs` (MIT/Apache).
- `external_message_pump = 1`: `OnScheduleMessagePumpWork(delay)` bir son tarih kaydedip
  ana run loop'taki `CFRunLoopSource`'u tetikler; kaynak bir `CFRunLoopTimer` kurar ve
  `cef::do_message_loop_work()` çağırır. 60 Hz yedek timer. İç içe döngülerde (menü, modal)
  yeniden girişe karşı guard. gpui-cef ölçümü: medyan ~0,2–1 ms (eski 60 Hz timer ~15 ms).
- Windows/Linux'ta `multi_threaded_message_loop = 1` (CEF kendi UI thread'inde) tercih edilir.

### 4.5 Sekme, pane ve sayfa modeli
- Browser, Monocode'un pane sisteminde bir pane'dir: bir sekme grubunun sekmesi, chat/dosya/
  terminal ile aynı grupta ya da yanında split (`docs/native/tab-groups.md`). Çoklu sayfa
  pane içinde ayrı şerit değil, grubun kendi şeridindeki sekmelerdir.
- Her sekme bir `CefBrowser`; durum (URL, başlık, favicon, yükleniyor, geçmiş, can_go_back/
  forward) `CefDisplayHandler`/`CefLoadHandler`'dan.
- Kalıcı `pageId`; sayfa sahibi (host) GPUI view ağacının dışında; layout sadece bounds verir.
- Aktif olmayan sekmenin view'u gizlenir → Chromium occlusion ile throttling (Faz 0/1'de
  doğrulanacak). OSR'da `WasHidden(true)` + begin frame durur.
- Discard politikası (3.1/5) + `DISPATCH_SOURCE_TYPE_MEMORYPRESSURE`.
- Popup'lar: `CefLifeSpanHandler::OnBeforePopup` → yeni sekme ya da opener'ı koruyan pencere.
- Kalıcılık: workspace'te pane ve sekme listesi (URL, başlık, profil, zoom); restore'da sekmeler
  uyur, görünür olunca yüklenir.

### 4.6 Input, odak, kısayollar, overlay
- **Child view**: AppKit olayları doğrudan Chromium'un view'una gider (scroll fazları,
  momentum, IME dahil).
- **Kısayollar**: `CefKeyboardHandler::OnPreKeyEvent`; tuş Monocode keybinding'iyle eşleşirse
  `true` dönüp GPUI'ye dispatch edilir, değilse sayfaya gider. `OnKeyEvent` sayfanın
  tüketmediği tuşlar için.
- **Odak**: GPUI ↔ sayfa geçişi `CefBrowserHost::SetFocus` + `makeFirstResponder:`.
- **Overlay**: sayfayla kesişen GPUI menü/modal/dialog/sürükleme için iki aşamalı çözüm:
  önce view'u gizleyip son karenin görüntüsünü GPUI'de göstermek; sonra (OSR geldiğinde)
  sayfayı overlay süresince OSR yüzeyine almak veya overlay'leri OSR ile çizmek.
- **OSR yolu** (Linux/Wayland ve overlay yüzeyleri): GPUI mouse/scroll/key olayları →
  `SendMouse*/SendKeyEvent`; IME → GPUI `EntityInputHandler` ↔ `ImeSetComposition/
  ImeCommitText/ImeFinishComposingText`; astral karakterler `ImeCommitText` ile.
- **Agent input**: CDP `Input.dispatchMouseEvent/KeyEvent/insertText`; UI yolundan bağımsız,
  arka plan sekmelerinde de çalışır, `isTrusted=true`.

### 4.7 OSR detayı (gerektiğinde)
- `shared_texture_enabled = 1` + `SetAsWindowless(parent)`; `OnAcceleratedPaint` paylaşımlı
  doku verir (macOS IOSurface, Windows D3D11 shared handle, Linux dmabuf). Doku callback
  dışında tutulamaz → uygulamaya ait 2–3 slotluk ring'e GPU kopyası.
- macOS: IOSurface → `MTLTexture` → ring → `CVPixelBuffer` → GPUI `surface()`.
- **GPUI yaması gerekli**: vendor'daki `gpui-pre-apple/src/metal_renderer.rs` `draw_surfaces`
  sadece NV12 kabul edip BGRA'da assert eder → BGRA dalı + `surface_bgra_fragment` shader.
  Windows (DirectX) ve Linux (Vulkan) renderer'ları için eşdeğer yama.
- Kare zamanlaması: `external_begin_frame_enabled` + GPUI display link → `SendExternalBeginFrame`.
- Maliyet: ~1 kare ek gecikme; erişilebilirlik, sürükle-bırak, popup katmanı, imleç bize kalır.
- `cef` crate'in `accelerated_osr` özelliği üç platformda doku import'unu içerir (wgpu tabanlı;
  GPUI'ye uyarlanacak).

## 5. CEF dağıtımı ve güncelleme

- Framework uygulama paketinin içinde gelir (K7). Kaynak: `cef-dll-sys`'in build için
  indirdiği SDK; `scripts/bundle-native-macos.sh` sürümü `Cargo.lock`'tan, mimariyi
  binary'den okuyup `Contents/Frameworks`'e kopyalar. Framework yalnızca kaynağı veya imza
  kimliği değiştiğinde yeniden kopyalanır ve imzalanır (dev döngüsünde ~2,5 s → ~7 s ilk).
- Sürüm: uygulama tek CEF sürümüne kilitli (K10); yükseltme `cef`/`cef-dll-sys` crate
  sürümüyle birlikte yapılır.
- Boyut: uygulama paketi ~250–300 MB büyür (DMG'de sıkışık ~100–130 MB).
- İlk tasarımdaki indirme yöneticisi (`monocode-cef::install`) K7 revizyonuyla kaldırıldı;
  kodu git geçmişinde (`bfa3c1ac`).

### 5.1 İmzalama ve güvenlik
- İmzalama içten dışa: framework'ün `Libraries/*.dylib`'leri → framework → beş helper →
  uygulama. Framework de bizim kimliğimizle imzalandığı için
  `disable-library-validation` **gerekmez**.
- **Lokal build** (ad-hoc veya `MONOCODE_NATIVE_LOCAL_SIGNING`): hardened runtime yok.
- **Dağıtım build'i** (Developer ID, `--options runtime`): uygulama
  `crates/monocode-gpui/macos/Entitlements.plist`, helper'lar
  `crates/monocode-cef/macos/Helper.entitlements` (`allow-jit`,
  `allow-unsigned-executable-memory`).
- **Anahtarlık (2026-11-22):** Chromium cookie/depolama anahtarını kendi kaydımızda tutar
  (`keychain_service_name` = "Monocode Browser Safe Storage"); CEF'in varsayılanı
  "Chromium Safe Storage" kullanıcının kurulu Chromium'unun kaydıdır, kullanılmaz. Takım
  kimliği olmayan (ad-hoc) imzalı build'lerde anahtarlık hiç kullanılmaz (`use-mock-keychain`):
  izin imzanın hash'ine bağlı olduğundan her build login şifresi sorup sayfayı bekletiyordu.
- Chromium sandbox'ı **açık**: GPU, ağ (`network`), depolama (`service`) ve renderer süreçleri
  sandbox'lı çalışır (Faz 0'da doğrulandı).
- **Kurumsal ortam notu** (geliştiricinin makinesi): Kandji/Iru MDM, SentinelOne EDR,
  sıkı Gatekeeper (`AllowIdentifiedDevelopers=0`, `DisableOverride=1`), içerik filtresi yok.
  Framework uygulamayla ve bizim imzamızla geldiği için Gatekeeper açısından ayrı bir
  durum yok; tek risk SentinelOne'ın davranış sezgisi. Ön test yapılmayacak (K13).
  Faz 0'da bu makinede sandbox'lı Chromium sorunsuz çalıştı.

## 6. Agent katmanı

### 6.1 Yerel araçlar (Monocode içindeki agent'ların varsayılanı)
- Monocode'un MCP sunucusu üzerinden; Claude, Codex ve ACP sağlayıcılarının hepsine kurulum
  gerektirmeden açılır. Araç seti Orca'nın komut setine göre **sıfırdan** (3.2):
  sekmeler, navigasyon, bekleme, snapshot (interactive/delta/selector/depth), screenshot,
  element aksiyonları (ref ile), koordinat mouse, klavye, eval, dialog, viewport/device,
  cookie/storage, console, network, HAR, intercept, upload/download, PDF.
- **Ref'ler**: Rust'a port edilen snapshot motoru (CDP `Accessibility.getFullAXTree` +
  DOM taraması), ref → backend node id; iframe/OOPIF desteği.
- **Aksiyon etkileri** + `expect_url` guard + sekme başına komut kuyruğu + kararlı hata kodları.
- **CDP**: `CefBrowserHost::SendDevToolsMessage` + `AddDevToolsMessageObserver`; sekme başına
  tek oturum, referans sayımlı paylaşım; Runtime/Network/Log/Page/Input/Accessibility/DOM.
- **Varlık ve odak koruması**: 3.2'deki davranışların tamamı.

### 6.2 Dış araçlar için CDP uç noktası
- Sekme başına (veya profil başına) **loopback + rastgele token** korumalı WebSocket; `Origin`
  başlıklı istekler reddedilir; `/json/version`, `/json/list` discovery yerelde cevaplanır ve
  sadece izin verilen sekme(ler) görünür.
- `Page.bringToFront` ve odak çalan komutlar yutulur; input kendi odak koruma yolumuzdan geçer;
  screenshot/PDF kendi yolumuzdan.
- Hedef: **dev-browser `--connect`**, agent-browser, Playwright `connectOverCDP`,
  chrome-devtools-mcp değişiklik yapmadan çalışsın. Uç noktayı açma kullanıcı ayarıdır.

### 6.3 Araç karşılaştırması (karar gerekçesi)
| | agent-browser | dev-browser | Monocode yerel araçlar |
|---|---|---|---|
| Model | Ayrı komutlar, a11y snapshot + `@eN` ref | Agent Playwright script'i yazar (QuickJS sandbox), isimli kalıcı sayfalar | Ayrı araçlar + ref + delta + etkiler |
| Güçlü yanı | Token tasarrufu, adım adım doğrulama | Çok adımlı akış tek turda, Playwright gücü | UI ile bütünleşik (varlık, odak, grab), kurulum yok, tüm sağlayıcılar |
| Bizdeki yeri | CDP uç noktasıyla isteğe bağlı | CDP uç noktasıyla isteğe bağlı | Varsayılan |

## 7. Hedef özellik kataloğu (özet)

Tam liste `research/01-orca-features.md` (181 özellik). Gruplar:
1. Sekmeler, pane'ler, yerleşim
2. Navigasyon, adres çubuğu, arama, yeni sekme sayfası, geçmiş
3. Terminal/chat/markdown'dan link yönlendirme
4. Profiller, oturumlar, cookie'ler, kimlik
5. Chrome Web Store eklentileri (hedef: seçilmiş 20–30)
6. Şifreler, passkey'ler, HTTP auth
7. Site izinleri
8. Sertifikalar
9. İndirmeler ve yüklemeler
10. DevTools
11. Bul, zoom, viewport, yazdırma ve PDF
12. Bağlam menüsü ve pano
13. Grab, Annotate, Draw (agent'a geri bildirim)
14. Agent kontrolü
15. Screenshot, konsol ve ağ kaydı
16. Uzak browser (yerelde çiz / sunucudan yayınla)
17. Telefon browser'ı
18. HTML doküman önizleme
19. Dikte ve input
20. Diğer ve güvenilirlik (Memory Saver, ses kapatma vb.)

## 8. Eklenti stratejisi (K4)
- Hedef küme 20–30 eklenti; liste kullanıcıyla belirlenecek. Her eklentinin manifest'inden
  gereken `chrome.*` API'leri çıkarılıp sadece onlar desteklenir.
- Yollar (spike sonucuna göre seçilir):
  a. **Hibrit**: aynı profili paylaşan Chrome-style CEF penceresi ile kurulum/yönetim, content
     script'ler gömülü sekmelerde (4.2 açık sorusu).
  b. **Kendi eklenti katmanımız** (Orca modeli): CRX3 indirme + doğrulama, manifest yeniden
     yazımı, API köprüleri, toolbar/popup/side panel GPUI'de.
  c. Uzun vadede CEF #3294'e katkı veya kendi Chromium derlemesi.
- Doğrudan port edilebilecek Orca mantıkları: CRX3 doğrulayıcı, Web Store kurulum/güncelleme
  protokolü, declarativeNetRequest motoru, native messaging host araması.

## 9. Platformlar (K12)
- **macOS** (ilk): NSView child, IOSurface OSR, CFRunLoop pompası, `.app` helper'lar.
- **Windows** (sonra): HWND child (`SetAsChild`), D3D11 shared handle OSR,
  `multi_threaded_message_loop`, helper = aynı exe veya ayrı exe, sandbox için bootstrap.
- **Linux** (sonra): X11'de child window; **Wayland'de OSR zorunlu** (CEF windowed Wayland'e
  gömülemez); dmabuf OSR; `multi_threaded_message_loop`.
- Platforma özgü kod `#[cfg]` arkasında; motor soyutlaması (host, çizim modu, pompa) üç
  platformu baştan kapsar.

## 10. Kaldırılacaklar (K11)
- WebKit tabanlı browser'ın tüm kodu, ayarları, testleri, self-test'leri, MCP araçları ve
  WebKit bağımlılıkları. Yerine bu dokümandaki tasarım gelir. Pane sistemi, workspace
  kalıcılığı, kısayol, ayarlar ve MCP sunucusu gibi uygulama altyapısı (motorla ilgisi
  olmayan kısımlar) yeni browser'a bağlanır.

## 11. Riskler ve açık sorular
| Konu | Not |
|---|---|
| Eklentiler | Gömülü (Alloy) sekmelerde Chrome eklenti API'si yok; hibrit modelin çalışıp çalışmadığı belirsiz. En büyük risk. |
| Overlay | Child view üstüne GPUI çizemez. Faz 1: son kare görüntüsü (K16); tooltip'ler ve ortak yardımcıları kullanmayan overlay'ler hâlâ sayfanın altında kalır. Kalıcı çözüm Faz 3 (OSR). |
| Gizli sekme throttling | Arka plandaki sekmenin view'u gizlenir ve `WasHidden` çağrılır. Faz 2: agent'ın sürdüğü sekme gizlenmez, pencere dışında çizer (K23); pencere ekran dışındayken Chromium'un görünen sekmeyi de gizlediği görüldü ve aynı yolla çözüldü. |
| DevTools yapışması | **Çözüldü (Faz 2, K28):** frontend kendi Alloy view'umuzda, CDP proxy'si üzerinden. |
| Battery Saver | **Çözüldü (K52, Faz 4'te revize):** Chromium'unki kapalı, arka plan sekmeleri düşük pilde donuyor. Ölçüm: Chromium'un Battery Saver'ı zorlanınca (`state` 3) görünen ve agent sekmesi 30,3 fps; kapalıyken 120 fps. Pil %7'deyken arka plan sekmesinin 1 sn'lik `setInterval`'ı 65 sn durdu (donma), görünen sekme 119,6 fps. Geliştirici kancaları: `MONOCODE_BROWSER_DISCARD_AFTER_SECS`, `MONOCODE_BROWSER_BATTERY_LOW=1`, `MONOCODE_BROWSER_CHROMIUM_BATTERY_SAVER=<0-3>`. |
| Memory Saver | Ölçüm (K64): 10 sekme (görünen + 7 Wikipedia + form + yerel sayfa), helper'lar 1507 MB / 14 süreç; 8'i discard edilince 461 MB / 6 süreç. Ana süreç 245 → 249 MB. |
| Passkey QR (hybrid) | **Faz 4 bulgusu:** Gmail'de QR telefonla okutuluyor ama bağlanmıyor. Şüphe: Mac BLE yakınlık sinyalini duyamıyor (ad-hoc imzada TCC Bluetooth izni her build'de düşüyor / bundle'sız çalıştırma). K58, K59. |
| Platform passkey | Apple'ın tarayıcı entitlement'ı olmadan Touch ID / iCloud Keychain ve macOS sistem passkey sağlayıcıları (1Password dahil) kullanılamaz; kapsam dışı. |
| Başlangıç kilitlenmesi | **Çözüldü (Faz 3):** `setlocale` ↔ `fork()` kilit sırası çakışması; `main`'in başında tek thread'liyken `setlocale`. |
| Çıkıştan sonra kalan süreç | **Faz 3 bulgusu:** ağır fork yükünde `exec` öncesi takılan fork çocuğu profili tutuyor. Çözüm uygulama geneli: `process.rs`'te `posix_spawn` (ayrı iş). |
| Bellek | Faz 2 ölçümü (`phase-2-done.md`): 1 sekmede helper'lar 482 MB, 5 sekmede 1077 MB (Chrome: 612 / 1063). Faz 0 ölçümü (Wikipedia/GitHub, bir sayfa): GPU 86–106 MB, ağ 64–66 MB, depolama ~50 MB, renderer 71–206 MB (Chromium bir yedek renderer da tutar). Ana süreç (GPUI dahil) 191–279 MB. |
| EDR | Framework artık uygulamayla ve bizim imzamızla geliyor; indirme kaynaklı risk kalktı. |
| Sandbox ↔ dağıtım | **Çözüldü (K7 revizyonu):** sandbox yalnızca uygulama bundle'ından okumaya izin veriyor; framework gömüldü. |
| Sürüm takibi | CEF ~4 haftada bir sürüm; güvenlik yamaları için yükseltme ritmi gerekir. |
| Boyut | Uygulama paketi ~250–300 MB büyür (DMG'de ~100–130 MB). |
| Chrome Web Store | CEF'te CWS kurulumunun Chrome-style'da bile çalıştığı doğrulanmadı. |

## 12. Faz planı

macOS için 6 faz (0–5), ardından platform fazı (6) ve opsiyonel faz (7). Her faz bir önceki
bitmeden başlamaz; her fazın başında kullanıcı onayı alınır.

### Faz 0 — Temel altyapı ve WebKit'in kaldırılması
- WebKit browser'ının kodu, ayarları, testleri, self-test'leri ve MCP araçları tamamen silinir.
- `cef` crate entegrasyonu, `monocode-cef-helper` binary'si, `.app` helper bundle'ları, build
  betikleri ve imzalama (§4.3, §5.1).
- ~~CEF indirme yöneticisi~~ (yazıldı, sonra K7 revizyonuyla kaldırıldı; yerine framework'ün
  bundle'a gömülmesi, §5).
- NSApp `CefAppProtocol` uyarlaması, CFRunLoop mesaj pompası, lazy CEF başlatma (§4.4).
- Pane içinde tek Chromium sekmesi (child NSView) + performans ölçümü (scroll fps, input
  gecikmesi, bellek).

### Faz 1 — Dev browser çekirdeği (bitti, `phase-1-done.md`)
- Çoklu sayfa = sekme grubunda çoklu sekme (K14), kalıcı `pageId` (pane id), adres çubuğu,
  geri/ileri/yenile/durdur.
- Ortak profil + proje başına izole profil seçeneği (K15), workspace kalıcılığı, lazy restore (K20).
- Kısayol yönlendirme (K17; macOS'ta `OnPreKeyEvent` gerekmedi, NSEvent monitörü yetiyor), odak,
  IME; overlay ilk çözümü (son kare, K16).
- DevTools (ayrı pencere, K18), sayfada bul, zoom, popup politikası, JS/HTTP auth diyalogları,
  indirmeler.
- Terminal/chat/markdown link yönlendirme, localhost dev server algılama (K19).

### Faz 2 — Agent temelleri (bitti, `phase-2-done.md`)
- Süreç içi CDP katmanı, sekme başına komut kuyruğu (§6.1).
- Yeni MCP araç seti (Orca komut setine göre) + Rust'a port edilmiş a11y snapshot motoru.
- Güvenilir input (`isTrusted=true`), console, network, screenshot, eval.
- Dış araçlar için token korumalı CDP uç noktası (dev-browser `--connect`, Playwright) (§6.2).
- Faz 1'den devralınan: yapışık DevTools (CDP frontend, K18), çıkışta düzgün `cef::shutdown`,
  her popup'ta `window.opener`, sahiplenilmeyen popup sızıntısı, dönüşte diyaloğa odak,
  scroll fps / input gecikmesi ölçümü (`phase-1-done.md`).

### Faz 3 — Gelişmiş agent ve geliştirici özellikleri (bitti, `phase-3-done.md`; kararlar K43–K53)
- Faz 2'den devralınan: Battery Saver ayarı (K52), başlangıç kilitlenmesi, programlı scroll'da uzun kareler, yapışık DevTools eksikleri, küçük kusurlar (`phase-2-done.md`).
- Aksiyon etkileri (K45), delta snapshot ve `expect_url` (K46).
- Varlık UI'ı (K47) ve kontrol modeli (K48).
- Grab (K49).
- PDF, upload, download (K50, K51).
- Overlay kancası (K44).
- ~~OSR~~ → Faz 6 (K43, K44).

### Faz 4 — Tam browser (kararlar K54–K66)
- **Önce:** `monocode-core/src/process.rs`'te `fork` → `posix_spawn` (kapanıştan sonra kalan süreç).
- **4a**
  - Faz 3'ten devralınan: yapışık DevTools'ta ~48 px görünüm farkı, DevTools sağ tık menüsünde "Send to Agent", popup indirmelerinin klasör yönlendirmesi, programlı scroll (`phase-3-done.md`).
  - Memory Saver (discard) ve throttle lease'leri (K64); Battery Saver yalnız arka planda (K52).
  - Site izinleri (K63), sertifika istisnası (K62), geçmiş (K55, K65), yeni sekme sayfası (K66), ses kapatma, yazdırma.
- **4b**
  - Şifre yöneticisi (K56, K57), autofill, Touch ID ile kasa kilidi.
  - Telefonla QR passkey (hybrid) düzeltmesi + Bluetooth izin akışı (K58, K59).
  - Chrome/Arc/Brave'den cookie ve şifre import'u (K61, §3.3).

### Faz 5 — Eklentiler (seçilmiş 20–30)
- Eklenti listesinin belirlenmesi ve gereken `chrome.*` API'lerinin çıkarılması.
- Hibrit Chrome-style pencere modeli ile kendi eklenti katmanı arasında spike ile seçim (§8).
- Web Store'dan kurulum, CRX3 doğrulama, otomatik güncelleme.
- Toolbar, popup, side panel, bağlam menüsü.

### Faz 6 — Windows ve Linux
- Windows: HWND child view + D3D11 OSR. Linux: X11 child view, Wayland'de OSR (§9).
- `multi_threaded_message_loop`, platforma göre CEF paketleme ve helper'lar, GPUI DirectX/Vulkan
  renderer yamaları.

### Faz 7 — Opsiyonel
- Uzak ve mobil browser (screencast protokolü, §3.1 madde 10).
- ~~Kendi CDN'imizden imzalı CEF dağıtımı~~ (geçersiz: CEF uygulamayla geliyor, K7).

## 13. Faz bitişi ve doküman bakımı

Her faz bittiğinde:
1. Bu klasöre `phase-<N>-done.md` yazılır (örn. `phase-0-done.md`, `phase-1-done.md`).
   İçeriği kısa maddeler hâlinde:
   - Yapılanlar (özellik ve önemli teknik değişiklikler)
   - Plandan sapmalar ve nedenleri
   - Ertelenenler / bir sonraki faza devredilenler
   - Ölçümler (varsa: performans, bellek, boyut)
   - Bilinen sorunlar
2. **Bu doküman (`context.md`) güncellenir**: fazda değişen veya yeni verilen kararlar §2'ye
   işlenir; mimari/indirme/agent bölümlerinde gerçekle uyuşmayan yerler düzeltilir;
   §11'deki çözülen riskler ve cevaplanan açık sorular güncellenir; §12'de fazın durumu
   işaretlenir ve sonraki fazların maddeleri gerekirse revize edilir.
3. Sonraki faza başlamadan önce kullanıcı onayı alınır.

### Faz durumu
| Faz | Durum | Bitiş dokümanı |
|---|---|---|
| 0 | Bitti | `phase-0-done.md` |
| 1 | Bitti | `phase-1-done.md` |
| 2 | Bitti | `phase-2-done.md` |
| 3 | Bitti | `phase-3-done.md` |
| 4 | Başladı (2026-11-23) | — |
| 5 | Başlamadı | — |
| 6 | Başlamadı | — |
| 7 | Başlamadı | — |

## 14. Kaynak repolar (yerel klonlar, geçici)
- Orca: `/tmp/orca-research/orca` (`stablyai/orca` @ `45aa8730`)
- gpui-cef: `/tmp/orca-research/ext/gpui-cef` (`bokuweb/gpui-cef`, CEF 151, OSR, MIT/Apache)
- cef-rs: `/tmp/orca-research/ext/cef-rs` (`tauri-apps/cef-rs`, `cef` 156.7.0)
