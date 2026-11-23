# Faz 4 — bitiş notu (2026-11-23)

Durum: bitti. Kararlar `context.md`'de (K54–K68). Kod `browser/cef-spike`'ta ve
`AK/monocode_enhancements`'a birleştirildi (`8ae0ef71`).

## Yapılanlar

### Süreçler: `fork` yok
- **Alt süreçler.** `monocode-core`'un başlattığı her çocuk macOS'ta `posix_spawn` ile açılıyor
  (`process::spawn/output/status`).
  - std `posix_spawn`'da sinyal maskesini sıfırlamıyor. Bu yüzden çağıran thread'in maskesi
    spawn sırasında boşaltılıyor.
  - Çıplak program adı + değişmiş PATH std'yi `fork`'a düşürüyordu. Program artık o PATH'te
    mutlak yola çevriliyor (`program_on_path`).
  - `pre_exec` kaldırıldı. Arka plan önceliği spawn'dan sonra `setpriority` ile veriliyor.
- **Terminaller (K68).** PTY çocukları küçük `monocode-pty-exec` üzerinden başlıyor: kontrol
  terminalini alıyor, sinyalleri sıfırlıyor ve `execve` ile programa geçiyor.
  - Helper yoksa uygulamanın kendi image initializer trampoline'i yedek yol.
  - Build betikleri, bundle, CI ve release workflow'ları helper'ı da derleyip paketliyor.

### 4a
- **Faz 3 artıkları.**
  - DevTools'taki ~48 px fark gerçek bir boşluk değildi: `ui_scale` %90 ile GPUI pikseli ve
    AppKit noktası karşılaştırılmıştı.
  - DevTools menüsündeki "Send Selected Element to Agent", Elements'ta seçili öğeyi gönderiyor
    (`$0`, DevTools'un kendi CDP oturumundan).
  - Popup'ın başlattığı indirme, açan sayfanın klasörüne gidiyor.
  - Smooth scroll ölçüldü, sorun yok: medyan 8,3 ms, 1350 karede 1 uzun kare.
- **Ses ve yazdırma.**
  - Sekmede hoparlör ikonu, sessize alma ve sekme menüsü; sessiz durumu kaydediliyor.
  - ⌘P ve "Print…" Chromium'un yazdırmasını açıyor; macOS'un yazdırma paneli çıkıyor.
- **Memory Saver ve lease'ler (K64).**
  - Saf politika `browser/lifecycle.rs`'te.
  - Discard edilen sekme görünce ya da agent kullanınca yeniden yükleniyor; scroll konumu
    geri geliyor.
  - Ayar Settings → General'da: Kapalı / 15 / 30 / 60 dk / 4 sa.
- **Battery Saver (K52).** Chromium'unki kapalı. Düşük pilde, lease'i olmayan arka plan
  sekmeleri donduruluyor.
- **Site izinleri (K63).**
  - İstekler sayfayı örtmeyen bir bantta soruluyor.
  - Agent'ın arka plan sekmesinde istekler reddediliyor ve agent'a `permissionRequest` olarak
    bildiriliyor.
  - Site bilgisi düğmesi ve Settings'te sıfırlama var.
- **Sertifika (K62).**
  - Yerel adreslerde self-signed sertifika sorulmadan kabul ediliyor.
  - Diğerlerinde GPUI uyarısı çıkıyor; "Proceed" yalnız oturum boyunca geçerli.
  - Agent'a `browser_cert_error`.
- **Geçmiş (K55, K65).**
  - Proje bazlı SQLite, 90 gün saklama.
  - ⌘Y penceresi ve adres çubuğu önerileri.
  - Agent için `browser_history`.
- **Yeni sekme (K66).**
  - GPUI sayfası; gezinene kadar CEF browser'ı oluşmuyor.
  - İçerik: projenin dev sunucuları, son açılanlar, en çok ziyaret edilenler.
  - Browser sekmesindeyken ⌘T artık yeni sekme açıyor.

### 4b
- **Şifre kasası (K56, K57, K60).**
  - Profil başına `Browser Passwords/<profil>.db`. Her değer AES-256-GCM ile şifreli
    (`ring`); anahtar Keychain'de.
  - "Save password?" bandı; araç çubuğunda "Fill as …" ve anahtar menüsü.
  - Settings → Passwords: göster/kopyala/düzenle için Touch ID.
  - Agent `browser_fill_login` ile dolduruyor; şifreler bütün araç cevaplarında maskeleniyor.
- **Import (K61).**
  - Chrome, Arc ve Brave'den cookie ve şifre; şifreler kasaya `merge_logins` kuralıyla giriyor.
  - Google ve YouTube cookie'leri, ülke alan adları dahil, alınmıyor.
- **Telefonla QR passkey (K58, K59).**
  - Bluetooth izni ve güç durumu kontrol ediliyor; sorun varsa bant çıkıyor.
  - Agent `passkeyRequest`'i görüyor ve `browser_dialog dismiss` ile iptal edebiliyor.
  - Dev bundle'ı, varsa "Monocode Local Signing" sertifikasıyla imzalanıyor
    (`scripts/native-dev-signing-cert.sh`).
  - Dağıtım entitlement'larına `com.apple.security.device.bluetooth` eklendi.
- **Mikrofon.** Sayfanın `getUserMedia`'sı, macOS kararı yoksa uygulama soruyu önce kendisi
  sorana kadar tutuluyor (`browser::media`).

## Plandan sapmalar
- **Battery Saver arka plan sekmelerini donduruyor, 30 fps'e indirmiyor.** Windowed view'da sekme
  başına kare hızı yok; Chromium'un ayarı da küresel.
- **PTY'de trampoline + ayrı helper.** Kontrol terminali `posix_spawn` dosya işlemleriyle
  alınamıyor (K68).
- **Autofill araç çubuğunda, sayfa üstünde açılır liste yok (K44).**
- **İş bölümü (K67).** Worktree'lerde build, ortak target'la bile diski doldurdu. Faz ortasından
  itibaren agent'lar yalnız kod yazdı; build ve testler ana worktree'de alındı.

## Ertelenenler / Faz 5'e devredilenler
- **1Password.** Faz 5'in ilk eklentisi. Kurulunca kendi kayıt ve doldurma tekliflerimiz kapanacak
  (`password_extension_installed()`).
- **Partitioned (CHIPS) cookie import'u.** CEF 156'nın `CefCookie`'sinde partition anahtarı yok.
- **HTTP Basic auth şifreleri.** Ne import ediliyor ne kaydediliyor.
- **Arka plan çocuklarında disk I/O kısıtlaması.** Başka bir süreç için ayarlayan public API yok.

## Ölçümler
- **Memory Saver.** 10 sekme: helper'lar 1507 MB / 14 süreç. 8'i discard edilince 461 MB /
  6 süreç.
- **Battery Saver.** Chromium'unki zorla açılınca görünen sekme 30,3 fps; bizimki açıkken
  görünen sekme 119,6 fps, arka plan sekmesinin zamanlayıcısı 65 sn durdu.
- **PTY exec adımı.** Uygulamanın kendisiyle ~270 ms sıcak, 4,6 sn soğuk (debug, 367 MB).
  `monocode-pty-exec` ile <10 ms.

## Testler
- Birleşik kodda (AK ile, `8ae0ef71`):
  - core 1460/1460, cef 21/21, betik testleri 32/32.
  - GPUI 1424/1425. Kalan `large_tsx…` Faz 2'den beri bilinen hata.
- Clippy `-D warnings`: Faz 4 kodu temiz. AK'den gelen dört bulgu duruyor (monday.com, PR
  diyaloğu, database testleri); bu faz dokunmadı.
- Bundle paketleme testinin fixture'ı düzeldi: helper, `monocode-pty-exec` ve framework eksikti
  (Faz 1'den beri kırıktı).

## Gerçek uygulamada doğrulama
- **Agent'ların ayrı bundle'larda araçlarla doğruladıkları:**
  - sertifika hataları ve yerel self-signed sertifika;
  - arka plan izin reddi;
  - Memory Saver discard ve yeniden yükleme;
  - ses durumu ve sessize almanın kalıcılığı;
  - ⌘P'nin yazdırma paneli;
  - geçmiş kaydı ve `browser_history`;
  - popup indirmesi;
  - DevTools ölçümü.
- **Yalnız testlerle sınananlar:**
  - izin ve sertifika arayüzü;
  - yeni sekme sayfası, ⌘Y ve adres önerileri;
  - şifre kasası ve autofill;
  - import arayüzü;
  - Bluetooth ve mikrofon bantları;
  - bundle'daki terminal.
- **Kullanıcının elle denemesi bekleniyor:**
  - imza sertifikası;
  - Gmail QR;
  - gerçek import;
  - şifre kaydetme ve doldurma;
  - mikrofon;
  - terminal;
  - kapanışta profil kilidi.

## Bilinen sorunlar
- **QR ve mikrofon** sabit imza olmadan her build'de yeniden izin istiyor. `SOCode.app` aynı
  bundle id'yi taşıdığı için TCC kaydı çakışıyor (§11).
- **Mikrofon düzeltmesi bir hipoteze dayanıyor.** Ses servisinin macOS sorusunda beklediği
  günlük ve kaynaktan çıkarıldı, canlı doğrulanmadı.
- **Ses tespitinin kör noktaları:** belgeye eklenmemiş `new Audio()`, shadow DOM, Web Audio ve
  süreç dışı iframe'ler.
- **İndirme için açılan popup** boş sekme olarak kalıyor. `noopener` linklerin indirmeleri
  yönlendirilmiyor.
- **İzin istekleri:**
  - Chromium aynı anda tek izin istemi gösteriyor.
  - macOS, cihaz sayımı sırasında arka plan reddinden önce sorabiliyor.
- **Sertifika hatasından sonra** sekmenin URL'i ve başlığı bazen eski kalıyor.
- **Şifre maskeleme** cross-origin iframe girişlerini ve dönüştürülmüş şifreleri (hash, base64)
  kapsamıyor. Ekran görüntüleri de maskelenmiyor.
- **Discard edilen sekme** geri/ileri geçmişini kaybediyor.

## Geliştirici kancaları
Kullanıcıya dönük değil.
- `MONOCODE_BROWSER_DISCARD_AFTER_SECS`, `MONOCODE_BROWSER_BATTERY_LOW=1`,
  `MONOCODE_BROWSER_CHROMIUM_BATTERY_SAVER=<0-3>`
- `MONOCODE_BROWSER_FIDO_LOG=1`: Chromium'un device-log'u `chromium.log`'a.
- `MONOCODE_PTY_EXEC=<yol>`: PTY exec helper'ının yeri (integration testi).
- `MONOCODE_BROWSER_GRAB_DEV` artık `selected`, `print`, `mute` ve `audio` komutlarını da alıyor.
- Faz 3 ve 2'dekiler: `MONOCODE_BROWSER_GRAB_DEV`, `MONOCODE_BROWSER_OPEN`,
  `MONOCODE_BROWSER_AGENT_DEV`, `MONOCODE_BROWSER_OPEN_DEVTOOLS`.
