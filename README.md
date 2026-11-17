# 国服战绩查询

Windows x64 便携工具。双击 `lol-records.exe`，登录国服英雄联盟大厅后点击「连接并同步」。程序先以普通权限尝试连接，系统拒绝读取客户端连接参数时才请求 UAC。授权仅用于当前的本机只读查询。取消授权后仍可查看已缓存的战绩。

这里显示当前账号的段位、近期对局和单局详情。目标机器两条 LCU 战绩接口均实测返回最近 20 场，翻页重复返回同一批。工具会在每次同步时合并历史，缓存位于 `%LOCALAPPDATA%\LolRecords\cache.json`。英雄和装备当前显示 ID。此前未加载过的单局详情需要客户端在线，并可能单独请求 UAC。

## 构建

从仓库根目录执行 `cargo build --release --locked --target x86_64-pc-windows-msvc`。需要 Rust 和 MSVC Windows 构建工具。运行时使用 Windows 自带的 PowerShell 查询客户端进程，用户无需安装 Rust 或浏览器组件。

## Gitee 发版

1. 递增 `Cargo.toml` 版本，同步 `Cargo.lock`，打对应的 `vX.Y.Z` 标签。
2. 从项目目录运行 `scripts\release.ps1`。签名需要 OpenSSL 支持 Ed25519，私钥由发版者保管。默认从 `%LOCALAPPDATA%\LolRecordsSigning\release-ed25519.pem` 读取。
3. 把 `dist\lol-records.exe` 和 `dist\lol-records.exe.sig` 一起上传到公开 Gitee 正式发行版。两份附件都能匿名完整下载后再发布。
4. 用上一版启动验证提示、下载、验签、替换和重启。EXE 需要放在当前用户可写目录。更新过程使用普通权限，旧版备份会保留到新版窗口完成初始化。

更新仓库和公开验签密钥在 `.cargo/config.toml`。私钥不进入仓库。签名覆盖版本、平台和 EXE 完整内容，与 Windows Authenticode 代码签名独立。Gitee 的项目首页可以另做下载介绍，更新程序直接访问发行版 API。

本项目使用 Riot 未为第三方正式支持的本机 LCU 接口，与 Riot、腾讯和 WeGame 无隶属关系。
