(() => {
  let preference = 'system';
  try { const saved = localStorage.getItem('newphoto.theme'); if (['light', 'dark', 'system'].includes(saved)) preference = saved; } catch {}
  const media = matchMedia('(prefers-color-scheme: dark)');
  function apply() { document.documentElement.dataset.theme = preference === 'system' ? (media.matches ? 'dark' : 'light') : preference; document.documentElement.dataset.themePreference = preference; }
  globalThis.setNewPhotoTheme = value => { if (!['light', 'dark', 'system'].includes(value)) return; preference = value; try { localStorage.setItem('newphoto.theme', value); } catch {} apply(); };
  media.addEventListener('change', apply);
  apply();
})();
