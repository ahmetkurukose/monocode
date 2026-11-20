# Browser: Chromium tabanlı tam browser — bağlam ve kararlar

Durum: karar dokümanı, kod yok. Başlangıç: 2026-11-20. Branch: `browser/cef-spike`.
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
- Browser, Monocode'un pane sisteminde bir pane'dir (kendi başlık sekmesi veya bir chat/dosya/
  terminal yanında split). Pane içinde **çoklu sekme** (Orca'nın `BrowserWorkspace` → çok
  `BrowserPage` modeli).
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
| Overlay | Child view üstüne GPUI çizemez; overlay çözümü UX kalitesini belirler. |
| Gizli sekme throttling | macOS windowed Alloy'da NSView gizleme ile throttling doğrulanacak. |
| Bellek | Faz 0 ölçümü (Wikipedia/GitHub, bir sayfa): GPU 86–106 MB, ağ 64–66 MB, depolama ~50 MB, renderer 71–206 MB (Chromium bir yedek renderer da tutar). Ana süreç (GPUI dahil) 191–279 MB. |
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

### Faz 1 — Dev browser çekirdeği
- Pane içinde çoklu sekme, kalıcı `pageId`, adres çubuğu, geri/ileri/yenile/durdur.
- Proje başına profil (`CefRequestContext`), workspace kalıcılığı, uyuyan sekme restore'u.
- Kısayol yönlendirme (`OnPreKeyEvent`), odak, IME; overlay ilk çözümü (view gizle + son kare).
- DevTools, sayfada bul, zoom, popup politikası, JS/HTTP auth diyalogları, indirmeler.
- Terminal/chat/markdown link yönlendirme, localhost dev server algılama.

### Faz 2 — Agent temelleri
- Süreç içi CDP katmanı, sekme başına komut kuyruğu (§6.1).
- Yeni MCP araç seti (Orca komut setine göre) + Rust'a port edilmiş a11y snapshot motoru.
- Güvenilir input (`isTrusted=true`), console, network, screenshot, eval.
- Dış araçlar için token korumalı CDP uç noktası (dev-browser `--connect`, Playwright) (§6.2).

### Faz 3 — Gelişmiş agent ve geliştirici özellikleri
- Delta snapshot, aksiyon etkileri, `expect_url` guard.
- Agent varlık UI'ı (rozet, imleç animasyonu) ve kullanıcı odağı koruması.
- Grab / Annotate / Draw ile agent'a geri bildirim.
- HAR kaydı, intercept, upload/download talepleri, PDF.
- GPUI Metal renderer BGRA yaması + OSR; overlay'lerin kalıcı çözümü (§4.7).

### Faz 4 — Tam browser
- Memory Saver (discard) ve throttle lease'leri.
- Site izinleri, sertifika istisnası, geçmiş, yeni sekme sayfası, ses kapatma, yazdırma.
- Keychain tabanlı şifre yöneticisi, autofill, Touch ID, passkey (USB).
- Chrome/Arc/Brave'den cookie ve şifre import'u (§3.3).

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
- Kendi CDN'imizden imzalı CEF dağıtımı (§5.1).

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
| 0 | Bitti, kullanıcı onayı bekliyor | `phase-0-done.md` |
| 1 | Başlamadı | — |
| 2 | Başlamadı | — |
| 3 | Başlamadı | — |
| 4 | Başlamadı | — |
| 5 | Başlamadı | — |
| 6 | Başlamadı | — |
| 7 | Başlamadı | — |

## 14. Kaynak repolar (yerel klonlar, geçici)
- Orca: `/tmp/orca-research/orca` (`stablyai/orca` @ `45aa8730`)
- gpui-cef: `/tmp/orca-research/ext/gpui-cef` (`bokuweb/gpui-cef`, CEF 151, OSR, MIT/Apache)
- cef-rs: `/tmp/orca-research/ext/cef-rs` (`tauri-apps/cef-rs`, `cef` 156.7.0)
