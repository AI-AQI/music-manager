# v0.2.5 启动流程验证

实测日期：2026-10-19。环境：Apple Silicon Mac，macOS 27.0.1。

使用当前代码构建独立的 Tauri debug App，产品名为“声场档案启动测试”，identifier 为 `com.musicmanager.startuptest.v025`，从原生窗口操作启动弹窗和目录选择器。正式版 identifier 保持 `com.musicmanager.app`。测试不使用现有正式音乐库或正式配置。

## 原生界面与文件检查

| 场景 | 实测结果 |
| --- | --- |
| 缺少本机 settings.json | 显示“打开已有音乐库 / 创建新音乐库 / 退出”，没有自动进入空库 |
| 选择创建并选定目录，尚未确认 | library.db 和 settings.json 均未生成 |
| 确认创建 | 在选定目录创建音乐库并进入主界面，保存位置 |
| 正常重启 | 读取已保存的音乐库，显示预置旧记录 |
| 退出后实际卸载测试卷，再启动 | 显示“音乐库暂不可用”，提供重试和退出；未重建挂载目录、未生成默认库、位置配置不变 |
| 未连接测试卷时重试 | 继续阻止加载，无创建或替代操作 |
| 重新挂载同一测试卷后重试 | 成功进入原音乐库，旧记录仍在 |
| 缺配置时，误选创建到已有 library.db 的目录并确认 | 提示已有库，拒绝覆盖；可重试返回启动选择 |
| 缺配置时，明确打开已有库 | 正常显示旧记录 |
| settings.json 为损坏 JSON | 停止加载，保留损坏配置，不生成默认库，不显示创建选择 |
| 错误提示中点击退出 | 测试应用退出 |

测试卷通过 `hdiutil create/attach/detach` 创建和实际挂载、卸载，挂载点为 `/Volumes/MMStartupTest`，数据库包含一条可识别的音乐记录。原记录在重连和显式打开后均保留。正常打开会进行应用原有的文件书签同步，因此不以正常使用后整个数据库文件字节完全相同作为标准。

该测试覆盖 macOS 卷消失和重新出现，未进行实体 USB 硬盘插拔，也未在 Intel Mac 上操作原生界面。Intel 构建与后端测试由原生 Intel CI runner 执行。

## 自动化回归

- `node --check frontend/app.js`
- `node --test scripts/test-database-switch.cjs`：15 项通过，覆盖启动对话框分支、取消、重试、阻止提前操作和切换中的异步请求。
- `cd src-tauri && cargo test --locked --offline --lib`：39 项通过，覆盖断开目录后重连、缺失/损坏配置、缺失库、孤立 WAL、防覆盖、设置保存失败等。
- `git diff --check`

发布工作流在两个 Mac 架构分别运行后端测试，并校验完整 App ad-hoc 签名及 DMG 完整性后上传 Release。
