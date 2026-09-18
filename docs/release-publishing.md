# Release publishing

## 使用方式与第一版范围

开发电脑修改代码并验证后，发布 `v0.2.0` 这样的版本标签；GitHub Actions
构建各平台二进制并发布。其他电脑不再需要 Rust、源码或 AI 协助编译。
本次只配置了流水线，尚未创建 GitHub 仓库或上传 OSS。

首次安装：下载对应平台压缩包并解压。Windows 在解压目录运行
`powershell -NoProfile -ExecutionPolicy Bypass -File .\Install.ps1`，安装到
`%LOCALAPPDATA%\peercarry` 并设置登录启动。已有旧版也需要这样安装一次，
才能获得更新入口。Linux/macOS 运行 `sh install.sh` 后按提示配置启动服务。

后续更新：托盘菜单选择“检查更新”或“更新并重启”。设置页可选择
`domestic`、`github` 或 `custom` 更新源，并填写相应地址与可信公钥。
正式发布包内置发布源和公钥；本地开发包未配置这些值时会明确提示。
源不可用时可手动切换，签名失败不会绕过校验。公钥只来自可信配置，
不会从下载服务器自动获取。

更新先验证 Ed25519 签名、SHA-256 和文件大小，再检查是否有传输占用。
有活动请求时此次安装停止，完成传输后重试。更新助手等待旧进程退出，
替换托盘程序并检查新版本健康接口；失败时恢复旧程序并尝试启动。
配置、历史及下载目录不参与替换。安装目录会保留备份和
`.peercarry-update-*` 状态目录，目前没有自动清理策略。

第一版应用内更新只替换托盘程序（包含内嵌服务）。独立 `peercarry` CLI
需要用新版安装包更新。不支持 macOS `.app` 内自更新；macOS/Linux
当前提供普通二进制包，目标机仍需系统桌面依赖。暂无系统代码签名、
macOS 公证、增量更新或无人值守强制升级。

The release workflow runs for tags matching `v*`. It builds the tray and CLI
on the four pinned GitHub-hosted runners below, uploads raw binaries and a
first-install archive, and publishes a signed `latest.json` last.

| Platform | Official runner label | Rust target |
| --- | --- | --- |
| Windows x64 | `windows-2025` | `x86_64-pc-windows-msvc` |
| Linux x64 | `ubuntu-22.04` | `x86_64-unknown-linux-gnu` |
| macOS arm64 | `macos-14` | `aarch64-apple-darwin` |
| macOS Intel x64 | `macos-15-intel` | `x86_64-apple-darwin` |

These labels are from the [GitHub-hosted runner reference](https://docs.github.com/en/actions/reference/workflows-and-actions/workflow-syntax).
The macOS arm64 tray build uses the host's native desktop dependencies.

## Required repository configuration

The tag must be `vMAJOR.MINOR.PATCH`; the workflow strips the `v` and verifies
the resulting version exactly matches `[workspace.package].version` in
`Cargo.toml` before signing.

Set the Actions variable `UPDATE_PUBLIC_KEY` to the lowercase 64-character
hex Ed25519 public key. Store `RELEASE_SIGNING_KEY` as the 64-character hex
Ed25519 seed secret. The workflow derives the public key from the seed and
stops before publishing if it does not equal `UPDATE_PUBLIC_KEY`. The build
jobs pass the public key as `PEERCARRY_UPDATE_PUBLIC_KEY` and the canonical
`PEERCARRY_UPDATE_GITHUB_URL` (`/releases/latest/download/latest.json`) at
compile time. When `DOWNLOAD_BASE_URL` is set, its `/latest.json` URL is also
compiled as `PEERCARRY_UPDATE_DOMESTIC_URL`.

The release job uses the system `GH_TOKEN` supplied by GitHub Actions. No
additional GitHub token is required.

To enable the optional Aliyun mirror, set repository variables
`OSS_BUCKET`, `OSS_ENDPOINT`, `DOWNLOAD_BASE_URL`, and the independent object
prefix `OSS_OBJECT_PREFIX`, plus secrets
`OSS_ACCESS_KEY_ID` and `OSS_ACCESS_KEY_SECRET`. `DOWNLOAD_BASE_URL` is the
public URL prefix (for example `https://download.example.com/peercarry/releases`)
and `OSS_OBJECT_PREFIX` is the exact bucket object prefix (for example
`peercarry/releases`). Files are written under `vVERSION/`, with raw binaries
under `vVERSION/raw/`, and the domestic manifest is copied to both that
version directory and the root `latest.json`. The official
`ossutil` client is downloaded only inside the release job. Every binary and
archive is uploaded first and `latest.json` is uploaded last.

## Manifest protocol

`latest.json` is an envelope:

```json
{"payload":"<base64 raw UTF-8 JSON>","signature":"<base64 Ed25519>"}
```

The signature covers the exact UTF-8 payload bytes. The payload contains
`version`, `notes`, and `artifacts`. Each artifact entry is keyed by Rust
target and points to the raw `peercarry-tray` binary, with its SHA-256 and byte
size. The install archive is deliberately not used as the update artifact.

The GitHub and OSS manifests are signed independently from the same key. Their
artifact URL bases differ, so each manifest is verified against its own public
URL; artifact bytes and SHA-256 values remain identical across both sources.

## Local fixture verification

The scripts use Python 3 and the pinned `cryptography==42.0.8`. With a test
32-byte seed, create a small binary and run:

```powershell
python -m pip install -r scripts/release/requirements.txt
python scripts/release/package.py --version v0.2.0 --triple x86_64-unknown-linux-gnu --platform "Linux x64" --tray .\fixture-tray --cli .\fixture-cli --out-dir .\fixture-dist
$env:RELEASE_SIGNING_KEY = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f"
python scripts/release/manifest.py build --version v0.2.0 --cargo-version Cargo.toml --base-url https://example.invalid/v0.2.0 --triple x86_64-unknown-linux-gnu .\fixture-tray peercarry-tray-x86_64-unknown-linux-gnu --output .\fixture-dist\latest.json
```

Derive the public key from the test seed and run `manifest.py verify`; the
command must print `manifest signature: OK`. Do not use fixture keys or files
in repository configuration.
