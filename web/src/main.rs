use leptos::prelude::*;
use std::cell::RefCell;
use wasm_bindgen::prelude::*;

thread_local! { static ROLE: RefCell<Option<RwSignal<Option<bool>>>> = const { RefCell::new(None) }; }
#[wasm_bindgen]
pub fn set_role(role: &str) {
    ROLE.with(|slot| {
        if let Some(signal) = *slot.borrow() {
            signal.set(if role.is_empty() {
                None
            } else {
                Some(role == "admin")
            });
        }
    });
}
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name=bootNewPhoto)]
    fn boot_newphoto();
}

#[component]
fn Icon(#[prop(default = "image")] kind: &'static str) -> impl IntoView {
    let path = match kind {
        "spark" => "M12 3l2.8 6.2L21 12l-6.2 2.8L12 21l-2.8-6.2L3 12l6.2-2.8L12 3z",
        "task" => "M9 5H5v16h14V5h-4M9 3h6v4H9zM8 12h8M8 16h5",
        "user" => "M20 21v-2a7 7 0 0 0-14 0v2M12 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8z",
        "model" => "M12 3l9 5-9 5-9-5 9-5zM3 12l9 5 9-5M3 16l9 5 9-5",
        "plugin" => "M8 3v5H3v8h5v5h8v-5h5V8h-5V3H8z",
        "sun" => {
            "M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v2M12 20v2M2 12h2M20 12h2M5 5l1 1M18 18l1 1M5 19l1-1M18 6l1-1"
        }
        "menu" => "M4 6h16M4 12h16M4 18h16",
        "logout" => "M9 4H4v16h5M13 8l4 4-4 4M8 12h13",
        _ => "M4 4h16v16H4zM4 16l5-5 4 4 3-3 4 4M15 8h.01",
    };
    view! {<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d=path/></svg>}
}
#[component]
fn Brand() -> impl IntoView {
    view! {<div class="brand"><span class="brand-mark"><Icon kind="spark"/></span><span>"NewPhoto"<small>"IMAGE WORKSPACE"</small></span></div>}
}
#[component]
fn App() -> impl IntoView {
    let role = RwSignal::new(None::<bool>);
    ROLE.with(|slot| *slot.borrow_mut() = Some(role));
    view! {
        <div class="theme-control"><button id="theme-toggle" class="icon-button" aria-label="切换主题" aria-expanded="false"><Icon kind="sun"/></button><div id="theme-menu" class="theme-menu" hidden><p>"外观"</p><button data-theme-value="light">"明亮"</button><button data-theme-value="dark">"暗黑"</button><button data-theme-value="system">"跟随系统"</button></div></div>
        <div id="boot-screen" class="boot-screen"><Brand/><p class="muted">"正在确认登录状态…"</p></div>
        <section id="login-page" class="login-page" hidden><div class="login-card"><Brand/><h1>"欢迎回来"</h1><p class="muted">"登录你的图片工作台，继续创造。"</p><form id="login-form"><label for="login-username">"用户名"</label><input id="login-username" name="username" autocomplete="username" required minlength="3" maxlength="64" placeholder="请输入用户名"/><label for="login-password">"密码"</label><div class="password-field"><input id="login-password" type="password" name="password" autocomplete="current-password" required maxlength="128" placeholder="请输入密码"/><button type="button" id="password-toggle" aria-label="显示密码">"显示"</button></div><p id="login-error" role="alert" class="error-text" hidden></p><button type="submit" class="primary full">"登录工作台"</button></form><div class="login-foot">"账号由管理员创建 · 安全的私有工作空间"</div></div><footer>"NewPhoto · 让每一个想法都有画面"</footer></section>
        <div id="app-shell" hidden>
            <button id="drawer-scrim" class="drawer-scrim" aria-label="关闭导航" hidden></button>
            <aside id="sidebar" class="sidebar"><a href="/generate" class="brand-link"><Brand/></a><div class="workspace-label">"个人工作空间"<span>"V1"</span></div><nav aria-label="主导航"><p class="nav-label">"工作台"</p><a href="/generate" data-route="/generate"><Icon kind="spark"/><span>"图片生成"</span></a><a href="/tasks" data-route="/tasks"><Icon kind="task"/><span>"生成任务"</span><span id="active-count" class="nav-count" hidden></span></a><a href="/images" data-route="/images"><Icon/><span>"图片记录"</span></a><a href="/account" data-route="/account"><Icon kind="user"/><span>"个人信息"</span></a><Show when=move||role.get()==Some(true)><div id="admin-navigation"><p class="nav-label admin-label">"管理"</p><a href="/admin/models" data-route="/admin/models"><Icon kind="model"/><span>"模型管理"</span></a><a href="/admin/users" data-route="/admin/users"><Icon kind="user"/><span>"用户管理"</span></a><a href="/admin/plugins" data-route="/admin/plugins"><Icon kind="plugin"/><span>"插件管理"</span></a></div></Show></nav><div class="sidebar-bottom"><div class="identity"><span id="user-avatar" class="avatar">"N"</span><div><strong id="sidebar-name"></strong><small id="sidebar-role"></small></div><span class="online-dot" aria-label="已登录"></span></div><button id="logout-button" class="logout-button"><Icon kind="logout"/>"退出登录"</button></div></aside>
            <header class="topbar"><button id="drawer-toggle" class="icon-button mobile-only" aria-label="打开导航"><Icon kind="menu"/></button><div class="breadcrumbs"><span>"工作空间"</span><span>"/"</span><strong id="breadcrumb-title">"图片生成"</strong></div><div class="topbar-right"><span class="private-badge">"私有工作台"</span><span id="topbar-username"></span></div></header>
            <main class="main-content"><div id="global-error" class="notice error-notice" role="alert" hidden></div>
                <GeneratePanel/><TasksPanel/><ImagesPanel/><AccountPanel/>
                <Show when=move||role.get()==Some(true)><AdminModelsPanel/><AdminUsersPanel/><AdminPluginsPanel/></Show>
                <section data-page="forbidden" hidden><div class="page-heading"><h1>"403 · 无权访问"</h1></div><div class="empty-state"><Icon kind="user"/><h2>"此页面仅限管理员"</h2><p>"你的账号没有管理权限。通用工作台仍可使用。"</p><a href="/generate" class="button">"返回工作台"</a></div></section>
                <section data-page="not-found" hidden><div class="empty-state"><h1>"404 · 页面不存在"</h1><a href="/generate" class="button">"返回工作台"</a></div></section>
            </main>
        </div>
        <dialog id="modal"><div class="dialog-header"><h2 id="modal-title"></h2><button id="modal-close" class="icon-button" aria-label="关闭对话框">"×"</button></div><div id="modal-content" class="dialog-content"></div><div id="modal-footer" class="dialog-footer"></div></dialog><div id="toast" class="toast" role="status" hidden></div>
    }
}
#[component]
fn GeneratePanel() -> impl IntoView {
    view! {
        <section data-page="/generate" hidden><div class="page-heading"><div><div class="eyebrow">"CREATE WITH INTENTION"</div><h1>"图片生成"</h1><p>"把想法变成画面。选择入口和模型，开始一次新的创作。"</p></div><a href="/images" class="button subtle">"查看图片记录 →"</a></div><div id="generation-denied" class="notice" hidden></div><div class="generate-layout"><div class="panel generation-controls"><div class="panel-heading"><h2>"创作设置"</h2><span class="micro-label">"01 / INPUT"</span></div><form id="generate-form"><label for="entry-select">"入口格式"</label><select id="entry-select" required></select><p id="entry-reason" class="field-hint"></p><label for="model-select">"模型配置"</label><select id="model-select" required></select><p id="model-hint" class="field-hint"></p><div id="input-image-field" hidden><label for="input-image">"原图"<span class="required">" *"</span></label><input id="input-image" type="file" accept="image/png,image/jpeg,image/webp"/><p id="upload-status" class="field-hint">"上传一张真实原图，最多 25 MiB / 40 MP；实际限制以模型为准。"</p></div><div class="label-row"><label for="prompt">"提示词"</label><span id="prompt-count" class="muted">"0 / 4000"</span></div><textarea id="prompt" rows="6" maxlength="4000" required placeholder="描述你想要的画面：主体、场景、风格、光线…"></textarea><div class="form-grid"><div><label for="size-select">"输出尺寸"</label><select id="size-select" required></select></div><div><label for="image-count">"生成数量"</label><input id="image-count" value="1 张" disabled/></div></div><p id="generate-error" class="error-text" role="alert" hidden></p><button id="generate-submit" type="submit" class="primary full"><Icon kind="spark"/>"开始生成"</button><p class="submit-hint">"图片由服务器保存 · 关闭页面不会取消任务"</p></form></div><div class="panel canvas-panel"><div class="panel-heading"><h2>"生成结果"</h2><span class="micro-label">"02 / OUTPUT"</span></div><div id="result-canvas" class="result-canvas"><div class="canvas-empty"><div class="canvas-symbol"><Icon/></div><h3>"你的下一幅作品，从这里开始"</h3><p>"填写提示词并提交，结果会显示在这里。"</p><span class="hint-pill">"生成 · 编辑 · 私有保存"</span></div></div><div id="generation-progress" class="canvas-footer" hidden></div></div></div><div class="workspace-note"><span class="online-dot"></span>"真实任务与上游状态 · 不模拟进度，不自动重复生成"</div></section>
    }
}
#[component]
fn TasksPanel() -> impl IntoView {
    view! {<section data-page="/tasks" hidden><div class="page-heading"><div><h1>"生成任务"</h1><p>"查看真实进度、耗时与每一次上游通信记录。"</p></div><button data-action="reload" class="button">"刷新列表"</button></div><div class="panel"><div class="table-toolbar"><input id="tasks-search" type="search" aria-label="搜索任务提示词" placeholder="搜索提示词或状态说明…"/><label id="tasks-all-wrap" class="checkbox-label" hidden><input id="tasks-all" type="checkbox"/>"全部用户"</label></div><div id="tasks-content"></div><div id="tasks-pagination" class="pagination"></div></div></section>}
}
#[component]
fn ImagesPanel() -> impl IntoView {
    view! {<section data-page="/images" hidden><div class="page-heading"><div><h1>"图片记录"</h1><p>"每一次创作，都保存在你的私有工作空间。"</p></div><a href="/generate" class="button primary">"新建创作"</a></div><div class="table-toolbar"><input id="images-search" type="search" aria-label="搜索图片提示词" placeholder="搜索提示词…"/><label id="images-all-wrap" class="checkbox-label" hidden><input id="images-all" type="checkbox"/>"全部用户"</label></div><div id="images-content"></div><div id="images-pagination" class="pagination"></div></section>}
}
#[component]
fn AccountPanel() -> impl IntoView {
    view! {<section data-page="/account" hidden><div class="page-heading"><div><h1>"个人信息"</h1><p>"管理显示名称，查看当前账号与权限。"</p></div></div><div class="panel account-panel"><h2>"账号资料"</h2><form id="account-form"><label for="account-username">"用户名"</label><input id="account-username" disabled/><label for="account-display">"显示名"</label><input id="account-display" required maxlength="128"/><div id="account-permissions" class="permission-summary"></div><p id="account-error" class="error-text" role="alert" hidden></p><button type="submit" class="primary">"保存资料"</button></form></div></section>}
}
#[component]
fn AdminModelsPanel() -> impl IntoView {
    view! {<section data-page="/admin/models" hidden><div class="page-heading"><div><h1>"模型管理"</h1><p>"将入口格式绑定到经过验证的上游模型配置。"</p></div><button id="new-model" class="primary">"＋ 新增模型"</button></div><div class="notice">"配置校验不等于上游验证。真实测试必须主动执行，且可能产生费用。"</div><div class="panel"><div class="table-toolbar"><input id="models-search" type="search" aria-label="搜索模型" placeholder="搜索模型显示名…"/><button data-action="reload" class="button">"刷新"</button></div><div id="models-content"></div><div id="models-pagination" class="pagination"></div></div></section>}
}
#[component]
fn AdminUsersPanel() -> impl IntoView {
    view! {<section data-page="/admin/users" hidden><div class="page-heading"><div><h1>"用户管理"</h1><p>"管理团队账号、业务权限与模型授权。"</p></div><button id="new-user" class="primary">"＋ 新增用户"</button></div><div class="panel"><div class="table-toolbar"><input id="users-search" type="search" aria-label="搜索用户名或显示名" placeholder="搜索用户名或显示名…"/><label class="checkbox-label"><input id="users-deleted" type="checkbox"/>"包含已删除"</label><button data-action="reload" class="button">"刷新"</button></div><div id="users-content"></div><div id="users-pagination" class="pagination"></div></div></section>}
}
#[component]
fn AdminPluginsPanel() -> impl IntoView {
    view! {<section data-page="/admin/plugins" hidden><div class="page-heading"><div><h1>"插件管理"</h1><p>"受限声明式入口，安装与生效分开管理。"</p></div><button id="install-plugin" class="primary">"＋ 安装插件"</button></div><div class="notice"><strong id="registry-label">"入口注册表"</strong>" · 稍后刷新不会自动启用。发布仅作用于明确选中的变更。"</div><div class="panel"><div class="panel-heading"><h2>"已安装插件"</h2><button data-action="reload" class="button">"刷新列表"</button></div><div id="plugins-content"></div><div id="plugins-pagination" class="pagination"></div></div><div class="workspace-note">"内置 OpenAI Image Generate / Edit 随应用提供，不属于可卸载插件。"</div></section>}
}
fn main() {
    leptos::mount::mount_to_body(App);
    boot_newphoto();
}
