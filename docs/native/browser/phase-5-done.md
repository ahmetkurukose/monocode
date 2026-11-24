# Faz 5 — bitiş notu (2026-11-24)

Durum: bitti. Kararlar `context.md`'de: K60, K69, K70. Kod `browser/cef-spike`'ta ve
`AK/monocode_enhancements`'a birleştirildi (`e08b8a8d`).

## Yapılanlar
- **İki spike, Proton Pass ile.**
  - **Hibrit** (`browser/ext-spike-hybrid`): Chromium'un eklenti sistemini `--load-extension`
    ile kullandı. Kısmen çalıştı:
    - `tabs.query` ve `windows` bizim sekmeleri görmüyor.
    - Eklenti sayfaları gömülü (Alloy) sekmelerde açılamıyor.
    - Kurulum ve güncelleme için CEF'te API yok.
  - **Kendi katmanımız** (`browser/ext-spike-own`): dört başarı maddesi de geçti. Fazın
    mimarisi bu oldu (K69).
- **Eklenti katmanı (5a).** Chromium'un eklenti sistemi kullanılmıyor; katman genel, yani
  eklentiye özel kod yok.
  - **Kurulum:** Web Store'dan indirme, CRX3 imza doğrulaması, kurma, kaldırma, aç/kapa.
  - **Güncelleme:** açılıştan 60 sn sonra ve 5 saatte bir. Yeni izin isteyen sürüm onay
    bekliyor.
  - **`mcext://` şeması:** web sayfaları yalnız eklentinin izin verdiği dosyalara
    (web_accessible_resources) erişebiliyor; manifest CSP uygulanıyor.
  - **Arka plan:** gizli bir sayfada dedicated worker. Çökünce yeniden başlıyor.
  - **Content script'ler:** CDP ile isolated ve MAIN world'e, cross-origin iframe'ler dahil.
  - **`chrome.*`:** API'ler tek bir callback/promise adaptöründen geçiyor ve bizim sekme
    modelimizle cevaplanıyor.
  - **Cross-origin fetch:** profilin cookie'leriyle akışlı bir proxy üzerinden.
  - **Site → eklenti mesajlaşması:** `externally_connectable` çalışıyor.
  - **Arayüz:** araç çubuğu düğmesi ve rozet, düğmenin altında açılan popup,
    Settings → Extensions.
  - **K60:** etkin bir şifre yöneticisi eklentisi varken kendi şifre yöneticimiz kayıt ve
    doldurma önermiyor.
- **Proton Pass:** kullanıcının gerçek girişiyle doğrulandı (2026-11-24).
- **Skill:** `.claude/skills/browser-extension/SKILL.md`. Yeni bir eklentiyi eklemenin adımları:
  - API envanterini çıkarmak, shim ve dispatcher'daki boşlukları doldurmak, testler;
  - dev kancasıyla gerçek uygulamada doğrulamak;
  - hesap gerektiren kontrolleri kullanıcıya bırakmak;
  - bilinen tuzaklar.

## Plandan sapmalar
- **Mimari, hibrit model değil kendi katmanımız.** Gerekçe yukarıdaki spike sonuçları (K69).
- **Faz Proton Pass ile kapandı.** Hedefteki 20–30 eklenti fazın içinde yapılmadı; bundan
  sonra skill'le ayrı işler olarak eklenecek.

## Ertelenenler
- **Diğer eklentiler:** 1Password, Bitwarden (ikisi de `PASSWORD_MANAGERS` listesinde),
  reklam engelleyici, React DevTools ve diğerleri.
- **Henüz yazılmamış parçalar:** `declarativeNetRequest`, `sidePanel`, `contextMenus`,
  `notifications`, `identity`, engelleyen `webRequest`, fetch proxy'sinde XHR, gerçek service
  worker yaşam döngüsü.

## Ölçümler (Proton Pass 1.44.0)
- **Arka planın hazır olma süresi:** soğukta 170–180 ms, sıcakta 43–82 ms.
- **Eklenti host'u:** renderer ~90 MB RSS. Eklenti kapatılınca uygulama ağacı 837 MB'tan
  789 MB'a düştü.

## Testler
- Birleşik kodda (`e08b8a8d`):
  - core 1506/1506, cef 21/21.
  - GPUI 1432/1433. Kalan `large_tsx…` Faz 2'den beri bilinen hata.
- Shim testi (QuickJS) 420 metodun her birinin callback'ini tam bir kez çağırdığını ve hatada
  `lastError`'u ayarladığını doğruluyor.

## Bilinen sorunlar
- **Fetch proxy'si:** yalnız `fetch` geçiyor, XHR geçmiyor; `redirect: manual` opak dönmüyor.
- **Content script kapsamı:** dosyalar kendi fonksiyon kapsamında çalışıyor; dosyalar arası üst
  düzey `var` paylaşımı yok.
- **OPFS:** `mcext://` şemasında çalışmıyor. Proton IndexedDB'ye düşüyor.
- **`permissions.request`:** opsiyonel izinlerden yalnız `privacy` veriliyor, diğerleri
  reddediliyor.
- **Dış CDP istemcileri:** eklentinin isolated world bağlamlarını görebiliyor; Chrome'da da
  böyle.

## Geliştirici kancaları
- **`MONOCODE_BROWSER_EXT_DEV=<dir>`:** `state.json` yazıyor, `cmd` dosyasından komut alıyor:
  - kurulum ve güncelleme: `install`, `updates`;
  - aç/kapa ve kaldırma: `enable`, `uninstall`;
  - deneme: `popup`, `eval`, `crash`.
- **`MONOCODE_NATIVE_DATA_DIR`:** log'u `<veri>/Logs/` altına alıyor. `MONOCODE_NATIVE_LOG_DIR`
  log klasörünü ayrıca seçiyor.
- **Betikler:** `scripts/browser-extensions/` altında `run.sh`, `cdp.mjs` ve `mcp.mjs`.
