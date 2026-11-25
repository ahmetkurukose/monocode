# SOCode browser: taşıma planı ve eşleme

Browser mode'un tarayıcısı, GR0SST'in Rust + GPUI + CEF tarayıcısından
(izinle, birlikte çalışarak) taşınır. Proje içinde her şeyin adı **socode**'dur.

## İlke

- Kullanıcının gördüğü tarayıcı özellikleri SOCode'dan gelir.
- Monocode'da kalanlar: agent katmanı (MCP araçları, dış CDP uç noktası, Grab,
  rozet ve kontrol devri, Send to Chat), proje entegrasyonu (proje profili ve
  geçmişi, dev sunucuları, Projeler bölümü, Dev mode panelleri) ve SOCode'da
  olmayanlar (passkey, Memory/Battery Saver, yerel sertifikalar K62, PDF).
- Motor: bizim CEF sürümümüz (`cef = "=156.7.0"`); SOCode'un motor
  parçaları bunun üzerine taşınır.
- Alınmayanlar (kullanıcı kararı): Spaces, Library, profiller, Boosts, Arc'tan
  içe aktarma, Easel/notlar, AI.

## Crate'ler

| Crate | İçerik |
| --- | --- |
| `crates/socode-browser` (`socode_browser`) | UI'sız model: tab'lar, favoriler, pinned, klasörler, arşiv, undo, split, omnibox, link kuralları. Zaman dışarıdan verilir; 312 birim testi. |
| `crates/socode-ai` (`socode_ai`) | Modelin ayarlarındaki AI tipleri (yalnız veri). AI özellikleri kullanılmaz. |

Kaynak commit: `55c624df` (2026-11-09).

## Eşleme

| Alan | Bugün (Monocode) | SOCode | Birleşim |
| --- | --- | --- | --- |
| Sidebar modeli | `modes::BrowseState` (tabs, favorites, pinned ağacı, home, names) | `socode_browser::Browser`: tek Space, `favorites` (profil), `pinned` + `pinned_items` + `folders`, `today`, `archive` | Model `Browser` olur; tek Space kullanılır, Space arayüzü yok. Her model tab'ı bir browser pane'ine bağlanır (`TabId` ↔ pane id). |
| Tab ↔ sayfa | Pane'ler `workspace.browser_panes` | `TabId`, `url`, `title`, `custom_title`, `pinned_url` | Model tab'ı pane'i belirler; sayfa olayları (`PageChanged`) modele `set_url`/`set_title` olarak döner. Uzlaştırma (`reconcile`) her model değişikliğinden sonra pane açar/kapatır. |
| ⌘W | Today kapanır, pinned uyur | `close_tab`: Today arşive, pinned/favori pinned URL'ine dönüp uyur | SOCode'unki. |
| Clear | Today'i kapatır | `clear_today` (arşive, ⌘Z ile geri) | SOCode'unki. |
| ⇧⌘T | Son 25 adres | `restore_last_archived` | SOCode'unki. |
| ⌘Z / ⇧⌘Z | Yok | `undo_sidebar` / `redo_sidebar` | SOCode'unki; uzlaştırma pane'leri geri getirir. |
| Otomatik arşiv | Yok | `archive_idle_except` (12 saat) | Dakikada bir; açık sayfa, ses çalan ve agent'ın kullandığı tab'lar korunur. |
| Yeniden adlandırma | `names` | `rename_tab` / `reset_tab_name` | SOCode'unki. |
| Pinned URL | `home` | `pinned_url`, `reset_pinned_tab`, `replace_pinned_url` | SOCode'unki. |
| Klasörler | `Pinned` ağacı | `create_folder`, `move_item_to_folder`, `move_folder`, `set_folder_collapsed` | SOCode'unki; klasör silmek tab'ları köke taşır. |
| Projeler bölümü | Proje pane'leri | — | Monocode'da kalır. |
| Link yönlendirme | K19 | `routing` | K19 varsayılan kural; SOCode kuralları üstüne. |

## Veri taşıma

Eski `browse` kaydı (`tabs`, `favorites`, `pinned`, `home`, `names`) ilk
açılışta modele aktarılır: her pane bir model tab'ı olur, favoriler ve pinned
(klasörleriyle) korunur, `home` → `pinned_url`, `names` → `custom_title`.

## Aşamalar

1. Model crate'leri (bitti) ve bu doküman.
2. Sidebar modelinin geçişi: arşiv, ⌘Z, otomatik arşiv.
3. Command Bar ve sidebar'da adres.
4. Split View ve Peek.
5. Little window, medya + PiP.
6. Capture ve Site Control Center.
7. Ortak parçaların değişimi (indirmeler, parolalar, eklentiler, …) veri
   taşımasıyla.
8. Link yönlendirme arayüzü.
