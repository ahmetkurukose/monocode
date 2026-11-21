# Faz 2 — bitiş notu (2026-11-21)

Durum: bitti. Kararlar `context.md`'de (K21–K42; K18 ve K28 güncellendi).

## Yapılanlar
- **CDP katmanı.** Her sayfanın tek bir ham DevTools kanalı var (`monocode-cef/src/devtools.rs`).
  Kanalı sayfanın bütün CDP kullanıcıları paylaşıyor: agent araçları, dış istemciler,
  DevTools. Paylaşımı `CdpClient` çoklayıcısı yapıyor (`monocode-core/src/browser/cdp.rs`):
  - id'leri yeniden numaralıyor,
  - cevabı soranına geri veriyor,
  - olayları bütün abonelere dağıtıyor,
  - sayfa kapanınca ya da çökünce bekleyen her çağrıya hemen hata dönüyor.
- **Main-thread iş kuyruğu.** Köprü ve proxy kendi thread'lerinde dinliyor. Her iş `Job` olarak
  main thread'e gidiyor ve uygulamanın `Host`'u ile çalışıyor (`browser/mod.rs`,
  `shell/browser_agent.rs`).
- **MCP sunucusu `monocode_browser` (K41).**
  - Desen: `code`/`db` köprüleriyle aynı. Loopback listener, oturum başına token,
    `browser-mcp` stdio alt komutu.
  - Bağlandığı yerler: Claude, Codex, ACP. Oturum ortamı `runtime.rs`'te veriliyor.
  - Onay: `browser_eval` allow list'te yok, her seferinde kullanıcıya soruluyor (K25).
  - Görüntüler MCP image content olarak dönüyor.
- **Araç seti (K24).** 20 araç: `browser_tabs`, `_tab_open/close/show`, `_goto/back/forward/reload`,
  `_wait`, `_snapshot`, `_click/fill/type/press/scroll`, `_screenshot`, `_eval`, `_console`,
  `_network`, `_dialog`. Hepsinde ortak olanlar:
  - sekme başına FIFO ve zaman aşımları (K35),
  - kullanıcı-aktif koruması (K34),
  - diyalog koruması,
  - 30 KB sınırı,
  - sabit hata kodları (K41).
- **Snapshot motoru.** Orca'nın motorunun Rust portu (`browser/tools/snapshot.rs`):
  - Erişilebilirlik ağacına ek olarak tıklanabilir öğeler için DOM taraması.
  - Aynı origin'li iframe'ler snapshot'ta; cross-origin iframe'ler ayrı oturumla (K42).
  - `@eN` ref'leri aynı belgede sonraki snapshot'larda korunuyor.
  - Kaybolan ref rol + isim + sıra ile kurtarılıyor; navigasyondan sonra `browser_stale_ref` (K33).
- **Sayfa kaydı (K36).** Açılıştan itibaren console (500) ve network (300) ring buffer'ları.
  Yeni sekmenin ana belgesi, isteğin başlangıcı kaçırılsa da cevabından kayda ekleniyor.
- **Uygulama tarafı.**
  - Kapsam: proje ve agent'ın kendi sekmeleri (K21).
  - Agent sekmesinin yeri: chat'in grubu, arka planda (K39).
  - `isolated` sekme: bellekte geçici profil (`Profile::Ephemeral`), kaydedilmiyor (K22).
  - Hiç gösterilmemiş sekme gerektiğinde arka planda yükleniyor.
  - `keep_drawn` (K23): agent'ın sürdüğü görünmeyen sekme pencere dışında çizmeye devam ediyor.
    Pencere küçültülmüş ya da başka Space'teyse görünen sekme de böyle.
  - Sayfa id'leri uygulamanın çalışması boyunca hiçbir başka sayfaya verilmiyor.
- **Dış CDP uç noktası (K26, K27).** Proje başına, browser seviyesinde proxy
  (`browser/proxy/`).
  - Güvenlik: loopback, token, `Origin`/`Host` korumaları.
  - Discovery: `/json/*`.
  - Target emülasyonu: `setAutoAttach`, `createTarget`, `closeTarget`; Puppeteer için tab
    target'ları.
  - Oturumlar: her dış istemci sayfada **kendi DevTools oturumunu** açıyor
    (`Target.attachToTarget` sayfanın kendisine). Ortak oturum yalnız yedek yol.
  - Ayarlar → General: "Allow external CDP connections" ve "Copy endpoint". Oturumlara
    `MONOCODE_BROWSER_CDP` env ile veriliyor.
- **Yapışık DevTools (K18, K28).** DevTools frontend'i (`devtools://devtools/bundled/devtools_app.html`)
  sayfanın altında, kendi Alloy view'unda açılıyor.
  - Bağlantı: proxy'nin sayfa seviyesindeki uç noktasına, iç bir token ile. Bu token yalnız
    `devtools://devtools` origin'ini kabul ediyor ve dış CDP ayarı kapalıyken de çalışıyor.
  - Elements, Console, Network, Sources çalışıyor.
  - Bölücü sürüklenebiliyor.
  - Ayrı pencere yalnız yedek yol.
- **Faz 1 artıkları.**
  - Çıkışta düzgün `cef::shutdown` (en çok 2 sn bekler; çıkış < 0,3 sn, helper kalmıyor).
  - Script'le açılan her popup'ta `window.opener`.
  - Sahipsiz popup'lar açanla ya da 10 sn sonra kapanıyor.
  - Diyaloğu açık sekmeye dönünce odak diyaloğa gidiyor.
- **Geliştirici kancaları.** Kullanıcıya dönük değil.
  - `MONOCODE_BROWSER_AGENT_DEV=<dosya>`: köprü env'ini ve CDP uç noktasını dosyaya yazar.
  - `MONOCODE_BROWSER_OPEN_DEVTOOLS=1`: yapışık DevTools'u açar.

## Plandan sapmalar
- **Dış istemcinin oturumu (K38).** Plan sayfanın tek oturumunu paylaşmaktı. Gerçekte V8, zaten açık
  `Runtime`'a ikinci `Runtime.enable`'ı yok sayıyor; Playwright'ın `page.evaluate`'i
  takılırdı. CEF sayfanın kendine attach'e izin verdiği için her istemci ayrı oturum alıyor. Bunun
  sonuçları:
  - `*.disable` çağrıları bizim kayda dokunmuyor.
  - İstemci kopunca script ve binding'leri bitiyor.
  - `Emulation.*` hâlâ sayfa geneli; istemcinin ayarladığı viewport vb. bizim araçları ve
    kullanıcıyı da etkiliyor.
- **`target=_blank`.** CEF `noopener`'ı bildirmediği için bu linkler de popup olarak oluşturulup
  sekmeye alınıyor; eskiden adres baştan açılıyordu. Chrome'a daha yakın; yeni sekme biraz
  geç beliriyor.
- **URL politikası (K40).** Kara liste yerine izin listesi: http/https, `about:blank`, data,
  blob. `javascript:` da reddediliyor; aksi hâlde `browser_eval` onayı atlanabilirdi.
- **Fazladan araçlar.** Plan 17 araçtı. Sekme göster/kapat ve geri/ileri ayrı araçlar olunca 20 oldu.

## Testler
- `monocode-core`: 1279+ test, `browser::` altında 86.
- `monocode-cef`: 11 test.
- `monocode-gpui`: tam set seri çalıştırıldı. Tek hata `editor_analysis::parity_tests::large_tsx_analysis…`
  (QuickJS "dead lock", `App.tsx` analizi). Faz 2 bu testin girdilerine dokunmuyor; ayrıca
  bakılmalı.
- Clippy `-D warnings` ile temiz (üç crate).

## Gerçek uygulamada doğrulama (K30)
Bütün denemeler `native-dev` bundle'ı ve geçici veri klasörüyle yapıldı. Test sayfası:
form, diyaloglar, console hatası, aynı ve farklı origin'li iframe.
- **MCP senaryosu.** 20 aracın hepsi geçti, iframe'ler dahil. Diğer geçenler:
  - ⌘A/C/V
  - arka plan sekmesinin screenshot'ı (JPEG, ≤1568 px, 40–60 KB)
  - alert/confirm/prompt
  - stale ref
  - izole profil
  - `file://` reddi
  - wait sırasında sekme kapanması (`browser_page_gone`, 1,5 sn)
- **Playwright `connectOverCDP`.** Geçti: `goto`, `fill`, `check`, `selectOption`, `click`, `evaluate`,
  cross-origin frame, diyalog, `screenshot`, `newPage`, `close`. Script ~350 ms sürdü.
  `browser.close()` uygulamanın sekmelerini kapatmıyor.
- **Puppeteer `connect({browserURL})`.** Geçti.
- **Yük testi.** 4 MCP istemcisi + sekme aç/kapa döngüsü + Playwright aynı anda, 246 çağrı:
  hatasız, panik yok.
- **Uçtan uca bulunup düzeltilenler:**
  - `networkidle` hiç bitmiyordu.
  - `browser_eval` `window`/DOM düğümünde hata veriyordu.
  - Hiç gösterilmemiş sekmede snapshot yarım belge yakalıyordu.
  - Pencere ekran dışındayken Puppeteer tıklaması 30 sn takılıyordu.
  - Sekme id'leri yeniden kullanılıyordu.
  - Yeni sekmenin ana belgesi kayda düşmüyordu.
- **Gözle doğrulanmadı** (makinede ekran kaydı/tuş izni yok):
  - agent sekmesinin chat'in arkasında odak çalmadan açılması,
  - yapışık DevTools'ta tıklama ve yazma.

## Ölçümler
MacBook Pro M4 Pro, dahili 120 Hz ProMotion ekran, macOS 26.6.2. Test sayfası: 1500 satır
(~10,5k DOM düğümü), 982×709 @2×. Sürüş CDP ile yapıldı:
- `Input.synthesizeScrollGesture` (12 000 px, 3000 px/s),
- 120/s wheel,
- 60 `keyDown`.

Karşılaştırma: Chrome 157 (geçici profil). Monocode CEF 156 kullanıyor.

| Battery Saver kapalı | Monocode | Chrome |
|---|---|---|
| Boşta rAF | 120,0 fps, p95 9,7 ms | 120,0 fps, p95 9,7 ms |
| Scroll gesture | 118,3 / 113,9 fps; ~1000 karenin 14 / 50'si 1,5×'ten uzun | 119,9 / 120,0 fps; 1 / 0 |
| Wheel | 120 fps, uzun kare yok | 120 fps, uzun kare yok |
| `keyDown` gidiş-dönüş (no-op farkı) p50 | ~0,65 ms | ~0,6–0,75 ms |
| `event.timeStamp` → handler p50 / p95 | 0,3 / 0,4–0,5 ms | 0,3 / 0,4–0,5 ms |

Tuş gecikmesi CDP input'uyla ölçüldü; macOS'un gerçek input yolu ölçülmedi.

| Bellek (RSS, MB) | Monocode | Chrome |
|---|---|---|
| 1 sekme | ana 290 (uygulamanın tamamı); helper 482 (5 süreç) | ana 292; helper 612 (7 süreç) |
| 5 sekme | ana 305; helper 1077 (9 süreç) | ana 304; helper 1063 (12 süreç) |

**Battery Saver.** Pil %20'nin altındayken Chromium'un Battery Saver'ı **hem Chrome'da hem Monocode'da**
sayfaları 30 fps'e kilitliyor. Monocode'da bunu kapatacak ayar yok (bkz. açık kararlar).

## Faz 3'e devredilenler
- Battery Saver politikası (kullanıcı kararı bekliyor).
- Programlı smooth scroll'da %1–5 uzun kare (Chrome'da ~%0); profillenmedi. Wheel Chrome'la aynı.
- Cevabından kayda eklenen ana belge isteği her zaman `GET` görünüyor. Yeni sekmeye giden form
  POST'u yanlış görünür. CEF'in kendi açtığı popup'lar da aynı durumda.
- `browser_scroll` ref ile kaydırınca öğenin değil sayfanın konumunu bildiriyor; bazı belge
  boyutları `0 B` görünüyor.
- Yüklenmemiş geri yüklenmiş sekmeler, yüklenene kadar dış CDP'de target olarak görünmüyor.
- **Yapışık DevTools eksikleri:**
  - Panelde ⌘F/⌘P uygulamaya gidiyor.
  - Dock tarafı menüsü çalışmıyor.
  - Panelin diyalog/indirme/popup'ları sayfanınki gibi görünüyor.
  - Açık olması kaydedilmiyor.
- Faz 3 planındakiler: delta snapshot, aksiyon etkileri, `expect_url`, agent varlık UI'ı,
  Grab/Annotate, HAR, intercept, upload/download, PDF, OSR.

## Bilinen sorunlar
- **Başlangıçta ara sıra kilitlenme.** ~15 açılışta 2 kez `cef_initialize` içinde main thread
  bir malloc kilidinde bekledi; o sırada başka bir thread çıkıyordu. Örnek yığın
  `/tmp/mcverify/cef-init-deadlock-sample.txt` (geçici). Ayrıca bir açılışta çıkış kancasından
  sonra ikinci bir uygulama süreci kaldı ve profili tuttu.
- Pane id'leri pencere başına; bugün tek ana pencere olduğu için çakışma yok.
- Dış CDP ayarındaki "Copy endpoint", odaktaki pane projenin oturumu değilse oturum 0 ile
  çalışıyor.
- İzole bir sekme ile ondan açılan popup aynı geçici profili paylaşıyor. Biri kapanınca profil
  bırakılıyor; sonraki popup yeni ve boş bir profil alıyor.
- Chromium başlarken 2 sn'lik bir UI takılma raporu çıkıyor (agent katmanıyla ilgisiz).
