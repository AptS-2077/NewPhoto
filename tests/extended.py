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
PG_CODE = "import pg from 'pg'; const client=new pg.Client({connectionString:process.env.TEST_DB_URL}); await client.connect(); try { const result=await client.query(process.env.TEST_SQL,JSON.parse(process.env.TEST_PARAMS)); console.log(JSON.stringify(Array.isArray(result) ? result.at(-1).rows : result.rows)); } finally { await client.end(); }"


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


def migration_round_trip():
    migrations = ROOT / 'server/migrations'
    initial, errors, upgrade, downgrade = [(migrations / name).read_text('utf8') for name in [
        '0001_initial.sql', '0002_task_error.sql', '0003_image_input.up.sql', '0003_image_input.down.sql'
    ]]
    legacy = """
        INSERT INTO users(id,username,display_name,role) VALUES('00000000-0000-0000-0000-000000000001','legacy','Legacy','user');
        INSERT INTO model_configs(id,display_name,entry_format_id,upstream_model,api_base_url,capabilities)
        VALUES('00000000-0000-0000-0000-000000000002','Legacy','openai.images.edit','mock-ok','https://example.invalid','{}');
        INSERT INTO assets(id,user_id,kind,object_key,thumbnail_key,digest,mime,width,height,byte_size)
        VALUES('00000000-0000-0000-0000-000000000003','00000000-0000-0000-0000-000000000001','input','old-original','old-thumbnail','legacy','image/png',1,1,4);
        INSERT INTO tasks(id,user_id,idempotency_key,request_hash,request,model_id,model_snapshot,entry_snapshot,registry_revision,status)
        VALUES('00000000-0000-0000-0000-000000000004','00000000-0000-0000-0000-000000000001','legacy','legacy',
            '{"input_asset_ids":["00000000-0000-0000-0000-000000000003"]}','00000000-0000-0000-0000-000000000002','{}','{"operation":"edit"}',1,'queued');
    """
    verify_upgrade = """
        DO $$ BEGIN
            IF NOT EXISTS (SELECT 1 FROM tasks WHERE mode='i2i' AND jsonb_array_length("references")=1)
                OR (SELECT count(*) FROM task_reference_assets) != 1 THEN
                RAISE EXCEPTION 'Legacy task backfill failed';
            END IF;
        END $$;
    """
    verify_downgrade = """
        DO $$ BEGIN
            IF (SELECT count(*) FROM tasks WHERE request->'input_asset_ids'->>0='00000000-0000-0000-0000-000000000003') != 1
                OR (SELECT count(*) FROM assets) != 1 OR to_regclass('task_reference_assets') IS NOT NULL THEN
                RAISE EXCEPTION 'Legacy task or asset lost during rollback';
            END IF;
        END $$;
    """
    sql('\n'.join(['BEGIN;', initial, errors, legacy, upgrade, verify_upgrade, downgrade, verify_downgrade, 'ROLLBACK;']))


def image_plugin(transport):
    root = ROOT / 'examples/image-to-image'
    manifest = json.loads((root / 'manifest.json').read_text('utf8'))
    definition = json.loads((root / 'entries/edit.json').read_text('utf8'))
    if transport == 'url':
        manifest['plugin_id'] += '-url'
        manifest['entrypoints'][0]['entry_format_id'] = manifest['plugin_id'] + '.edit'
        definition['request']['path'] = 'i2i-url'
        definition['request']['reference_images']['encoding'] = 'url'
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w', zipfile.ZIP_DEFLATED) as archive:
        archive.writestr('manifest.json', json.dumps(manifest))
        archive.writestr('entries/edit.json', json.dumps(definition))
    return manifest['entrypoints'][0]['entry_format_id'], output.getvalue()


def check_image_input(admin, generate_model):
    fixture = json.loads((ROOT / 'examples/image-to-image/golden-fixture.json').read_text('utf8'))
    png = urllib.request.urlopen(suite.MOCK + '/fixture.png', timeout=5).read()
    models = []
    for transport in ['base64', 'url']:
        entry_id, content = image_plugin(transport)
        publish(admin, install(admin, content))
        models.append(suite.make_model(admin, 'Image input ' + transport, 'mock-i2i-golden', entry=entry_id))
    edit = suite.make_model(admin, 'OpenAI multipart edit', entry='openai.images.edit')
    owner, _ = suite.user(admin, 'reference-owner', model_ids=[generate_model['id'], edit['id'], *(model['id'] for model in models)])
    other, _ = suite.user(admin, 'reference-other', model_ids=[models[0]['id']])
    formats = owner.ok('GET', '/entry-formats')
    entry = next(item for item in formats['items'] if item['id'] == models[0]['entry_format_id'])
    suite.expect(entry['capabilities'] == ['image.input'] and entry['image_input']['max_count'] == 4)
    suite.expect(not next(item for item in formats['items'] if item['id'] == generate_model['entry_format_id'])['capabilities'])
    settings = admin.ok('GET', '/admin/settings')
    suite.expect(settings['enabled'] and settings['max_bytes'] == 10 * 1024 * 1024)
    suite.expect(other.request('GET', '/admin/settings')[0] == 403)
    for name, content, mime in [
        ('big.png', png.ljust(11 * 1024 * 1024, b'\0'), 'image/png'),
        ('image.gif', b'GIF89a', 'image/gif'),
        ('forged.png', b'not an image', 'image/png'),
        ('spoofed.jpg', png, 'image/jpeg'),
    ]:
        result = owner.upload('/images/reference', 'image', name, content, mime=mime, expected=422)
        suite.expect(result['error']['code'] == 'VALIDATION_ERROR' and result['error']['message'])
    reference = owner.upload('/images/reference', 'image', 'reference.png', png, mime='image/png')
    reference_id = reference['reference_id']
    suite.expect(reference_id == reference['id'] and reference['width'] == 1024)
    stored = sql("SELECT object_key,thumbnail_key FROM assets WHERE id=$1", [reference_id])[0]
    for key in stored.values():
        suite.expect(not (ROOT / '.cache' / DATABASE_NAME / key).read_bytes().startswith(png[:8]))
    suite.expect(owner.request('GET', reference['preview_url'].removeprefix('/api/v1'), raw=True)[1] == png)
    suite.expect(other.request('GET', reference['preview_url'].removeprefix('/api/v1'), raw=True)[0] == 404)
    generated = suite.wait_task(owner, suite.submit(owner, suite.task_body(owner, generate_model)))
    suite.expect(generated['mode'] == 't2i' and generated['status'] == 'succeeded' and not generated['reference_assets'])
    generated_id = generated['assets'][0]['id']
    one = [{'id': reference_id}]
    base = suite.task_body(owner, models[0], mode='i2i', references=one, prompt=fixture['prompt'])
    request_headers = lambda: {'Idempotency-Key': str(uuid.uuid4())}
    status, denied = other.request('POST', '/tasks', base, request_headers())
    suite.expect(status == 403 and denied['error']['code'] == 'PERMISSION_DENIED')
    status, rejected = owner.request('POST', '/tasks', suite.task_body(owner, generate_model, mode='i2i', references=one), request_headers())
    suite.expect(status == 422 and rejected['error']['code'] == 'IMAGE_INPUT_UNSUPPORTED')
    for references in [[], one * 2, [{'id': reference_id, 'weight': -1}], [{'id': reference_id, 'weight': 1.1}]]:
        suite.expect(owner.request('POST', '/tasks', {**base, 'references': references}, request_headers())[0] == 422)
    for model, references in zip(models, [one, [*one, {'id': generated_id, 'weight': 0.25}]]):
        result = suite.wait_task(owner, suite.submit(owner, suite.task_body(owner, model, mode='i2i', references=references, prompt=fixture['prompt'])))
        suite.expect(result['status'] == 'succeeded', result)
        suite.expect(result['mode'] == 'i2i' and result['usage'] == fixture['usage'])
        suite.expect([item['id'] for item in result['reference_assets']] == [item['id'] for item in references])
        suite.expect([item['weight'] for item in result['reference_assets']] == fixture['weights'][:len(references)])
        suite.expect(len(result['assets']) == 1)
        history = owner.ok('GET', '/images')['items']
        suite.expect(any(item['task_id'] == result['id'] and item['reference_assets'] == result['reference_assets'] for item in history))
    multipart = suite.wait_task(owner, suite.submit(owner, suite.task_body(owner, edit, mode='i2i', references=one)))
    suite.expect(multipart['status'] == 'succeeded', multipart)
    legacy = owner.upload('/assets/inputs', 'image', 'legacy.png', png)
    old = suite.wait_task(owner, suite.submit(owner, suite.task_body(owner, edit, input_asset_ids=[legacy['id']])))
    suite.expect(old['status'] == 'succeeded' and old['reference_assets'][0]['id'] == legacy['id'])
    new_settings = admin.ok('PATCH', '/admin/settings', {'revision': settings['revision'], 'enabled': False, 'max_bytes': 1024 * 1024})
    suite.expect(new_settings['revision'] == settings['revision'] + 1)
    suite.expect(admin.request('PATCH', '/admin/settings', {'revision': settings['revision'], 'enabled': True})[0] == 409)
    suite.expect(owner.ok('GET', '/entry-formats')['image_input'] == {'enabled': False, 'max_bytes': 1024 * 1024})
    suite.expect(owner.request('POST', '/tasks', base, request_headers())[1]['error']['code'] == 'IMAGE_INPUT_DISABLED')
    suite.expect(owner.upload('/images/reference', 'image', 'disabled.png', png, mime='image/png', expected=409)['error']['code'] == 'IMAGE_INPUT_DISABLED')
    admin.ok('PATCH', '/admin/settings', {'revision': new_settings['revision'], 'enabled': True, 'max_bytes': settings['max_bytes']})


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
    suite.check('I2I migration upgrades and reverts a populated V1 database', migration_round_trip)
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
        suite.check('I2I upload, ownership, history, transports, usage and admin settings', lambda: check_image_input(admin, model))
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
