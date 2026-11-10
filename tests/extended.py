import concurrent.futures
import copy
import io
import json
import os
import subprocess
import time
import urllib.request
import uuid
import zipfile
from urllib.parse import quote, urlsplit, urlunsplit

import acceptance as suite


ROOT = suite.ROOT
DATABASE_NAME = 'newphoto_acceptance_' + str(int(time.time()))
ORIGINAL_URL = (ROOT / '.cache/database-url').read_text().strip()
PARTS = urlsplit(ORIGINAL_URL)
DATABASE_URL = urlunsplit(PARTS._replace(path='/' + DATABASE_NAME))
ENV = os.environ.copy()
ENV.update({'DATABASE_URL': DATABASE_URL, 'NEWPHOTO_DATA_DIR': str(ROOT / '.cache' / DATABASE_NAME), 'NEWPHOTO_COOKIE_SECURE': 'false', 'NEWPHOTO_BIND': '127.0.0.1:18080', 'NEWPHOTO_PUBLIC_ORIGIN': 'http://127.0.0.1:18080', 'NEWPHOTO_ALLOWED_UPSTREAMS': '127.0.0.1:19091,127.0.0.1:19092', 'NEWPHOTO_ADMIN_PASSWORD': suite.PASSWORD})
EXECUTABLE = ROOT / 'target/debug/newphoto.exe'
PG_CODE = "import pg from 'pg'; const client=new pg.Client({connectionString:process.env.TEST_DB_URL}); await client.connect(); try { const result=await client.query(process.env.TEST_SQL,JSON.parse(process.env.TEST_PARAMS)); console.log(JSON.stringify(result.rows)); } finally { await client.end(); }"


def sql(statement, params=None, database_url=DATABASE_URL):
    environment = ENV | {'TEST_DB_URL': database_url, 'TEST_SQL': statement, 'TEST_PARAMS': json.dumps(params or [])}
    result = subprocess.run(['node', '--input-type=module', '-e', PG_CODE], cwd=ROOT, env=environment, capture_output=True, text=True, check=True)
    return json.loads(result.stdout)


def package(plugin_id, entry_id, version='1.0.0', mutate=None):
    manifest = json.loads((ROOT / 'examples/simple-image/manifest.json').read_text('utf8'))
    definition = json.loads((ROOT / 'examples/simple-image/entries/generate.json').read_text('utf8'))
    manifest.update({'plugin_id': plugin_id, 'version': version})
    manifest['entrypoints'][0]['entry_format_id'] = entry_id
    if mutate:
        mutate(definition)
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
        archive.writestr('manifest.json', json.dumps(manifest))
        archive.writestr('entries/generate.json', json.dumps(definition))
    return output.getvalue()


def install(admin, content, expected=201):
    return admin.upload('/admin/plugins/install', 'package', 'extended.npplugin', content, expected=expected)


def publish(admin, installation, expected=200):
    body = {'expected_registry_revision': admin.ok('GET', '/entry-formats')['registry_revision'], 'request_id': str(uuid.uuid4()), 'changes': [{'id': installation['id'], 'revision': installation['revision']}]}
    return admin.ok('POST', '/admin/registry/refresh', body, expected=expected)


def remaining_admin():
    row = sql("SELECT username FROM users WHERE role='admin' AND status='enabled' AND deleted_at IS NULL")
    suite.expect(len(row) == 1, 'concurrent changes must leave exactly one admin')
    return suite.Client().login(row[0]['username'], suite.PASSWORD)


def concurrent_admin_changes(admin, method):
    other, info = suite.user(admin, 'race-' + method.lower(), role='admin')
    current = admin.ok('GET', '/auth/me')['user']
    with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
        futures = [pool.submit(actor.request, method, '/admin/users/' + target['id'], {'revision': target['revision'], **({'role': 'user'} if method == 'PATCH' else {})}) for actor, target in [(admin, info), (other, current)]]
        statuses = [future.result()[0] for future in futures]
    suite.expect(statuses.count(200) == 1 and all(status in [200, 401, 403, 409] for status in statuses), str(statuses))
    return remaining_admin()


def run():
    suite.BASE = ENV['NEWPHOTO_PUBLIC_ORIGIN']
    subprocess.run([str(EXECUTABLE), 'admin', 'create', '--username', 'clean-admin'], cwd=ROOT, env=ENV, check=True, capture_output=True)
    log = (ROOT / '.cache/isolated-server.log').open('ab')
    process = subprocess.Popen([str(EXECUTABLE), 'serve'], cwd=ROOT, env=ENV, stdout=log, stderr=log, creationflags=subprocess.CREATE_NO_WINDOW)
    try:
        for attempt in range(100):
            try:
                urllib.request.urlopen(suite.BASE + '/health/ready', timeout=1).close()
                break
            except OSError:
                time.sleep(.1)
        admin = suite.Client().login('clean-admin', suite.PASSWORD)
        suite.check('D01 clean native database migration, CLI administrator and first login', lambda: suite.expect(not admin.ok('GET', '/admin/models')['items'] and not admin.ok('GET', '/admin/plugins')['items'] and len(admin.ok('GET', '/entry-formats')['items']) == 2))
        root_holder = [admin]
        suite.check('A11 concurrent cross-deletion preserves a live administrator', lambda: root_holder.__setitem__(0, concurrent_admin_changes(root_holder[0], 'DELETE')))
        suite.check('A11 concurrent cross-demotion preserves a live administrator', lambda: root_holder.__setitem__(0, concurrent_admin_changes(root_holder[0], 'PATCH')))
        admin = root_holder[0]
        model = suite.make_model(admin, 'Extended Mock')
        suite.check('MODEL invalid default parameters rejected', lambda: suite.expect(admin.request('PATCH', '/admin/models/' + model['id'], {'revision': model['revision'], 'defaults': {'count': 2}})[0] == 422))
        current = admin.ok('GET', '/admin/models/' + model['id'])
        admin.ok('PATCH', '/admin/models/' + model['id'], {'revision': current['revision'], 'display_name': 'Kept key'})
        suite.check('MODEL stale write returns 409', lambda: suite.expect(admin.request('PATCH', '/admin/models/' + model['id'], {'revision': current['revision'], 'display_name': 'stale'})[0] == 409))
        suite.check('MODEL keep preserves secret without revealing it', lambda: suite.expect(admin.ok('GET', '/admin/models/' + model['id'])['has_secret'] and 'non-sensitive-mock-key' not in json.dumps(admin.ok('GET', '/admin/models/' + model['id']))))
        current = admin.ok('GET', '/admin/models/' + model['id'])
        suite.check('MODEL clear rejects enabled bearer model', lambda: suite.expect(admin.request('PATCH', '/admin/models/' + model['id'], {'revision': current['revision'], 'credential_action': 'clear'})[0] == 422))
        admin.ok('PATCH', '/admin/models/' + model['id'], {'revision': current['revision'], 'credential_action': 'clear', 'enabled': False})
        suite.check('MODEL explicit clear removes saved key', lambda: suite.expect(not admin.ok('GET', '/admin/models/' + model['id'])['has_secret']))
        current = admin.ok('GET', '/admin/models/' + model['id'])
        admin.ok('PATCH', '/admin/models/' + model['id'], {'revision': current['revision'], 'credential_action': 'replace', 'credential': 'another-non-sensitive-test-key', 'enabled': True})
        model = admin.ok('GET', '/admin/models/' + model['id'])
        content = package('example.extended', 'example.extended.generate')
        first = install(admin, content)
        second = install(admin, package('example.unselected', 'example.unselected.generate'))
        publish(admin, first)
        suite.check('P04 selection publishes only requested plugin', lambda: suite.expect(admin.ok('GET', '/admin/plugins/' + second['id'])['state'] == 'installed_pending' and not any(entry['id'] == 'example.unselected.generate' for entry in admin.ok('GET', '/entry-formats')['items'])))
        conflict = install(admin, package('example.conflict', 'example.extended.generate'))
        before = admin.ok('GET', '/entry-formats')
        suite.check('P06 conflicting entry publication leaves registry atomic', lambda: suite.expect(publish(admin, conflict, 422)['error']['code'] == 'VALIDATION_ERROR' and admin.ok('GET', '/entry-formats') == before))
        adapter = suite.make_model(admin, 'Old snapshot', 'mock-slow', 'example.extended.generate', suite.MOCK + '/adapter/')
        old_id = suite.submit(admin, suite.task_body(admin, adapter))
        suite.wait_task(admin, old_id, {'submitting'})
        second_version = install(admin, package('example.extended', 'example.extended.generate', '1.1.0', lambda definition: definition['request']['fields'].__setitem__('model', {'constant': 'mock-ok'})))
        publish(admin, second_version)
        new_id = suite.submit(admin, suite.task_body(admin, adapter))
        old_task = suite.wait_task(admin, old_id)
        new_task = suite.wait_task(admin, new_id)
        snapshots = sql("SELECT id,entry_snapshot->>'version' AS version,entry_snapshot->>'digest' AS digest FROM tasks WHERE id=ANY($1::uuid[]) ORDER BY created_at", [[old_id, new_id]])
        suite.check('P08 in-flight task retains old package while new tasks use upgraded mapping', lambda: suite.expect(old_task['status'] == new_task['status'] == 'succeeded' and [row['version'] for row in snapshots] == ['1.0.0', '1.1.0'] and snapshots[0]['digest'] != snapshots[1]['digest']))
        changed = admin.ok('GET', '/admin/plugins/' + second_version['id'])
        admin.ok('PATCH', '/admin/plugins/' + changed['id'], {'revision': changed['revision'], 'desired_enabled': False})
        changed = admin.ok('GET', '/admin/plugins/' + changed['id'])
        suite.check('P09 active model reference blocks disable publication', lambda: suite.expect(publish(admin, changed, 409)['error']['code'] == 'PLUGIN_IN_USE'))
        suite.check('P13 unsupported request source mapping is rejected during install', lambda: suite.expect(install(admin, package('example.invalid', 'example.invalid.generate', mutate=lambda definition: definition['request']['fields'].__setitem__('model', {'from': '/request/missing'})), 422)['error']['code'] == 'VALIDATION_ERROR'))
        huge = io.BytesIO()
        with zipfile.ZipFile(huge, 'w', zipfile.ZIP_DEFLATED) as archive:
            archive.writestr('too-large.json', b'0' * (4 * 1024 * 1024 + 1))
        suite.check('P11 bounded decompression rejects oversized entry', lambda: suite.expect(install(admin, huge.getvalue(), 422)['error']['code'] == 'VALIDATION_ERROR'))
        suite.check('G12 oversized image upload is bounded', lambda: suite.expect(admin.upload('/assets/inputs', 'image', 'too-large.png', b'0' * (25 * 1024 * 1024 + 1), expected=422)['error']['code'] == 'VALIDATION_ERROR'))
        expired, expired_info = suite.user(admin, 'expired')
        sql("UPDATE sessions SET expires_at=now()-interval '1 second' WHERE user_id=$1", [expired_info['id']])
        suite.check('AUTH absolute session expiry returns 401', lambda: suite.expect(expired.request('GET', '/auth/me')[0] == 401))
        idle, idle_info = suite.user(admin, 'idle')
        sql("UPDATE sessions SET last_seen_at=now()-interval '121 minutes' WHERE user_id=$1", [idle_info['id']])
        suite.check('AUTH idle session expiry returns 401', lambda: suite.expect(idle.request('GET', '/auth/me')[0] == 401))
        sql("UPDATE app_settings SET value='true' WHERE key='generation_paused'")
        suite.check('RECOVERY pause blocks new tasks without upstream submission', lambda: suite.expect(admin.request('POST', '/tasks', suite.task_body(admin, model), {'Idempotency-Key': str(uuid.uuid4())})[1]['error']['code'] == 'GENERATION_PAUSED'))
        sql("UPDATE app_settings SET value='false' WHERE key='generation_paused'")
        for upstream, expected_code in [('mock-http-404', 'UPSTREAM_HTTP_ERROR'), ('mock-read-body', 'SUBMISSION_UNKNOWN'), ('mock-empty', 'RESULT_UNAVAILABLE')]:
            failure_model = suite.make_model(admin, 'Machine code', upstream)
            task = suite.wait_task(admin, suite.submit(admin, suite.task_body(admin, failure_model)))
            suite.check('TASK persisted ' + expected_code, lambda task=task, expected_code=expected_code: suite.expect(task['error_code'] == expected_code))
        task = suite.wait_task(admin, suite.submit(admin, suite.task_body(admin, model)))
        suite.check('TASK searchable joined list returns task and real upstream status', lambda: suite.expect(any(item['id'] == task['id'] for item in admin.ok('GET', '/tasks?search=' + quote('验收样例'))['items']) and admin.ok('GET', '/tasks?all_users=true')['items']))
        suite.check('TASK timing includes all persisted execution stages', lambda: suite.expect(abs(task['total_ms'] - task['queue_ms'] - task['execution_ms']) <= 2 and task['execution_ms'] >= 0))
        suite.check('API plugin and model lists expose bounded pagination', lambda: suite.expect(admin.ok('GET', '/admin/plugins?per_page=1')['has_more'] and len(admin.ok('GET', '/models?per_page=1')['items']) == 1 and admin.ok('GET', '/admin/plugins')['per_page'] == 20))
    finally:
        process.terminate()
        process.wait(timeout=10)
        log.close()


if __name__ == '__main__':
    sql('CREATE DATABASE ' + DATABASE_NAME, database_url=ORIGINAL_URL)
    try:
        run()
    finally:
        sql('DROP DATABASE ' + DATABASE_NAME + ' WITH (FORCE)', database_url=ORIGINAL_URL)
        (ROOT / 'docs/extended-test-results.json').write_text(json.dumps({'timestamp': time.strftime('%Y-%m-%dT%H:%M:%S%z'), 'environment': 'Fresh isolated PostgreSQL database, native process and Mock only; main user configuration untouched.', 'checks': suite.RESULTS}, ensure_ascii=False, indent=2), encoding='utf8')
    if any(result['result'] == 'failed' for result in suite.RESULTS):
        raise SystemExit(1)
