// 在页面绘制前应用已保存的界面偏好，避免闪烁：主题（light / dark；未设置时跟随系统）和导航是否展开（树页和手机上的导航是临时盖在内容上的抽屉，不记）。
(() => {
  try {
    const saved = localStorage.getItem('rp-theme');
    if (saved === 'light' || saved === 'dark') document.documentElement.dataset.theme = saved;
    if (localStorage.getItem('rp-nav') === 'open' && location.pathname !== '/tree' && innerWidth >= 768) document.documentElement.dataset.nav = 'open';
  } catch { /* 隐私模式等情况下忽略 */ }
})();
