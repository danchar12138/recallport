// 在页面绘制前应用已保存的界面偏好，避免闪烁：主题（light / dark；未设置时跟随系统）和导航是否展开（宽屏上默认展开，收成图标条后记住；手机上的导航是临时抽屉，不记）。
(() => {
  try {
    const saved = localStorage.getItem('rp-theme');
    if (saved === 'light' || saved === 'dark') document.documentElement.dataset.theme = saved;
    if (localStorage.getItem('rp-nav') !== 'rail' && innerWidth >= 768) document.documentElement.dataset.nav = 'open';
  } catch { /* 隐私模式等情况下忽略 */ }
})();
