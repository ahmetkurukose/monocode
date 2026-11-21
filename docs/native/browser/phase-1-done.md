# Faz 1 — bitiş notu (2026-11-21)

Durum: bitti. Mimari ve kararlar `context.md`'de (K14–K20).

## Yapılanlar
- **Sekmeler (K14).** Her sayfa bir sekme grubunun sekmesi; sekme modeli
  `tab-unified-window-management` çalışmasından geliyor (`docs/native/tab-groups.md`). Popup,
  `target=_blank`, orta tıklama ve ⌘-tıklama açanın yanında yeni sekme açar:
  - Jestle açılan öne gelir, diğerleri arkada açılır.
  - Jestsiz script popup'ları sayfa başına 2 saniyede en çok 4.
  - `window.close()` yalnızca kendi sekmesini kapatır.
- **OAuth popup'ları.** Boyut belirten `window.open`'larda `window.opener` korunur:
  - Popup `OnBeforePopup`'ta gizli bir view'da oluşturulur.
  - Yeni sekme açılınca view oraya taşınır.
- **Profiller (K15).** Varsayılan tek ortak profil.
  - Proje menüsünde "Isolated browser profile" seçeneği var.
  - Seçenek açılınca proje ayrı bir `CefRequestContext` alır (`Profiles/<ad>-<hash>`).
  - Değişince projenin açık sayfaları yeni profilde yeniden açılır.
- **Kısayollar (K17).** Sayfa odaktayken ⌘/⌃ kısayolları uygulamaya gider. Sayfada kalanlar:
  - Düzenleme: ⌘C/V/X/A/Z, ⇧⌘Z.
  - İmleç tuşları.
  - Emacs tuşları (⌃A/E/…).
  - IME ile yazarken her tuş.

  Karar `shell/shortcuts.rs`'te saf bir fonksiyon (`native_keeps`) ve testli. macOS'ta CEF
  `OnPreKeyEvent` gerekmedi; mevcut NSEvent monitörü her tuşu Chromium'dan önce görüyor.
- **Browser kısayolları.**
  - ⌘L: adres çubuğu.
  - ⌘R: yenile; ⇧⌘R: önbelleksiz yenile.
  - ⌘F, ⌘G, ⇧⌘G: sayfada bul.
  - ⌘+/−/0: sayfa zoom'u.
  - ⌥⌘I: DevTools.
- **Sayfada bul.** GPUI bulma çubuğu: "n / m" sayacı, önceki/sonraki, Esc.
- **Zoom.** Chromium'un kuralıyla site başına; Chrome'un hazır yüzdeleri. Pane başına kaydedilir
  ve yeniden açılışta geri gelir.
- **DevTools (K18).** Ayrı pencere: araç çubuğu düğmesi ve ⌥⌘I. Yapışık hâli yazıldı, sonra
  kaldırıldı (aşağıda).
- **Overlay (K16).** Menü, modal veya bildirim sayfayı örterken sayfanın son karesi gösterilir.
  - Görüntü CDP `Page.captureScreenshot` ile alınır.
  - Alma zamanı: örtme başlarken ve her yüklemeden 500 ms sonra.
  - Retina'da keskin.
  - Doku, değiştiğinde ve sayfa kapanınca GPU atlas'ına geri verilir.
- **İndirmeler.**
  - `~/Downloads`'a diyalogsuz iner; ad çakışırsa `ad (1).uzantı` olur.
  - Araç çubuğunda ilerleme göstergesi var; listesinde iptal ve Finder'da göster.
- **HTTP girişi ve JS diyalogları.** Basic/Digest girişi ve `alert`, `confirm`, `prompt`,
  `beforeunload` GPUI modalı olarak açılır, sırayla kuyruğa alınır. Kimlik bilgileri log'a
  yazılmaz.
- **Link yönlendirme (K19).** Karar `shell/browser_links.rs`'te saf bir fonksiyon:
  `opens_in_app`.
  - localhost, `*.localhost`, `*.local`, loopback ve makinenin kendi IP'leri Monocode'da
    açılır; diğerleri sistem tarayıcısında.
  - ⇧ ile tıklama tersini yapar.
  - Ayarlar → General → "Open all links in Monocode".
  - Uygulandığı yerler: terminal linkleri, markdown ve yanıt linkleri, transcript menüsü, run
    chip'inin "Open in the browser" seçeneği.
  - OAuth girişleri, PR akışı ve `file://` sistem tarayıcısında kalır.
- **Restore (K20).** Önceki oturumun sekmeleri görünür olunca yüklenir.
- **Adres çubuğu ve odak.** Adres ve bulma alanları hangi yoldan odak alırsa alsın klavyeyi
  Chromium'dan geri alır. Etkin browser pane'ine tıklamak artık odağı sayfaya geri vermiyor.

## Plandan sapmalar
- **Sekmeler (K14).** "Her sayfa ayrı başlık sekmesi" iken sekme grubu modeline uyarlandı.
- **DevTools (K18): yapışık değil, ayrı pencere.** Yapışık DevTools, uygulamayı her açılışta
  ve tıklamada kapatıyordu. Sebep:
  - CEF macOS'ta DevTools'u Alloy style açamıyor ("Alloy style is not supported for this
    browser") ve Chrome style'a geçiyor.
  - Chrome style bir host view'un içine konamıyor; `ShowDevTools` child view ile bir CHECK'te
    duruyor.
  - Gizli pencerede açıp view'u taşımak çökmedi, ama taşınan şey kendi penceresini bekleyen bir
    Chrome style view'du; güvenilir bulunmadı.
  - Yapışık DevTools'a dönüş yolu: Faz 2'deki CDP uç noktasıyla DevTools frontend'ini kendi
    Alloy sayfamızda yüklemek.
  - Kaydedilmiş "DevTools açık" durumu artık okunmuyor (testli).
- **Overlay (K16).** Örtülen sayfa gizlenmiyor, pencere dışına taşınıyor: gizli view'da Chromium
  çizmeyi durduruyor ve görüntü alınamıyor.
- **Link işinin tabanı.** Link yönlendirme agent'ı eski tabanda (WebKit browser'ının üstünde)
  çalışmıştı. Davranışı ikinci bir agent yeni koda taşıdı; eski `LinkQueue` yeniden kuruldu ve
  pencere başına ayrıldı.

## Testler
- `monocode-gpui`: 1295 test geçti (seri, `--test-threads=1`).
- `monocode-core`: 1196 test geçti.
- Clippy `-D warnings` ile temiz.
- Bu fazın yeni testleri:
  - kısayol kuralı, zoom adımları, bulma sayacı
  - overlay yerleşimi ve doku iadesi
  - popup sınırı ve sekme yeri
  - indirme adları, profil kimliği
  - link kararı (6 birim, 5 GPUI testi)
  - adres alanına tıklamanın odağı koruması
- `monocode-gpui` testleri tam paralel çalışınca birbirine karışıp test sürecini durduruyor. Bu
  paket, projenin kendi betiği gibi seri çalıştırılmalı.
- Düzeltilen yarışlı test: `code_insight::diagnostics_land_per_server…`. Test, sunucunun
  `didClose`'u almasını beklemeden sayıyordu.

## Elle doğrulama
- **Doğrulandı:** sayfa yükleme, adres çubuğuna yazma, ayrı pencerede DevTools.
- **Henüz elle denenmedi** (birim ve GPUI testleriyle kontrol edildi):
  - overlay son karesi
  - OAuth popup'ının `window.opener` ile girişi geri iletmesi
  - indirmeler
  - HTTP girişi ve JS diyalogları
  - izole profil
  - zoom'un yeniden açılışta gelmesi
  - IME
  - terminal linklerinin yönlendirilmesi

## Faz 2'ye devredilenler
- Yapışık DevTools (CDP frontend).
- Çıkışta düzgün `cef::shutdown`. Şu an helper süreçleri ana süreçle birlikte kapanıyor.
- `window.opener` düz `window.open` ve orta tıklamada da korunmalı.
- Sahiplenilmeden açanı kapanan popup uygulama kapanana kadar gizli yaşıyor.
- Diyaloğu açık bir sekmeye dönünce odak diyaloğa değil sayfaya gidiyor.
- Scroll fps ve input gecikmesi ölçümü (Faz 0'dan beri).
- Windows ve Linux'taki yer tutucu kod derlenmedi.

## Bilinen sorunlar
- **Overlay.** Tooltip'ler ve ortak modal/popover/bildirim yardımcılarını kullanmayan
  overlay'ler sayfanın altında kalır.
- **Kısayol yan etkisi.** Sayfa odaktayken ⌘↑/↓ ve ⇧⌘←/→ oturum ya da proje değiştirmez; metin
  alanlarında imleç için sayfaya bırakıldı.
- **Bellek.** Görünür her sayfa tam çözünürlükte bir son kare tutar; büyük bir Retina pane'de
  yaklaşık 25 MB.
- **Geliştirici kancası.** `MONOCODE_BROWSER_OPEN=<url>` açılışta bir sayfa açar. Ölçüm ve el
  testi için tutuluyor; kullanıcıya dönük bir özellik değil.
