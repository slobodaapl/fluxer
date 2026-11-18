#!/usr/bin/env python3
"""Install Native's per-user launcher/icon without changing classic Fluxer."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess


def quoted_exec(path: Path) -> str:
    # Desktop Entry spec quoting, not shell quoting. Percent must be escaped
    # before the launcher performs its field-code expansion.
    value = str(path).replace('%', '%%')
    for char in ('\\', '"', '`', '$'):
        value = value.replace(char, '\\' + char)
    return '"' + value + '"'


def install(binary: Path, data: Path, native_root: Path) -> Path:
    binary = binary.resolve(strict=True)
    if not binary.is_file() or not os.access(binary, os.X_OK):
        raise ValueError('The native executable is missing or not executable')
    template = (native_root / 'packaging/linux/app.fluxer.Native.desktop').read_text()
    contents = template.replace('Exec=fluxer-desktop', 'Exec=' + quoted_exec(binary))
    applications = data / 'applications'
    applications.mkdir(parents=True, exist_ok=True)
    launcher = applications / 'app.fluxer.Native.desktop'
    temporary = launcher.with_suffix('.desktop.tmp')
    temporary.write_text(contents)
    os.replace(temporary, launcher)
    source = native_root.parent / 'fluxer_desktop/build_resources/icons-stable'
    for size in [16, 24, 32, 48, 64, 128, 256, 512]:
        directory = data / f'icons/hicolor/{size}x{size}/apps'
        directory.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source / f'{size}x{size}.png', directory / 'app.fluxer.Native.png')
    return launcher


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    native_root = Path(__file__).resolve().parents[1]
    parser.add_argument('--binary', type=Path, default=native_root / 'target/debug/fluxer-desktop')
    parser.add_argument('--data-home', type=Path, default=Path(os.environ.get('XDG_DATA_HOME', Path.home() / '.local/share')))
    args = parser.parse_args()
    if not args.data_home.is_absolute():
        parser.error('--data-home / XDG_DATA_HOME must be absolute')
    launcher = install(args.binary, args.data_home, native_root)
    update = shutil.which('update-desktop-database')
    if update:
        subprocess.run([update, str(launcher.parent)], check=False)
    print(launcher)


if __name__ == '__main__':
    main()
