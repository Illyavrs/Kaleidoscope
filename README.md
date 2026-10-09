# Kaleidoscope

Illyaverse 的原創 Rust 連線助手。Rust 核心、程序／清理 tests、scripts 與 `ui/` 的 Tauri／Next.js 視窗集中在同一個 Kaleidoscope 產品 repository。stable 使用 `-devserver`，lazer 沿用已安裝的客戶端和現有圖譜庫，在啟動前載入原創 Transfiguration 外掛。設定可保存 Illyaverse 部署的 HTTPS 位址，每次啟動固定使用當次保存的位址。

協調開發從完整 Illyaverse parent checkout 執行，使用根 Cargo／npm workspace 與 lockfile；介面是 `tools/kaleidoscope/ui`，Tauri manifest 是 `ui/src-tauri/Cargo.toml`，透過 `../..` path dependency 使用本目錄的核心。bridge、部署與整合文件由 parent 的其他子專案提供；單獨 clone 本產品不含完整整合環境。Git 子模組登記狀態以 parent 的 `.gitmodules` 為準。

操作與退出清理見[啟動器指南](../../docs/kaleidoscope-launcher.md)。公開 DNS／HTTPS 環境不需要 hosts。現在的本機 `illya.test` 測試仍需開發 DNS／CA，真實原生遊玩尚待驗證。

Windows storage resolver 從 `%APPDATA%\osu` 或含 `framework.ini` 的 portable
安裝目錄開始，再依 `storage.ini` 的 `FullPath` 找出既有有效路徑。直接沿用
`client.realm`／`files`，不建立空白替代庫、不複製或匯入、不建立 junction。
Transfiguration 沿用原本的 `game.ini`，共享 gameplay／UI／audio／input 設定。
只有帳號、session 和記住登入狀態使用同一資料目錄的 `auth.illyaverse.json`，
按每個 canonical endpoint 分開保存。官方帳號欄位在 native 設定載入前過濾，
不會傳入私服 API；保存共享設定時保留官方七個帳號欄位的原始內容。
舊 `game.illyaverse.ini` 不匯入、不複製或遷移。私服執行不切換已確認的圖譜位置。
`%LOCALAPPDATA%\Illyaverse\Kaleidoscope` 保存控制、ready 和 session v3 復原紀錄，
session 記錄原本 library 的路徑及目錄身分，並按已安裝 DLL 的雜湊／檔案身分清理。
舊 session v1 只用於清理原 `%LOCALAPPDATA%\Illyaverse\lazer` 的自有外掛；此路徑
不再是玩家圖譜庫。隔離 fixture 已驗證 stock config 的共享設定載入／保存、
官方帳號欄位保留及 native remember／logout handlers；API worker 在入口截停，
未啟動 GameHost 或連線。原生圖譜、私服登入、GUI 和退出清理仍需實測。

連線設定預設 `https://illya.test`，接受 HTTPS origin；不能包含登入資訊、路徑、
query 或 fragment，也不能選擇官方 `ppy.sh` 網域。只接受 ASCII 多段 DNS 名稱，
不接受 IP、單段主機名稱或尾端句點。lazer 支援自訂 HTTPS port，stable 只支援
443。設定變更用於下次啟動；session v3 保存當次位址，執行期間不會切換。
ready、進度與失敗訊號均按當次 session 位址驗證，TLS 驗證和拒絕官方 fallback
仍然生效。舊 session v1／v2 只沿用當時的 `https://illya.test`。

rulesets 鎖使用 `.kaleidoscope.lock`，同時取得舊 `.illyaverse-launcher.lock`，
確保新舊啟動器不能同時安裝。兩個 lock inode 均保留，不以刪除舊檔進行遷移。

以下 CLI hosts／profile 命令保留給開發者部署規劃，不是玩家 launcher 的伺服器設定。

```powershell
cargo run -p kaleidoscope -- --help
cargo run -p kaleidoscope -- profile --client stable --domain illya.example.com
cargo run -p kaleidoscope -- profile --client lazer --domain illya.example.com
```

`inspect`、`plan`、`apply` 與 `restore` 全部預設只讀／預覽。只有 `apply --write` 或 `restore --write` 會修改指定 hosts 檔。每次修改先產生原始位元組備份，並驗證結果。這些命令不啟動遊戲、不安裝憑證、不修改 DNS 快取。`launcher status/settings/set-endpoint/set-client/start/stop/recover` 使用同一個連線與 session 核心。

完整操作方式、TLS 要求與 lazer 相容性限制見 [連線文件](../../docs/connection.md)。

## 本機測試 package

目前新增一種明確標示為 `cli_connection_test` 的暫時命令列測試包。
它使用普通 debug CLI build 與 production bridge，沒有 Tauri 視窗；完整 GUI
release 套件仍需完成。這兩種套件的 verifier 與 metadata 分開，不能互相當成
完成證據。從測試包目錄的 PowerShell 執行一個指令即可選擇並啟動已安裝的 lazer：

```powershell
.\Kaleidoscope.exe launcher start lazer "$env:LOCALAPPDATA\osulazer\current\osu!.exe"
```

CLI 也可讀取或保存下次啟動位址：

```powershell
.\Kaleidoscope.exe launcher settings
.\Kaleidoscope.exe launcher set-endpoint "https://your-illyaverse.example"
```

自訂安裝可替換最後的 `osu!.exe` 路徑。支援的是 compatibility manifest 內精確
hash 的 lazer `2026.1005.1`；未驗證版本會拒絕啟動。沿用原本圖譜庫，使用網站
帳號登入私服。先前 `illya.test` 的本機 DNS／CA 設定仍需完成。關閉 lazer 後
guardian 會清理自有 DLL；中斷後可執行 `.\Kaleidoscope.exe launcher recover`。
遊戲／guardian 執行期間不要移動或刪除這個測試包。

以下建置指令只讀取既有 production bridge，不開啟玩家遊戲、登入、圖譜庫、
hosts 或 CA。`--inspect-package-bindings` 只讀取 EXE 所在套件中的 DLL，確認
編譯時固定的 bridge／stock assembly hashes、預設位址與可設定 endpoint 政策，
以及共享 library／設定和帳號隔離政策，不執行遊戲或背景 worker。

```powershell
pwsh -NoProfile -File tools/kaleidoscope/scripts/package-cli-test.ps1
pwsh -NoProfile -File tools/kaleidoscope/scripts/verify-cli-test-package.ps1 -Directory <cli-package-directory>
pwsh -NoProfile -File tools/kaleidoscope/scripts/verify-cli-test-package.ps1 -ArchivePath <cli-package.zip>
pwsh -NoProfile -File tools/kaleidoscope/tests/cli-package-integrity.ps1 -PackageDirectory <cli-package-directory>
```

CLI 預設輸出到新的 `.artifacts/packages/kaleidoscope-cli-test-<時間>-<nonce>/`
和同名 ZIP。manifest 明確保留 `native_gui_verified:false` 與
`native_login_verified:false`；package tests 不能替代原生登入／送分／退出清理實測。

`pwsh -File tools/kaleidoscope/scripts/package-launcher.ps1` 預設 `-BuildProfile Release`，會驗證所選官方安裝、
建置正式 bridge／介面／原生啟動器，再執行 `--verify-embedded-interface`。
Rust 使用現有 repository `target/`，停用 incremental，完成後還原 process
environment。這個指令會執行原生啟動器的無視窗介面檢查，不啟動遊戲。

`-BuildProfile Debug` 使用同一個現有 `target/debug/` 建置 GUI 測試包，仍啟用
`custom-protocol` 並驗證內嵌正式介面。manifest 和 README 明確標示 debug，
保留同樣的檔案／hash／介面檢查；這項建置選項本身不代表原生 GUI 已通過實測。

預設輸出是新的 `data-kaleidoscope-package-<時間>-<nonce>/` 與同名 `.zip`。
`-OutputDirectory` 必須在 repository 內、父目錄已存在，目錄和 ZIP 都必須尚未
存在。必要 payload 先複製到唯一 sibling staging 目錄；所有內容完成後最後才
寫入 `package.json`，驗證組裝目錄與 ZIP 一致後才移至指定輸出。既有 artifacts
不會覆寫或刪除；失敗的本次 staging 可保留供檢查，不能當成完成 package。

完成 package 包含 `Kaleidoscope.exe`、正式 bridge、`artifact.json`、
`compatibility.json`、`LICENSE`、bridge 的 `THIRD-PARTY-NOTICES.md`、五份 bridge `licenses/`
授權文字、`README.txt` 與 `Transfiguration-README.md`。`package.json` 記錄每份
payload 的 SHA256、預設 endpoint 和可設定 HTTPS origin 政策、build profile、`fixture:false`、完成狀態及介面檢查
類型。verifier 拒絕缺檔、額外檔案、reparse points、hash 不符、不完整／重複
JSON fields、fixture bridge、不符合端點政策及無效相容性 metadata；ZIP 不需解壓
即可檢查每個 entry 與內容 hashes。

GUI 套件另包含 `Kaleidoscope-THIRD-PARTY-NOTICES.md` 和 `licenses/Nunito.txt`、
`licenses/NotoSansTC.txt`，保存內嵌字型的來源、copyright 和 OFL 授權全文。
這三份 GUI payload 也列入 SHA256 與 directory／ZIP 檢查；CLI 套件不含字型。

```powershell
pwsh -File tools/kaleidoscope/scripts/verify-launcher-package.ps1 -Directory <package-directory>
pwsh -File tools/kaleidoscope/scripts/verify-launcher-package.ps1 -ArchivePath <package.zip>
pwsh -NoProfile -File tools/kaleidoscope/tests/package-integrity.ps1
```

最後一個指令只在自己的 identity-marked fixture 目錄使用文字 EXE／DLL bytes，
不執行任何 EXE／DLL。它只證明 package 組裝及拒絕條件，metadata 明確為
`synthetic_package_integrity`、`embedded_interface_verified:false`；production
verifier 預設拒絕這類 fixture。package hashes 及 packager 所記錄的介面檢查
不能證明實際遊戲登入或原生 UI。這是需要本機 DNS／CA 的 `illya.test` 測試
package，公開下載尚未承諾，原生 GUI／登入／遊玩仍待使用者實機驗證。
