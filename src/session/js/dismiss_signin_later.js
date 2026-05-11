(function () {
  const labels = ["\u5f8c\u3067", "Later", "Not now", "Skip for now"];
  const providerLabels = [
    "Microsoft \u3067\u7d9a\u884c",
    "Apple \u3067\u7d9a\u884c",
    "Google \u3067\u7d9a\u884c",
  ];

  const visible = (el) => {
    if (!el) return false;
    const style = window.getComputedStyle(el);
    if (style.visibility === "hidden" || style.display === "none") return false;
    const r = el.getBoundingClientRect();
    return r.width > 4 && r.height > 4;
  };

  const textOf = (el) => (el?.innerText || el?.textContent || "").replace(/\s+/g, " ").trim();
  const isInteractive = (el) =>
    el.matches('button, [role="button"], a, [tabindex]:not([tabindex="-1"])');
  const interactiveFor = (el) =>
    el.closest('button, [role="button"], a, [tabindex]:not([tabindex="-1"])') || el;

  const candidates = [
    ...document.querySelectorAll('button, [role="button"], a, [tabindex]:not([tabindex="-1"])'),
    ...document.querySelectorAll("div, span"),
  ];

  let target = null;
  for (const el of candidates) {
    if (!visible(el)) continue;
    const text = textOf(el);
    const interactive = isInteractive(el);
    if (!labels.some((label) => text === label || (interactive && text.includes(label)))) continue;
    if (!interactive && text.length > 40) continue;

    const modalText = textOf(el.closest('[role="dialog"], [aria-modal="true"], main, body'));
    if (
      providerLabels.some((label) => modalText.includes(label)) ||
      modalText.toLowerCase().includes("sign in")
    ) {
      target = interactiveFor(el);
      break;
    }
  }

  if (!target || !visible(target)) return null;

  target.scrollIntoView({ block: "center", inline: "center", behavior: "smooth" });
  const r = target.getBoundingClientRect();
  const tx = r.left + r.width * (0.45 + Math.random() * 0.1);
  const ty = r.top + r.height * (0.45 + Math.random() * 0.1);
  const sx = Math.max(8, Math.min(window.innerWidth - 8, tx + (Math.random() - 0.5) * 180));
  const sy = Math.max(8, Math.min(window.innerHeight - 8, ty - 80 - Math.random() * 80));
  const steps = 9 + Math.floor(Math.random() * 7);

  for (let i = 0; i <= steps; i++) {
    const t = i / steps;
    const ease = t * t * (3 - 2 * t);
    const x = sx + (tx - sx) * ease + (Math.random() - 0.5) * 2;
    const y = sy + (ty - sy) * ease + (Math.random() - 0.5) * 2;
    target.dispatchEvent(new MouseEvent("mousemove", { bubbles: true, clientX: x, clientY: y }));
  }

  const opts = { bubbles: true, cancelable: true, clientX: tx, clientY: ty, button: 0 };
  for (const ev of ["pointerover", "pointerenter", "pointerdown", "pointerup"]) {
    try {
      target.dispatchEvent(new PointerEvent(ev, opts));
    } catch (_) {}
  }
  for (const ev of ["mouseover", "mouseenter", "mousedown", "mouseup", "click"]) {
    target.dispatchEvent(new MouseEvent(ev, opts));
  }

  return "clicked:" + textOf(target).slice(0, 40);
})();
