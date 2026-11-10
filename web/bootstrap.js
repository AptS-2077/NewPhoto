import init, { set_role } from './pkg/newphoto.js';
import { bootNewPhoto } from './app.js';
globalThis.bootNewPhoto = bootNewPhoto;
globalThis.setNewPhotoRole = set_role;
try { await init(); } catch { document.body.textContent = 'NewPhoto 加载失败，请刷新重试。若问题持续，请检查静态资源与 WebAssembly 配置。'; }
