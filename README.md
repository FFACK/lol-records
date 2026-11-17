# 国服战绩查询 · LoL Records

**Windows x64 单文件免安装工具**，查询你在英雄联盟国服的段位、近期战绩和单局详情。

[查看 Gitee 发行版](https://gitee.com/zeng-yehong/lol-records/releases) · [反馈问题](https://gitee.com/zeng-yehong/lol-records/issues)

## 产品功能

- **账号与段位**。查看当前登录账号、召唤师等级、单双排和灵活排位段位。
- **近期战绩**。本次国服 LCU 实测返回最近 20 场，工具会在后续同步时合并本地历史。
- **单局详情**。查看双方击杀、死亡、助攻、补刀和装备。英雄和装备使用中文名称。战绩列表的英雄头像按需从 Riot Data Dragon 下载并缓存在本机，断网时仍可查看战绩和已下载的头像。
- **离线查看**。打开过的单局详情保存在本机，可以离线查看。查看未缓存的详情时，工具通过当前连接向游戏客户端发起查询。

## 下载与使用

1. 从 [Gitee 发行版](https://gitee.com/zeng-yehong/lol-records/releases) 的附件下载 `lol-records.exe`。若发行版列表为空，暂未提供公开下载。
2. 登录国服英雄联盟大厅，双击 EXE，点击「连接并同步」。
3. 若 Windows 弹出 UAC 提示，确认后即可同步。本次打开工具期间，刷新和查看单局详情复用该连接。关闭工具或连接断开后，再连接可能需要重新确认。取消后仍可查看已缓存的战绩。

## 更新路径

- **应用内更新**。启动时在后台检查 Gitee 正式发行版。出现「更新到」按钮后，点击下载，工具会验签、替换并重启。程序所在目录需要可写。
- **手动更新**。从 Gitee 发行版下载新版 EXE，关闭工具后替换旧文件。缓存保存在 `%LOCALAPPDATA%\LolRecords\cache.json`，替换 EXE 会保留历史。

## 构建

从仓库根目录执行 `cargo build --release --locked --target x86_64-pc-windows-msvc`。需要 Rust 和 MSVC Windows 构建工具。运行时使用 Windows 自带的 PowerShell 查询客户端进程，用户无需安装 Rust 或浏览器组件。

## Gitee 发版

1. 递增 `Cargo.toml` 版本，同步 `Cargo.lock`，打对应的 `vX.Y.Z` 标签。
2. 从项目目录运行 `scripts\release.ps1`。签名需要 OpenSSL 支持 Ed25519，私钥由发版者保管。默认从 `%LOCALAPPDATA%\LolRecordsSigning\release-ed25519.pem` 读取。
3. 把 `dist\lol-records.exe` 和 `dist\lol-records.exe.sig` 一起上传到公开 Gitee 正式发行版。两份附件都能匿名完整下载后再发布。
4. 用上一版启动验证提示、下载、验签、替换和重启。EXE 需要放在当前用户可写目录。更新过程使用普通权限，旧版备份会保留到新版窗口完成初始化。

更新仓库和公开验签密钥在 `.cargo/config.toml`。私钥不进入仓库。签名覆盖版本、平台和 EXE 完整内容，与 Windows Authenticode 代码签名独立。独立静态介绍页的源码在 [docs/](docs/)，更新程序直接访问发行版 API。

本项目使用 Riot 未为第三方正式支持的本机 LCU 接口，与 Riot、腾讯和 WeGame 无隶属关系。
