import concurrent.futures
import copy
import http.cookiejar
import io
import json
import os
import pathlib
import secrets
import subprocess
import time
import urllib.error
import urllib.request
import uuid
import zipfile

ROOT = pathlib.Path(__file__).resolve().parent.parent
BASE = 'http://127.0.0.1:8080'
MOCK = 'http://127.0.0.1:19091'
RESULTS = []
RUN = time.strftime('%m%d%H%M%S')
PASSWORD = 'NewPhoto-Test-' + secrets.token_hex(12)


class Client:
    def __init__(self):
        self.jar = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar))
        self.csrf = ''
        self.user = None

    def request(self, method, path, body=None, headers=None, raw=False):
        actual_headers = {'Origin': BASE, 'X-CSRF-Token': self.csrf}
        if body is not None and not isinstance(body, bytes):
            body = json.dumps(body, ensure_ascii=False).encode()
            actual_headers['Content-Type'] = 'application/json'
        actual_headers.update(headers or {})
        request = urllib.request.Request(BASE + '/api/v1' + path, data=body, method=method, headers=actual_headers)
        try:
            response = self.opener.open(request, timeout=40)
        except urllib.error.HTTPError as error:
            response = error
        content = response.read()
        if raw:
            return response.status, content, response.headers
        try:
            parsed = json.loads(content)
        except ValueError:
            raise AssertionError(f'{path}: response is not JSON (HTTP {response.status})')
        return response.status, parsed

    def ok(self, method, path, body=None, headers=None, expected=200):
        status, result = self.request(method, path, body, headers)
        assert status in expected if isinstance(expected, tuple) else status == expected, (method, path, status, result)
        return result

    def login(self, username, password):
        result = self.ok('POST', '/auth/login', {'username': username, 'password': password})
        self.csrf = result['csrf_token']
        self.user = result['user']
        return self

    def upload(self, path, field, filename, content, extra=None, expected=201):
        boundary = 'NewPhoto' + uuid.uuid4().hex
        parts = []
        for key, value in (extra or {}).items():
            parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="{key}"\r\n\r\n{value}\r\n'.encode())
        parts.append(f'--{boundary}\r\nContent-Disposition: form-data; name="{field}"; filename="{filename}"\r\nContent-Type: application/octet-stream\r\n\r\n'.encode() + content + b'\r\n')
        parts.append(f'--{boundary}--\r\n'.encode())
        return self.ok('POST', path, b''.join(parts), {'Content-Type': f'multipart/form-data; boundary={boundary}'}, expected)


def check(name, operation):
    started = time.monotonic()
    try:
        operation()
        RESULTS.append({'name': name, 'result': 'passed', 'seconds': round(time.monotonic() - started, 3)})
        print('PASS', name, flush=True)
    except Exception as error:
        RESULTS.append({'name': name, 'result': 'failed', 'error': str(error), 'seconds': round(time.monotonic() - started, 3)})
        print('FAIL', name, str(error), flush=True)


def expect(condition, message='assertion failed'):
    assert condition, message


def wait_task(client, task_id, statuses=None, timeout=35):
    statuses = statuses or {'succeeded', 'failed', 'submission_unknown', 'result_unavailable'}
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        task = client.ok('GET', '/tasks/' + task_id)['task']
        if task['status'] in statuses:
            return task
        time.sleep(.2)
    raise AssertionError(f'task {task_id} did not reach {statuses}: {task["status"]}')


def metrics():
    return json.load(urllib.request.urlopen(MOCK + '/metrics', timeout=5))


def make_model(admin, name, upstream='mock-ok', entry='openai.images.generate', base=MOCK + '/v1/'):
    result = admin.ok('POST', '/admin/models', {'display_name': name + '-' + RUN, 'entry_format_id': entry, 'upstream_model': upstream, 'api_base_url': base, 'credential_action': 'replace', 'credential': 'non-sensitive-mock-key', 'capabilities': {'sizes': ['1024x1024'], 'input_mimes': ['image/png', 'image/jpeg', 'image/webp'], 'max_input_bytes': 26214400, 'max_pixels': 40000000}, 'defaults': {}, 'enabled': True})
    return admin.ok('GET', '/admin/models/' + result['id'])


def task_body(admin, model, **overrides):
    revision = admin.ok('GET', '/entry-formats')['registry_revision']
    body = {'entry_format_id': model['entry_format_id'], 'model_config_id': model['id'], 'model_revision': model['revision'], 'registry_revision': revision, 'prompt': '验收样例：清晨的绿色山丘，柔和光线', 'input_asset_ids': [], 'output': {'size': '1024x1024', 'count': 1}}
    body.update(overrides)
    return body


def submit(client, body, key=None):
    return client.ok('POST', '/tasks', body, {'Idempotency-Key': key or str(uuid.uuid4())}, 202)['task_id']


def user(admin, suffix, permissions=None, model_ids=None, role='user'):
    username = 'test-' + suffix + '-' + RUN
    result = admin.ok('POST', '/admin/users', {'username': username, 'display_name': '验收 ' + suffix, 'password': PASSWORD, 'role': role, 'permissions': permissions if permissions is not None else ['image.generate', 'task.read_own', 'asset.download_own'], 'model_access': 'selected', 'model_ids': model_ids or []})
    return Client().login(username, PASSWORD), admin.ok('GET', '/admin/users/' + result['id'])


def package_variant(version='1.0.0', plugin_id=None, **changes):
    manifest = json.loads((ROOT / 'examples/simple-image/manifest.json').read_text('utf8'))
    definition = json.loads((ROOT / 'examples/simple-image/entries/generate.json').read_text('utf8'))
    manifest['version'] = version
    manifest['plugin_id'] = plugin_id or 'example.acceptance-' + RUN
    manifest['entrypoints'][0]['entry_format_id'] = manifest['plugin_id'] + '.generate'
    manifest.update(changes)
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
        archive.writestr('manifest.json', json.dumps(manifest, ensure_ascii=False))
        archive.writestr('entries/generate.json', json.dumps(definition))
    return output.getvalue()


def restart_server():
    pid_path = ROOT / '.cache/server-pid'
    pid = int(pid_path.read_text().strip())
    if os.name == 'nt':
        subprocess.run(['taskkill', '/PID', str(pid), '/F'], check=True, capture_output=True)
    else:
        os.kill(pid, 15)
    time.sleep(.8)
    environment = os.environ.copy()
    environment.update({'DATABASE_URL': (ROOT / '.cache/database-url').read_text().strip(), 'NEWPHOTO_COOKIE_SECURE': 'false', 'NEWPHOTO_PUBLIC_ORIGIN': BASE, 'NEWPHOTO_ALLOWED_UPSTREAMS': '127.0.0.1:19091,127.0.0.1:19092'})
    executable = ROOT / 'target/debug' / ('newphoto.exe' if os.name == 'nt' else 'newphoto')
    with (ROOT / '.cache/server.stdout.log').open('ab') as stdout, (ROOT / '.cache/server.stderr.log').open('ab') as stderr:
        process = subprocess.Popen([str(executable), 'serve'], cwd=ROOT, env=environment, stdout=stdout, stderr=stderr, creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
    pid_path.write_text(str(process.pid))
    for _ in range(100):
        try:
            if urllib.request.urlopen(BASE + '/health/ready', timeout=1).status == 200:
                return
        except (OSError, urllib.error.URLError):
            pass
        time.sleep(.1)
    raise AssertionError('server did not restart')


def main():
    admin_password = os.environ.get('NEWPHOTO_TEST_ADMIN_PASSWORD')
    if not admin_password:
        raise SystemExit('Set NEWPHOTO_TEST_ADMIN_PASSWORD to the local test administrator password')
    admin = Client().login('admin', admin_password)
    outsider = Client()
    check('A01 real server session and auth/me', lambda: expect(admin.ok('GET', '/auth/me')['user']['role'] == 'admin'))
    check('A03 unauthenticated API 401', lambda: expect(outsider.request('GET', '/tasks')[0] == 401))
    check('CSRF and Origin enforced', lambda: expect(admin.request('PATCH', '/me', {'display_name': 'blocked'}, {'X-CSRF-Token': '', 'Origin': 'https://evil.invalid'})[0] == 403))
    check('G01 built-in formats exist', lambda: expect({'openai.images.generate', 'openai.images.edit'}.issubset({item['id'] for item in admin.ok('GET', '/entry-formats')['items']})))
    generate = make_model(admin, 'Mock 生成')
    edit = make_model(admin, 'Mock 编辑', entry='openai.images.edit')
    alternate = make_model(admin, 'Mock 未授权模型')
    alice, alice_info = user(admin, 'alice', model_ids=[generate['id'], edit['id']])
    bob, bob_info = user(admin, 'bob', model_ids=[generate['id']])
    denied, denied_info = user(admin, 'denied', permissions=[], model_ids=[])
    viewer, viewer_info = user(admin, 'viewer', permissions=['task.read_own'], model_ids=[generate['id']])
    check('A06 ordinary users cannot request admin API', lambda: expect(all(alice.request('GET', path)[0] == 403 for path in ['/admin/users', '/admin/models', '/admin/plugins'])))
    check('A12 PATCH /me rejects role and permissions', lambda: expect(alice.request('PATCH', '/me', {'role': 'admin', 'permissions': [], 'revision': 1})[0] == 422))
    body = task_body(admin, generate)
    check('A07 generation permission server check', lambda: expect(denied.request('POST', '/tasks', body, {'Idempotency-Key': str(uuid.uuid4())})[0] == 403))
    check('A09 model authorization server check', lambda: expect(alice.request('POST', '/tasks', task_body(admin, alternate), {'Idempotency-Key': str(uuid.uuid4())})[0] == 403))
    check('permission dependency validation', lambda: expect(admin.request('PATCH', '/admin/users/' + alice_info['id'], {'revision': alice_info['revision'], 'permissions': ['image.generate']})[0] == 422))
    key = str(uuid.uuid4())
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as executor:
        ids = list(executor.map(lambda _: submit(alice, body, key), range(2)))
    check('G08 concurrent identical requests share one task', lambda: expect(ids[0] == ids[1]))
    check('G08 same key different request returns 409', lambda: expect(alice.request('POST', '/tasks', {**body, 'prompt': 'different'}, {'Idempotency-Key': key})[0] == 409))
    generated = wait_task(alice, ids[0])
    check('G02 MOCK generation persists actual image bytes', lambda: expect(generated['status'] == 'succeeded' and len(generated['assets']) == 1, generated))
    asset_id = generated['assets'][0]['id']
    check('G13 authenticated PNG download with disposition', lambda: expect((lambda response: response[0] == 200 and response[1].startswith(bytes([137, 80, 78, 71])) and response[2].get('Content-Disposition', '').startswith('attachment;'))(alice.request('GET', '/assets/' + asset_id + '/download', raw=True))))
    check('A08 other user task and result returns 404', lambda: expect(bob.request('GET', '/tasks/' + ids[0])[0] == 404 and bob.request('GET', '/assets/' + asset_id + '/preview', raw=True)[0] == 404 and bob.request('GET', '/assets/' + asset_id + '/download', raw=True)[0] == 404))
    png = urllib.request.urlopen(MOCK + '/fixture.png').read()
    (ROOT / '.cache/input.png').write_bytes(png)
    source = alice.upload('/assets/inputs', 'image', 'source.png', png)
    check('A08 other user input returns 404', lambda: expect(bob.request('GET', '/assets/' + source['id'] + '/preview', raw=True)[0] == 404))
    edit_body = task_body(admin, edit, input_asset_ids=[source['id']])
    before_edits = metrics()['edits']
    edited = wait_task(alice, submit(alice, edit_body))
    check('G03 MOCK editing sends real PNG multipart bytes', lambda: expect(edited['status'] == 'succeeded' and metrics()['edits'] == before_edits + 1))
    check('G04 edit requires image and correct model binding', lambda: expect(alice.request('POST', '/tasks', task_body(admin, edit), {'Idempotency-Key': str(uuid.uuid4())})[0] == 422 and alice.request('POST', '/tasks', {**edit_body, 'entry_format_id': 'openai.images.generate'}, {'Idempotency-Key': str(uuid.uuid4())})[0] == 422))
    check('G12 forged image upload rejected', lambda: expect(alice.request('POST', '/assets/inputs', b'not multipart', {'Content-Type': 'multipart/form-data; boundary=x'})[0] == 422))
    check('G12 invalid size and count rejected', lambda: expect(all(alice.request('POST', '/tasks', {**body, 'output': output}, {'Idempotency-Key': str(uuid.uuid4())})[0] == 422 for output in [{'size': 'illegal', 'count': 1}, {'size': '1024x1024', 'count': 2}])))
    check('G14 API base prefix preserved exactly once', lambda: expect('/v1/images/generations' in metrics()['paths'] and '/v1/images/edits' in metrics()['paths'] and not any('/v1/v1/' in path for path in metrics()['paths'])))
    failure_tasks = {}
    for upstream, expected in [('mock-http-404', 'failed'), ('mock-http-500', 'failed'), ('mock-read-body', 'submission_unknown'), ('mock-empty', 'result_unavailable'), ('mock-conflict', 'result_unavailable'), ('mock-download-404', 'result_unavailable')]:
        model = make_model(admin, upstream, upstream)
        task_id = submit(admin, task_body(admin, model))
        result = wait_task(admin, task_id)
        failure_tasks[upstream] = task_id
        check('G06/G07/P13 ' + upstream + ' preserves outcome', lambda result=result, expected=expected: expect(result['status'] == expected, result))
        if upstream in ['mock-http-404', 'mock-http-500', 'mock-read-body']:
            records = admin.ok('GET', '/tasks/' + task_id + '/upstream-requests')['items']
            expected_http = 404 if upstream.endswith('404') else 500 if upstream.endswith('500') else 200
            check('real HTTP record ' + upstream, lambda: expect(records[0]['http_status'] == expected_http))
    disconnected = make_model(admin, 'Mock 连接失败', base='http://127.0.0.1:19092/v1/')
    disconnect_id = submit(admin, task_body(admin, disconnected))
    disconnect = wait_task(admin, disconnect_id)
    disconnect_records = admin.ok('GET', '/tasks/' + disconnect_id + '/upstream-requests')['items']
    check('G07 connection failure has no invented HTTP code', lambda: expect(disconnect['status'] == 'failed' and disconnect_records[0]['http_status'] is None and disconnect_records[0]['transport_error']))
    current = admin.ok('GET', '/admin/models/' + generate['id'])
    admin.ok('PATCH', '/admin/models/' + generate['id'], {'revision': current['revision'], 'display_name': 'Mock 生成已更新-' + RUN})
    check('G08 idempotency replay precedes stale model revision', lambda: expect(submit(alice, body, key) == ids[0]))
    current = admin.ok('GET', '/admin/models/' + generate['id'])
    admin.ok('PATCH', '/admin/models/' + generate['id'], {'revision': current['revision'], 'enabled': False})
    check('G05 disabled model rejects new task', lambda: expect(alice.request('POST', '/tasks', body, {'Idempotency-Key': str(uuid.uuid4())})[0] == 409))
    current = admin.ok('GET', '/admin/models/' + generate['id'])
    admin.ok('PATCH', '/admin/models/' + generate['id'], {'revision': current['revision'], 'enabled': True})
    generate = admin.ok('GET', '/admin/models/' + generate['id'])
    alice_info = admin.ok('GET', '/admin/users/' + alice_info['id'])
    admin.ok('PATCH', '/admin/users/' + alice_info['id'], {'revision': alice_info['revision'], 'permissions': ['task.read_own']})
    check('G13 permission revocation affects existing session', lambda: expect(alice.request('GET', '/assets/' + asset_id + '/download', raw=True)[0] == 403 and alice.request('GET', '/assets/' + asset_id + '/preview', raw=True)[0] == 200 and alice.request('POST', '/tasks', task_body(admin, generate), {'Idempotency-Key': str(uuid.uuid4())})[0] == 403))
    alice_info = admin.ok('GET', '/admin/users/' + alice_info['id'])
    admin.ok('PATCH', '/admin/users/' + alice_info['id'], {'revision': alice_info['revision'], 'permissions': ['image.generate', 'task.read_own', 'asset.download_own']})
    entry_id = 'example.acceptance-' + RUN + '.generate'
    package = package_variant()
    installed = admin.upload('/admin/plugins/install', 'package', 'example.npplugin', package, expected=(200, 201))
    plugin_id = installed['id']
    check('P02 installed plugin remains pending and invisible', lambda: expect(admin.ok('GET', '/admin/plugins/' + plugin_id)['state'] == 'installed_pending' and not any(item['id'] == entry_id for item in admin.ok('GET', '/entry-formats')['items'])))
    check('P12 ordinary user plugin operations 403', lambda: expect(alice.request('POST', '/admin/registry/refresh', {})[0] == 403 and alice.request('DELETE', '/admin/plugins/' + plugin_id, {'revision': 1})[0] == 403))
    restart_server()
    check('P03/D02 restart preserves pending plugin and session', lambda: expect(admin.ok('GET', '/admin/plugins/' + plugin_id)['state'] == 'installed_pending' and not any(item['id'] == entry_id for item in admin.ok('GET', '/entry-formats')['items'])))
    plugin = admin.ok('GET', '/admin/plugins/' + plugin_id)
    publication = {'expected_registry_revision': admin.ok('GET', '/entry-formats')['registry_revision'], 'request_id': str(uuid.uuid4()), 'changes': [{'id': plugin_id, 'revision': plugin['revision']}]}
    published = admin.ok('POST', '/admin/registry/refresh', publication)
    check('P04 atomic selected-plugin publication', lambda: expect(any(item['id'] == entry_id for item in admin.ok('GET', '/entry-formats')['items'])))
    check('P07 publication retry is idempotent', lambda: expect(admin.ok('POST', '/admin/registry/refresh', publication)['registry_revision'] == published['registry_revision']))
    check('P07 stale registry revision returns 409', lambda: expect(admin.request('POST', '/admin/registry/refresh', {**publication, 'request_id': str(uuid.uuid4())})[0] == 409))
    adapter = make_model(admin, 'Mock 插件生成', 'mock-url', entry_id, MOCK + '/adapter/')
    adapter_result = wait_task(admin, submit(admin, task_body(admin, adapter)))
    check('P05 plugin request mapping completes image task', lambda: expect(adapter_result['status'] == 'succeeded'))
    check('download client does not forward API authorization', lambda: expect(not metrics()['downloadAuthorizationSeen']))
    check('P09 referenced plugin removal returns 409', lambda: expect(admin.request('DELETE', '/admin/plugins/' + plugin_id, {'revision': plugin['revision']})[0] == 409))
    plugin = admin.ok('GET', '/admin/plugins/' + plugin_id)
    admin.ok('PATCH', '/admin/plugins/' + plugin_id, {'revision': plugin['revision'], 'alias': '已验证示例插件', 'notes': '自动化验收', 'config': {}, 'desired_enabled': True})
    check('P10 draft configuration persists without auto-publication', lambda: expect(admin.ok('GET', '/admin/plugins/' + plugin_id)['alias'] == '已验证示例插件' and admin.ok('GET', '/admin/plugins/' + plugin_id)['active_enabled']))
    check('P10 immutable manifest cannot be edited', lambda: expect(admin.request('PATCH', '/admin/plugins/' + plugin_id, {'revision': plugin['revision'] + 1, 'manifest': {}})[0] == 422))
    feature = package_variant(plugin_id='example.feature', kind='feature')
    check('P11 feature kind rejected explicitly', lambda: expect(admin.request('POST', '/admin/plugins/install', multipart_bytes('package', feature)[0], multipart_bytes('package', feature)[1])[0] == 422))
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w') as archive:
        archive.writestr('../escape.txt', 'malicious')
    invalid = output.getvalue()
    bad_body, bad_headers = multipart_bytes('package', invalid)
    check('P11 archive path traversal rejected', lambda: expect(admin.request('POST', '/admin/plugins/install', bad_body, bad_headers)[0] == 422))
    slow = make_model(admin, 'Mock 缓慢生成', 'mock-slow')
    storage_task = submit(admin, task_body(admin, slow))
    wait_task(admin, storage_task, {'submitting'})
    images_path = ROOT / 'data/images'
    backup_path = ROOT / ('data/images-fault-backup-' + RUN)
    try:
        images_path.rename(backup_path)
        images_path.write_text('temporary storage fault')
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            current = admin.ok('GET', '/tasks/' + storage_task)['task']
            if current['retry_count'] >= 1:
                break
            time.sleep(.2)
        expect(current['retry_count'] >= 1, 'storage fault was not observed')
    finally:
        if images_path.is_file():
            images_path.unlink()
        backup_path.rename(images_path)
    count_before = metrics()['models'].get('mock-slow', 0)
    recovered = wait_task(admin, storage_task)
    check('G10 storage retry recovers receipt without new generation', lambda: expect(recovered['status'] == 'succeeded' and metrics()['models']['mock-slow'] == count_before))
    interrupted_id = submit(admin, task_body(admin, slow))
    wait_task(admin, interrupted_id, {'submitting'})
    count_before = metrics()['models'].get('mock-slow', 0)
    restart_server()
    interrupted = wait_task(admin, interrupted_id)
    check('G11 crash after submission enters submission_unknown', lambda: expect(interrupted['status'] == 'submission_unknown' and metrics()['models']['mock-slow'] == count_before))
    check('G09/D02 restart retains tasks and authorized image bytes', lambda: expect(alice.ok('GET', '/tasks/' + ids[0])['task']['status'] == 'succeeded' and alice.request('GET', '/assets/' + asset_id + '/download', raw=True)[0] == 200))
    temporary, temporary_info = user(admin, 'temporary', model_ids=[])
    admin.ok('PATCH', '/admin/users/' + temporary_info['id'], {'revision': temporary_info['revision'], 'status': 'disabled'})
    check('A10 disabled user session revoked', lambda: expect(temporary.request('GET', '/auth/me')[0] == 401))
    temporary_info = admin.ok('GET', '/admin/users/' + temporary_info['id'])
    admin.ok('PATCH', '/admin/users/' + temporary_info['id'], {'revision': temporary_info['revision'], 'status': 'enabled'})
    temporary.login(temporary_info['username'], PASSWORD)
    temporary_info = admin.ok('GET', '/admin/users/' + temporary_info['id'])
    admin.ok('POST', '/admin/users/' + temporary_info['id'] + '/reset-password', {'revision': temporary_info['revision'], 'password': PASSWORD + '-changed'})
    check('A10 password reset revokes every old session', lambda: expect(temporary.request('GET', '/auth/me')[0] == 401))
    temporary.login(temporary_info['username'], PASSWORD + '-changed')
    temporary_info = admin.ok('GET', '/admin/users/' + temporary_info['id'])
    admin.ok('DELETE', '/admin/users/' + temporary_info['id'], {'revision': temporary_info['revision']})
    check('A10 soft deletion revokes session without deleting history', lambda: expect(temporary.request('GET', '/auth/me')[0] == 401 and admin.ok('GET', '/admin/users/' + temporary_info['id'])['deleted_at']))
    check('A11 administrator cannot demote or delete self', lambda: expect(admin.request('PATCH', '/admin/users/' + admin.user['id'], {'revision': admin.ok('GET', '/admin/users/' + admin.user['id'])['revision'], 'role': 'user'})[0] == 422 and admin.request('DELETE', '/admin/users/' + admin.user['id'], {'revision': 1})[0] == 422))
    old_cookie = '; '.join(cookie.name + '=' + cookie.value for cookie in bob.jar)
    bob.ok('POST', '/auth/logout', {})
    check('A13 old cookie fails after logout', lambda: expect(outsider.request('GET', '/auth/me', headers={'Cookie': old_cookie})[0] == 401))
    wrong = Client()
    wrong_messages = []
    for _ in range(13):
        response = wrong.request('POST', '/auth/login', {'username': 'nonexistent-' + RUN, 'password': 'incorrect'})
        wrong_messages.append(response)
    check('A02 uniform credential errors and login rate limiting', lambda: expect(all(item[1]['error']['message'] == '用户名或密码错误' for item in wrong_messages[:12]) and wrong_messages[12][0] == 429))
    state = {'run': RUN, 'password': PASSWORD, 'alice': alice_info, 'bob': bob_info, 'denied': denied_info, 'viewer': viewer_info, 'generate_model': generate, 'edit_model': edit, 'adapter_model': adapter, 'task_id': ids[0], 'asset_id': asset_id, 'plugin_id': plugin_id, 'failure_tasks': failure_tasks, 'slow_model': slow}
    (ROOT / '.cache/acceptance-state.json').write_text(json.dumps(state, ensure_ascii=False, indent=2), encoding='utf8')


def multipart_bytes(field, content):
    boundary = 'np-fixed-test-boundary'
    return (f'--{boundary}\r\nContent-Disposition: form-data; name="{field}"; filename="test.npplugin"\r\nContent-Type: application/octet-stream\r\n\r\n'.encode() + content + f'\r\n--{boundary}--\r\n'.encode(), {'Content-Type': f'multipart/form-data; boundary={boundary}'})


if __name__ == '__main__':
    try:
        main()
    finally:
        (ROOT / 'docs').mkdir(exist_ok=True)
        (ROOT / 'docs/api-test-results.json').write_text(json.dumps({'timestamp': time.strftime('%Y-%m-%dT%H:%M:%S%z'), 'environment': 'Local PostgreSQL + bounded HTTP Mock. Real provider validation is separate.', 'checks': RESULTS}, ensure_ascii=False, indent=2), encoding='utf8')
    if any(result['result'] == 'failed' for result in RESULTS):
        raise SystemExit(1)
