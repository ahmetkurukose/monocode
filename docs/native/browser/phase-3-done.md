# Faz 3 — bitiş notu (2026-11-22)

Durum: bitti. Kararlar `context.md`'de (K43–K53).

## Yapılanlar
- **Aksiyon etkileri (K45).** Şu araçlar cevaplarında ne olduğunu da söylüyor:
  - Kapsananlar: click, fill, type, press, scroll ve upload.
  - Bekleme: navigasyon başlarsa en çok 5 sn; sonra 100 ms DOM sessizliği (en çok 750 ms).
    Sessizlik, sayfanın göremediği izole bir world'deki MutationObserver ile sayılıyor.
  - Rapor: URL/başlık değişimi, yeni sekme, indirme, console hataları ve diyalog.
  - Delta: en çok 40 satır; fazlası "snapshot al" diyor.
  - Sayfada hiçbir şey değişmediyse ikinci snapshot alınmıyor.
- **Delta snapshot, `expect_url` (K46).**
  - `browser_snapshot {delta:true}` yalnız değişenleri veriyor.
  - Her sayfa aracında `expect_url` var. Sayfa yanlışsa araç hiçbir şey yapmadan
    `browser_expected_url_mismatch` dönüyor.
- **Kontrol modeli (K48).** Saf mantık `browser/control.rs`'te. Uygulamanın input izleyicisi
  tıklama, tuş, scroll ve fare hareketini ayırıyor.
  - Agent aktifken kullanıcının tıklaması ya da tuşu kontrolü alıyor; scroll almıyor.
  - Kontrol "Give back to agent" düğmesiyle ya da 16 sn sessizlikle agent'a dönüyor.
  - Bu sırada agent'ın input araçları `browser_user_control` dönüyor.
- **Varlık arayüzü (K47).**
  - Sekme ikonunda vurgu renginde bir nokta çıkıyor.
  - Araç çubuğunda "Agent · <oturum>" yazıyor. Kontrol kullanıcıdayken geri sayım ve
    düğme de görünüyor.
  - Agent'ın son komutundan 20 sn sonra ikisi de kalkıyor.
  - Yeniden çizim zamanlayıcıyla yapılıyor, kare başına sorgu yok.
- **Grab (K49).**
  - Açma: araç çubuğu düğmesi, **⌥⌘G** ya da sağ tıkta "Send to Agent". Çıkış Esc.
  - Seçici, sayfanın göremediği bir katmanda çalışıyor; seçme tıklaması sayfaya gitmiyor.
  - Toplanan bilgi:
    - seçici,
    - a11y rol/isim,
    - yakın metin,
    - stil özeti,
    - ≤4 KB HTML,
    - React bileşen zinciri ve kaynak dosyası,
    - kırpılmış görüntü.
  - Sırlar hem sayfada hem Rust'ta temizleniyor.
  - Teslim: sekmeyi açan agent'ın chat'ine, yoksa odaktaki chat'e; `.md` ve görüntü eki olarak.
- **Yeni araçlar (K50, K51).**
  - `browser_pdf`, `browser_upload` ve `browser_download` eklendi.
  - Yollar symlink ve `..` çözüldükten sonra yalnız proje klasöründe kabul ediliyor.
  - `browser_screenshot`'ın `path`'i de proje klasörüyle sınırlandı.
- **Overlay kancası (K44).**
  - GPUI'nin deferred katmanında çizilen her şey ve tooltip'ler sayfayla kesişince otomatik
    cover bildiriyor. Bunlar menüler, popover'lar, modal'lar ve anchored öğeler.
  - Vendor'daki `gpui-pre` yamalı: ~50 satır, "Monocode" işaretli.
  - Sürüklenen önizleme bu kancanın dışında kalıyor; kendisi `cover` çağırmalı.
- **Başlangıç kilitlenmesi.**
  - Sebep: macOS libc'de `setlocale` ile `fork()` arasındaki kilit sırası çakışması.
    Chromium, `cef_initialize` içinde `setlocale` çağırırken bellek ayırıyor. Aynı anda arka
    plan thread'leri `fork` yapıyordu (git, sağlayıcılar).
  - Çözüm: `main`'in ilk satırında, uygulama tek thread'liyken `setlocale(LC_ALL, "")`.
  - C test programı: düzeltmesiz 30/30 kilitlendi, düzeltmeyle 0/30.
- **Battery Saver (K52).**
  - Settings → General'da "Browser battery saver"; varsayılan açık.
  - Kapatınca Chromium'un `battery_saver_mode.state` değeri 0'a yazılıyor.
- **Yapışık DevTools.**
  - DevTools odaktayken kısayolları DevTools'a gidiyor. Uygulamada kalanlar yalnız ⌘W, ⌥⌘I,
    ⌘1–9, ⇧⌘[ / ⇧⌘] ve ⌘Q.
  - Dock menüsü kapalı: CEF bunun için kanca vermiyor.
  - DevTools'un açtığı web linkleri normal sekme olarak açılıyor.
  - DevTools'un diyalogları "DevTools says" başlığıyla ayrı gösteriliyor; agent'lar bunları
    görmüyor.
  - Açık olması ve yüksekliği pane başına kaydediliyor.
- **Dış CDP.**
  - Geri yüklenmiş ama yüklenmemiş sekmeler de target olarak listeleniyor.
  - Playwright bağlanınca bu sekmeler yükleniyor.
- **Faz 2'nin küçük kusurları.**
  - `browser_scroll` artık öğenin kendi kaydırma konumunu bildiriyor.
  - `0 B` boyutlar düzeldi.
  - Ana belge isteğinin yöntemi bilinmiyorsa yanlış `GET` yerine `?` yazıyor.

## Plandan sapmalar
- **Grab'in kısayolu ⌥⌘G.** ⇧⌘G zaten "find previous".
- **Esc sayfa kısayolu yapılmadı.** Yapılsaydı web sayfalarına hiç ulaşmazdı. Grab modunda Esc'i
  seçici ve uygulama yakalıyor.
- **Agent'ın tıklamasıyla açılan sekmeler arka planda açılıyor.** `target=_blank` dahil, kullanıcı
  kontrolü almadıysa. Sebep: önde açılan sekme kullanıcının görünümünü alıyordu.

## Testler
- `monocode-core`: 1319 test.
- `monocode-cef`: 14 test.
- `monocode-gpui`: tam seri set 1338 testten 1 hata. Kalan hata Faz 2'den beri bilinen
  `editor_analysis::…large_tsx…`.
  - Ayarlar testi (CDP varsayılanı) sıraya bağımlıydı; düzeltildi. Düzeltmeden sonra tam set
    yeniden çalıştırılmadı.
- Clippy `-D warnings` ile temiz.

## Gerçek uygulamada doğrulama
`native-dev` bundle'ı, geçici veri klasörü ve yerel test sayfaları kullanıldı. Yerel test
sayfaları: form, upload, download, grab, React dev build, efektler.
- **Geçenler:**
  - Etkiler: arka plan sekmesinde de; izole world CEF'te çalışıyor.
  - Delta ve `expect_url`.
  - PDF.
  - Upload: script ile açılan gizli input dahil.
  - Download: art arda 7 indirme proje klasörüne.
  - Grab: %100 ve %150 zoom'da kırpma piksel düzeyinde doğru; React bileşen zinciri ve kaynak
    dosyası yakalandı.
  - DevTools yüksekliğinin geri gelmesi.
  - Playwright'ın 11 geri yüklenmiş sekmeyi görmesi.
  - Battery Saver.
  - 20 açılış: kilitlenme yok.
- **Uçtan uca bulunup düzeltilenler:**
  - Yalnız metin değişince "hiçbir şey değişmedi" deniyordu.
  - Agent tıklamasıyla açılan sekme öne geçiyordu.
  - Üçüncü indirmeden sonra indirme yoktu; Chromium'un çoklu indirme izni engelliyordu.
  - Gizli file input'a upload yapılamıyordu.
  - %150 zoom'da screenshot ve kırpma yanlış yerden alınıyordu.
- **Kullanıcının elle doğrulaması (2026-11-23).** `native-dev` uygulaması ayrı bir veri
  klasörüyle açıldı. Kullanıcı Faz 3'ün görünen kısımlarını denedi ve "düzgün çalışıyor" dedi:
  - rozet ve etiket,
  - gerçek tıklamanın kontrolü alması ve "Give back to agent",
  - overlay'in son kareye geçmesi,
  - Grab seçicisi,
  - yapışık DevTools.

## Ölçümler
- **Tıklama süresi, etkilerle birlikte.** Sayfayı değiştirmeyen ya da tek öğe ekleyen tıklama
  medyan ~167 ms (Faz 2: ~160 ms). Arka plan sekmesinde 168–183 ms. Navigasyon yapan tıklama
  ~59 ms.
- **Kırpma, cihaz oranı 2.** %100'de 240×120 CSS'lik kart 480×240; %150'de 720×360. Konum tam.
- **Açılış.** Açılış–snapshot–çıkış döngüsü ~6 sn; çıkış 0,1 sn.

## Faz 4'e devredilenler / ayrı iş
- **Çıkıştan sonra kalan süreç (ayrı iş, uygulama geneli).** Ağır fork yükünde 50 açılışın
  7'sinde kaldı.
  - Bu bir fork çocuğu: `exec`'ten önce `xpc_atfork_child` içinde, Chromium'un allocator
    kilidinde takılıyor ve profili tutuyor.
  - Çözüm: `monocode-core/src/process.rs`'te `fork` yerine `posix_spawn`. Bunun için
    `pre_exec` kaldırılmalı ve `PATH` + çıplak program adı kullanılmamalı.
- **DevTools.** Yapışık DevTools açıkken sayfa görünüm alanı yerleşimin verdiğinden ~48 px kısa
  ölçüldü (434 / 482). Gerçek bir boşluk mu, doğrulanmadı.
- **Yapışık DevTools'un sağ tık menüsü.** "Send to Agent" orada da görünüyor ama bir şey yapmıyor.
- **İndirme hedefi.** Agent'ın düz `browser_click` ile başlattığı indirme `~/Downloads`'a gidiyor.
  Bu tasarım gereği; projeye yalnız `browser_download` indiriyor. Popup'ın başlattığı indirme
  klasör yönlendirmesine girmeyebilir.
- **Smooth scroll.** Programlı smooth scroll'daki uzun kareler için muhtemel sebep, mesaj pompası
  zamanlayıcısının gecikmesi; ölçülmedi.
- **Tooltip yan etkisi.** Sayfanın üstüne sarkan tooltip açıkken sayfa son kareye geçiyor
  (K44 gereği).

## Geliştirici kancaları
Kullanıcıya dönük değil.
- `MONOCODE_BROWSER_GRAB_DEV=<dosya>`: `grab <pane> <x> <y>`, `pick <pane>`, `zoom <pane> <yüzde>`.
- Faz 2'dekiler: `MONOCODE_BROWSER_OPEN`, `MONOCODE_BROWSER_AGENT_DEV`,
  `MONOCODE_BROWSER_OPEN_DEVTOOLS`.
