#!/usr/bin/env python3
"""Build a drag-to-Applications DMG from an already verified release binary.

The bundle is intentionally not Developer ID signed: no identity is configured.
Keep the executable unchanged, including its existing linker signature, so the
raw-binary updater can continue replacing it without invalidating a bundle seal.
"""
import argparse
import hashlib
import plistlib
import re
import shutil
import subprocess
import tempfile
from pathlib import Path


def run(*args):
    return subprocess.check_output(args)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--version', required=True)
    parser.add_argument('--triple', choices=['aarch64-apple-darwin', 'x86_64-apple-darwin'], required=True)
    parser.add_argument('--tray', type=Path, required=True)
    parser.add_argument('--out-dir', type=Path, required=True)
    args = parser.parse_args()
    version = args.version.removeprefix('v')
    if not re.fullmatch(r'\d+\.\d+\.\d+', version):
        parser.error('Expected stable semantic version')
    arch = 'arm64' if args.triple.startswith('aarch64') else 'x86_64'
    if run('lipo', '-archs', str(args.tray)).decode().strip() != arch:
        raise ValueError('Binary architecture does not match package')
    output = args.out_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    dmg = output / f'PeerCarry-{version}-macOS-{arch}.dmg'
    if dmg.exists():
        raise FileExistsError(dmg)
    repo = Path(__file__).resolve().parents[2]
    with tempfile.TemporaryDirectory() as temp:
        root = Path(temp)
        content = root / 'image'
        app = content / 'PeerCarry.app'
        macos = app / 'Contents/MacOS'
        resources = app / 'Contents/Resources'
        macos.mkdir(parents=True)
        resources.mkdir()
        executable = macos / 'peercarry-tray'
        shutil.copy2(args.tray, executable)
        executable.chmod(0o755)
        info = {
            'CFBundleIdentifier': 'io.github.juliohuang.peercarry',
            'CFBundleName': 'PeerCarry', 'CFBundleDisplayName': 'PeerCarry',
            'CFBundleExecutable': 'peercarry-tray', 'CFBundlePackageType': 'APPL',
            'CFBundleShortVersionString': version, 'CFBundleVersion': version,
            'CFBundleIconFile': 'PeerCarry', 'LSUIElement': True,
            'LSMinimumSystemVersion': '11.0', 'NSHighResolutionCapable': True,
        }
        with (app / 'Contents/Info.plist').open('wb') as f:
            plistlib.dump(info, f)
        run('plutil', '-lint', str(app / 'Contents/Info.plist'))
        iconset = root / 'PeerCarry.iconset'
        iconset.mkdir()
        original = repo / 'crates/peercarry-tray/assets/tray-icon.png'
        for size in (16, 32, 128, 256, 512):
            for scale in (1, 2):
                suffix = '@2x' if scale == 2 else ''
                run('sips', '-z', str(size * scale), str(size * scale), str(original),
                    '--out', str(iconset / f'icon_{size}x{size}{suffix}.png'))
        run('iconutil', '-c', 'icns', str(iconset), '-o', str(resources / 'PeerCarry.icns'))
        shutil.copy2(repo / 'LICENSE', resources / 'LICENSE')
        (content / 'Applications').symlink_to('/Applications', target_is_directory=True)
        (content / '安装说明.txt').write_text(
            '将 PeerCarry.app 拖到 Applications，然后从应用程序打开。\n'
            '启动后图标在屏幕顶部菜单栏，不显示主窗口。需要时可复制到用户自己的 ~/Applications。\n'
            '请勿直接从只读磁盘映像启动，否则无法自动更新。升级前先退出旧版本。\n'
            '本包尚未取得 Apple Developer ID 签名和公证。若提示无法验证开发者，\n'
            '请确认来自官方发布页，再按 Apple 官方说明在系统设置 > 隐私与安全性中允许打开。\n'
            'https://support.apple.com/102445\n'
            '不要关闭系统安全保护。若提示损坏或仍打不开，请反馈完整错误。\n'
            '剪贴板、文件传输及自动更新包含在桌面版中；命令行/AI Hook 请另下载完整 tar.gz 包。\n'
            '卸载时退出应用并移到废纸篓，配置、历史和下载文件保留。\n\n'
            'Drag PeerCarry.app to Applications, then open the installed app.\n'
            'Look for its menu-bar icon. Do not run directly from the read-only DMG.\n'
            'Not Developer ID signed or notarized. macOS 11 or newer.\n', encoding='utf-8')
        run('hdiutil', 'create', '-volname', 'PeerCarry', '-srcfolder', str(content),
            '-format', 'UDZO', '-ov', str(dmg))
        run('hdiutil', 'verify', str(dmg))
        mount = root / 'mounted'
        mount.mkdir()
        run('hdiutil', 'attach', str(dmg), '-readonly', '-nobrowse', '-mountpoint', str(mount))
        try:
            installed = mount / 'PeerCarry.app/Contents/MacOS/peercarry-tray'
            assert installed.read_bytes() == args.tray.read_bytes(), 'Executable changed during packaging'
            assert installed.stat().st_mode & 0o111
            assert (mount / 'Applications').readlink() == Path('/Applications')
            run('plutil', '-lint', str(mount / 'PeerCarry.app/Contents/Info.plist'))
        finally:
            run('hdiutil', 'detach', str(mount))
    digest = hashlib.sha256(dmg.read_bytes()).hexdigest()
    Path(str(dmg) + '.sha256').write_text(f'{digest}  {dmg.name}\n', encoding='ascii')
    print(f'PASS: native {arch} DMG created, mounted, verified; executable preserved: {dmg.name}')


if __name__ == '__main__':
    main()
