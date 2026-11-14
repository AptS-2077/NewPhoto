import argparse
import gzip
import hashlib
import io
import json
import os
import pathlib
import re
import struct
import subprocess
import tarfile
import tempfile
import tomllib
import zipfile

TARGETS = {
    'x86_64-pc-windows-msvc': ('windows-x86_64', '.zip'),
    'x86_64-unknown-linux-gnu': ('linux-x86_64', '.tar.gz'),
    'aarch64-unknown-linux-gnu': ('linux-aarch64', '.tar.gz'),
    'x86_64-apple-darwin': ('macos-x86_64', '.tar.gz'),
    'aarch64-apple-darwin': ('macos-aarch64', '.tar.gz'),
}
WEB_FILES = ['index.html', 'app.js', 'style.css', 'theme.js', 'bootstrap.js', 'pkg/newphoto.js', 'pkg/newphoto_bg.wasm']
SECRET_PATTERN = re.compile(rb'-----BEGIN (?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY-----|\bgh[pousr]_[A-Za-z0-9]{20,}|\bgithub_pat_[A-Za-z0-9_]{30,}|\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}|\b(?:AKIA|ASIA)[A-Z0-9]{16}\b')


def version_from_tag(source, tag):
    if not re.fullmatch(r'v\d+\.\d+\.\d+(?:-[A-Za-z0-9.-]+)?', tag):
        raise ValueError('Invalid release tag')
    version = tomllib.loads((source / 'Cargo.toml').read_text('utf8'))['workspace']['package']['version']
    if tag != 'v' + version:
        raise ValueError('Release tag does not match Cargo version')
    return version


def architecture(binary, target):
    if target.endswith('windows-msvc'):
        offset = struct.unpack_from('<I', binary, 0x3c)[0]
        valid = binary[:2] == b'MZ' and binary[offset:offset + 4] == b'PE\0\0' and struct.unpack_from('<H', binary, offset + 4)[0] == 0x8664
    elif 'linux' in target:
        machine = 183 if target.startswith('aarch64') else 62
        valid = binary[:6] == b'\x7fELF\x02\x01' and struct.unpack_from('<H', binary, 18)[0] == machine
    else:
        machine = 0x100000c if target.startswith('aarch64') else 0x1000007
        valid = binary[:4] == b'\xcf\xfa\xed\xfe' and struct.unpack_from('<I', binary, 4)[0] == machine
    if not valid:
        raise ValueError('Executable architecture does not match ' + target)


def scan(files):
    for name, content in files.items():
        path = pathlib.PurePosixPath(name)
        if path.is_absolute() or '..' in path.parts or '\\' in name or path.name in {'.env', 'master.key'}:
            raise ValueError('Unsafe package path: ' + name)
        if SECRET_PATTERN.search(content):
            raise ValueError('Credential signature detected in ' + name)
        if any(part in {'.git', '.cache', 'data', 'secrets', 'node_modules', 'target'} for part in path.parts):
            raise ValueError('Private or build directory in package: ' + name)


def read_file(path):
    if not path.is_file() or path.is_symlink():
        raise ValueError('Missing file or symbolic link: ' + str(path))
    return path.read_bytes()


def build(source, web, target, tag, output):
    version = version_from_tag(source, tag)
    source_sha = subprocess.check_output(['git', '-C', str(source), 'rev-parse', 'HEAD'], text=True).strip()
    binary_name = 'newphoto.exe' if 'windows' in target else 'newphoto'
    executable = source / 'target' / target / 'release' / binary_name
    executable_bytes = read_file(executable)
    architecture(executable_bytes, target)
    if subprocess.check_output([str(executable), '--version'], text=True).strip() != 'newphoto ' + version:
        raise ValueError('Executable version mismatch')
    subprocess.run([str(executable), '--help'], check=True, stdout=subprocess.DEVNULL)
    with tempfile.TemporaryDirectory(prefix='newphoto-release-check-') as temporary:
        key = pathlib.Path(temporary) / 'master.key'
        subprocess.run([str(executable), 'key', 'generate', '--output', str(key)], check=True, stdout=subprocess.DEVNULL)
        if not key.is_file() or len(key.read_bytes()) != 44:
            raise ValueError('CLI key generation smoke check failed')
    files = {binary_name: executable_bytes}
    for name in WEB_FILES:
        files['web/dist/' + name] = read_file(web / name)
    if not files['web/dist/pkg/newphoto_bg.wasm'].startswith(b'\0asm\x01\0\0\0'):
        raise ValueError('Invalid browser WASM')
    for name in ['LICENSE', 'docs/api.md']:
        files[name] = read_file(source / name)
    files['README.md'] = read_file(pathlib.Path(__file__).with_name('release-readme.md'))
    files['.env.example'] = ('DATABASE_URL=\nNEWPHOTO_MASTER_KEY_FILE=secrets/master.key\nNEWPHOTO_BIND=127.0.0.1:8080\nNEWPHOTO_PUBLIC_ORIGIN=http://127.0.0.1:8080\nNEWPHOTO_COOKIE_SECURE=false\nNEWPHOTO_DATA_DIR=data\nNEWPHOTO_WEB_DIR=web/dist\nNEWPHOTO_ALLOWED_UPSTREAMS=\nNEWPHOTO_RESULT_HOSTS=\nNEWPHOTO_TASK_CONCURRENCY=2\nRUST_LOG=newphoto=info\n').encode()
    manifest = {'version': version, 'tag': tag, 'source_commit': source_sha, 'target': target, 'checks': ['native CLI version', 'native CLI help', 'native CLI key generation', 'executable architecture', 'WASM magic', 'package allowlist and credential signature scan'], 'sha256': {name: hashlib.sha256(content).hexdigest() for name, content in sorted(files.items())}}
    files['BUILD-INFO.json'] = (json.dumps(manifest, ensure_ascii=False, indent=2) + '\n').encode()
    scan(files)
    platform, extension = TARGETS[target]
    stem = 'newphoto-' + version + '-' + platform
    output.mkdir(parents=True, exist_ok=True)
    archive = output / (stem + extension)
    if archive.exists():
        raise ValueError('Archive already exists: ' + str(archive))
    if extension == '.zip':
        with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED, compresslevel=9) as package:
            for name, content in sorted(files.items()):
                entry = zipfile.ZipInfo(stem + '/' + name, (2026, 1, 1, 0, 0, 0))
                entry.compress_type = zipfile.ZIP_DEFLATED
                entry.external_attr = 0o100644 << 16
                package.writestr(entry, content)
    else:
        with archive.open('wb') as raw, gzip.GzipFile(filename='', mode='wb', fileobj=raw, mtime=0) as compressed, tarfile.open(fileobj=compressed, mode='w') as package:
            for name, content in sorted(files.items()):
                entry = tarfile.TarInfo(stem + '/' + name)
                entry.size = len(content)
                entry.mode = 0o755 if name == binary_name else 0o644
                entry.mtime = 0
                package.addfile(entry, io.BytesIO(content))
    verify_archive(archive, target, tag, source_sha)
    print('Created and verified ' + archive.name + ' sha256=' + hashlib.sha256(archive.read_bytes()).hexdigest())


def verify_archive(archive, target, tag, source_sha):
    platform, extension = TARGETS[target]
    prefix = 'newphoto-' + tag.removeprefix('v') + '-' + platform + '/'
    files = {}
    if extension == '.zip':
        with zipfile.ZipFile(archive) as package:
            for entry in package.infolist():
                if entry.is_dir() or entry.file_size > 200 * 1024 * 1024 or entry.external_attr >> 16 & 0o170000 == 0o120000:
                    raise ValueError('Unsafe ZIP member')
                if not entry.filename.startswith(prefix) or entry.filename in files:
                    raise ValueError('Unexpected or duplicate ZIP member')
                files[entry.filename] = package.read(entry)
    else:
        with tarfile.open(archive, 'r:gz') as package:
            for entry in package:
                if not entry.isfile() or entry.size > 200 * 1024 * 1024 or not entry.name.startswith(prefix) or entry.name in files:
                    raise ValueError('Unsafe tar member')
                files[entry.name] = package.extractfile(entry).read()
                if entry.name == prefix + 'newphoto' and entry.mode & 0o111 != 0o111:
                    raise ValueError('Executable permission missing')
    files = {name.removeprefix(prefix): content for name, content in files.items()}
    binary_name = 'newphoto.exe' if 'windows' in target else 'newphoto'
    expected = {binary_name, 'LICENSE', 'docs/api.md', 'README.md', '.env.example', 'BUILD-INFO.json'} | {'web/dist/' + name for name in WEB_FILES}
    if set(files) != expected:
        raise ValueError('Package file allowlist mismatch')
    scan(files)
    architecture(files[binary_name], target)
    manifest = json.loads(files.pop('BUILD-INFO.json'))
    if manifest['target'] != target or manifest['tag'] != tag or manifest['version'] != tag.removeprefix('v') or manifest['source_commit'] != source_sha:
        raise ValueError('Package provenance mismatch')
    if manifest['sha256'] != {name: hashlib.sha256(content).hexdigest() for name, content in sorted(files.items())}:
        raise ValueError('Package content hash mismatch')
    return manifest


def main():
    parser = argparse.ArgumentParser()
    subcommands = parser.add_subparsers(dest='command', required=True)
    validate = subcommands.add_parser('validate')
    validate.add_argument('--source', type=pathlib.Path, required=True)
    validate.add_argument('--tag', required=True)
    package = subcommands.add_parser('build')
    package.add_argument('--source', type=pathlib.Path, required=True)
    package.add_argument('--web', type=pathlib.Path, required=True)
    package.add_argument('--tag', required=True)
    package.add_argument('--target', choices=TARGETS, required=True)
    package.add_argument('--output', type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    source = arguments.source.resolve()
    if arguments.command == 'validate':
        print('Validated source version: ' + version_from_tag(source, arguments.tag))
    else:
        build(source, arguments.web.resolve(), arguments.target, arguments.tag, arguments.output.resolve())


if __name__ == '__main__':
    main()
