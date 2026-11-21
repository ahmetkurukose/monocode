# Faz 0 — bitiş notu (2026-11-20)

Durum: bitti, kullanıcı onayı bekliyor. Mimari ve kararlar: `context.md`.

## Yapılanlar
- **WebKit browser'ı kaldırıldı** (`4e09fa10`): pane'ler, ayar sayfası, agent MCP araçları,
  self-test'ler, reload-on-save izleyicisi ve core köprüsü. Monos HTML kendi WKWebView
  sandbox'ını koruyor; `objc2-web-kit` yalnızca onun kullandığı feature'larla kaldı.
- **`crates/monocode-cef`**: GPUI'den bağımsız Chromium katmanı.
  - `runtime`: lazy CEF başlatma (main thread), `CefAppProtocol` için NSApp uyarlaması,
    `external_message_pump` + CFRunLoop kaynağı/zamanlayıcısı (common modes, yeniden girişe
    karşı korumalı).
  - `view`: Alloy style, child NSView sayfa; adres, başlık, yükleniyor/geri/ileri, ilerleme,
    yükleme hatası ve popup olayları kanal üzerinden.
  - `monocode-cef-helper`: sandbox'ı başlatıp framework'ü yükleyen ~560 KB'lık helper.
- **Build ve paketleme** (`d295a460` ve bu faz):
  - Crate native workspace'te; uygulama macOS'ta ona bağımlı.
  - CEF SDK checkout başına bir kez çözülür (`.cargo/config.toml`), native betikleri
    `~/Library/Caches/Monocode/cef-sdk`'yı paylaşır.
  - `bundle-native-macos.sh`: framework'ü SDK'dan kopyalar ve imzalar, beş
    `<App> Helper*.app`'i oluşturur, içten dışa imzalar.
  - CI: macOS'ta helper derlenir, ninja kurulur.
- **Browser pane** (`browser.rs`, `shell/browser_panes.rs`):
  - Grup şeridinin "+" menüsü → **New Browser**; sayfa o grupta sekme olur.
    Pane sahibi oturumun projesini ödünç alır (terminal pane'leri
    gibi); sahip yoksa görünmez bir sahip oturum açılır.
  - Araç çubuğu: geri, ileri, yenile/durdur, adres alanı (URL, host, localhost/IP veya arama).
    Yükleme çubuğu ve hata satırı.
  - Sekmede globe ikonu ve sayfa başlığı; adres ve başlık workspace'e kaydedilir, restore'da
    sayfa tekrar yüklenir. WebKit döneminden kalan kayıtlı sayfalar Chromium'da açılır.
  - Görünürlük: her sayfa çizildiği kareyi işaretler; en son çizilen süpürücü, o karede
    çizilmeyenleri (arkadaki sekme) ve menü, modal veya bildirim kartının örttüğü sayfaları
    gizler (`Host::set_shown` → `WasHidden`, Chromium throttling yapar).
- **Chromium sandbox'ı açık**: GPU, ağ, depolama ve renderer süreçleri sandbox'lı
  (`--no-sandbox` yok).

## Plandan sapmalar
- **K7–K9 revize: CEF uygulamayla geliyor, indirilmiyor.** İndirme yöneticisi yazıldı ve
  çalıştı (devam eden indirme, SHA-1 doğrulama, açma). Ancak Chromium'un macOS sandbox'ı
  helper'lara yalnızca çalışan uygulamanın bundle'ından okuma izni veriyor; bundle dışındaki
  framework yüklenemedi ve GPU süreci açılamadığı için Chromium uygulamayı sonlandırdı.
  `main_bundle_path` ile ayrı bir runtime bundle'ı da denendi, sonuç aynı. Kullanıcı
  sandbox'ı kapatmak yerine gömmeyi seçti. İndirme kodu kaldırıldı (geçmişte `bfa3c1ac`).
- `disable-library-validation` artık gerekmiyor: framework bizim kimliğimizle imzalanıyor.
- CEF başlatma, ilk karede değil o karenin ardından yapılıyor; pane önce
  "Starting Chromium…" gösteriyor.

## Ölçümler
- **Başlatma**: sıcak < 1 s; yeni kopyalanmış bundle'ın ilk açılışında ~2,5 s.
- **Bellek** (tek sayfa; Wikipedia ve GitHub):

  | Süreç | Bellek |
  |---|---|
  | Ana süreç (GPUI dahil) | 191–279 MB |
  | GPU | 86–106 MB |
  | Ağ | 64–66 MB |
  | Depolama | ~50 MB |
  | Renderer | 71–206 MB, ayrıca Chromium'un tuttuğu bir yedek renderer |
- **Boyut**: native-dev bundle 481 MB (framework ~300 MB). Release DMG ölçülmedi.
- **Paketleme**: ilk paketleme ~7 s, framework değişmediğinde ~2,5 s.

## Ertelenenler
- **Scroll fps ve input gecikmesi ölçümü**: girdi otomasyonu olmadığı için yapılmadı. Elle
  denenmeli (ProMotion ekranda uzun bir sayfada kaydırma; Chrome ile yan yana).
- `cef::shutdown()` çağrılmıyor; helper süreçleri ana süreçle birlikte kapanıyor.
  Faz 1'de düzgün kapanış (sayfaları kapat → shutdown).
- Pane içinde çoklu sekme, popup'ların yeni sekmede açılması (şimdilik aynı sayfada),
  DevTools ve zoom düğmeleri (API hazır), `OnPreKeyEvent` ile kısayol eşleme → Faz 1.
- Windows ve Linux (stub "desteklenmiyor" der) → Faz 6.

## Bilinen sorunlar
- **Overlay**: menü, modal ve bildirim kartı sayfayı örterse sayfa tamamen gizlenir (son karenin
  görüntüsü gösterilmiyor). Tooltip'ler ve bu üç yardımcıyı kullanmayan diğer overlay'ler
  sayfanın altında kalır (§4.6, aşama 1/2).
- `MONOCODE_BROWSER_OPEN=<url>` geliştirici kancası açılışta bir sayfa açar (ölçüm için);
  kalıcı bir özellik değil.
- Bu ağdan `example.com`'a erişilemiyor (curl ile de); sayfa doğru biçimde yükleniyor
  durumunda bekliyor. Sorun browser'da değil.
- `monocode-gpui`'de 3 test bu fazdan önce de başarısızdı
  (`many_blank_tabs_fit_…`, `worktree_tabs_cycle_close_…`, `copies_keep_running_sessions_…`);
  1 test (`a_feature_knows_its_projects_…`) tam suite'te ara sıra başarısız oluyor.
