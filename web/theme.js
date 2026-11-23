// 在页面绘制前应用已保存的界面偏好，避免闪烁：主题（light / dark；未设置时跟随系统）和树页的导航是否固定展开。
(() => {
  try {
    const saved = localStorage.getItem('rp-theme');
    if (saved === 'light' || saved === 'dark') document.documentElement.dataset.theme = saved;
    if (localStorage.getItem('rp-nav') === 'wide') document.documentElement.dataset.nav = 'wide';
  } catch { /* 隐私模式等情况下忽略 */ }
})();
