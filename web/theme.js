// 在页面绘制前应用已保存的主题，避免闪烁。选项为 light / dark；未设置时跟随系统。
(() => {
  try {
    const saved = localStorage.getItem('rp-theme');
    if (saved === 'light' || saved === 'dark') document.documentElement.dataset.theme = saved;
  } catch { /* 隐私模式等情况下忽略 */ }
})();
