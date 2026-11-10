const state = { user: null, csrf: '', epoch: 0, route: '', formats: [], models: [], registryRevision: 0, activeTask: null, input: null, uploadPending: false, page: {}, rows: {}, poll: null, registryPoll: null, generationKey: '', generationBody: '', modelTest: false, generateLoad: 0 };
const pending = new WeakSet();
const terminal = new Set(['succeeded', 'failed', 'submission_unknown', 'result_unavailable']);
const statusNames = { queued: '排队中', preparing: '准备中', submitting: '等待上游', saving: '保存成果', succeeded: '已完成', failed: '失败', submission_unknown: '结果待确认', result_unavailable: '成果不可获取' };
const pageNames = { '/generate': '图片生成', '/tasks': '生成任务', '/images': '图片记录', '/account': '个人信息', '/admin/models': '模型管理', '/admin/users': '用户管理', '/admin/plugins': '插件管理' };
const permissionNames = { 'image.generate': '图片生成 / 编辑', 'task.read_own': '查看本人任务与图片', 'asset.download_own': '下载本人图片' };
const find = selector => document.querySelector(selector);
const all = selector => [...document.querySelectorAll(selector)];
function node(tag, attributes = {}, ...children) {
  const target = document.createElement(tag);
  for (const [key, value] of Object.entries(attributes)) {
    if (key.startsWith('on') && typeof value === 'function') target.addEventListener(key.slice(2).toLowerCase(), value);
    else if (['value', 'checked', 'disabled', 'hidden', 'multiple', 'required'].includes(key)) target[key] = value;
    else if (value !== null && value !== undefined) target.setAttribute(key, value);
  }
  for (const child of children.flat(Infinity)) if (child !== null && child !== undefined) target.append(child instanceof Node ? child : document.createTextNode(String(child)));
  return target;
}
function button(label, action, className = '') { return node('button', { type: 'button', class: className, onClick: action }, label); }
function input(name, value = '', attributes = {}) { return node('input', { name, id: `field-${name}`, value, ...attributes }); }
function select(name, options, value, attributes = {}) {
  const control = node('select', { name, id: `field-${name}`, ...attributes }, options.map(option => node('option', { value: option.value ?? option, disabled: option.disabled || false }, option.label ?? option)));
  if (value !== undefined) control.value = value;
  return control;
}
function field(label, control, hint = '') { return node('div', { class: 'form-field' }, node('label', { for: control.id }, label), control, hint ? node('p', { class: 'field-hint' }, hint) : null); }
function errorText(target, message = '') { target.textContent = message; target.hidden = !message; }
function toast(message) { const target = find('#toast'); target.textContent = message; target.hidden = false; clearTimeout(toast.timer); toast.timer = setTimeout(() => { target.hidden = true; }, 4500); }
function can(permission) { return state.user?.role === 'admin' || state.user?.permissions.includes(permission); }
function timestamp(value) { return value ? new Date(value).toLocaleString('zh-CN', { hour12: false }) : '—'; }
function duration(milliseconds) { if (milliseconds === null || milliseconds === undefined) return '—'; const seconds = Math.max(0, Math.floor(milliseconds / 1000)); return seconds < 60 ? `${seconds} 秒` : `${Math.floor(seconds / 60)} 分 ${seconds % 60} 秒`; }
function badge(label, type = '') { return node('span', { class: `badge ${type}` }, label); }
function taskBadge(status) { return badge(statusNames[status] || status, status === 'succeeded' ? 'success' : terminal.has(status) ? 'error' : 'pending'); }
function httpLabel(record) { if (!record) return '尚无上游请求'; return `${record.phase === 'result_download' ? '成果下载' : '生成请求'} · ${record.http_status ? `HTTP ${record.http_status}` : '无 HTTP 响应'}${record.transport_error ? ` · ${record.transport_error}` : ''}`; }
function empty(target, title, description, retry) { target.replaceChildren(node('div', { class: 'empty-state' }, node('h2', {}, title), node('p', {}, description), retry ? button('重新加载', retry) : null)); }
function loading(target) { target.replaceChildren(node('div', { class: 'loading', role: 'status' }, node('div', { class: 'spinner' }), '正在加载…')); }
async function busy(target, operation) {
  if (pending.has(target)) return;
  pending.add(target);
  const buttons = target.matches('button') ? [target] : [...target.querySelectorAll('button[type=submit]')];
  const previous = buttons.map(control => [control, control.disabled, control.textContent]);
  for (const [control] of previous) { control.disabled = true; control.textContent = '处理中…'; }
  try { return await operation(); } finally { pending.delete(target); for (const [control, disabled, label] of previous) { control.disabled = disabled; control.textContent = label; } }
}
async function api(path, options = {}) {
  const epoch = state.epoch;
  const headers = new Headers(options.headers || {});
  if (options.body && !(options.body instanceof FormData)) headers.set('Content-Type', 'application/json');
  if (options.method && !['GET', 'HEAD'].includes(options.method)) headers.set('X-CSRF-Token', state.csrf);
  let response;
  try { response = await fetch(`/api/v1${path}`, { ...options, headers, credentials: 'same-origin', cache: 'no-store' }); }
  catch { throw Object.assign(new Error('网络连接失败，请检查服务状态后重试'), { cancelled: epoch !== state.epoch }); }
  if (epoch !== state.epoch) throw Object.assign(new Error('账号或页面状态已改变'), { cancelled: true });
  let result;
  try { result = await response.json(); } catch { throw new Error('服务器响应异常，请检查网络或服务状态'); }
  if (!response.ok) {
    if (response.status === 401 && !options.public) goLogin();
    throw Object.assign(new Error(result.error?.message || `请求失败（HTTP ${response.status}）`), { status: response.status, code: result.error?.code });
  }
  return result;
}
function dialog(title, content, actions = []) {
  const modal = find('#modal');
  if (modal.open) modal.close();
  find('#modal-title').textContent = title;
  find('#modal-content').replaceChildren(content);
  find('#modal-footer').replaceChildren(...actions);
  modal.showModal();
  return modal;
}
async function allItems(path) {
  const items = [];
  for (let page = 1; ; page += 1) {
    const result = await api(`${path}${path.includes('?') ? '&' : '?'}per_page=100&page=${page}`);
    items.push(...result.items);
    if (!result.has_more) return items;
  }
}
function formDialog(title, fields, operation, label = '保存') {
  const form = node('form', { id: 'object-form' }, fields);
  const error = node('p', { class: 'error-text', role: 'alert', hidden: true }); form.append(error);
  const submit = node('button', { type: 'submit', form: form.id, class: 'primary' }, label);
  form.addEventListener('submit', event => { event.preventDefault(); busy(form, async () => { submit.disabled = true; errorText(error); try { await operation(form); } catch (failure) { if (!failure.cancelled) errorText(error, failure.message); } finally { submit.disabled = false; } }); });
  return dialog(title, form, [button('取消', () => find('#modal').close()), submit]);
}
function confirmAction(title, message, label, operation) {
  const error = node('p', { class: 'error-text', role: 'alert', hidden: true });
  const action = button(label, () => busy(action, async () => { try { await operation(); find('#modal').close(); toast('操作已完成'); await loadPage(false); } catch (failure) { if (!failure.cancelled) errorText(error, failure.message); } }), 'danger');
  dialog(title, node('div', {}, node('p', {}, message), error), [button('取消', () => find('#modal').close()), action]);
}
function resetPrivateState() {
  state.generateLoad += 1;
  state.epoch += 1; clearTimeout(state.poll); clearTimeout(state.registryPoll);
  Object.assign(state, { user: null, csrf: '', models: [], formats: [], activeTask: null, input: null, rows: {}, page: {}, generationKey: '', generationBody: '', route: '', parentTask: null, uploadPending: false, modelTest: false });
  all('[id$="-content"]').filter(target => target.id !== 'modal-content').forEach(target => target.replaceChildren());
  all('#app-shell form').forEach(form => form.reset());
  all('#app-shell input[type=search]').forEach(control => { control.value = ''; });
  for (const id of ['tasks-all', 'images-all', 'users-deleted']) { const control = find(`#${id}`); if (control) control.checked = false; }
  find('#prompt-count').textContent = '0 / 4000';
  for (const id of ['entry-select', 'model-select', 'size-select', 'generation-progress']) find(`#${id}`).replaceChildren();
  for (const id of ['sidebar-name', 'sidebar-role', 'topbar-username', 'model-hint', 'entry-reason']) find(`#${id}`).textContent = '';
  for (const id of ['generate-error', 'global-error']) errorText(find(`#${id}`));
  find('#upload-status').textContent = '上传一张真实原图，最多 25 MiB / 40 MP；实际限制以模型为准。';
  delete find('#result-canvas').dataset.asset;
  find('#result-canvas').replaceChildren(node('div', { class: 'canvas-empty' }, node('h3', {}, '你的下一幅作品，从这里开始'), node('p', {}, '填写提示词并提交，结果会显示在这里。')));
  find('#generation-progress').hidden = true; find('#modal').close();
  find('#modal-content').replaceChildren(); find('#modal-footer').replaceChildren(); find('#account-permissions').replaceChildren();
  globalThis.setNewPhotoRole('');
}
function goLogin() { resetPrivateState(); find('#app-shell').hidden = true; find('#boot-screen').hidden = true; find('#login-page').hidden = false; history.replaceState({}, '', '/login'); document.title = '登录 · NewPhoto'; }
async function identity(result) {
  state.user = result.user; state.csrf = result.csrf_token;
  globalThis.setNewPhotoRole(state.user.role);
  await new Promise(resolve => queueMicrotask(resolve));
  find('#sidebar-name').textContent = state.user.display_name;
  find('#sidebar-role').textContent = state.user.role === 'admin' ? '管理员' : '普通用户';
  find('#user-avatar').textContent = [...state.user.display_name][0] || 'N';
  find('#topbar-username').textContent = state.user.username;
  find('#tasks-all-wrap').hidden = state.user.role !== 'admin'; find('#images-all-wrap').hidden = state.user.role !== 'admin';
}
async function navigate(path, replace = false) {
  closeDrawer();
  if (!path.includes('test=1')) state.modelTest = false;
  if (replace) history.replaceState({}, '', path); else if (location.pathname + location.search !== path) history.pushState({}, '', path);
  await loadPage();
}
function closeDrawer() { find('#sidebar').classList.remove('open'); find('#drawer-scrim').hidden = true; }
async function loadPage(refreshIdentity = true) {
  clearTimeout(state.poll);
  if (!state.user) return;
  const route = location.pathname === '/' ? '/generate' : location.pathname;
  try {
    if (refreshIdentity) await identity(await api('/auth/me'));
    if (!state.user) return;
    state.route = route;
    const page = route.startsWith('/admin/') && state.user.role !== 'admin' ? 'forbidden' : pageNames[route] ? route : 'not-found';
    all('[data-page]').forEach(target => { target.hidden = target.dataset.page !== page; });
    all('[data-route]').forEach(target => target.classList.toggle('active', target.dataset.route === route));
    find('#breadcrumb-title').textContent = pageNames[route] || (page === 'forbidden' ? '无权访问' : '页面不存在');
    document.title = `${pageNames[route] || '工作台'} · NewPhoto`;
    errorText(find('#global-error'));
    if (page === '/generate') await loadGenerate();
    else if (page === '/account') loadAccount();
    else if (page.startsWith('/admin/') || ['/tasks', '/images'].includes(page)) await loadList(page);
    schedulePolling();
  } catch (failure) { if (!failure.cancelled && state.user) errorText(find('#global-error'), failure.message); }
}
function table(target, headings, rows) {
  target.replaceChildren(node('div', { class: 'table-wrap' }, node('table', {}, node('thead', {}, node('tr', {}, headings.map(heading => node('th', { scope: 'col' }, heading)))), node('tbody', {}, rows.map(cells => node('tr', {}, cells.map(cell => node('td', {}, cell))))))));
}
function pagination(name, result) {
  const target = find(`#${name}-pagination`); if (!target) return;
  target.replaceChildren(button('上一页', () => { state.page[state.route] -= 1; loadList(state.route); }), node('span', {}, `第 ${result.page || 1} 页`), button('下一页', () => { state.page[state.route] = (state.page[state.route] || 1) + 1; loadList(state.route); }));
  target.firstChild.disabled = (result.page || 1) <= 1; target.lastChild.disabled = !result.has_more;
}
async function loadList(route, quiet = false) {
  const name = route.split('/').pop(); const target = find(`#${name}-content`); if (!target) return;
  if (['tasks', 'images'].includes(name) && !can('task.read_own')) { empty(target, '当前账号没有查看权限', '通用菜单仍可使用，请联系管理员授予 task.read_own。'); find(`#${name}-pagination`).replaceChildren(); return; }
  if (!quiet) loading(target);
  const query = new URLSearchParams({ page: String(state.page[route] || 1), search: find(`#${name}-search`)?.value || '' });
  if (find(`#${name}-all`)?.checked) query.set('all_users', 'true');
  if (name === 'users' && find('#users-deleted').checked) query.set('include_deleted', 'true');
  try {
    const result = await api(`${route}?${query}`);
    if (state.route !== route) return;
    state.rows[route] = result.items;
    if (name === 'plugins') find('#registry-label').textContent = `入口注册表 · revision ${result.registry_revision}`;
    if (!result.items.length) empty(target, query.get('search') ? '没有匹配的记录' : name === 'models' ? '尚未配置模型' : name === 'plugins' ? '尚未安装入口插件' : name === 'images' ? '还没有生成的图片' : name === 'tasks' ? '还没有生成任务' : '没有可显示的用户', name === 'models' ? '新增模型并完成授权后，用户才能生成图片。' : name === 'plugins' ? '两个内置入口已随应用提供；可在此安装受限声明式插件。' : '调整搜索条件，或开始一次新的创作。');
    else if (name === 'tasks') renderTasks(target, result.items);
    else if (name === 'images') renderImages(target, result.items);
    else if (name === 'users') renderUsers(target, result.items);
    else if (name === 'models') renderModels(target, result.items);
    else renderPlugins(target, result.items);
    pagination(name, result);
  } catch (failure) { if (!failure.cancelled) empty(target, failure.status === 403 ? '当前账号没有访问权限' : '加载失败', failure.message, () => loadList(route)); }
}
function renderTasks(target, items) {
  const active = items.filter(item => !terminal.has(item.status)).length; find('#active-count').textContent = active; find('#active-count').hidden = !active;
  table(target, ['任务 / 提示词', '模型', '状态', '已耗时', '最近上游返回', '创建时间', '操作'], items.map(item => [node('div', {}, node('span', { class: 'cell-title' }, item.request.prompt.slice(0, 24)), node('span', { class: 'cell-sub' }, item.username || '本人')), item.model_display_name, taskBadge(item.status), node('span', { 'data-elapsed-id': item.id, 'data-elapsed-base': item.elapsed_ms, 'data-elapsed-clock': performance.now(), 'data-elapsed-live': String(!terminal.has(item.status)) }, duration(item.elapsed_ms)), badge(httpLabel(item.latest_upstream), item.latest_upstream?.http_status >= 400 ? 'error' : ''), timestamp(item.created_at), node('div', { class: 'row-actions' }, button('详情', () => taskDetail(item.id)), button('再次生成', () => again(item), 'subtle'))]));
}
function renderImages(target, items) {
  target.replaceChildren(node('div', { class: 'image-grid' }, items.map(item => node('article', { class: 'image-card' }, button(node('img', { src: `/api/v1/assets/${item.id}/preview?thumb=true`, alt: item.prompt || '生成结果', loading: 'lazy' }), () => imageDetail(item)), node('div', { class: 'image-card-content' }, node('strong', {}, item.model_display_name), node('p', {}, `${item.width} × ${item.height} · ${timestamp(item.created_at)}`), node('div', { class: 'row-actions' }, button('查看任务', () => taskDetail(item.task_id)), downloadButton(item.id)))))));
}
function downloadButton(id) { return can('asset.download_own') ? node('a', { href: `/api/v1/assets/${id}/download`, class: 'button', download: '' }, '下载原图') : node('button', { type: 'button', disabled: true, title: '当前账号没有下载权限，请联系管理员' }, '无下载权限'); }
function imageDetail(item) { dialog('图片预览', node('div', {}, node('img', { class: 'preview-large', src: `/api/v1/assets/${item.id}/preview`, alt: item.prompt || '生成结果' }), node('p', { class: 'muted' }, item.prompt || ''), node('p', { class: 'field-hint' }, `${item.width} × ${item.height} · ${item.mime || ''} · ${item.entry_format_id || ''}`)), [button('关联任务', () => taskDetail(item.task_id)), downloadButton(item.id)]); }
async function taskDetail(id) {
  const content = node('div'); loading(content); dialog('任务详情', content);
  try {
    const [result, upstream] = await Promise.all([api(`/tasks/${id}`), api(`/tasks/${id}/upstream-requests`)]);
    const task = result.task;
    const pairs = [['任务 ID', task.id], ['状态', statusNames[task.status]], ['错误码', task.error_code], ['提示词', task.request.prompt], ['模型', task.model_display_name], ['创建时间', timestamp(task.created_at)], ['开始时间', timestamp(task.started_at)], ['结束时间', timestamp(task.ended_at)], ['排队耗时', duration(task.queue_ms)], ['执行耗时', duration(task.execution_ms)], ['总耗时', duration(task.total_ms ?? task.elapsed_ms)], ['显示时区', Intl.DateTimeFormat().resolvedOptions().timeZone], ['说明', task.message]];
    content.replaceChildren(node('dl', { class: 'detail-grid' }, pairs.flatMap(([name, value]) => [node('dt', {}, name), node('dd', {}, value || '—')])), node('h2', { class: 'section-divider' }, '上游通信记录'));
    if (!upstream.items.length) content.append(node('p', { class: 'field-hint' }, '尚未发起上游请求。'));
    for (const record of upstream.items) content.append(node('div', { class: 'notice' }, node('strong', {}, httpLabel(record)), node('p', {}, record.summary), node('small', {}, `${timestamp(record.started_at)} → ${timestamp(record.ended_at)} · ${duration(record.duration_ms)}${record.upstream_request_id ? ` · request ID: ${record.upstream_request_id}` : ''}`)));
    for (const asset of task.assets) content.append(node('img', { class: 'preview-large', src: `/api/v1/assets/${asset.id}/preview`, alt: '任务生成结果' }));
    find('#modal-footer').replaceChildren(button('刷新详情', () => taskDetail(id)), button('再次生成（可能计费）', () => again(task)), ...task.assets.map(asset => downloadButton(asset.id)));
  } catch (failure) { if (!failure.cancelled) empty(content, '无法加载任务', failure.message); }
}
async function again(task) {
  if (!can('image.generate')) { toast('当前账号没有生成权限，请联系管理员'); return; }
  const action = button('创建新的生成任务', async () => { find('#modal').close(); await navigate('/generate'); find('#prompt').value = task.request.prompt; find('#prompt').dispatchEvent(new Event('input')); state.parentTask = task.id; if (task.request.input_asset_ids?.length) toast('编辑任务需要重新上传原图；再次提交可能产生新的费用'); }, 'primary');
  dialog('再次生成', node('p', {}, '这会创建关联的新任务，可能产生新的上游调用费用。不会覆盖或自动重试原任务。'), [button('取消', () => find('#modal').close()), action]);
}
async function loadGenerate(preserve = true) {
  const load = ++state.generateLoad;
  const previousEntry = find('#entry-select').value;
  const previousModel = find('#model-select').value;
  const previousSize = find('#size-select').value;
  const formats = await api('/entry-formats');
  if (load !== state.generateLoad || state.route !== '/generate') return;
  const selectedEntry = preserve && formats.items.some(item => item.id === previousEntry && item.available) ? previousEntry : formats.items.find(item => item.available)?.id || formats.items[0]?.id || '';
  const models = await allItems(`/models?entry_format_id=${encodeURIComponent(selectedEntry)}`);
  if (load !== state.generateLoad || state.route !== '/generate') return;
  state.formats = formats.items; state.registryRevision = formats.registry_revision;
  const entrySelect = find('#entry-select');
  entrySelect.replaceChildren(...state.formats.map(item => node('option', { value: item.id, disabled: !item.available }, item.name)));
  entrySelect.value = selectedEntry;
  state.models = models;
  renderModelChoices(preserve ? previousModel : '', previousSize);
  const denied = !can('image.generate');
  find('#generation-denied').hidden = !denied;
  find('#generation-denied').textContent = '当前账号没有生成权限，请联系管理员';
  updateGenerateButton();
  const requestedModel = new URLSearchParams(location.search).get('model');
  if (requestedModel && state.user.role === 'admin' && !state.modelTest) {
    state.modelTest = new URLSearchParams(location.search).get('test') === '1';
    const model = await api(`/admin/models/${requestedModel}`);
    if (load !== state.generateLoad || state.route !== '/generate') return;
    if (entrySelect.value !== model.entry_format_id) { const models = await allItems(`/models?entry_format_id=${encodeURIComponent(model.entry_format_id)}`); if (load !== state.generateLoad || state.route !== '/generate') return; entrySelect.value = model.entry_format_id; state.models = models; }
    renderModelChoices(requestedModel);
    toast(state.modelTest ? '真实测试可能计费。请确认参数与原图后主动提交。' : '已选择模型');
  }
  if (state.activeTask) await pollActiveTask();
}
function renderModelChoices(previous = '', size = '') {
  const control = find('#model-select');
  control.replaceChildren(...state.models.map(item => node('option', { value: item.id }, item.display_name)));
  if (state.models.some(item => item.id === previous)) control.value = previous;
  if (!state.models.length) control.append(node('option', { value: '' }, '暂无可用模型，请联系管理员'));
  if (previous && !state.models.some(item => item.id === previous)) { control.prepend(node('option', { value: '' }, '原模型已失效，请重新选择')); control.value = ''; errorText(find('#generate-error'), '模型或入口已失效，请确认后重新选择；提示词草稿已保留。'); }
  const entry = state.formats.find(item => item.id === find('#entry-select').value);
  find('#input-image-field').hidden = entry?.operation !== 'edit';
  find('#entry-reason').textContent = state.models.length ? '' : '暂无可用模型，请联系管理员';
  renderSizes(size);
}
function renderSizes(previous = '') {
  const model = state.models.find(item => item.id === find('#model-select').value);
  find('#size-select').replaceChildren(...(model?.capabilities.sizes || []).map(size => node('option', { value: size }, size)));
  if (model?.capabilities.sizes.includes(previous)) find('#size-select').value = previous;
  else if (model?.capabilities.sizes.includes(model.defaults?.size)) find('#size-select').value = model.defaults.size;
  find('#model-hint').textContent = model ? `已授权 · 配置版本 ${model.revision}` : '只有启用且获授权的模型配置会显示在这里。';
  updateGenerateButton();
}
function updateGenerateButton() {
  const control = find('#generate-submit');
  if (pending.has(find('#generate-form'))) return;
  control.disabled = !can('image.generate') || !state.models.length || !find('#model-select').value || state.uploadPending;
  control.textContent = state.modelTest ? '执行真实测试（可能计费）' : '开始生成';
}
async function uploadInput(event) {
  if (state.uploadPending) { event.target.value = ''; return; }
  const file = event.target.files[0]; state.input = null;
  if (!file) return;
  const model = state.models.find(item => item.id === find('#model-select').value);
  if (file.size > Math.min(26214400, model?.capabilities.max_input_bytes || 26214400)) { errorText(find('#generate-error'), '图片超过平台或模型体积上限'); event.target.value = ''; return; }
  state.uploadPending = true; updateGenerateButton(); find('#upload-status').textContent = '正在上传并校验原图…';
  try { const data = new FormData(); data.append('image', file); state.input = await api('/assets/inputs', { method: 'POST', body: data }); find('#upload-status').textContent = `原图已校验 · ${state.input.width} × ${state.input.height} · ${state.input.mime}`; errorText(find('#generate-error')); }
  catch (failure) { if (!failure.cancelled) { errorText(find('#generate-error'), failure.message); find('#upload-status').textContent = '上传失败，请重新选择图片重试。'; } }
  finally { state.uploadPending = false; updateGenerateButton(); }
}
async function submitGeneration(event) {
  event.preventDefault();
  await busy(find('#generate-form'), async () => {
    errorText(find('#generate-error'));
    try {
      if (!can('image.generate')) throw new Error('当前账号没有生成权限，请联系管理员');
      const model = state.models.find(item => item.id === find('#model-select').value);
      const entry = state.formats.find(item => item.id === find('#entry-select').value);
      if (!model || !entry) throw new Error('暂无可用模型，请联系管理员');
      if (entry.operation === 'edit' && !state.input) throw new Error('请先上传并校验一张原图');
      const body = { entry_format_id: entry.id, model_config_id: model.id, model_revision: model.revision, registry_revision: state.registryRevision, prompt: find('#prompt').value, input_asset_ids: entry.operation === 'edit' ? [state.input.id] : [], output: { size: find('#size-select').value, count: 1 } };
      if (state.parentTask) body.parent_task_id = state.parentTask;
      const serialized = JSON.stringify(body);
      if (state.generationBody !== serialized) { state.generationBody = serialized; state.generationKey = crypto.randomUUID(); }
      if (state.modelTest) body.confirm_cost = true;
      const result = await api(state.modelTest ? `/admin/models/${model.id}/test` : '/tasks', { method: 'POST', headers: { 'Idempotency-Key': state.generationKey }, body: JSON.stringify(body) });
      state.activeTask = result.task_id;
      toast('任务已持久创建；关闭网页不会取消执行'); await pollActiveTask(); schedulePolling();
    } catch (failure) { if (!failure.cancelled) errorText(find('#generate-error'), failure.message); }
  });
  updateGenerateButton();
}
async function pollActiveTask() {
  if (!state.activeTask || !can('task.read_own')) return;
  const { task } = await api(`/tasks/${state.activeTask}`);
  const canvas = find('#result-canvas'); const footer = find('#generation-progress'); footer.hidden = false;
  if (task.assets?.length) {
    const asset = task.assets[0]; if (canvas.dataset.asset !== asset.id) { canvas.replaceChildren(node('img', { class: 'result-image', src: `/api/v1/assets/${asset.id}/preview`, alt: task.request.prompt })); canvas.dataset.asset = asset.id; }
  } else canvas.replaceChildren(node('div', { class: 'canvas-empty' }, terminal.has(task.status) ? null : node('div', { class: 'spinner' }), node('h3', {}, statusNames[task.status]), node('p', {}, task.message || '任务由服务器执行，请稍候。'), node('span', { class: 'hint-pill' }, httpLabel(task.latest_upstream))));
  footer.replaceChildren(node('div', {}, taskBadge(task.status), node('p', { 'data-elapsed-base': task.elapsed_ms, 'data-elapsed-clock': performance.now(), 'data-elapsed-live': String(!terminal.has(task.status)) }, `已耗时 ${duration(task.elapsed_ms)}`), node('p', {}, httpLabel(task.latest_upstream))), node('div', { class: 'row-actions' }, button('任务详情', () => taskDetail(task.id)), ...task.assets.map(asset => downloadButton(asset.id))));
  state.activeTaskTerminal = terminal.has(task.status);
}
function schedulePolling() {
  clearTimeout(state.poll);
  if (!state.user) return;
  if (state.route === '/tasks' && state.rows['/tasks']?.some(task => !terminal.has(task.status)) || state.route === '/generate' && state.activeTask && !state.activeTaskTerminal) state.poll = setTimeout(async () => { try { if (state.route === '/tasks') await loadList('/tasks', true); else await pollActiveTask(); } catch (failure) { if (!failure.cancelled && state.user) errorText(find('#global-error'), failure.message); } schedulePolling(); }, document.hidden ? 10000 : 2000);
}
function loadAccount() {
  find('#account-username').value = state.user.username; find('#account-display').value = state.user.display_name;
  find('#account-permissions').replaceChildren(node('p', {}, `角色：${state.user.role === 'admin' ? '管理员 · 全部业务与管理权限' : '普通用户'}`), ...Object.entries(permissionNames).map(([key, value]) => badge(value, can(key) ? 'success' : '')), node('p', {}, `模型授权：${state.user.model_access === 'all_enabled' || state.user.role === 'admin' ? '全部有效模型' : `指定 ${state.user.model_ids.length} 个模型`}`));
}
function cellTitle(title, subtitle) { return node('div', {}, node('span', { class: 'cell-title' }, title), node('span', { class: 'cell-sub' }, subtitle)); }
function renderUsers(target, items) {
  table(target, ['用户名 / 显示名', '角色', '状态', '模型授权', '创建 / 最近登录', '操作'], items.map(item => [cellTitle(item.username, item.display_name), badge(item.role === 'admin' ? '管理员' : '普通用户'), badge(item.deleted_at ? '已删除' : item.status === 'enabled' ? '启用' : '停用', item.status === 'enabled' && !item.deleted_at ? 'success' : 'error'), item.model_access === 'all_enabled' ? '全部有效模型' : `指定 ${item.model_ids.length} 个`, cellTitle(timestamp(item.created_at), timestamp(item.last_login_at)), item.deleted_at ? '历史责任归属保留' : node('div', { class: 'row-actions' }, button('详情 / 编辑', () => userForm(item)), button('重置密码', () => passwordForm(item)), button(item.status === 'enabled' ? '停用' : '启用', () => confirmAction('修改用户状态', `确认${item.status === 'enabled' ? '停用' : '启用'}用户 ${item.username}？停用将撤销全部会话。`, '确认修改', () => api(`/admin/users/${item.id}`, { method: 'PATCH', body: JSON.stringify({ revision: item.revision, status: item.status === 'enabled' ? 'disabled' : 'enabled' }) }))), button('删除', () => confirmAction('删除用户', `确认删除 ${item.username}？账号将立即失效，会话被撤销；历史任务和图片保留。`, '删除用户', () => api(`/admin/users/${item.id}`, { method: 'DELETE', body: JSON.stringify({ revision: item.revision }) })), 'danger'))]));
}
async function userForm(existing) {
  try {
    const models = await allItems('/admin/models');
    const user = existing ? await api(`/admin/users/${existing.id}`) : { username: '', display_name: '', role: 'user', status: 'enabled', permissions: Object.keys(permissionNames), model_access: 'selected', model_ids: [] };
    const grants = select('model_ids', models.map(model => ({ value: model.id, label: `${model.display_name}${model.enabled ? '' : '（停用）'}` })), undefined, { multiple: true });
    [...grants.options].forEach(option => { option.selected = user.model_ids.includes(option.value); });
    const permissionControls = Object.entries(permissionNames).map(([key, label]) => node('label', { class: 'checkbox-label' }, input('permissions', key, { type: 'checkbox', checked: user.permissions.includes(key) }), label));
    const fields = [field('用户名', input('username', user.username, { required: true, disabled: !!existing, minlength: 3, maxlength: 64, autocomplete: 'off' }), '创建后不可修改；ASCII 小写字母、数字、点、下划线或短横线。'), field('显示名', input('display_name', user.display_name, { required: true, maxlength: 128 })), ...(!existing ? [field('初始密码', input('password', '', { type: 'password', required: true, minlength: 12, maxlength: 128, autocomplete: 'new-password' }), '12–128 个字符，不自动去除空格。')] : []), node('div', { class: 'form-grid' }, field('角色', select('role', [{ value: 'user', label: '普通用户' }, { value: 'admin', label: '管理员' }], user.role)), field('状态', select('status', [{ value: 'enabled', label: '启用' }, { value: 'disabled', label: '停用' }], user.status))), node('h2', {}, '业务权限'), node('div', { class: 'checkbox-stack' }, permissionControls), field('模型授权模式', select('model_access', [{ value: 'selected', label: '指定模型' }, { value: 'all_enabled', label: '全部有效模型' }], user.model_access)), field('指定模型（可多选）', grants, 'selected 集合为空可以保存，但该用户当前没有可用模型。管理员拥有全部有效业务权限。')];
    formDialog(existing ? `用户详情 · ${user.username}` : '新增用户', fields, async form => {
      const data = new FormData(form); const body = { display_name: data.get('display_name'), role: data.get('role'), status: data.get('status'), permissions: data.getAll('permissions'), model_access: data.get('model_access'), model_ids: data.getAll('model_ids') };
      if (existing) body.revision = user.revision; else { body.username = data.get('username'); body.password = data.get('password'); }
      await api(existing ? `/admin/users/${user.id}` : '/admin/users', { method: existing ? 'PATCH' : 'POST', body: JSON.stringify(body) }); find('#modal').close(); toast('用户与授权已原子保存'); await loadList('/admin/users');
    }, existing ? '保存修改' : '创建用户');
  } catch (failure) { if (!failure.cancelled) toast(failure.message); }
}
function passwordForm(user) { formDialog(`重置密码 · ${user.username}`, [node('p', { class: 'notice' }, '成功后将撤销该用户的全部会话。密码不会在详情中回显。'), field('新密码', input('password', '', { type: 'password', required: true, minlength: 12, maxlength: 128, autocomplete: 'new-password' }))], async form => { await api(`/admin/users/${user.id}/reset-password`, { method: 'POST', body: JSON.stringify({ revision: user.revision, password: new FormData(form).get('password') }) }); find('#modal').close(); toast('密码已重置，全部旧会话已撤销'); await loadList('/admin/users'); }, '重置并撤销会话'); }
function renderModels(target, items) {
  table(target, ['模型配置 / 上游标识', '入口格式', '状态', '凭据', '版本', '操作'], items.map(item => [cellTitle(item.display_name, item.upstream_model), item.entry_format_id, badge(item.enabled ? '启用' : '停用', item.enabled ? 'success' : ''), item.has_secret ? '•••••••• · 已配置' : '未配置', `r${item.revision}`, node('div', { class: 'row-actions' }, button('详情 / 编辑', () => modelForm(item)), button('真实测试', () => { state.modelTest = false; navigate(`/generate?model=${item.id}&test=1`); }), button(item.enabled ? '停用' : '启用', () => confirmAction('修改模型状态', `确认${item.enabled ? '停用' : '启用'} ${item.display_name}？停用立即阻止尚未发出的调用。`, '确认修改', () => api(`/admin/models/${item.id}`, { method: 'PATCH', body: JSON.stringify({ revision: item.revision, enabled: !item.enabled }) }))), button('删除', () => confirmAction('删除模型', `确认删除 ${item.display_name}？历史任务保留，后续调用会被禁止。`, '删除模型', () => api(`/admin/models/${item.id}`, { method: 'DELETE', body: JSON.stringify({ revision: item.revision }) })), 'danger'))]));
}
async function modelForm(existing) {
  try {
    const formats = (await api('/entry-formats')).items;
    const model = existing ? await api(`/admin/models/${existing.id}`) : { display_name: '', entry_format_id: formats[0]?.id, upstream_model: '', api_base_url: 'https://api.openai.com/v1/', enabled: false, capabilities: { sizes: ['1024x1024'], input_mimes: ['image/png', 'image/jpeg', 'image/webp'], max_input_bytes: 26214400, max_pixels: 40000000 }, defaults: {} };
    const sizes = node('textarea', { name: 'sizes', id: 'field-sizes', rows: 2, required: true }, model.capabilities.sizes.join(', '));
    const defaults = node('textarea', { name: 'defaults', id: 'field-defaults', rows: 2 }, JSON.stringify(model.defaults, null, 2));
    const mimes = select('input_mimes', ['image/png', 'image/jpeg', 'image/webp'], undefined, { multiple: true }); [...mimes.options].forEach(option => { option.selected = model.capabilities.input_mimes?.includes(option.value); });
    const hasSecret = !!model.has_secret;
    const credentialAction = select('credential_action', [{ value: 'keep', label: '保留已有密钥' }, { value: 'replace', label: hasSecret ? '替换为新密钥' : '写入新密钥' }, { value: 'clear', label: '明确清空密钥' }], hasSecret ? 'keep' : 'replace');
    const credentialInput = input('credential', '', { type: 'password', autocomplete: 'new-password', disabled: hasSecret, required: !hasSecret });
    credentialAction.addEventListener('change', () => { credentialInput.disabled = credentialAction.value !== 'replace'; credentialInput.required = credentialAction.value === 'replace'; if (credentialInput.disabled) credentialInput.value = ''; });
    const fields = [field('显示名', input('display_name', model.display_name, { required: true, maxlength: 128 })), field('入口格式', select('entry_format_id', formats.map(item => ({ value: item.id, label: item.name })), model.entry_format_id)), field('上游真实模型标识', input('upstream_model', model.upstream_model, { required: true }), '按目标账号实际支持的模型填写，不预设未经验证的最新名称。'), field('API 地址前缀', input('api_base_url', model.api_base_url, { type: 'url', required: true }), '例如 https://provider.example/v1/；只拼接一次相对操作路径。内网须由服务器放行。'), field('凭据操作', credentialAction, model.has_secret ? '已有密钥：••••••••。无法读取原值。' : '当前未配置密钥。请选择写入新密钥后填写。'), field('新密钥', credentialInput, '只有选择写入或替换时才可输入并保存；保留不会覆盖旧值。'), field('已验证支持的输出尺寸（逗号分隔）', sizes), field('支持的原图格式（可多选）', mimes), node('div', { class: 'form-grid' }, field('原图上限（字节）', input('max_input_bytes', model.capabilities.max_input_bytes || 26214400, { type: 'number', min: 1, max: 26214400 })), field('像素上限', input('max_pixels', model.capabilities.max_pixels || 40000000, { type: 'number', min: 1, max: 40000000 }))), field('默认参数（JSON 对象）', defaults, '当前仅执行经过入口定义验证的公共字段，不展示未验证的高级参数。'), node('label', { class: 'checkbox-label' }, input('enabled', 'true', { type: 'checkbox', checked: model.enabled }), '启用此模型配置')];
    formDialog(existing ? `模型详情 · ${model.display_name}` : '新增模型配置', fields, async form => {
      const data = new FormData(form); const body = { display_name: data.get('display_name'), entry_format_id: data.get('entry_format_id'), upstream_model: data.get('upstream_model'), api_base_url: data.get('api_base_url'), credential_action: data.get('credential_action'), capabilities: { sizes: data.get('sizes').split(',').map(size => size.trim()).filter(Boolean), input_mimes: data.getAll('input_mimes'), max_input_bytes: Number(data.get('max_input_bytes')), max_pixels: Number(data.get('max_pixels')) }, defaults: JSON.parse(data.get('defaults') || '{}'), enabled: data.has('enabled') };
      if (body.credential_action === 'replace') body.credential = data.get('credential'); if (existing) body.revision = model.revision;
      await api(existing ? `/admin/models/${model.id}` : '/admin/models', { method: existing ? 'PATCH' : 'POST', body: JSON.stringify(body) }); find('#modal').close(); toast('配置校验通过，尚未验证上游'); await loadList('/admin/models');
    });
  } catch (failure) { if (!failure.cancelled) toast(failure.message); }
}
function renderPlugins(target, items) {
  table(target, ['插件 / ID', '版本 / 摘要', '期望状态', '当前生效', '发布状态', '操作'], items.map(item => [cellTitle(item.alias || item.manifest.name, item.plugin_id), cellTitle(item.version, item.digest.slice(0, 16)), badge(item.desired_enabled ? '启用' : '停用'), badge(item.active_enabled ? '启用' : '未启用', item.active_enabled ? 'success' : ''), badge(item.state === 'installed_pending' ? '已安装，待生效' : '已发布', item.state === 'installed_pending' ? 'pending' : 'success'), node('div', { class: 'row-actions' }, button('详情 / 配置', () => pluginForm(item)), ...(item.state === 'installed_pending' ? [button('刷新生效', () => publishDialog(item))] : []), button('移除', () => confirmAction('移除插件', `确认移除 ${item.manifest.name} ${item.version}？必须已停用并发布，且没有模型或在途任务引用。历史结果保留。`, '移除插件', () => api(`/admin/plugins/${item.id}`, { method: 'DELETE', body: JSON.stringify({ revision: item.revision }) })), 'danger'))]));
}
function installPlugin() {
  formDialog('安装入口插件', [node('p', { class: 'notice' }, '仅安装受限声明式 .npplugin 包。不执行脚本、原生库或任意代码；包上限 10 MiB。安装成功后再选择是否发布。'), field('插件包', input('package', '', { type: 'file', accept: '.npplugin,.zip', required: true })), node('label', { class: 'checkbox-label' }, input('approve_no_auth', 'true', { type: 'checkbox' }), '明确批准该包使用无认证 HTTP 入口（仅适用于受信任服务）')], async form => {
    const result = await api('/admin/plugins/install', { method: 'POST', body: new FormData(form) });
    await loadList('/admin/plugins');
    const plugin = await api(`/admin/plugins/${result.id}`);
    if (plugin) publishDialog(plugin, true); else { find('#modal').close(); toast(result.message); }
  }, '验证并安装');
}
function publishDialog(plugin, installed = false) {
  const requestId = crypto.randomUUID();
  const expectedRevision = Number(find('#registry-label').textContent.match(/revision (\d+)/)?.[1]);
  const error = node('p', { class: 'error-text', role: 'alert', hidden: true });
  const publish = button('立即刷新生效', () => busy(publish, async () => {
    try { const result = await api('/admin/registry/refresh', { method: 'POST', body: JSON.stringify({ expected_registry_revision: expectedRevision, request_id: requestId, changes: [{ id: plugin.id, revision: plugin.revision }] }) }); find('#modal').close(); toast(`已发布入口注册表 revision ${result.registry_revision}`); await loadList('/admin/plugins'); }
    catch (failure) { if (!failure.cancelled) errorText(error, failure.message); }
  }), 'primary');
  dialog(installed ? '插件已安装，尚未生效' : '确认发布指定变更', node('div', {}, node('p', {}, '本次仅发布下面这项变更，不会顺带激活其他待生效插件。在途任务继续使用原快照。'), node('div', { class: 'notice' }, `${plugin.manifest.name} · ${plugin.version} · ${plugin.desired_enabled ? '启用' : '停用'} · 配置 r${plugin.revision}`), node('p', { class: 'field-hint' }, '稍后刷新将保留待生效状态；关闭页面、F5 和服务重启都不会自动激活。'), error), [button('稍后刷新', () => { find('#modal').close(); toast('已保留待生效状态'); }), publish]);
}
async function pluginForm(existing) {
  try {
    const plugin = await api(`/admin/plugins/${existing.id}`);
    formDialog(`插件详情 · ${plugin.manifest.name}`, [node('div', { class: 'notice' }, `ID: ${plugin.plugin_id} · ${plugin.version} · digest: ${plugin.digest}`), node('p', {}, plugin.manifest.description), node('pre', {}, JSON.stringify({ manifest: plugin.manifest, entries: plugin.entries.map(entry => ({ id: entry.id, operation: entry.operation })), current_state: plugin.state, published_revision: plugin.published_revision }, null, 2)), field('管理员显示别名', input('alias', plugin.alias, { maxlength: 128 })), field('备注', node('textarea', { name: 'notes', id: 'field-notes', rows: 2, maxlength: 4096 }, plugin.notes)), field('插件配置（JSON）', node('textarea', { name: 'config', id: 'field-config', rows: 4 }, JSON.stringify(plugin.config, null, 2)), '按插件 Schema 校验。保存只更新期望配置，之后需明确发布。'), node('label', { class: 'checkbox-label' }, input('desired_enabled', 'true', { type: 'checkbox', checked: plugin.desired_enabled }), '期望启用（保存待刷新）')], async form => {
      const data = new FormData(form); await api(`/admin/plugins/${plugin.id}`, { method: 'PATCH', body: JSON.stringify({ revision: plugin.revision, alias: data.get('alias'), notes: data.get('notes'), config: JSON.parse(data.get('config')), desired_enabled: data.has('desired_enabled') }) }); find('#modal').close(); toast('配置已保存待刷新，生效状态未改变'); await loadList('/admin/plugins');
    }, '保存待刷新');
  } catch (failure) { if (!failure.cancelled) toast(failure.message); }
}
function themeState() { const preference = document.documentElement.dataset.themePreference || 'system'; all('[data-theme-value]').forEach(control => control.setAttribute('aria-pressed', String(control.dataset.themeValue === preference))); }
function bindEvents() {
  find('#login-form').addEventListener('submit', event => {
    event.preventDefault(); busy(event.currentTarget, async () => {
      errorText(find('#login-error'));
      try { const result = await api('/auth/login', { method: 'POST', public: true, body: JSON.stringify({ username: find('#login-username').value, password: find('#login-password').value }) }); await identity(result); find('#login-password').value = ''; find('#login-page').hidden = true; find('#app-shell').hidden = false; await navigate('/generate', true); registryWatch(); }
      catch (failure) { if (!failure.cancelled) errorText(find('#login-error'), failure.message); }
    });
  });
  find('#password-toggle').addEventListener('click', event => { const control = find('#login-password'); control.type = control.type === 'password' ? 'text' : 'password'; event.currentTarget.textContent = control.type === 'password' ? '显示' : '隐藏'; event.currentTarget.setAttribute('aria-label', control.type === 'password' ? '显示密码' : '隐藏密码'); });
  find('#theme-toggle').addEventListener('click', () => { const menu = find('#theme-menu'); menu.hidden = !menu.hidden; find('#theme-toggle').setAttribute('aria-expanded', String(!menu.hidden)); themeState(); });
  all('[data-theme-value]').forEach(control => control.addEventListener('click', () => { globalThis.setNewPhotoTheme(control.dataset.themeValue); themeState(); find('#theme-menu').hidden = true; find('#theme-toggle').setAttribute('aria-expanded', 'false'); }));
  document.addEventListener('click', event => { if (!event.target.closest('.theme-control')) { find('#theme-menu').hidden = true; find('#theme-toggle').setAttribute('aria-expanded', 'false'); } });
  document.addEventListener('keydown', event => { if (event.key === 'Escape') { find('#theme-menu').hidden = true; closeDrawer(); } });
  find('#modal-close').addEventListener('click', () => find('#modal').close());
  find('#modal').addEventListener('close', () => { if (!find('#modal').open) { find('#modal-content').replaceChildren(); find('#modal-footer').replaceChildren(); } });
  find('#drawer-toggle').addEventListener('click', () => { find('#sidebar').classList.toggle('open'); find('#drawer-scrim').hidden = !find('#sidebar').classList.contains('open'); });
  find('#drawer-scrim').addEventListener('click', closeDrawer);
  find('#logout-button').addEventListener('click', event => busy(event.currentTarget, async () => {
    try { await api('/auth/logout', { method: 'POST', body: '{}', public: true }); goLogin(); toast('已退出登录'); }
    catch (failure) { if (failure.status === 401) goLogin(); else errorText(find('#global-error'), `退出失败，请重试：${failure.message}`); }
  }));
  document.addEventListener('click', event => {
    const anchor = event.target.closest('a[href^="/"]');
    if (anchor && !anchor.getAttribute('href').startsWith('/api/') && !event.ctrlKey && !event.metaKey) { event.preventDefault(); navigate(anchor.getAttribute('href')); }
    const action = event.target.closest('[data-action="reload"]'); if (action) busy(action, () => loadPage());
    if (event.target.closest('#new-user')) userForm();
    if (event.target.closest('#new-model')) modelForm();
    if (event.target.closest('#install-plugin')) installPlugin();
  });
  all('input[type=search]').forEach(control => { let timer; control.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(() => { state.page[state.route] = 1; loadList(state.route); }, 300); }); });
  for (const id of ['tasks-all', 'images-all']) find(`#${id}`).addEventListener('change', () => { state.page[state.route] = 1; loadList(state.route); });
  document.addEventListener('change', event => { if (event.target.id === 'users-deleted') { state.page[state.route] = 1; loadList(state.route); } });
  find('#entry-select').addEventListener('change', async () => { const load = ++state.generateLoad; const entry = find('#entry-select').value; state.input = null; find('#input-image').value = ''; errorText(find('#generate-error')); try { const models = await allItems(`/models?entry_format_id=${encodeURIComponent(entry)}`); if (load !== state.generateLoad || state.route !== '/generate') return; state.models = models; renderModelChoices(); } catch (failure) { if (!failure.cancelled) errorText(find('#generate-error'), failure.message); } });
  find('#model-select').addEventListener('change', () => { state.generateLoad += 1; errorText(find('#generate-error')); renderSizes(); });
  find('#input-image').addEventListener('change', uploadInput);
  find('#prompt').addEventListener('input', event => { find('#prompt-count').textContent = `${[...event.target.value].length} / 4000`; });
  find('#generate-form').addEventListener('submit', submitGeneration);
  find('#account-form').addEventListener('submit', event => { event.preventDefault(); busy(event.currentTarget, async () => { errorText(find('#account-error')); try { const result = await api('/me', { method: 'PATCH', body: JSON.stringify({ display_name: find('#account-display').value, revision: state.user.revision }) }); await identity({ user: result.user, csrf_token: state.csrf }); toast('账号资料已保存'); } catch (failure) { if (!failure.cancelled) errorText(find('#account-error'), failure.message); } }); });
  window.addEventListener('popstate', () => { if (state.user) loadPage(); else goLogin(); });
  document.addEventListener('visibilitychange', async () => { if (!document.hidden && state.user) { await loadPage(); } else schedulePolling(); });
  window.addEventListener('focus', async () => { if (state.user) await loadPage(); });
  setInterval(() => { all('[data-elapsed-live="true"]').forEach(target => { const elapsed = Number(target.dataset.elapsedBase) + Math.max(0, performance.now() - Number(target.dataset.elapsedClock)); target.textContent = duration(elapsed); }); }, 1000);
}
function registryWatch() {
  clearTimeout(state.registryPoll);
  if (!state.user) return;
  state.registryPoll = setTimeout(async () => {
    try { await identity(await api('/auth/me')); if (state.route === '/generate') { const result = await api('/entry-formats'); if (result.registry_revision !== state.registryRevision) { await loadGenerate(); toast('入口注册表已更新；仍有效的创作草稿已保留'); } } }
    catch (failure) { if (!failure.cancelled && state.user) errorText(find('#global-error'), failure.message); }
    registryWatch();
  }, 15000);
}
export async function bootNewPhoto() {
  bindEvents(); themeState();
  try { await identity(await api('/auth/me', { public: true })); find('#boot-screen').hidden = true; find('#app-shell').hidden = false; await navigate(location.pathname === '/login' || location.pathname === '/' ? '/generate' : location.pathname + location.search, true); registryWatch(); }
  catch (failure) { goLogin(); if (failure.status !== 401) errorText(find('#login-error'), `无法检查会话：${failure.message}`); }
}
