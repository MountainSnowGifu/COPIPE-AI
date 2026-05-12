(function () {
  const el = window.__copipeFindInput
    ? window.__copipeFindInput()
    : document.querySelector(
        '#userInput, textarea, [contenteditable="true"][role="textbox"], [role="textbox"][contenteditable="true"]',
      );
  if (!el) return;
  const r = el.getBoundingClientRect();
  const tx = r.left + r.width * (0.35 + Math.random() * 0.3);
  const ty = r.top + r.height * (0.35 + Math.random() * 0.3);
  const sx = window.innerWidth * (0.3 + Math.random() * 0.4);
  const sy = window.innerHeight * (0.3 + Math.random() * 0.4);
  const cx =
    sx + (tx - sx) * (0.3 + Math.random() * 0.4) + (Math.random() - 0.5) * 120;
  const cy =
    sy + (ty - sy) * (0.3 + Math.random() * 0.4) + (Math.random() - 0.5) * 80;
  const steps = 12 + Math.floor(Math.random() * 8);
  for (let i = 0; i <= steps; i++) {
    const t = i / steps;
    const u = 1 - t;
    const mx = u * u * sx + 2 * u * t * cx + t * t * tx;
    const my = u * u * sy + 2 * u * t * cy + t * t * ty;
    el.dispatchEvent(
      new MouseEvent("mousemove", { bubbles: true, clientX: mx, clientY: my }),
    );
    // カーソルが要素に近づいたとき mouseover/mouseenter も発火（リアルなホバー挙動）
    if (i === Math.floor(steps * 0.7)) {
      el.dispatchEvent(
        new MouseEvent("mouseover", {
          bubbles: true,
          clientX: mx,
          clientY: my,
        }),
      );
      el.dispatchEvent(
        new MouseEvent("mouseenter", {
          bubbles: false,
          clientX: mx,
          clientY: my,
        }),
      );
    }
  }
  const mo = {
    bubbles: true,
    cancelable: true,
    clientX: tx,
    clientY: ty,
    button: 0,
  };
  el.dispatchEvent(new MouseEvent("mousedown", mo));
  el.dispatchEvent(new MouseEvent("mouseup", mo));
  el.dispatchEvent(new MouseEvent("click", mo));
  el.focus();
})();
