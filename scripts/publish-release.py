import argparse
import hashlib
import importlib.util
import json
import os
import pathlib
import urllib.error
import urllib.parse
import urllib.request

specification = importlib.util.spec_from_file_location('release_package', pathlib.Path(__file__).with_name('package-release.py'))
package = importlib.util.module_from_spec(specification)
specification.loader.exec_module(package)


def request(method, url, token, payload=None, binary=False, allow_missing=False):
    if urllib.parse.urlsplit(url).hostname not in {'api.github.com', 'uploads.github.com'}:
        raise ValueError('Unexpected GitHub API host')
    headers = {'Authorization': 'Bearer ' + token, 'Accept': 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28', 'User-Agent': 'NewPhoto-platform-release'}
    body = None
    if payload is not None:
        body = payload if binary else json.dumps(payload, ensure_ascii=False).encode()
        headers['Content-Type'] = 'application/octet-stream' if binary else 'application/json'
    opener = urllib.request.build_opener(NoRedirect())
    try:
        with opener.open(urllib.request.Request(url, data=body, headers=headers, method=method), timeout=180) as response:
            return json.load(response)
    except urllib.error.HTTPError as error:
        if allow_missing and error.code == 404:
            return None
        raise RuntimeError('GitHub API returned HTTP ' + str(error.code) + ' for ' + method) from None


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, request, response, code, message, headers, new_url):
        return None


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--directory', type=pathlib.Path, required=True)
    arguments = parser.parse_args()
    token = os.environ['GITHUB_TOKEN']
    repository = os.environ['GITHUB_REPOSITORY']
    tag = os.environ['RELEASE_TAG']
    source_sha = os.environ['SOURCE_SHA']
    if repository != 'AptS-2077/NewPhoto':
        raise ValueError('Unexpected publication repository')
    archives = []
    for target, (platform, extension) in package.TARGETS.items():
        archive = arguments.directory / ('newphoto-' + tag.removeprefix('v') + '-' + platform + extension)
        package.verify_archive(archive, target, tag, source_sha)
        archives.append(archive)
    if {path.name for path in arguments.directory.iterdir()} != {path.name for path in archives}:
        raise ValueError('Unexpected release artifact')
    checksums = ''.join(hashlib.sha256(path.read_bytes()).hexdigest() + '  ' + path.name + '\n' for path in sorted(archives))
    checksum_path = arguments.directory / 'SHA256SUMS'
    checksum_path.write_text(checksums, encoding='utf8', newline='\n')
    archives.append(checksum_path)
    base = 'https://api.github.com/repos/' + repository
    reference = request('GET', base + '/git/ref/tags/' + urllib.parse.quote(tag, safe=''), token)
    if reference['object']['sha'] != source_sha:
        raise ValueError('Source tag moved during build')
    release = request('GET', base + '/releases/tags/' + urllib.parse.quote(tag, safe=''), token, allow_missing=True)
    notes = ('NewPhoto ' + tag + ' 多平台预编译测试版。\n\n'
             '包含 Windows x64、Linux x64/ARM64、macOS Intel/Apple Silicon 五种包；每包包含服务程序、编译后的网页、空配置模板及启动说明。需自行准备 PostgreSQL、主密钥和上游模型配置。\n\n'
             '每个平台已执行原生 Rust 单元测试和 CLI 版本、帮助、密钥生成检查。包已按白名单扫描，不包含用户数据、真实凭据、数据库、日志或本地验收资料。\n\n'
             'Linux x64 要求 glibc 2.35+，Linux ARM64 要求 glibc 2.39+；macOS 建议 14+。Windows/macOS 尚未代码签名或公证。真实供应商出图及生产 HTTPS 部署仍需在目标环境验证。\n\n'
             '下载后先核对 SHA256SUMS，再阅读包内 README.md。原始源码提交：`' + source_sha + '`。')
    if release is None:
        release = request('POST', base + '/releases', token, {'tag_name': tag, 'target_commitish': source_sha, 'name': 'NewPhoto ' + tag, 'body': notes, 'draft': True, 'prerelease': True})
    if not release['draft']:
        raise ValueError('Release already published; refusing to replace assets')
    upload = release['upload_url'].split('{', 1)[0]
    for archive in archives:
        digest = 'sha256:' + hashlib.sha256(archive.read_bytes()).hexdigest()
        existing = next((asset for asset in release['assets'] if asset['name'] == archive.name), None)
        if existing:
            if existing.get('digest') != digest or existing['size'] != archive.stat().st_size:
                raise ValueError('Existing draft asset differs: ' + archive.name)
            continue
        asset = request('POST', upload + '?' + urllib.parse.urlencode({'name': archive.name}), token, archive.read_bytes(), binary=True)
        if asset['state'] != 'uploaded' or asset['size'] != archive.stat().st_size or asset.get('digest') != digest:
            raise ValueError('Uploaded asset verification failed: ' + archive.name)
    release = request('GET', base + '/releases/' + str(release['id']), token)
    if {asset['name'] for asset in release['assets']} != {archive.name for archive in archives}:
        raise ValueError('Release asset list mismatch')
    release = request('PATCH', base + '/releases/' + str(release['id']), token, {'draft': False, 'prerelease': True, 'body': notes})
    print('Published ' + release['html_url'])


if __name__ == '__main__':
    main()
