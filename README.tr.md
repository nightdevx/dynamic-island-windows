<p align="center">
  <img src="assets/island.png" width="88" alt="">
</p>

<h1 align="center">Windows için Dynamic Island</h1>

<p align="center">
  Ekranın üst ortasında, iPhone'daki Dynamic Island gibi küçük bir hap: çalan müzik, Claude Code oturumların, ses seviyesi ve mikrofon/kamera kullanımı tek bakışta.
</p>

<p align="center">
  <a href="https://github.com/nightdevx/dynamic-island-windows/releases/latest"><img src="https://img.shields.io/github/v/release/nightdevx/dynamic-island-windows?label=indir" alt="İndir"></a>
  <img src="https://img.shields.io/badge/Windows-10%20%7C%2011-0078D4" alt="Windows 10 | 11">
  <img src="https://img.shields.io/badge/Rust-native-B7410E" alt="Rust">
  <a href="LICENSE"><img src="https://img.shields.io/badge/lisans-MIT-green" alt="MIT lisansı"></a>
</p>

<p align="center">
  <a href="README.md">English</a> · <b>Türkçe</b>
</p>

<p align="center">
  <img src="docs/compact-music.png" alt="Müzik çalarken kapalı island">
  &nbsp;
  <img src="docs/compact-claude.png" alt="İzin bekleyen bir Claude Code oturumu">
</p>
<p align="center">
  <img src="docs/expanded-music.png" alt="Açık island, müzik sayfası">
  &nbsp;
  <img src="docs/expanded-claude.png" alt="Açık island, Claude sayfası">
</p>

## Özellikler

- **Çalan müzik:** Windows medya kontrollerinde görünen her oynatıcı (Spotify, tarayıcılar, Medya Oynatıcı, Apple Music…). Şarkı, sanatçı, kapak ve ilerleme. Önceki, oynat/duraklat, sonraki, karıştır ve tekrar düğmeleri (oynatıcı destekliyorsa). İlerleme çubuğuna tıklayınca o yere atlar, fare tekerleği oynatıcılar arasında geçer. Müzik çalarken kapalı hapta küçük bir ekolayzer döner ve kenarında kapak renginde soluk bir parıltı olur.
- **Claude Code:** açık oturumlar ve ne yaptıkları (çalışıyor, izin bekliyor, yanıt hazır), 5 saatlik ve haftalık plan limitlerinin ne kadarının kullanıldığı. Seni bekleyen bir oturum kısa bir önizleme açar. Bir oturuma tıklamak onun penceresini öne getirir.
- **Dosya rafı:** dosyaları island'a sürükleyin (ya da sağ tık → Gönder → Dynamic Island), rafında beklesinler; sonra oradan istediğiniz klasöre, e-postaya ya da sohbete sürükleyin. Dosyalarınız yerinden oynamaz: raf yalnızca nerede olduklarını hatırlar, bırakınca kopyalanır.
- **Rafta ekran görüntüleri:** her yeni ekran görüntüsü (Win + PrtScn, Ekran Alıntısı Aracı) kısa bir önizlemeyle rafın ikinci listesine, çekildiği saatle eklenir.
- **Toplu seçim:** iki listede de tıklayarak seçin, seçilenlerin hepsini birden sürükleyip bırakın. Çift tıklama dosyayı açar; uzun listeyi fare tekerleği kaydırır.
- **Ses seviyesi:** sistem sesi değişince seviye kısa süre görünür.
- **Mikrofon ve kamera:** bir uygulama kullanırken hap uzar ve ucunda nokta yanar: mikrofonda turuncu, kamerada yeşil.
- **Yolunuza çıkmaz:** tam ekran uygulamalarda ve oyunlarda kendiliğinden gizlenir, diğer zamanlarda pencerelerin üstünde kalır.

<p align="center">
  <img src="docs/shelf.png" alt="Dosya rafı">
</p>
<p align="center">
  <img src="docs/peek.png" alt="Önizleme: izin bekleyen bir oturum">
  &nbsp;
  <img src="docs/volume.png" alt="Ses önizlemesi">
</p>

## Bilgisayarı yormaz

Island, çalışma zamanı ya da tarayıcı motoru olmayan, Rust ile yazılmış yerel bir uygulama. Bütün animasyonları Windows'un kendi kompozitörü (DWM) çalıştırır; uygulama yalnızca gösterdiği bir şey değişince çizer. Bir şey değişmezken uyur ve işlemciyi neredeyse hiç kullanmaz; bellek 10 MB civarındadır. Kurulum programı 2 MB.

## Kurulum

[Releases](https://github.com/nightdevx/dynamic-island-windows/releases/latest) sayfasından `DynamicIsland-Setup-<sürüm>.exe` dosyasını indirip çalıştırın. Yönetici izni gerekmez; yalnızca sizin kullanıcınıza kurulur. Kurulumda şunları seçebilirsiniz:

- **Windows açılınca başlat**
- **Claude Code'a bağla** (Claude Code kuruluysa görünür): Claude sekmesinin oturumlarınızı görmesi için island'ın hook'larını `~/.claude/settings.json` dosyasına ekler. Dosyadaki diğer her şey olduğu gibi kalır, ilk seferde yanına bir yedek alınır. Kaldırınca hook'lar da silinir.

Kurulum programı henüz imzalı değil, bu yüzden Windows SmartScreen uyarı verebilir: **Ek bilgi → Yine de çalıştır**.

Gereksinimler: Windows 10 (1809) veya Windows 11, 64 bit.

## Kullanım

- Açmak için island'ın üzerine gelin ya da tıklayın. Üstteki sekmeler Müzik, Claude ve Raf arasında geçer.
- Island'ın üzerinde fare tekerleği: başka oynatıcıya geçer.
- Dosyaları island'a sürükleyin: rafa konur. Rafta (Dosyalar ve Ekran görüntüleri) tıklayarak seçin, birini sürükleyince seçilenlerin hepsi bırakılır ve raftan kalkar; çift tıklama açar. "Tümünü seç" hepsini seçer; "Kaldır" / "Temizle" öğeleri listeden çıkarır, dosyaları silmez.
- Island'a ya da tepsi simgesine sağ tık: başlangıçta çalıştır, ayarlar, çıkış.

### Ayarlar

Ayarlar `%APPDATA%\DynamicIsland\settings.ini` dosyasındadır (sağ tık → ayarlar) ve kaydettiğiniz anda uygulanır.

| Ayar | Varsayılan | |
|---|---|---|
| `expand_on_hover` | `true` | Üzerine gelince açılsın; `false` ise yalnızca tıklayınca |
| `hover_delay_ms` | `300` | Açılmadan önce bekleme süresi (ms) |
| `hide_exe` | boş | Bu programlar öndeyken gizlen, örn. `valorant.exe, obs64.exe` |
| `show_exe` | boş | Bu programlar tam ekran olsa da görünür kal |
| `equalizer` | `true` | Müzik çalarken hareketli ekolayzer |
| `screenshots` | `true` | Yeni ekran görüntüleri önizlemeyle rafa eklenir |

### Claude Code'u elle bağlama

Kurulumda bu seçeneği atladıysanız `island.exe --setup-claude` komutu aynı işi yapar (`--remove-claude` geri alır). Ya da aşağıdakini `~/.claude/settings.json` dosyasına ekleyin (`<kullanıcı>` yerine Windows kullanıcı adınızı yazın):

<details>
<summary>hooks</summary>

```json
{
  "hooks": {
    "SessionStart":      [{ "hooks": [{ "type": "command", "command": "C:/Users/<kullanıcı>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "timeout": 5 }] }],
    "UserPromptSubmit":  [{ "hooks": [{ "type": "command", "command": "C:/Users/<kullanıcı>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "PostToolUse":       [{ "hooks": [{ "type": "command", "command": "C:/Users/<kullanıcı>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "PermissionRequest": [{ "hooks": [{ "type": "command", "command": "C:/Users/<kullanıcı>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "Notification":      [{ "matcher": "permission_prompt", "hooks": [{ "type": "command", "command": "C:/Users/<kullanıcı>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "Stop":              [{ "hooks": [{ "type": "command", "command": "C:/Users/<kullanıcı>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "async": true }] }],
    "SessionEnd":        [{ "hooks": [{ "type": "command", "command": "C:/Users/<kullanıcı>/AppData/Local/DynamicIsland/island.exe", "args": ["--hook"], "timeout": 5 }] }]
  }
}
```

</details>

## Gizlilik

Bilgisayarınızdan tek bir istek dışında hiçbir şey çıkmaz: plan limitlerinizi göstermek için island, Claude Code'un kaydettiği oturum bilgisini (`~/.claude/.credentials.json`) okuyup Anthropic'ten kullanımınızı sorar. Bilgi yalnızca `api.anthropic.com` adresine HTTPS ile gider, saklanmaz ve log'a yazılmaz; sorgu en fazla 3 dakikada bir ve yalnızca Claude kullanılırken yapılır.

Mikrofon ve kamera bilgisi, görev çubuğundaki mikrofon simgesini de besleyen Windows izin kayıtlarından gelir.

## Kaldırma

Ayarlar → Uygulamalar → Dynamic Island → Kaldır. Uygulama, log'ları, açılışta çalıştırma kaydı ve Claude Code hook'ları silinir; ayar dosyanız kalır.

## Kaynaktan derleme

Rust (stable, MSVC) ve kurulum programı için [Inno Setup 6](https://jrsoftware.org/isinfo.php) gerekir.

```powershell
cargo build --release                      # island.exe
cargo test
powershell -File scripts\package.ps1       # kurulum programı: dist\DynamicIsland-Setup-<sürüm>.exe
powershell -File scripts\make-icon.ps1     # assets\island.ico'yu yeniden çizer
```

## İletişim

nightdijital · [nightdijital.com](https://nightdijital.com) · [iletisim@nightdijital.com](mailto:iletisim@nightdijital.com)

[MIT lisansı](LICENSE) ile yayınlanmıştır.
